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
    /// Switch the agent's permission mode.
    Mode(String),
    /// Quit the agent.
    End,
    /// Stop the current turn (Esc).
    Interrupt,
    /// Send the screen for a while (a phone is watching).
    WatchScreen,
    Restart(Restart),
}

/// Restart the agent with another model or effort (see `Daemon::set_model`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restart {
    pub resume: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<String>,
}

/// Permission modes the phone may ask for (Claude's names).
pub const MODES: [&str; 5] = [
    "default",
    "acceptEdits",
    "plan",
    "auto",
    "bypassPermissions",
];

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
    /// Mode changes not yet handed to the wrapper, per launch.
    mode_sets: HashMap<String, String>,
    /// Launches asked to quit, not yet told.
    ends: std::collections::HashSet<String>,
    /// Launches asked to stop their turn, not yet told.
    interrupts: std::collections::HashSet<String>,
    /// Launches asked for their screen, not yet told.
    screens: std::collections::HashSet<String>,
    /// The permission mode the agent shows, per launch.
    modes: HashMap<String, String>,
    /// The permission menu currently on screen, per launch.
    on_screen: HashMap<String, ScreenPermission>,
    /// The last tool the agent announced, per session (to describe a menu).
    last_tool: HashMap<SessionId, ToolInfo>,
    /// Launch → (agent, canonical folder, first seen), as the wrapper reported them.
    wrappers: HashMap<String, (styr_core::ids::AgentKind, std::path::PathBuf, Instant)>,
    /// What each wrapper understands (`styr_core::ipc::WRAPPER_LEVEL` when it attached).
    levels: HashMap<String, u32>,
    /// Restarts not yet handed to the wrapper, per launch.
    restarts: HashMap<String, Restart>,
    /// Launches restarting for the phone: their agent's end and new start are one
    /// session going on, not an end.
    restarting: HashMap<String, Instant>,
    /// The model and effort the phone last restarted a launch with.
    chosen: HashMap<String, (Option<String>, Option<String>)>,
}

fn canon(p: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned())
}

/// What a menu read off the screen is about, when the agent didn't announce the tool:
/// the `$ command` line if there is one (Codex), else the first line that isn't
/// "Environment: …" / "Reason: …".
pub fn menu_tool(p: &styr_core::ipc::TerminalPrompt) -> ToolInfo {
    let lines: Vec<&str> = p
        .detail
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // Codex's file edits: "Destination:" then the path, hard-wrapped over lines.
    if let Some(i) = lines.iter().position(|l| l.starts_with("Destination:")) {
        let mut path = lines[i]
            .trim_start_matches("Destination:")
            .trim()
            .to_owned();
        for l in &lines[i + 1..] {
            if l.contains(": ") || l.starts_with("$ ") {
                break;
            }
            path.push_str(l);
        }
        if !path.is_empty() {
            let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
            return ToolInfo {
                name: "edit".into(),
                kind: ToolKind::Edit,
                title: format!("Edit {name}"),
                input: serde_json::json!({"path": path}),
                locations: vec![path.into()],
            };
        }
    }
    if let Some(i) = lines.iter().position(|l| l.starts_with("$ ")) {
        // The command is the "$ " line plus whatever wrapped below it (the options
        // aren't part of the detail).
        let mut cmd = lines[i].trim_start_matches("$ ").to_owned();
        for l in &lines[i + 1..] {
            if !(cmd.ends_with('/') || cmd.ends_with('-') && !cmd.ends_with(" -")) {
                cmd.push(' ');
            }
            cmd.push_str(l);
        }
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

    /// Record which actions a wrapper understands (sent with every attach).
    pub fn note_level(&self, launch: &str, level: u32) {
        self.lock().levels.insert(launch.to_owned(), level);
    }

    /// Which actions `launch` understands: 0 if it's from before levels were sent.
    pub fn level_of(&self, launch: &str) -> u32 {
        self.lock().levels.get(launch).copied().unwrap_or(0)
    }

    /// Record what a wrapper runs and where (sent with every attach).
    pub fn note_wrapper(
        &self,
        launch: &str,
        agent: styr_core::ids::AgentKind,
        cwd: &std::path::Path,
    ) {
        let mut g = self.lock();
        g.wrappers
            .entry(launch.to_owned())
            .or_insert_with(|| (agent, canon(cwd), Instant::now()));
    }

    fn launch_live(g: &Inner, launch: &str) -> bool {
        g.attached.contains_key(launch)
            || g.last_seen
                .get(launch)
                .is_some_and(|t| t.elapsed() < LIVE_GRACE)
    }

    /// Whether `launch` is a live wrapper of `agent` in `cwd`.
    pub fn wrapper_in(
        &self,
        launch: &str,
        agent: styr_core::ids::AgentKind,
        cwd: &std::path::Path,
    ) -> bool {
        let g = self.lock();
        Self::launch_live(&g, launch)
            && g.wrappers
                .get(launch)
                .is_some_and(|(a, c, _)| *a == agent && *c == canon(cwd))
    }

    /// Live wrappers of `agent` in `cwd`, newest first.
    pub fn live_wrappers_in(
        &self,
        agent: styr_core::ids::AgentKind,
        cwd: &std::path::Path,
    ) -> Vec<String> {
        let g = self.lock();
        let cwd = canon(cwd);
        let mut v: Vec<(&String, Instant)> = g
            .wrappers
            .iter()
            .filter(|(l, (a, c, _))| *a == agent && *c == cwd && Self::launch_live(&g, l))
            .map(|(l, (_, _, t))| (l, *t))
            .collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
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
        // A pending menu selection goes out first, then a mode change.
        if g.ends.remove(launch) {
            let _ = tx.send(TerminalAction::End);
        } else if let Some(r) = g.restarts.remove(launch) {
            let _ = tx.send(TerminalAction::Restart(r));
        } else if g.interrupts.remove(launch) {
            let _ = tx.send(TerminalAction::Interrupt);
        } else if g.screens.remove(launch) {
            let _ = tx.send(TerminalAction::WatchScreen);
        } else if let Some(i) = g.selects.remove(launch) {
            let _ = tx.send(TerminalAction::Select(i));
        } else if let Some(m) = g.mode_sets.remove(launch) {
            let _ = tx.send(TerminalAction::Mode(m));
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
        // Busy with it, not gone: it stays live until it attaches again.
        g.last_seen.insert(launch.to_owned(), Instant::now());
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

    /// Ask the wrapper to switch its agent's permission mode.
    pub fn set_mode(&self, launch: &str, mode: &str) {
        let mut g = self.lock();
        g.last_seen.insert(launch.to_owned(), Instant::now());
        match g.attached.remove(launch) {
            Some(tx) => {
                if tx.send(TerminalAction::Mode(mode.to_owned())).is_err() {
                    g.mode_sets.insert(launch.to_owned(), mode.to_owned());
                }
            }
            None => {
                g.mode_sets.insert(launch.to_owned(), mode.to_owned());
            }
        }
    }

    /// Ask the wrapper for its screen (for the next 20 s).
    pub fn watch_screen(&self, launch: &str) {
        let mut g = self.lock();
        g.last_seen.insert(launch.to_owned(), Instant::now());
        let sent = g
            .attached
            .remove(launch)
            .is_some_and(|tx| tx.send(TerminalAction::WatchScreen).is_ok());
        if !sent {
            g.screens.insert(launch.to_owned());
        }
    }

    /// Ask the wrapper to restart its agent (another model or effort).
    pub fn restart(&self, launch: &str, r: Restart) {
        let mut g = self.lock();
        g.last_seen.insert(launch.to_owned(), Instant::now());
        g.chosen
            .insert(launch.to_owned(), (r.model.clone(), r.effort.clone()));
        g.restarting.insert(launch.to_owned(), Instant::now());
        if let Some(tx) = g.attached.remove(launch) {
            if let Err(TerminalAction::Restart(r)) = tx.send(TerminalAction::Restart(r)) {
                g.restarts.insert(launch.to_owned(), r);
            }
        } else {
            g.restarts.insert(launch.to_owned(), r);
        }
    }

    /// Whether `launch` is restarting for the phone (asked for within the last minute).
    pub fn restarting(&self, launch: &str) -> bool {
        self.lock()
            .restarting
            .get(launch)
            .is_some_and(|at| at.elapsed() < Duration::from_secs(60))
    }

    /// The restarted agent is up again.
    pub fn restarted(&self, launch: &str) {
        self.lock().restarting.remove(launch);
    }

    /// The model and effort the phone last chose for `launch`.
    pub fn chosen(&self, launch: &str) -> (Option<String>, Option<String>) {
        self.lock().chosen.get(launch).cloned().unwrap_or_default()
    }

    /// Ask the wrapper to stop its agent's current turn.
    pub fn interrupt(&self, launch: &str) {
        let mut g = self.lock();
        g.last_seen.insert(launch.to_owned(), Instant::now());
        let sent = g
            .attached
            .remove(launch)
            .is_some_and(|tx| tx.send(TerminalAction::Interrupt).is_ok());
        if !sent {
            g.interrupts.insert(launch.to_owned());
        }
    }

    /// Ask the wrapper to quit its agent.
    pub fn end(&self, launch: &str) {
        let mut g = self.lock();
        g.last_seen.insert(launch.to_owned(), Instant::now());
        g.selects.remove(launch);
        g.mode_sets.remove(launch);
        let sent = g
            .attached
            .remove(launch)
            .is_some_and(|tx| tx.send(TerminalAction::End).is_ok());
        if !sent {
            g.ends.insert(launch.to_owned());
        }
    }

    /// The wrapper read a (new) mode off the screen; true if it changed.
    pub fn note_mode(&self, launch: &str, mode: Option<String>) -> bool {
        let mut g = self.lock();
        match mode {
            Some(m) => g.modes.insert(launch.to_owned(), m.clone()).as_ref() != Some(&m),
            None => g.modes.remove(launch).is_some(),
        }
    }

    pub fn mode_of(&self, session: &SessionId) -> Option<String> {
        let g = self.lock();
        g.launch_of
            .get(session)
            .and_then(|l| g.modes.get(l))
            .cloned()
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
        // A session started from the phone: Claude and agy announce themselves once
        // they're up (after any "trust this folder?" question), so their first message
        // waits for that. Codex only creates its session with the first message; its
        // wrapper waits for Codex's input box instead.
        let started = self.summary(session).ok().flatten().is_some_and(|s| {
            !s.native_id.starts_with("launch:") || s.agent == styr_core::ids::AgentKind::Codex
        });
        let typed = safe && started && self.terminals.try_deliver(session);
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
                // What the menu itself says it's about ("$ command", "Destination:")
                // wins: the last tool the agent announced can be an older one, and an
                // approval card must never show a different command than the one asked about.
                let from_screen = menu_tool(&p);
                let tool = if from_screen.kind != ToolKind::Other {
                    from_screen
                } else {
                    self.terminals
                        .lock()
                        .last_tool
                        .get(&summary.id)
                        .cloned()
                        .unwrap_or(from_screen)
                };
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

        // A mode change waits for the wrapper too, after any menu answer.
        t.set_mode("L", "plan");
        let mut rx = t.attach("L");
        assert_eq!(rx.try_recv().unwrap(), TerminalAction::Mode("plan".into()));
        assert!(t.note_mode("L", Some("plan".into())));
        assert!(!t.note_mode("L", Some("plan".into())), "unchanged");

        // Quitting goes before anything else still waiting.
        t.set_mode("L", "auto");
        t.end("L");
        let mut rx = t.attach("L");
        assert_eq!(rx.try_recv().unwrap(), TerminalAction::End);
        assert!(
            t.lock().mode_sets.is_empty(),
            "a mode change after quitting is dropped"
        );
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
            detail: "Environment: local\nReason: Do you want to allow creating x?\n$ touch x.txt"
                .into(),
            options: vec!["Yes, proceed".into(), "No".into()],
            selected: 0,
        };
        let t = menu_tool(&p);
        assert_eq!(
            (t.kind, t.title.as_str()),
            (ToolKind::Execute, "Run `touch x.txt`")
        );
        let hooks = styr_core::ipc::TerminalPrompt {
            title: "Hooks need review".into(),
            detail: "6 hooks are new or changed.".into(),
            options: vec![],
            selected: 0,
        };
        assert_eq!(menu_tool(&hooks).title, "6 hooks are new or changed.");
        // A file edit, with the destination path wrapped like on a real 100-column screen.
        let edits = styr_core::ipc::TerminalPrompt {
            title: "Would you like to make the following edits?".into(),
            detail: "Description: Apply proposed file edits\nDestination:\n/private/tmp/w/637c37c2/scr\natchpad/cap/notes.txt".into(),
            options: vec!["Yes, proceed".into(), "No".into()],
            selected: 0,
        };
        let t = menu_tool(&edits);
        assert_eq!(
            (t.kind, t.title.as_str()),
            (ToolKind::Edit, "Edit notes.txt")
        );
        assert_eq!(
            t.input["path"],
            "/private/tmp/w/637c37c2/scratchpad/cap/notes.txt"
        );
    }
}
