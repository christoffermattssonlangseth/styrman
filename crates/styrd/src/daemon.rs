//! Daemon state: the session registry, derived status, and the event bus.
//!
//! Privacy rule: until a session is shared (`/styr`), only its lifecycle
//! (`SessionStarted`, `SessionEnded`, `SharingChanged`) is persisted, so local
//! `styr status` can list it. Content events of unshared sessions update the
//! in-memory status and are otherwise dropped; nothing of them reaches the phone.

use crate::store::{SessionRecord, Store};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use styr_core::StatusTracker;
use styr_core::adapter::{AgentAdapter, HookDecision, RawHook, SessionView};
use styr_core::event::{Event, EventKind, EventSource, PermissionOutcome, Resolver, now_ms};
use styr_core::ids::{AgentKind, NativeSessionRef, SessionId};
use styr_core::ipc::{Request, Response, SessionSummary, SessionUpdate};
use tokio::sync::broadcast;

pub struct Daemon {
    inner: Mutex<Inner>,
    bus: broadcast::Sender<Event>,
    adapters: HashMap<AgentKind, Arc<dyn AgentAdapter>>,
}

struct Inner {
    store: Store,
    trackers: HashMap<SessionId, StatusTracker>,
}

fn always_persisted(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::SessionStarted { .. }
            | EventKind::SessionEnded { .. }
            | EventKind::SharingChanged { .. }
    )
}

impl Daemon {
    /// Load state from the store. Permission requests left open by a previous
    /// daemon are resolved as cancelled: their hook processes saw EOF and failed open.
    pub fn open(store: Store, adapters: Vec<Arc<dyn AgentAdapter>>) -> Result<Arc<Self>> {
        let mut inner = Inner {
            store,
            trackers: HashMap::new(),
        };
        let now = now_ms();
        for rec in inner.store.sessions()? {
            let kinds = inner.store.event_kinds(&rec.id)?;
            let mut tracker = StatusTracker::replay(&kinds);
            let dangling: Vec<_> = tracker.open_requests().cloned().collect();
            for request in dangling {
                let kind = EventKind::PermissionResolved {
                    request,
                    outcome: PermissionOutcome::Cancelled,
                    by: Resolver::Daemon,
                };
                tracker.apply(&kind);
                inner
                    .store
                    .append(&rec.id, EventSource::Daemon, kind, now)?;
            }
            inner.trackers.insert(rec.id.clone(), tracker);
        }
        let (bus, _) = broadcast::channel(1024);
        Ok(Arc::new(Self {
            inner: Mutex::new(inner),
            bus,
            adapters: adapters.into_iter().map(|a| (a.kind(), a)).collect(),
        }))
    }

    pub fn adapter(&self, agent: AgentKind) -> Option<&Arc<dyn AgentAdapter>> {
        self.adapters.get(&agent)
    }

    /// Find or create the session record and describe it for an adapter.
    pub fn session_view(&self, native: &NativeSessionRef) -> Result<SessionView> {
        let mut g = self.lock();
        let inner = &mut *g;
        let rec = match inner.store.session_by_native(native)? {
            Some(rec) => rec,
            None => {
                inner
                    .store
                    .upsert_session(&SessionUpdate::for_native(native.clone()), now_ms())?
                    .0
            }
        };
        let open_requests = inner
            .trackers
            .get(&rec.id)
            .map(|t| t.open_requests().cloned().collect())
            .unwrap_or_default();
        Ok(SessionView {
            id: rec.id,
            native: rec.native,
            shared: rec.shared,
            open_requests,
        })
    }

    /// Handle one hook. Never fails: anything unexpected becomes `PassThrough`,
    /// so the agent behaves exactly as it would without Styrman.
    pub async fn handle_hook(self: &Arc<Self>, hook: RawHook) -> Option<serde_json::Value> {
        let adapter = self.adapter(hook.agent).cloned()?;
        let decision = match self.process_hook(adapter.as_ref(), &hook).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(agent = %hook.agent, event = %hook.event, "hook failed open: {e:#}");
                HookDecision::PassThrough
            }
        };
        adapter.render_reply(&hook, &decision)
    }

    async fn process_hook(
        self: &Arc<Self>,
        adapter: &dyn AgentAdapter,
        hook: &RawHook,
    ) -> Result<HookDecision> {
        let Some(native) = adapter.native_session(hook) else {
            return Ok(HookDecision::PassThrough);
        };
        let view = self.session_view(&native)?;
        let plan = adapter.on_hook(hook, &view)?;
        tracing::debug!(session = %view.id, event = %hook.event, shared = view.shared, n = plan.events.len(), "hook");
        let update = plan
            .session
            .clone()
            .unwrap_or_else(|| SessionUpdate::for_native(native.clone()));
        let mut events = plan.events;
        if let Some(shared) = plan.set_sharing
            && shared != view.shared
        {
            events.insert(0, EventKind::SharingChanged { shared });
        }
        self.ingest(&update, EventSource::Hook, events)?;
        if let Some(reply) = plan.reply {
            return Ok(reply);
        }
        // Waiting for the phone (permissions, prompts) lands in checkpoints 4-5;
        // until then every hook passes straight through.
        Ok(HookDecision::PassThrough)
    }

    /// Live stream of persisted events of shared sessions (for the phone API).
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.bus.subscribe()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic while holding the lock must not take the daemon down for good.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Register/refresh a session and record events. Returns the session id and the
    /// seqs of the events that were persisted.
    pub fn ingest(
        &self,
        update: &SessionUpdate,
        source: EventSource,
        events: Vec<EventKind>,
    ) -> Result<(SessionId, Vec<u64>)> {
        let now = now_ms();
        let mut g = self.lock();
        let inner = &mut *g;
        let (rec, _) = inner.store.upsert_session(update, now)?;
        let mut seqs = Vec::new();
        let mut published = Vec::new();
        let tracker = inner.trackers.entry(rec.id.clone()).or_default();
        let mut shared = rec.shared;
        for kind in events {
            tracker.apply(&kind);
            if let EventKind::SharingChanged { shared: s } = kind {
                inner.store.set_shared(&rec.id, s, now)?;
                shared = s;
            }
            if shared || always_persisted(&kind) {
                let ev = inner.store.append(&rec.id, source, kind, now)?;
                seqs.push(ev.seq);
                if shared {
                    published.push(ev);
                }
            }
        }
        drop(g);
        for ev in published {
            let _ = self.bus.send(ev); // no subscribers is fine
        }
        Ok((rec.id, seqs))
    }

    /// Turn sharing on/off for a native session (creating the record if needed).
    /// A no-op when the state is unchanged.
    pub fn set_sharing(&self, native: &NativeSessionRef, shared: bool) -> Result<SessionId> {
        let current = self
            .lock()
            .store
            .session_by_native(native)?
            .map(|r| r.shared);
        let update = SessionUpdate::for_native(native.clone());
        if current == Some(shared) {
            let rec = self
                .lock()
                .store
                .session_by_native(native)?
                .context("session vanished")?;
            return Ok(rec.id);
        }
        let (id, _) = self.ingest(
            &update,
            EventSource::Daemon,
            vec![EventKind::SharingChanged { shared }],
        )?;
        Ok(id)
    }

    pub fn session_by_native(&self, native: &NativeSessionRef) -> Result<Option<SessionRecord>> {
        self.lock().store.session_by_native(native)
    }

    pub fn summaries(&self, include_unshared: bool) -> Result<Vec<SessionSummary>> {
        let g = self.lock();
        let mut out = Vec::new();
        for rec in g.store.sessions()? {
            if !include_unshared && !rec.shared {
                continue;
            }
            let status = g
                .trackers
                .get(&rec.id)
                .map(|t| t.status())
                .unwrap_or(styr_core::SessionStatus::Waiting);
            out.push(SessionSummary {
                id: rec.id,
                agent: rec.native.agent,
                native_id: rec.native.native_id,
                cwd: rec.cwd,
                title: rec.title,
                model: rec.model,
                shared: rec.shared,
                status,
                created_ms: rec.created_ms,
                updated_ms: rec.updated_ms,
                last_seq: rec.last_seq,
            });
        }
        Ok(out)
    }

    pub fn events(&self, session: &SessionId, since_seq: u64, limit: u32) -> Result<Vec<Event>> {
        self.lock()
            .store
            .events_since(session, since_seq, limit.min(5000))
    }

    /// Answer one IPC request.
    pub async fn handle(self: &Arc<Self>, req: Request) -> Response {
        let result: Result<Response> = match req {
            Request::Hello => Ok(Response::Hello {
                protocol: styr_core::ipc::PROTOCOL_VERSION,
                daemon_version: styr_core::VERSION.to_owned(),
                pid: std::process::id(),
            }),
            Request::ListSessions { include_unshared } => self
                .summaries(include_unshared)
                .map(|sessions| Response::Sessions { sessions }),
            Request::Ingest {
                session,
                source,
                events,
            } => self
                .ingest(&session, source, events)
                .map(|(session, seqs)| Response::Ingested { session, seqs }),
            Request::SetSharing { native, shared } => {
                self.set_sharing(&native, shared).map(|_| Response::Ok)
            }
            Request::Hook { hook } => Ok(Response::HookReply {
                stdout: self.handle_hook(hook).await,
            }),
            Request::Events {
                session,
                since_seq,
                limit,
            } => self
                .events(&session, since_seq, limit)
                .map(|events| Response::Events { events }),
        };
        result.unwrap_or_else(|e| Response::Error {
            message: format!("{e:#}"),
        })
    }
}
