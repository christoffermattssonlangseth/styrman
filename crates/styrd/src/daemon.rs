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

/// What the phone API listens to.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // short-lived broadcast values
pub enum BusMsg {
    /// A persisted event of a shared session.
    Event(Event),
    /// A session's summary (status, metadata, sharing) may have changed.
    SessionChanged(SessionId),
}

pub struct Daemon {
    inner: Mutex<Inner>,
    bus: broadcast::Sender<BusMsg>,
    pub auth: crate::auth::Auth,
    api_urls: Mutex<Vec<String>>,
    /// Device ids revoked while connected; their WebSockets close.
    pub revoked: broadcast::Sender<String>,
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
            auth: crate::auth::Auth::default(),
            revoked: broadcast::channel(16).0,
            api_urls: Mutex::new(Vec::new()),
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
    pub fn subscribe(&self) -> broadcast::Receiver<BusMsg> {
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
        // No subscribers is fine.
        for ev in published {
            let _ = self.bus.send(BusMsg::Event(ev));
        }
        let _ = self.bus.send(BusMsg::SessionChanged(rec.id.clone()));
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

    fn summary_of(inner: &Inner, rec: SessionRecord) -> SessionSummary {
        let status = inner
            .trackers
            .get(&rec.id)
            .map(|t| t.status())
            .unwrap_or(styr_core::SessionStatus::Waiting);
        SessionSummary {
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
        }
    }

    pub fn summaries(&self, include_unshared: bool) -> Result<Vec<SessionSummary>> {
        let g = self.lock();
        let recs = g.store.sessions()?;
        Ok(recs
            .into_iter()
            .filter(|r| include_unshared || r.shared)
            .map(|r| Self::summary_of(&g, r))
            .collect())
    }

    pub fn summary(&self, id: &SessionId) -> Result<Option<SessionSummary>> {
        let g = self.lock();
        Ok(g.store.session(id)?.map(|r| Self::summary_of(&g, r)))
    }

    // ---- phones ----

    pub fn set_api_urls(&self, urls: Vec<String>) {
        *self.api_urls.lock().unwrap_or_else(|p| p.into_inner()) = urls;
    }

    pub fn api_urls(&self) -> Vec<String> {
        self.api_urls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn host_name() -> String {
        let h = gethostname::gethostname().to_string_lossy().into_owned();
        h.strip_suffix(".local").unwrap_or(&h).to_owned()
    }

    /// Redeem a pairing code for a new device token. `Ok(None)`: bad or used code.
    pub fn pair(
        &self,
        code: &str,
        device_name: &str,
    ) -> Result<Option<styr_core::phone::PairResponse>> {
        if !self.auth.redeem(code) {
            return Ok(None);
        }
        let id = SessionId::generate().0; // a ULID
        let secret = crate::auth::new_secret();
        let name: String = device_name.trim().chars().take(64).collect();
        let name = if name.is_empty() {
            "iPhone".to_owned()
        } else {
            name
        };
        self.lock().store.insert_device(
            &id,
            &name,
            &crate::auth::hash_secret(&secret),
            now_ms(),
        )?;
        tracing::info!(device = %id, %name, "paired a new device");
        Ok(Some(styr_core::phone::PairResponse {
            token: crate::auth::make_token(&id, &secret),
            device_id: id,
            host: Self::host_name(),
        }))
    }

    /// The device id for a valid, unrevoked bearer token.
    pub fn authenticate(&self, token: &str) -> Option<String> {
        let (id, secret) = crate::auth::parse_token(token)?;
        let mut g = self.lock();
        let dev = g.store.device(id).ok()??;
        if dev.revoked || !crate::auth::secret_matches(secret, &dev.secret_hash) {
            return None;
        }
        let _ = g.store.touch_device(id, now_ms());
        Some(dev.id)
    }

    pub fn devices(&self) -> Result<Vec<styr_core::ipc::DeviceInfo>> {
        Ok(self
            .lock()
            .store
            .devices()?
            .into_iter()
            .map(|d| styr_core::ipc::DeviceInfo {
                id: d.id,
                name: d.name,
                created_ms: d.created_ms,
                last_seen_ms: d.last_seen_ms,
                revoked: d.revoked,
            })
            .collect())
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
            Request::CreatePairing => {
                let (code, expires_ms) = self.auth.new_code();
                Ok(Response::Pairing {
                    code,
                    expires_ms,
                    urls: self.api_urls(),
                    host: Self::host_name(),
                })
            }
            Request::ListDevices => self.devices().map(|devices| Response::Devices { devices }),
            Request::RevokeDevice { id } => {
                let revoked = self.lock().store.revoke_device(&id);
                match revoked {
                    Ok(true) => {
                        let _ = self.revoked.send(id);
                        Ok(Response::Ok)
                    }
                    Ok(false) => Err(anyhow::anyhow!("no active device {id}")),
                    Err(e) => Err(e),
                }
            }
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
