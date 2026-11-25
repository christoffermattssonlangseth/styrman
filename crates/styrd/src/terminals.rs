//! Typing phone prompts into the agent's own terminal.
//!
//! `styr claude` runs the agent inside a pseudo-terminal it controls and holds an
//! `AttachTerminal` request open. A phone prompt for a session that runs inside
//! such a wrapper is handed to it, typed into the agent's prompt box and submitted,
//! so it looks exactly like typed input. When the agent then reports that prompt
//! (`UserPromptSubmit`), it is recognised and attributed to the phone.
//!
//! Safety: nothing is typed while a permission prompt is open (the keystrokes could
//! answer it) or after the session ended; the wrapper itself also waits while the
//! user is halfway through typing a line.

use crate::daemon::Daemon;
use crate::prompts::QueuedPrompt;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use styr_core::SessionStatus;
use styr_core::ids::{DeviceId, SessionId};
use tokio::sync::oneshot;

/// A wrapper that hasn't re-attached for this long is considered gone.
const LIVE_GRACE: Duration = Duration::from_secs(15);

#[derive(Default)]
pub struct Terminals {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Session → the launch (wrapper) it runs in.
    launch_of: HashMap<SessionId, String>,
    /// Launch → its open AttachTerminal request.
    attached: HashMap<String, oneshot::Sender<String>>,
    last_seen: HashMap<String, Instant>,
    /// Prompts waiting for a safe moment, per session.
    queue: HashMap<SessionId, VecDeque<QueuedPrompt>>,
    /// Typed into the terminal, not yet reported back by the agent.
    typed: HashMap<SessionId, VecDeque<QueuedPrompt>>,
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

    pub fn attach(&self, launch: &str) -> oneshot::Receiver<String> {
        let (tx, rx) = oneshot::channel();
        let mut g = self.lock();
        g.attached.insert(launch.to_owned(), tx);
        g.last_seen.insert(launch.to_owned(), Instant::now());
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

    /// Hand the next queued prompt to the session's wrapper if one is attached.
    pub fn try_deliver(&self, session: &SessionId) -> bool {
        let mut g = self.lock();
        let Some(launch) = g.launch_of.get(session).cloned() else {
            return false;
        };
        if !g.attached.contains_key(&launch) {
            return false;
        }
        let Some(p) = g.queue.get_mut(session).and_then(VecDeque::pop_front) else {
            return false;
        };
        let tx = g.attached.remove(&launch).expect("checked");
        g.last_seen.insert(launch, Instant::now());
        match tx.send(p.text.clone()) {
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

    /// If `text` is a prompt we typed for the phone, who sent it.
    pub fn claim_typed(&self, session: &SessionId, text: &str) -> Option<DeviceId> {
        let mut g = self.lock();
        let q = g.typed.get_mut(session)?;
        let i = q.iter().position(|p| p.text.trim() == text.trim())?;
        q.remove(i).map(|p| p.device)
    }

    pub fn forget(&self, session: &SessionId) {
        let mut g = self.lock();
        g.launch_of.remove(session);
        g.queue.remove(session);
        g.typed.remove(session);
    }
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

    /// Long-poll from a wrapper: resolves with the next prompt to type.
    pub async fn attach_terminal(self: &Arc<Self>, launch: &str) -> Option<String> {
        let rx = self.terminals.attach(launch);
        for s in self.terminals.sessions_of(launch) {
            self.pump_terminal(&s);
        }
        rx.await.ok()
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
        assert_eq!(rx.try_recv().unwrap(), "one");
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
}
