//! Agents running inside `styr <agent>`'s terminal wrapper.
//!
//! The wrapper holds an `AttachTerminal` request open and gets back one action:
//! - **type a prompt** from the phone into the agent's prompt box (it then looks
//!   exactly like typed input; the agent's `UserPromptSubmit`/`PreInvocation` for it
//!   is recognised and attributed to the phone);
//! - **select an option** in the agent's own permission menu, which the wrapper read
//!   off the screen and reported with `TerminalPrompt` (agy has no permission hook).
//!
//! Safety: prompts are never typed while a permission menu is open or after the
//! session ended; the wrapper also waits while the user is halfway through a line,
//! and only selects an option if the same menu is still on screen.

use crate::daemon::Daemon;
use crate::prompts::QueuedPrompt;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use styr_core::SessionStatus;
use styr_core::event::{
    EventKind, EventSource, OptionKind, PermissionOption, PermissionOutcome, Resolver, ToolInfo,
    ToolKind,
};
use styr_core::ids::{DeviceId, NativeSessionRef, RequestId, SessionId};
use styr_core::ipc::{SessionUpdate, TerminalPrompt};
use tokio::sync::oneshot;

/// A wrapper that hasn't re-attached for this long is considered gone.
const LIVE_GRACE: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalAction {
    Type(String),
    Select(usize),
}

/// An on-screen permission menu turned into a permission request.
#[derive(Debug, Clone)]
struct ScreenPermission {
    request: RequestId,
    session: SessionId,
    prompt: TerminalPrompt,
    options: Vec<PermissionOption>,
}

#[derive(Default)]
pub struct Terminals {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Session → the launch (wrapper) it runs in.
    launch_of: HashMap<SessionId, String>,
    /// Launch → its open AttachTerminal request.
    attached: HashMap<String, oneshot::Sender<TerminalAction>>,
    last_seen: HashMap<String, Instant>,
    /// Prompts waiting for a safe moment, per session.
    queue: HashMap<SessionId, VecDeque<QueuedPrompt>>,
    /// Typed into the terminal, not yet reported back by the agent.
    typed: HashMap<SessionId, VecDeque<QueuedPrompt>>,
    /// Menu selections not yet handed to the wrapper, per launch.
    selects: HashMap<String, usize>,
    /// The permission menu currently on screen, per launch.
    on_screen: HashMap<String, ScreenPermission>,
    /// The last tool the agent announced, per session (to describe a menu).
    last_tool: HashMap<SessionId, ToolInfo>,
    /// Launch → (agent, canonical folder, first seen), as the wrapper reported them.
    wrappers: HashMap<String, (styr_core::ids::AgentKind, std::path::PathBuf, Instant)>,
}

fn canon(p: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned())
}

/// What a menu read off the screen is about, when the agent didn't announce the tool:
/// the `$ command` line if there is one (Codex), else the first line that isn't
/// "Environment: …" / "Reason: …".
pub fn menu_tool(p: &styr_core::ipc::TerminalPrompt) -> ToolInfo {
    let lines: Vec<&str> = p.detail.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if let Some(cmd) = lines.iter().find_map(|l| l.strip_prefix("$ ")) {
        return ToolInfo {
            name: "command".into(),
            kind: ToolKind::Execute,
            title: format!("Run `{cmd}`"),
            input: serde_json::json!({"command": cmd}),
            locations: vec![],
        };
    }
    let title = lines
        .iter()
        .find(|l| !l.starts_with("Environment:") && !l.starts_with("Reason:"))
        .or(lines.first())
        .map_or_else(|| p.title.clone(), |l| (*l).to_owned());
    ToolInfo {
        name: "permission".into(),
        kind: ToolKind::Other,
        title,
        input: serde_json::Value::Null,
        locations: vec![],
    }
}

/// Map a menu label onto ACP's option kinds.
pub fn option_kind(label: &str) -> OptionKind {
    let l = label.to_lowercase();
    if l.starts_with("no") || l.contains("cancel") || l.contains("deny") || l.contains("reject") {
        OptionKind::RejectOnce
    } else if l.contains("always") || l.contains("don't ask") {
        OptionKind::AllowAlways
    } else {
        OptionKind::AllowOnce
    }
}

impl Terminals {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn link(&self, session: &SessionId, launch: &str) {
        let mut g = self.lock();
        g.launch_of.insert(session.clone(), launch.to_owned());
        g.last_seen
            .entry(launch.to_owned())
            .or_insert_with(Instant::now);
    }

    pub fn note_tool(&self, session: &SessionId, tool: &ToolInfo) {
        self.lock().last_tool.insert(session.clone(), tool.clone());
    }

    /// The session runs in a wrapper that is attached now or was very recently.
    pub fn is_live(&self, session: &SessionId) -> bool {
        let g = self.lock();
        g.launch_of.get(session).is_some_and(|l| {
            g.attached.contains_key(l)
                || g.last_seen.get(l).is_some_and(|t| t.elapsed() < LIVE_GRACE)
        })
    }

    pub fn enqueue(&self, session: &SessionId, p: QueuedPrompt) -> usize {
        let mut g = self.lock();
        let q = g.queue.entry(session.clone()).or_default();
        q.push_back(p);
        q.len()
    }

    /// Take a queued prompt back (the wrapper is gone; deliver another way).
    pub fn take_queued(&self, session: &SessionId) -> Option<QueuedPrompt> {
        self.lock().queue.get_mut(session)?.pop_front()
    }

    /// Record what a wrapper runs and where (sent with every attach).
    pub fn note_wrapper(&self, launch: &str, agent: styr_core::ids::AgentKind, cwd: &std::path::Path) {
        let mut g = self.lock();
        g.wrappers
            .entry(launch.to_owned())
            .or_insert_with(|| (agent, canon(cwd), Instant::now()));
    }

    fn launch_live(g: &Inner, launch: &str) -> bool {
        g.attached.contains_key(launch)
            || g.last_seen.get(launch).is_some_and(|t| t.elapsed() < LIVE_GRACE)
    }

    /// Whether `launch` is a live wrapper of `agent` in `cwd`.
    pub fn wrapper_in(&self, launch: &str, agent: styr_core::ids::AgentKind, cwd: &std::path::Path) -> bool {
        let g = self.lock();
        Self::launch_live(&g, launch)
            && g.wrappers
                .get(launch)
                .is_some_and(|(a, c, _)| *a == agent && *c == canon(cwd))
    }

    /// Live wrappers of `agent` in `cwd`, newest first.
    pub fn live_wrappers_in(&self, agent: styr_core::ids::AgentKind, cwd: &std::path::Path) -> Vec<String> {
        let g = self.lock();
        let cwd = canon(cwd);
        let mut v: Vec<(&String, Instant)> = g
            .wrappers
            .iter()
            .filter(|(l, (a, c, _))| *a == agent && *c == cwd && Self::launch_live(&g, l))
            .map(|(l, (_, _, t))| (l, *t))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v.into_iter().map(|(l, _)| l.clone()).collect()
    }

    /// The launch a session is linked to.
    pub fn launch_of(&self, session: &SessionId) -> Option<String> {
        self.lock().launch_of.get(session).cloned()
    }

    pub fn attach(&self, launch: &str) -> oneshot::Receiver<TerminalAction> {
        let (tx, rx) = oneshot::channel();
        let mut g = self.lock();
        g.last_seen.insert(launch.to_owned(), Instant::now());
        // A pending menu selection goes out first.
        if let Some(i) = g.selects.remove(launch) {
            let _ = tx.send(TerminalAction::Select(i));
        } else {
            g.attached.insert(launch.to_owned(), tx);
        }
        rx
    }

    /// Sessions linked to a launch.
    pub fn sessions_of(&self, launch: &str) -> Vec<SessionId> {
        self.lock()
            .launch_of
            .iter()
            .filter(|(_, l)| l.as_str() == launch)
            .map(|(s, _)| s.clone())
            .collect()
    }

    /// Hand the next queued prompt to the session's wrapper if one is attached and
    /// no permission menu is on screen.
    pub fn try_deliver(&self, session: &SessionId) -> bool {
        let mut g = self.lock();
        let Some(launch) = g.launch_of.get(session).cloned() else {
            return false;
        };
        if !g.attached.contains_key(&launch) || g.on_screen.contains_key(&launch) {
            return false;
        }
        let Some(p) = g.queue.get_mut(session).and_then(VecDeque::pop_front) else {
            return false;
        };
        let tx = g.attached.remove(&launch).expect("checked");
        g.last_seen.insert(launch, Instant::now());
        match tx.send(TerminalAction::Type(p.text.clone())) {
            Ok(()) => {
                g.typed.entry(session.clone()).or_default().push_back(p);
                true
            }
            Err(_) => {
                g.queue.entry(session.clone()).or_default().push_front(p);
                false
            }
        }
    }

    fn select(&self, launch: &str, index: usize) {
        let mut g = self.lock();
        match g.attached.remove(launch) {
            Some(tx) => {
                if tx.send(TerminalAction::Select(index)).is_err() {
                    g.selects.insert(launch.to_owned(), index);
                }
            }
            None => {
                g.selects.insert(launch.to_owned(), index);
            }
        }
    }

    /// If `text` is a prompt we typed for the phone, who sent it.
    pub fn claim_typed(&self, session: &SessionId, text: &str) -> Option<DeviceId> {
        let mut g = self.lock();
        let q = g.typed.get_mut(session)?;
        let i = q.iter().position(|p| p.text.trim() == text.trim())?;
        q.remove(i).map(|p| p.device)
    }

    pub fn forget(&self, session: &SessionId) {
        let mut g = self.lock();
        if let Some(l) = g.launch_of.remove(session) {
            g.on_screen.remove(&l);
        }
        g.queue.remove(session);
        g.typed.remove(session);
        g.last_tool.remove(session);
    }
}

fn native_of(s: &styr_core::ipc::SessionSummary) -> SessionUpdate {
    SessionUpdate::for_native(NativeSessionRef::new(s.agent, s.native_id.clone()))
}

impl Daemon {
    /// Type the next queued phone prompt into the session's terminal, if it's safe now.
    pub fn pump_terminal(&self, session: &SessionId) -> bool {
        let safe = matches!(
            self.summary(session)
                .ok()
                .flatten()
                .map(|s| (s.shared, s.status)),
            Some((
                true,
                SessionStatus::Waiting | SessionStatus::Working | SessionStatus::Error
            ))
        );
        let typed = safe && self.terminals.try_deliver(session);
        if typed {
            tracing::info!(%session, "typing a phone prompt into the terminal");
        }
        typed
    }

    /// Long-poll from a wrapper: resolves with the next thing to do in its terminal.
    pub async fn attach_terminal(self: &Arc<Self>, launch: &str) -> Option<TerminalAction> {
        let was_live: Vec<(SessionId, bool)> = self
            .terminals
            .sessions_of(launch)
            .into_iter()
            .map(|s| {
                let live = self.terminals.is_live(&s);
                (s, live)
            })
            .collect();
        let rx = self.terminals.attach(launch);
        for (s, live) in was_live {
            if !live {
                // The phone shows whether a session can take prompts: refresh it.
                self.notify_session_changed(&s);
            }
            self.pump_terminal(&s);
        }
        rx.await.ok()
    }

    /// A wrapper saw the agent's permission menu appear (`Some`) or go away (`None`).
    pub fn terminal_prompt(
        &self,
        launch: &str,
        prompt: Option<TerminalPrompt>,
    ) -> anyhow::Result<()> {
        let previous = self.terminals.lock().on_screen.get(launch).cloned();
        match (prompt, previous) {
            (Some(p), Some(prev))
                if prev.prompt.options == p.options && prev.prompt.detail == p.detail =>
            {
                // Same menu (maybe the highlight moved): nothing new.
                if let Some(e) = self.terminals.lock().on_screen.get_mut(launch) {
                    e.prompt = p;
                }
            }
            (Some(p), prev) => {
                if let Some(prev) = prev {
                    self.terminals.lock().on_screen.remove(launch);
                    self.close_screen_permission(&prev);
                }
                // The most recently active shared session in this wrapper.
                let Some(summary) = self
                    .terminals
                    .sessions_of(launch)
                    .into_iter()
                    .filter_map(|s| self.summary(&s).ok().flatten())
                    .filter(|s| s.shared)
                    .max_by_key(|s| s.updated_ms)
                else {
                    return Ok(());
                };
                let tool = self
                    .terminals
                    .lock()
                    .last_tool
                    .get(&summary.id)
                    .cloned()
                    .unwrap_or_else(|| menu_tool(&p));
                let options: Vec<PermissionOption> = p
                    .options
                    .iter()
                    .enumerate()
                    .map(|(i, label)| PermissionOption {
                        id: format!("opt-{i}"),
                        kind: option_kind(label),
                        label: label.clone(),
                    })
                    .collect();
                let request = RequestId::generate();
                self.terminals.lock().on_screen.insert(
                    launch.to_owned(),
                    ScreenPermission {
                        request: request.clone(),
                        session: summary.id.clone(),
                        prompt: p.clone(),
                        options: options.clone(),
                    },
                );
                tracing::info!(session = %summary.id, title = %p.title, "permission menu on screen");
                self.ingest(
                    &native_of(&summary),
                    EventSource::Pty,
                    vec![EventKind::PermissionRequested {
                        request,
                        call: None,
                        tool,
                        reason: Some(p.title.clone()),
                        options,
                        expires_at_ms: None,
                    }],
                )?;
            }
            (None, Some(prev)) => {
                self.terminals.lock().on_screen.remove(launch);
                self.close_screen_permission(&prev);
                // Typing may be safe again.
                self.pump_terminal(&prev.session);
            }
            (None, None) => {}
        }
        Ok(())
    }

    /// The menu left the screen: if nobody answered on the phone, the terminal did.
    fn close_screen_permission(&self, p: &ScreenPermission) {
        let Ok(Some(s)) = self.summary(&p.session) else {
            return;
        };
        if s.status != SessionStatus::NeedsApproval {
            return; // already resolved (the phone answered)
        }
        let _ = self.ingest(
            &native_of(&s),
            EventSource::Pty,
            vec![EventKind::PermissionResolved {
                request: p.request.clone(),
                outcome: PermissionOutcome::Cancelled,
                by: Resolver::Terminal,
            }],
        );
    }

    /// The phone answered an on-screen menu: select that option in the terminal.
    /// `None` if `request` isn't an on-screen menu.
    pub fn resolve_screen_permission(
        &self,
        session: &SessionId,
        request: &RequestId,
        option: &str,
        device: DeviceId,
    ) -> Option<anyhow::Result<()>> {
        let (launch, perm) = {
            let g = self.terminals.lock();
            let (l, p) = g
                .on_screen
                .iter()
                .find(|(_, p)| &p.request == request && &p.session == session)?;
            (l.clone(), p.clone())
        };
        let Some(index) = perm.options.iter().position(|o| o.id == option) else {
            return Some(Err(anyhow::anyhow!(
                "'{option}' is not an option of that request"
            )));
        };
        self.terminals.select(&launch, index);
        tracing::info!(%session, %request, index, "selecting a menu option for the phone");
        let Ok(Some(s)) = self.summary(session) else {
            return Some(Ok(()));
        };
        Some(
            self.ingest(
                &native_of(&s),
                EventSource::Pty,
                vec![EventKind::PermissionResolved {
                    request: request.clone(),
                    outcome: PermissionOutcome::Selected {
                        option: option.to_owned(),
                        kind: perm.options[index].kind,
                    },
                    by: Resolver::Phone(device),
                }],
            )
            .map(|_| ()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(t: &str) -> QueuedPrompt {
        QueuedPrompt {
            text: t.into(),
            device: "phone".into(),
        }
    }

    #[test]
    fn deliver_then_claim() {
        let t = Terminals::default();
        let s = SessionId::from("S");
        t.link(&s, "L");
        assert!(t.is_live(&s), "freshly linked counts as live");
        t.enqueue(&s, p("one"));
        assert!(!t.try_deliver(&s), "no wrapper attached yet");
        let mut rx = t.attach("L");
        assert!(t.try_deliver(&s));
        assert_eq!(rx.try_recv().unwrap(), TerminalAction::Type("one".into()));
        assert_eq!(t.claim_typed(&s, "  one "), Some(DeviceId::from("phone")));
        assert_eq!(t.claim_typed(&s, "one"), None, "claimed once");
    }

    #[test]
    fn a_vanished_wrapper_keeps_the_prompt() {
        let t = Terminals::default();
        let s = SessionId::from("S");
        t.link(&s, "L");
        drop(t.attach("L"));
        t.enqueue(&s, p("x"));
        assert!(!t.try_deliver(&s));
        assert_eq!(t.take_queued(&s), Some(p("x")));
    }

    #[test]
    fn a_selection_waits_for_the_wrapper() {
        let t = Terminals::default();
        t.select("L", 2);
        let mut rx = t.attach("L");
        assert_eq!(rx.try_recv().unwrap(), TerminalAction::Select(2));
    }

    #[test]
    fn option_kinds_from_labels() {
        assert_eq!(option_kind("Yes, run command"), OptionKind::AllowOnce);
        assert_eq!(
            option_kind(
                "Yes, and always allow in this conversation for commands that start with 'touch'"
            ),
            OptionKind::AllowAlways
        );
        assert_eq!(option_kind("No, cancel"), OptionKind::RejectOnce);
    }
}

#[cfg(test)]
mod menu_tool_tests {
    use super::*;

    #[test]
    fn a_codex_menu_is_titled_by_its_command() {
        let p = styr_core::ipc::TerminalPrompt {
            title: "Would you like to run the following command?".into(),
            detail: "Environment: local\nReason: Do you want to allow creating x?\n$ touch x.txt".into(),
            options: vec!["Yes, proceed".into(), "No".into()],
            selected: 0,
        };
        let t = menu_tool(&p);
        assert_eq!((t.kind, t.title.as_str()), (ToolKind::Execute, "Run `touch x.txt`"));
        let hooks = styr_core::ipc::TerminalPrompt {
            title: "Hooks need review".into(),
            detail: "6 hooks are new or changed.".into(),
            options: vec![],
            selected: 0,
        };
        assert_eq!(menu_tool(&hooks).title, "6 hooks are new or changed.");
    }
}
