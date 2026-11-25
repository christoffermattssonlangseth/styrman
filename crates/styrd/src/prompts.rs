//! Prompts from the phone, delivered through the agent's background wait hook
//! (Claude: the plugin's async `asyncRewake` Stop hook).
//!
//! Per session there is at most one waiting hook and a FIFO of queued prompts:
//! - a prompt sent while a hook waits is handed to it at once (Claude wakes up);
//! - a prompt sent while the agent is busy is queued and taken by the next wait;
//! - a newer wait, a prompt typed in the terminal, unsharing or the session ending
//!   releases the waiting hook (it exits quietly).

use crate::daemon::Daemon;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use styr_core::ids::{DeviceId, SessionId};
use tokio::sync::oneshot;

pub const MAX_PROMPT_CHARS: usize = 32_000;

#[derive(Debug, Clone, PartialEq)]
pub struct QueuedPrompt {
    pub text: String,
    pub device: DeviceId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Handed to a waiting hook; the agent is starting on it.
    Delivered,
    /// Queued (position, 1-based) until the agent next stops.
    Queued(usize),
}

type Waiter = (u64, oneshot::Sender<Option<QueuedPrompt>>);

#[derive(Default)]
pub struct Prompts {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    queues: HashMap<SessionId, VecDeque<QueuedPrompt>>,
    waiters: HashMap<SessionId, Waiter>,
    next_gen: u64,
}

impl Prompts {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn submit(&self, session: &SessionId, prompt: QueuedPrompt) -> Delivery {
        let mut g = self.lock();
        let prompt = match g.waiters.remove(session) {
            Some((_, tx)) => match tx.send(Some(prompt)) {
                Ok(()) => return Delivery::Delivered,
                Err(back) => back.expect("sent Some"),
            },
            None => prompt,
        };
        let q = g.queues.entry(session.clone()).or_default();
        q.push_back(prompt);
        Delivery::Queued(q.len())
    }

    /// Let a waiting hook go without a prompt.
    pub fn release(&self, session: &SessionId) {
        if let Some((_, tx)) = self.lock().waiters.remove(session) {
            let _ = tx.send(None);
        }
    }

    /// Release the waiter and drop queued prompts (unshared / ended).
    pub fn clear(&self, session: &SessionId) {
        self.release(session);
        self.lock().queues.remove(session);
    }

    pub fn queued(&self, session: &SessionId) -> usize {
        self.lock().queues.get(session).map_or(0, VecDeque::len)
    }

    fn take_queued(&self, session: &SessionId) -> Option<QueuedPrompt> {
        self.lock().queues.get_mut(session)?.pop_front()
    }

    fn requeue_front(&self, session: &SessionId, p: QueuedPrompt) {
        self.lock()
            .queues
            .entry(session.clone())
            .or_default()
            .push_front(p);
    }

    fn register(&self, session: &SessionId) -> (u64, oneshot::Receiver<Option<QueuedPrompt>>) {
        let (tx, rx) = oneshot::channel();
        let mut g = self.lock();
        g.next_gen += 1;
        let generation = g.next_gen;
        if let Some((_, old)) = g.waiters.insert(session.clone(), (generation, tx)) {
            let _ = old.send(None); // only the newest wait for a session stays
        }
        (generation, rx)
    }

    fn unregister(&self, session: &SessionId, generation: u64) {
        let mut g = self.lock();
        if g.waiters
            .get(session)
            .is_some_and(|(gen_, _)| *gen_ == generation)
        {
            g.waiters.remove(session);
        }
    }
}

/// Unregisters on drop and puts back a prompt that arrived as the wait was cancelled.
struct WaitGuard<'a> {
    prompts: &'a Prompts,
    session: SessionId,
    generation: u64,
    rx: oneshot::Receiver<Option<QueuedPrompt>>,
}

impl Drop for WaitGuard<'_> {
    fn drop(&mut self) {
        self.prompts.unregister(&self.session, self.generation);
        if let Ok(Some(p)) = self.rx.try_recv() {
            self.prompts.requeue_front(&self.session, p);
        }
    }
}

impl Daemon {
    /// Wait for the next phone prompt for `session` (or take a queued one).
    pub async fn wait_for_prompt(
        self: &Arc<Self>,
        session: &SessionId,
        timeout: Duration,
    ) -> Option<QueuedPrompt> {
        if let Some(p) = self.prompts.take_queued(session) {
            return Some(p);
        }
        let (generation, rx) = self.prompts.register(session);
        let mut guard = WaitGuard {
            prompts: &self.prompts,
            session: session.clone(),
            generation,
            rx,
        };
        match tokio::time::timeout(timeout, &mut guard.rx).await {
            Ok(Ok(p)) => p,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(t: &str) -> QueuedPrompt {
        QueuedPrompt {
            text: t.into(),
            device: "d".into(),
        }
    }

    #[test]
    fn queue_then_take_in_order() {
        let ps = Prompts::default();
        let s = SessionId::from("S");
        assert_eq!(ps.submit(&s, p("a")), Delivery::Queued(1));
        assert_eq!(ps.submit(&s, p("b")), Delivery::Queued(2));
        assert_eq!(ps.take_queued(&s), Some(p("a")));
        assert_eq!(ps.queued(&s), 1);
        ps.clear(&s);
        assert_eq!(ps.queued(&s), 0);
    }

    #[test]
    fn a_newer_wait_releases_the_older() {
        let ps = Prompts::default();
        let s = SessionId::from("S");
        let (_, mut old) = ps.register(&s);
        let (_, mut new) = ps.register(&s);
        assert_eq!(old.try_recv(), Ok(None), "old waiter released");
        assert_eq!(ps.submit(&s, p("hi")), Delivery::Delivered);
        assert_eq!(new.try_recv(), Ok(Some(p("hi"))));
    }

    #[test]
    fn a_prompt_for_a_vanished_waiter_is_queued() {
        let ps = Prompts::default();
        let s = SessionId::from("S");
        let (_, rx) = ps.register(&s);
        drop(rx);
        assert_eq!(ps.submit(&s, p("hi")), Delivery::Queued(1));
    }
}
