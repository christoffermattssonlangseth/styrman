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
use styr_core::adapter::{AgentAdapter, HookDecision, RawHook, SessionView, WaitFor};
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
    pub auth_throttle: crate::api::AuthThrottle,
    api_urls: Mutex<Vec<String>>,
    /// Device ids revoked while connected; their WebSockets close.
    pub revoked: broadcast::Sender<String>,
    adapters: HashMap<AgentKind, Arc<dyn AgentAdapter>>,
    pub(crate) broker: crate::permissions::Broker,
    pub(crate) prompts: crate::prompts::Prompts,
    pub(crate) terminals: crate::terminals::Terminals,
    pusher: std::sync::RwLock<Option<Arc<crate::push::Pusher>>>,
    data_dir: Mutex<Option<std::path::PathBuf>>,
    /// Device id → open WebSocket count (a connected app is in the foreground).
    live: Mutex<HashMap<String, usize>>,
}

struct Inner {
    store: Store,
    trackers: HashMap<SessionId, StatusTracker>,
}

/// A short session title from the first prompt (one line, ≤ 80 chars).
fn first_line_title(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() <= 80 {
        line.to_owned()
    } else {
        format!("{}…", line.chars().take(79).collect::<String>())
    }
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
            auth_throttle: Default::default(),
            revoked: broadcast::channel(16).0,
            api_urls: Mutex::new(Vec::new()),
            adapters: adapters.into_iter().map(|a| (a.kind(), a)).collect(),
            broker: Default::default(),
            prompts: Default::default(),
            terminals: Default::default(),
            pusher: Default::default(),
            data_dir: Mutex::new(None),
            live: Mutex::new(HashMap::new()),
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
    pub async fn handle_hook(self: &Arc<Self>, hook: RawHook) -> styr_core::ipc::HookOutput {
        let Some(adapter) = self.adapter(hook.agent).cloned() else {
            return Default::default();
        };
        let decision = match self.process_hook(adapter.as_ref(), &hook).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(agent = %hook.agent, event = %hook.event, "hook failed open: {e:#}");
                HookDecision::PassThrough
            }
        };
        if decision == HookDecision::PassThrough {
            return Default::default();
        }
        styr_core::ipc::HookOutput {
            stdout: adapter.render_reply(&hook, &decision),
            exit: adapter
                .render_exit(&hook, &decision)
                .map(|(code, stderr)| styr_core::ipc::HookExit { code, stderr }),
        }
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
        if let Some(launch) = &hook.launch_id {
            self.terminals.link(&view.id, launch);
        }
        // A prompt we typed into the terminal for the phone comes back as a normal
        // UserPromptSubmit: attribute it to the phone.
        for e in events.iter_mut() {
            if let EventKind::UserMessage { text, origin } = e
                && *origin == styr_core::event::Origin::Terminal
                && let Some(device) = self.terminals.claim_typed(&view.id, text)
            {
                *origin = styr_core::event::Origin::Remote(device);
            }
        }
        let options_for = |request: &styr_core::ids::RequestId| {
            events.iter().find_map(|e| match e {
                EventKind::PermissionRequested {
                    request: r,
                    options,
                    ..
                } if r == request => Some(options.clone()),
                _ => None,
            })
        };
        // A prompt typed in the terminal, unsharing or the session ending stands down
        // any background wait for a phone prompt.
        let typed_in_terminal = events.iter().any(|e| {
            matches!(
                e,
                EventKind::UserMessage {
                    origin: styr_core::event::Origin::Terminal,
                    ..
                }
            )
        });
        let gone = plan.set_sharing == Some(false)
            || events
                .iter()
                .any(|e| matches!(e, EventKind::SessionEnded { .. }));
        let wait = match &plan.wait {
            Some(WaitFor::Permission { request, timeout }) => {
                options_for(request).map(|o| (request.clone(), o, *timeout))
            }
            _ => None,
        };
        self.ingest(&update, EventSource::Hook, events)?;
        if gone {
            self.prompts.clear(&view.id);
            self.terminals.forget(&view.id);
        } else if typed_in_terminal {
            self.prompts.release(&view.id);
        }
        if let Some(reply) = plan.reply {
            return Ok(reply);
        }
        let shared_now = plan.set_sharing.unwrap_or(view.shared);
        if let (true, Some(WaitFor::NextPrompt { timeout })) = (shared_now, &plan.wait) {
            return Ok(match self.wait_for_prompt(&view.id, *timeout).await {
                Some(p) => {
                    tracing::info!(session = %view.id, "delivering a prompt from the phone");
                    let ev = vec![
                        EventKind::TurnStarted { turn: None },
                        EventKind::UserMessage {
                            text: p.text.clone(),
                            origin: styr_core::event::Origin::Remote(p.device),
                        },
                    ];
                    self.ingest(&SessionUpdate::for_native(native), EventSource::Daemon, ev)?;
                    HookDecision::Prompt(p.text)
                }
                None => HookDecision::PassThrough,
            });
        }
        if let (true, Some((request, options, timeout))) = (shared_now, wait) {
            // Only shared sessions ever wait for the phone; others pass straight through.
            let outcome = self
                .wait_for_permission(view.id.clone(), native, request, options, timeout)
                .await;
            return Ok(HookDecision::Permission(outcome));
        }
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
        let mut title: Option<String> = None;
        for kind in events {
            tracker.apply(&kind);
            if let EventKind::UserMessage { text, .. } = &kind
                && rec.title.is_none()
                && title.is_none()
            {
                title = Some(first_line_title(text));
            }
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
        if let Some(t) = title {
            inner.store.set_title_if_empty(&rec.id, &t)?;
        }
        drop(g);
        // No subscribers is fine.
        for ev in published {
            let _ = self.bus.send(BusMsg::Event(ev));
        }
        let _ = self.bus.send(BusMsg::SessionChanged(rec.id.clone()));
        // The status may now allow typing a held phone prompt.
        self.pump_terminal(&rec.id);
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
        if !shared {
            self.prompts.clear(&id);
        }
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

    /// Delete a session and its history; phones drop it from their list.
    pub fn forget_session(&self, id: &SessionId) -> Result<bool> {
        self.terminals.forget(id);
        let existed = {
            let mut g = self.lock();
            g.trackers.remove(id);
            g.store.delete_session(id)?
        };
        self.prompts.clear(id);
        let _ = self.bus.send(BusMsg::SessionChanged(id.clone()));
        Ok(existed)
    }

    /// End sessions whose agent process is gone (closed terminal, crash, kill -9):
    /// they never sent SessionEnd and would otherwise wait for you forever.
    pub fn reap_dead_sessions(&self) -> Result<usize> {
        let candidates: Vec<SessionRecord> = {
            let g = self.lock();
            g.store
                .sessions()?
                .into_iter()
                .filter(|r| r.agent_pid.is_some())
                .filter(|r| {
                    g.trackers.get(&r.id).map(|t| t.status())
                        != Some(styr_core::SessionStatus::Done)
                })
                .collect()
        };
        let mut n = 0;
        for rec in candidates {
            let pid = rec.agent_pid.expect("filtered") as libc::pid_t;
            // SAFETY: kill with signal 0 only checks that the process exists.
            let alive = unsafe { libc::kill(pid, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
            if alive {
                continue;
            }
            tracing::info!(session = %rec.id, pid, "agent process is gone; ending the session");
            self.ingest(
                &SessionUpdate::for_native(rec.native.clone()),
                EventSource::Daemon,
                vec![EventKind::SessionEnded {
                    reason: Some("the agent process exited".into()),
                }],
            )?;
            self.prompts.clear(&rec.id);
            n += 1;
        }
        Ok(n)
    }

    // ---- phones ----

    pub fn pusher(&self) -> Option<Arc<crate::push::Pusher>> {
        self.pusher
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// (Re)load the APNs config from the data dir.
    pub fn reload_push(&self, dir: Option<&std::path::Path>) -> Result<bool> {
        if let Some(d) = dir {
            *self.data_dir.lock().unwrap_or_else(|p| p.into_inner()) = Some(d.to_path_buf());
        }
        let Some(dir) = self
            .data_dir
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        else {
            return Ok(false);
        };
        let p = crate::push::Pusher::load(&dir)?.map(Arc::new);
        let on = p.is_some();
        *self.pusher.write().unwrap_or_else(|p| p.into_inner()) = p;
        Ok(on)
    }

    pub(crate) fn lock_store_devices(&self) -> Result<Vec<crate::store::DeviceRecord>> {
        self.lock().store.devices()
    }

    pub fn set_push_token(
        &self,
        device: &str,
        token: Option<&str>,
        env: Option<&str>,
    ) -> Result<()> {
        self.lock().store.set_push_token(device, token, env)
    }

    pub fn device_connected(&self, device: &str, up: bool) {
        let mut g = self.live.lock().unwrap_or_else(|p| p.into_inner());
        let n = g.entry(device.to_owned()).or_insert(0);
        if up {
            *n += 1;
        } else {
            *n = n.saturating_sub(1);
            if *n == 0 {
                g.remove(device);
            }
        }
    }

    pub fn is_device_live(&self, device: &str) -> bool {
        self.live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(device)
    }

    /// A prompt typed on the phone for a shared session.
    pub fn send_prompt(
        &self,
        session: &SessionId,
        text: &str,
        device: styr_core::ids::DeviceId,
    ) -> Result<crate::prompts::Delivery> {
        let text = text.trim();
        anyhow::ensure!(!text.is_empty(), "empty prompt");
        anyhow::ensure!(
            text.chars().count() <= crate::prompts::MAX_PROMPT_CHARS,
            "prompt too long"
        );
        let summary = self
            .summary(session)?
            .filter(|s| s.shared)
            .context("unknown session")?;
        anyhow::ensure!(
            summary.status != styr_core::SessionStatus::Done,
            "the session has ended"
        );
        let prompt = crate::prompts::QueuedPrompt {
            text: text.to_owned(),
            device: device.clone(),
        };
        if self.terminals.is_live(session) {
            // Runs inside `styr claude`: type it into the terminal (when it's safe).
            let n = self.terminals.enqueue(session, prompt);
            let typed = n == 1 && self.pump_terminal(session);
            let delivery = if typed {
                crate::prompts::Delivery::Delivered
            } else {
                crate::prompts::Delivery::Queued(n)
            };
            tracing::info!(%session, ?delivery, "prompt from the phone (terminal)");
            return Ok(delivery);
        }
        let delivery = self.prompts.submit(
            session,
            crate::prompts::QueuedPrompt {
                text: text.to_owned(),
                device,
            },
        );
        tracing::info!(%session, ?delivery, "prompt from the phone");
        Ok(delivery)
    }

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

    fn devices_with_token(&self) -> Vec<String> {
        self.lock_store_devices()
            .unwrap_or_default()
            .into_iter()
            .filter(|d| !d.revoked && d.push_token.is_some())
            .map(|d| d.name)
            .collect()
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
            Request::Hook { hook } => {
                let out = self.handle_hook(hook).await;
                Ok(Response::HookReply {
                    stdout: out.stdout,
                    exit: out.exit,
                })
            }
            Request::CreatePairing => {
                let (code, expires_ms) = self.auth.new_code();
                Ok(Response::Pairing {
                    code,
                    expires_ms,
                    urls: self.api_urls(),
                    host: Self::host_name(),
                })
            }
            Request::SendPrompt { session, text } => self
                .send_prompt(&session, &text, "cli".into())
                .map(|d| Response::PromptAccepted {
                    delivered: d == crate::prompts::Delivery::Delivered,
                }),
            Request::PushReload => self.reload_push(None).map(|on| Response::PushStatus {
                configured: on,
                details: self.pusher().is_some_and(|p| p.cfg.details),
                devices_with_token: self.devices_with_token(),
            }),
            Request::PushStatus => Ok(Response::PushStatus {
                configured: self.pusher().is_some(),
                details: self.pusher().is_some_and(|p| p.cfg.details),
                devices_with_token: self.devices_with_token(),
            }),
            Request::PushTest => {
                let n = crate::push::Notification {
                    title: "Styrman".into(),
                    body: "Push notifications work.".into(),
                    session: None,
                    category: None,
                    collapse_id: Some("styrman-test".into()),
                };
                let results = self.push_to_devices(&n, true).await;
                Ok(Response::PushResults {
                    results: results
                        .into_iter()
                        .map(|(id, r)| (id, format!("{r:?}")))
                        .collect(),
                })
            }
            Request::AttachTerminal { launch_id } => match self.attach_terminal(&launch_id).await {
                Some(text) => Ok(Response::TypePrompt { text }),
                None => Err(anyhow::anyhow!("detached")),
            },
            Request::ForgetSessions { ids } => ids
                .iter()
                .try_for_each(|id| self.forget_session(id).map(|_| ()))
                .map(|()| Response::Ok),
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
