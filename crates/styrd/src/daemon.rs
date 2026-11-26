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
    /// A phone registered a Live Activity token.
    LiveTokens,
    /// The reply being written in a session's terminal (see `ServerMsg::Preview`).
    Preview(SessionId, Option<String>),
    /// A session's terminal screen, while a phone watches (see `ServerMsg::Screen`).
    Screen(SessionId, String, u16),
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
    /// `/styr` typed in a `styr <agent>` terminal before its session's first hook.
    pending_shares: Mutex<HashMap<String, bool>>,
    pub(crate) peer_state: crate::peers::Peers,
    pusher: std::sync::RwLock<Option<Arc<crate::push::Pusher>>>,
    data_dir: Mutex<Option<std::path::PathBuf>>,
    /// Device id → open WebSocket count (a connected app is in the foreground).
    live: Mutex<HashMap<String, usize>>,
    /// Transcripts followed for replies as they're written.
    pub(crate) follower: crate::follow::Follower,
}

struct Inner {
    store: Store,
    trackers: HashMap<SessionId, StatusTracker>,
    /// The agent's texts this turn, per session: the same reply can come from the
    /// transcript (as it's written) and from the end-of-turn hook; keep the first.
    said: HashMap<SessionId, std::collections::HashSet<String>>,
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

/// The stand-in native id of a session shared before its agent created it.
pub(crate) fn placeholder_native(agent: AgentKind, launch: &str) -> NativeSessionRef {
    NativeSessionRef::new(agent, format!("launch:{launch}"))
}

fn always_persisted(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::SessionStarted { .. }
            | EventKind::SessionEnded { .. }
            | EventKind::SharingChanged { .. }
    )
}

/// Why a session can't take an action its wrapper doesn't know yet.
const OLDER_WRAPPER: &str = "This session was started before Styrman was updated. Start it again";

impl Daemon {
    /// Load state from the store. Permission requests left open by a previous
    /// daemon are resolved as cancelled: their hook processes saw EOF and failed open.
    pub fn open(store: Store, adapters: Vec<Arc<dyn AgentAdapter>>) -> Result<Arc<Self>> {
        let mut inner = Inner {
            store,
            trackers: HashMap::new(),
            said: HashMap::new(),
        };
        let now = now_ms();
        let mut relink = Vec::new();
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
            if let Some(launch) = &rec.launch_id
                && tracker.status() != styr_core::SessionStatus::Done
            {
                relink.push((rec.id.clone(), launch.clone()));
            }
            inner.trackers.insert(rec.id.clone(), tracker);
        }
        let (bus, _) = broadcast::channel(1024);
        let daemon = Arc::new(Self {
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
            pending_shares: Mutex::new(HashMap::new()),
            peer_state: Default::default(),
            pusher: Default::default(),
            data_dir: Mutex::new(None),
            live: Mutex::new(HashMap::new()),
            follower: Default::default(),
        });
        for (session, launch) in relink {
            daemon.terminals.link(&session, &launch);
        }
        Ok(daemon)
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
        let hook = self.fix_codex_launch(adapter.as_ref(), hook);
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
        // The first hook from a terminal that had a placeholder adopts it.
        let adopted = match &hook.launch_id {
            Some(l) => self.adopt_placeholder(hook.agent, l, &native)?,
            None => false,
        };
        let view = self.session_view(&native)?;
        if let Some(v) = &hook.agent_version {
            self.saw_agent_version(hook.agent, v);
        }
        let plan = adapter.on_hook(hook, &view)?;
        tracing::debug!(session = %view.id, event = %hook.event, shared = view.shared, n = plan.events.len(), "hook");
        let update = plan
            .session
            .clone()
            .unwrap_or_else(|| SessionUpdate::for_native(native.clone()));
        let mut events = plan.events;
        if adopted {
            // The placeholder already announced the session.
            events.retain(|e| !matches!(e, EventKind::SessionStarted { .. }));
        }
        // Restarted from the phone (another model): the agent quitting and resuming is
        // the same session going on.
        if let Some(l) = hook.launch_id.as_deref()
            && self.terminals.restarting(l)
        {
            if events
                .iter()
                .any(|e| matches!(e, EventKind::SessionStarted { .. }))
            {
                self.terminals.restarted(l);
            }
            events.retain(|e| {
                !matches!(
                    e,
                    EventKind::SessionStarted { .. } | EventKind::SessionEnded { .. }
                )
            });
        }
        // A `/styr` caught by the terminal wrapper before this session existed.
        let pending = hook.launch_id.as_ref().and_then(|l| {
            self.pending_shares
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(l)
        });
        if let Some(shared) = plan.set_sharing.or(pending)
            && shared != view.shared
        {
            events.insert(0, EventKind::SharingChanged { shared });
        }
        if let Some(launch) = &hook.launch_id {
            self.terminals.link(&view.id, launch);
        }
        for e in &events {
            if let EventKind::ToolCall {
                tool,
                status: styr_core::event::ToolStatus::Pending,
                ..
            } = e
            {
                self.terminals.note_tool(&view.id, tool);
            }
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
        // Text the agent wrote before this step goes first (from its transcript).
        if events.iter().any(|e| {
            matches!(
                e,
                EventKind::ToolCall { .. }
                    | EventKind::PermissionRequested { .. }
                    | EventKind::AssistantMessage { .. }
                    | EventKind::TurnEnded { .. }
            )
        }) {
            self.follower.catch_up(self, &view.id);
        }
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
    /// Whether a permission request is still waiting for an answer.
    pub fn request_open(&self, session: &SessionId, request: &styr_core::ids::RequestId) -> bool {
        self.lock()
            .trackers
            .get(session)
            .is_some_and(|t| t.open_requests().any(|r| r == request))
    }

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
            match &kind {
                EventKind::UserMessage { .. } => {
                    inner.said.remove(&rec.id);
                }
                EventKind::AssistantMessage {
                    text,
                    mode: styr_core::event::TextMode::Final,
                    ..
                } if !text.trim().is_empty() => {
                    let new = inner
                        .said
                        .entry(rec.id.clone())
                        .or_default()
                        .insert(text.trim().to_owned());
                    if !new {
                        continue;
                    }
                }
                _ => {}
            }
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
        let tracker = inner.trackers.get(&rec.id);
        let status = tracker
            .map(|t| t.status())
            .unwrap_or(styr_core::SessionStatus::Waiting);
        let activity = tracker.and_then(|t| t.activity());
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
            in_terminal_wrapper: false,
            accepts_prompts: false,
            activity,
            mode: None,
            helper_of: None,
            effort: None,
            outdated: false,
        }
    }

    fn with_capabilities(&self, mut s: SessionSummary) -> SessionSummary {
        s.in_terminal_wrapper = self.terminals.is_live(&s.id);
        s.mode = s
            .in_terminal_wrapper
            .then(|| self.terminals.mode_of(&s.id))
            .flatten();
        if s.in_terminal_wrapper
            && let Some(launch) = self.terminals.launch_of(&s.id)
        {
            let (model, effort) = self.terminals.chosen(&launch);
            if model.is_some() {
                s.model = model;
            }
            s.effort = effort;
        }
        s.helper_of = self.helper_of(&s.id);
        s.outdated = s.in_terminal_wrapper
            && self
                .terminals
                .launch_of(&s.id)
                .is_some_and(|l| self.terminals.level_of(&l) < styr_core::ipc::WRAPPER_LEVEL);
        // Claude can also take prompts through its background Stop hook.
        s.accepts_prompts = s.in_terminal_wrapper || s.agent == AgentKind::Claude;
        s
    }

    pub fn summaries(&self, include_unshared: bool) -> Result<Vec<SessionSummary>> {
        let g = self.lock();
        let recs = g.store.sessions()?;
        let list: Vec<SessionSummary> = recs
            .into_iter()
            .filter(|r| include_unshared || r.shared)
            .map(|r| Self::summary_of(&g, r))
            .collect();
        drop(g);
        Ok(list
            .into_iter()
            .map(|s| self.with_capabilities(s))
            .collect())
    }

    pub fn summary(&self, id: &SessionId) -> Result<Option<SessionSummary>> {
        let s = {
            let g = self.lock();
            g.store.session(id)?.map(|r| Self::summary_of(&g, r))
        };
        Ok(s.map(|s| self.with_capabilities(s)))
    }

    pub fn notify_session_changed(&self, id: &SessionId) {
        let _ = self.bus.send(BusMsg::SessionChanged(id.clone()));
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
    /// Shared Claude and Codex sessions with a transcript, for `follow`.
    pub fn follow_targets(&self) -> Result<Vec<crate::follow::Target>> {
        let g = self.lock();
        Ok(g.store
            .sessions()?
            .into_iter()
            .filter(|r| r.shared)
            .filter(|r| matches!(r.native.agent, AgentKind::Claude | AgentKind::Codex))
            .filter_map(|r| {
                let status = g.trackers.get(&r.id).map(|t| t.status());
                if status == Some(styr_core::SessionStatus::Done) {
                    return None;
                }
                Some(crate::follow::Target {
                    active: matches!(
                        status,
                        Some(
                            styr_core::SessionStatus::Working
                                | styr_core::SessionStatus::NeedsApproval
                        )
                    ),
                    path: r.transcript_path?,
                    id: r.id,
                    native: r.native,
                })
            })
            .collect())
    }

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

    // ---- agent versions ----

    fn saw_agent_version(&self, agent: AgentKind, version: &str) {
        let key = format!("seen.{agent}");
        let mut g = self.lock();
        if g.store.meta(&key).ok().flatten().as_deref() != Some(version) {
            tracing::info!(%agent, version, "agent version");
            let _ = g.store.set_meta(&key, version);
        }
    }

    fn is_verified(&self, agent: AgentKind, version: &str) -> bool {
        let built_in = self
            .adapter(agent)
            .is_some_and(|a| a.verified_versions().contains(&version));
        built_in
            || self
                .lock()
                .store
                .meta(&format!("verified.{agent}"))
                .ok()
                .flatten()
                .is_some_and(|list| list.split(',').any(|v| v == version))
    }

    pub fn mark_verified(&self, agent: AgentKind, version: &str) -> Result<()> {
        let key = format!("verified.{agent}");
        let mut g = self.lock();
        let mut list: Vec<String> = g
            .store
            .meta(&key)?
            .map(|l| l.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
        if !list.iter().any(|v| v == version) {
            list.push(version.to_owned());
            g.store.set_meta(&key, &list.join(","))?;
        }
        Ok(())
    }

    pub fn agent_versions(&self) -> Vec<styr_core::ipc::AgentVersionInfo> {
        AgentKind::ALL
            .into_iter()
            .filter_map(|agent| {
                let version = self
                    .lock()
                    .store
                    .meta(&format!("seen.{agent}"))
                    .ok()
                    .flatten()?;
                let verified = self.is_verified(agent, &version);
                Some(styr_core::ipc::AgentVersionInfo {
                    agent,
                    version,
                    verified,
                })
            })
            .collect()
    }

    /// Things worth telling the user about, shown by `styr status`, `styr doctor` and the phone.
    pub fn warnings(&self) -> Vec<String> {
        self.agent_versions()
            .into_iter()
            .filter(|a| !a.verified)
            .map(|a| {
                let name = match a.agent {
                    AgentKind::Claude => "Claude Code",
                    AgentKind::Antigravity => "Antigravity",
                    AgentKind::Codex => "Codex",
                    AgentKind::Pty => "agent",
                };
                format!("{name} {} hasn't been checked with Styrman yet. Run `styr doctor --live` on the Mac.", a.version)
            })
            .collect()
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

    pub fn set_notify(&self, device: &str, on: bool, finished: bool) -> Result<()> {
        self.lock().store.set_notify(device, on, finished)
    }

    /// Like `set_activity_token`, without waking the Live Activity loop (it calls this).
    pub fn set_activity_token_quiet(
        &self,
        device: &str,
        start: bool,
        token: Option<&str>,
    ) -> Result<()> {
        self.lock().store.set_activity_token(device, start, token)
    }

    pub fn set_activity_token(&self, device: &str, start: bool, token: Option<&str>) -> Result<()> {
        self.lock().store.set_activity_token(device, start, token)?;
        // A new token gets the current state right away.
        let _ = self.bus.send(BusMsg::LiveTokens);
        Ok(())
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
        anyhow::ensure!(
            summary.accepts_prompts,
            "This session wasn't started through Styrman, so it can't take messages from the phone. Start it again in a new terminal tab after `styr setup`."
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

    /// From the phone: switch a session's permission mode. Only Claude, and only inside
    /// `styr claude` (the wrapper presses Shift+Tab until its footer shows `mode`).
    pub fn set_mode(&self, session: &SessionId, mode: &str) -> Result<()> {
        anyhow::ensure!(
            crate::terminals::MODES.contains(&mode),
            "unknown mode {mode}"
        );
        let summary = self
            .summary(session)?
            .filter(|s| s.shared)
            .context("unknown session")?;
        anyhow::ensure!(
            summary.agent == AgentKind::Claude,
            "Only Claude's mode can be changed from the phone so far."
        );
        anyhow::ensure!(
            summary.status != styr_core::SessionStatus::Done,
            "the session has ended"
        );
        let launch = self
            .terminals
            .launch_of(session)
            .filter(|_| self.terminals.is_live(session))
            .context("To change its mode from here, start it with `styr claude`.")?;
        self.terminals.set_mode(&launch, mode);
        tracing::info!(%session, mode, "mode change from the phone");
        Ok(())
    }

    /// From the phone: quit the agent in its terminal (and close the tab if the phone
    /// opened it), then stop sharing. A session that has already ended is just removed.
    pub fn end_session(&self, session: &SessionId) -> Result<()> {
        let summary = self
            .summary(session)?
            .filter(|s| s.shared)
            .context("unknown session")?;
        let native =
            styr_core::ids::NativeSessionRef::new(summary.agent, summary.native_id.clone());
        if summary.status != styr_core::SessionStatus::Done {
            let launch = self
                .terminals
                .launch_of(session)
                .filter(|_| self.terminals.is_live(session))
                .context(
                    "This session isn't running in `styr` on the Mac, so it can't be ended from here. Quit it in its terminal.",
                )?;
            self.terminals.end(&launch);
            tracing::info!(%session, "ending the session from the phone");
        }
        self.set_sharing(&native, false)?;
        Ok(())
    }

    /// From the phone: restart the agent with another model and/or effort, resuming
    /// the same conversation in the same terminal. Only while it waits for a prompt:
    /// restarting mid-turn would lose the turn.
    pub fn set_model(
        &self,
        session: &SessionId,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<()> {
        let summary = self
            .summary(session)?
            .filter(|s| s.shared)
            .context("unknown session")?;
        anyhow::ensure!(
            model.is_some() || effort.is_some(),
            "Choose a model or effort."
        );
        let safe = |v: &Option<String>| {
            v.as_deref().is_none_or(|v| {
                !v.is_empty()
                    && v.len() <= 80
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-._:[]/".contains(&b))
            })
        };
        anyhow::ensure!(safe(&model) && safe(&effort), "That isn't a model name.");
        let launch = self
            .terminals
            .launch_of(session)
            .filter(|_| self.terminals.is_live(session))
            .context("Its model can only be changed for sessions started with `styr` (or from the phone).")?;
        anyhow::ensure!(
            self.terminals.level_of(&launch) >= 2,
            "{OLDER_WRAPPER} to change its model from here."
        );
        anyhow::ensure!(
            !matches!(
                summary.status,
                styr_core::SessionStatus::Working | styr_core::SessionStatus::NeedsApproval
            ),
            "It's working: change the model once it's done (or stop it first)."
        );
        let mode = (summary.agent == AgentKind::Claude)
            .then(|| self.terminals.mode_of(session))
            .flatten();
        tracing::info!(%session, ?model, ?effort, "restarting with another model from the phone");
        self.terminals.restart(
            &launch,
            crate::terminals::Restart {
                resume: summary.native_id,
                model,
                effort,
                mode,
            },
        );
        Ok(())
    }

    /// From the phone: send this session's terminal screen for the next 20 s.
    pub fn watch_screen(&self, session: &SessionId) -> Result<()> {
        self.summary(session)?
            .filter(|s| s.shared)
            .context("unknown session")?;
        let launch = self
            .terminals
            .launch_of(session)
            .filter(|_| self.terminals.is_live(session))
            .context("Its terminal can only be shown for sessions started with `styr` (or from the phone).")?;
        anyhow::ensure!(
            self.terminals.level_of(&launch) >= 1,
            "{OLDER_WRAPPER} to see its terminal here."
        );
        self.terminals.watch_screen(&launch);
        Ok(())
    }

    /// From the phone: stop the current turn (Esc in the agent's terminal). The turn is
    /// marked as interrupted right away: Claude reports no end for an interrupted turn.
    pub fn interrupt(&self, session: &SessionId) -> Result<()> {
        let summary = self
            .summary(session)?
            .filter(|s| s.shared)
            .context("unknown session")?;
        anyhow::ensure!(
            matches!(
                summary.status,
                styr_core::SessionStatus::Working | styr_core::SessionStatus::NeedsApproval
            ),
            "It isn't working on anything right now."
        );
        let launch = self
            .terminals
            .launch_of(session)
            .filter(|_| self.terminals.is_live(session))
            .context("To stop it from here, start it with `styr` (or from the phone). Press Esc in its terminal.")?;
        anyhow::ensure!(
            self.terminals.level_of(&launch) >= 1,
            "{OLDER_WRAPPER} to stop it from here. Press Esc in its terminal for now."
        );
        self.terminals.interrupt(&launch);
        let native = styr_core::ids::NativeSessionRef::new(summary.agent, summary.native_id);
        self.ingest(
            &SessionUpdate::for_native(native),
            EventSource::Daemon,
            vec![EventKind::TurnEnded {
                turn: None,
                reason: styr_core::event::TurnEndReason::Interrupted,
                last_message: None,
            }],
        )?;
        tracing::info!(%session, "turn stopped from the phone");
        Ok(())
    }

    /// `/styr` (or `/styr off`) caught by a `styr <agent>` terminal wrapper, for an
    /// agent that rejects unknown slash commands itself (Codex). Applies to the
    /// sessions running in that terminal, or to its first one once its hooks arrive.
    /// Returns how many sessions changed now (0: deferred).
    pub fn share_launch(
        &self,
        launch_id: &str,
        shared: bool,
        agent: Option<AgentKind>,
        cwd: Option<std::path::PathBuf>,
        agent_pid: Option<u32>,
    ) -> Result<usize> {
        let mut n = 0;
        for id in self.terminals.sessions_of(launch_id) {
            let Some(rec) = self.lock().store.session(&id)? else {
                continue;
            };
            if self
                .summary(&id)?
                .is_some_and(|s| s.status != styr_core::SessionStatus::Done)
            {
                self.set_sharing(&rec.native, shared)?;
                n += 1;
            }
        }
        if n == 0
            && shared
            && let Some(agent) = agent
        {
            // No session yet (Codex creates it with the first prompt): put a
            // placeholder on the phone now. Prompts sent to it are typed into the
            // terminal; the first hook from this terminal adopts it.
            let native = placeholder_native(agent, launch_id);
            let update = SessionUpdate {
                cwd: cwd.clone(),
                agent_pid,
                launch_id: Some(launch_id.to_owned()),
                ..SessionUpdate::for_native(native.clone())
            };
            let (id, _) = self.ingest(
                &update,
                EventSource::Daemon,
                vec![
                    EventKind::SessionStarted {
                        agent,
                        native,
                        cwd,
                        workspace_roots: vec![],
                        model: None,
                        title: None,
                        start: styr_core::event::StartKind::New,
                    },
                    EventKind::SharingChanged { shared: true },
                ],
            )?;
            self.terminals.link(&id, launch_id);
            self.notify_session_changed(&id);
            return Ok(1);
        }
        if n == 0 {
            self.pending_shares
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(launch_id.to_owned(), shared);
        }
        Ok(n)
    }

    /// Codex may run hooks in its shared background server, whose environment names
    /// whichever terminal started it (or none). Link such a hook to the right
    /// `styr codex` instead: the one its session already runs in, else the newest
    /// live wrapper in the hook's folder that has no real session yet.
    fn fix_codex_launch(&self, adapter: &dyn AgentAdapter, mut hook: RawHook) -> RawHook {
        if hook.agent != AgentKind::Codex {
            return hook;
        }
        let Some(cwd) = hook.payload["cwd"].as_str().map(std::path::PathBuf::from) else {
            return hook;
        };
        if hook
            .launch_id
            .as_deref()
            .is_some_and(|l| self.terminals.wrapper_in(l, AgentKind::Codex, &cwd))
        {
            return hook;
        }
        let known = adapter
            .native_session(&hook)
            .and_then(|n| self.session_by_native(&n).ok().flatten());
        if let Some(l) = known
            .as_ref()
            .and_then(|rec| self.terminals.launch_of(&rec.id))
        {
            hook.launch_id = Some(l);
            return hook;
        }
        let free = self
            .terminals
            .live_wrappers_in(AgentKind::Codex, &cwd)
            .into_iter()
            .find(|l| {
                self.terminals.sessions_of(l).iter().all(|s| {
                    self.lock()
                        .store
                        .session(s)
                        .ok()
                        .flatten()
                        .is_none_or(|r| r.native.native_id.starts_with("launch:"))
                })
            });
        if let Some(l) = free {
            tracing::debug!(launch = %l, stale = ?hook.launch_id, "Codex hook matched to its terminal by folder");
            hook.launch_id = Some(l);
        }
        hook
    }

    /// If `launch` has a placeholder session and `native` doesn't exist yet, turn the
    /// placeholder into `native`. Returns whether it did.
    fn adopt_placeholder(
        &self,
        agent: AgentKind,
        launch: &str,
        native: &NativeSessionRef,
    ) -> Result<bool> {
        let mut g = self.lock();
        let Some(rec) = g
            .store
            .session_by_native(&placeholder_native(agent, launch))?
        else {
            return Ok(false);
        };
        if g.store.session_by_native(native)?.is_some() {
            return Ok(false);
        }
        g.store.rename_native(&rec.id, &native.native_id)?;
        tracing::info!(session = %rec.id, native = %native.native_id, "placeholder session adopted");
        Ok(true)
    }

    fn push_status(&self) -> Response {
        let p = self.pusher();
        Response::PushStatus {
            configured: p.is_some(),
            details: p.as_ref().is_some_and(|p| p.cfg.details),
            devices_with_token: self.devices_with_token(),
            relay: p.as_ref().and_then(|p| p.relay_url().map(str::to_owned)),
        }
    }

    /// Styrman's data folder (once the daemon has started).
    pub fn data_dir_path(&self) -> Option<std::path::PathBuf> {
        self.data_dir()
    }

    fn data_dir(&self) -> Option<std::path::PathBuf> {
        self.data_dir
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
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

    /// Whether `request` of `session` is still waiting for an answer.
    pub fn is_open_request(
        &self,
        session: &SessionId,
        request: &styr_core::ids::RequestId,
    ) -> bool {
        self.lock()
            .trackers
            .get(session)
            .is_some_and(|t| t.open_requests().any(|r| r == request))
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
            Request::StartSession { agent, cwd, prompt } => self
                .start_session(agent, &cwd.to_string_lossy(), prompt, "cli".into())
                .map(|session| Response::SessionStarting { session }),
            Request::ShareLaunch {
                launch_id,
                shared,
                agent,
                cwd,
                agent_pid,
            } => self
                .share_launch(&launch_id, shared, agent, cwd, agent_pid)
                .map(|n| Response::LaunchShared { sessions: n }),
            Request::SendPrompt { session, text } => self
                .send_prompt(&session, &text, "cli".into())
                .map(|d| Response::PromptAccepted {
                    delivered: d == crate::prompts::Delivery::Delivered,
                }),
            Request::PushReload => self.reload_push(None).map(|_| self.push_status()),
            Request::PushStatus => Ok(self.push_status()),
            Request::Peers { launch_id } => {
                let me = launch_id.as_deref().and_then(|l| self.session_of_launch(l));
                self.peers(me.as_ref().map(|s| &s.id))
                    .map(|sessions| Response::Peers {
                        me: me.map(|s| s.id),
                        sessions,
                    })
            }
            Request::Ask {
                launch_id,
                to,
                text,
                wait_secs,
            } => {
                let wait = wait_secs.map(std::time::Duration::from_secs);
                self.ask(launch_id.as_deref(), &to, &text, wait)
                    .await
                    .map(|o| match o {
                        crate::peers::AskOutcome::Reply(r) => Response::AskReply {
                            reply: Some(r),
                            timed_out: false,
                        },
                        crate::peers::AskOutcome::Sent => Response::AskReply {
                            reply: None,
                            timed_out: false,
                        },
                        crate::peers::AskOutcome::TimedOut => Response::AskReply {
                            reply: None,
                            timed_out: true,
                        },
                    })
            }
            Request::PushRelay { url } => match self.data_dir() {
                Some(dir) => match crate::push::register_relay(&dir, &url).await {
                    Ok(()) => self.reload_push(None).map(|_| self.push_status()),
                    Err(e) => Err(e),
                },
                None => Err(anyhow::anyhow!("no data directory")),
            },
            Request::PushTest => {
                let n = crate::push::Notification {
                    title: "Styrman".into(),
                    body: "Push notifications work.".into(),
                    session: None,
                    category: None,
                    collapse_id: Some("styrman-test".into()),
                    request: None,
                };
                let results = self.push_to_devices(&n, true).await;
                Ok(Response::PushResults {
                    results: results
                        .into_iter()
                        .map(|(id, r)| (id, format!("{r:?}")))
                        .collect(),
                })
            }
            Request::AttachTerminal {
                launch_id,
                agent,
                cwd,
                level,
            } => {
                self.terminals.note_level(&launch_id, level);
                if let (Some(agent), Some(cwd)) = (agent, &cwd) {
                    self.terminals.note_wrapper(&launch_id, agent, cwd);
                }
                match self.attach_terminal(&launch_id).await {
                    Some(crate::terminals::TerminalAction::Type(text)) => {
                        Ok(Response::TypePrompt { text })
                    }
                    Some(crate::terminals::TerminalAction::Select(index)) => {
                        Ok(Response::SelectOption { index })
                    }
                    Some(crate::terminals::TerminalAction::Mode(mode)) => {
                        Ok(Response::SetMode { mode })
                    }
                    Some(crate::terminals::TerminalAction::End) => Ok(Response::EndSession),
                    Some(crate::terminals::TerminalAction::Interrupt) => Ok(Response::Interrupt),
                    Some(crate::terminals::TerminalAction::Restart(r)) => Ok(Response::Restart {
                        resume: r.resume,
                        model: r.model,
                        effort: r.effort,
                        mode: r.mode,
                    }),
                    Some(crate::terminals::TerminalAction::WatchScreen) => {
                        Ok(Response::WatchScreen)
                    }
                    None => Err(anyhow::anyhow!("detached")),
                }
            }
            Request::TerminalPrompt { launch_id, prompt } => self
                .terminal_prompt(&launch_id, prompt)
                .map(|()| Response::Ok),
            Request::TerminalScreen {
                launch_id,
                text,
                cols,
            } => {
                for id in self.terminals.sessions_of(&launch_id) {
                    if self.summary(&id).ok().flatten().is_some_and(|s| s.shared) {
                        let _ = self.bus.send(BusMsg::Screen(id, text.clone(), cols));
                    }
                }
                Ok(Response::Ok)
            }
            Request::TerminalPreview { launch_id, text } => {
                // Only while it's working: a finished reply comes as a message.
                for id in self.terminals.sessions_of(&launch_id) {
                    let working =
                        self.summary(&id).ok().flatten().is_some_and(|s| {
                            s.shared && s.status == styr_core::SessionStatus::Working
                        });
                    let text = text.clone().filter(|_| working);
                    let _ = self.bus.send(BusMsg::Preview(id, text));
                }
                Ok(Response::Ok)
            }
            Request::TerminalMode { launch_id, mode } => {
                if self.terminals.note_mode(&launch_id, mode) {
                    for id in self.terminals.sessions_of(&launch_id) {
                        let _ = self.bus.send(BusMsg::SessionChanged(id));
                    }
                }
                Ok(Response::Ok)
            }
            Request::Diagnostics => self.devices().map(|devices| Response::Diagnostics {
                daemon_version: styr_core::VERSION.to_owned(),
                pid: std::process::id(),
                api_urls: self.api_urls(),
                push_configured: self.pusher().is_some(),
                devices,
                agents: self.agent_versions(),
                warnings: self.warnings(),
            }),
            Request::MarkVerified { agent, version } => {
                self.mark_verified(agent, &version).map(|()| Response::Ok)
            }
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
