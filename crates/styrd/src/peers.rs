//! Agents talking to each other (`styr peers`, `styr ask`).
//!
//! An agent asks another shared session something: the message is typed into that
//! session's terminal like a phone prompt, labelled with who it's from, and the
//! asking agent gets the reply when the other agent's turn ends. Agents only do this
//! when the user asks them to (their instructions say so); everything shows up on
//! the phone in both sessions, and approvals stay exactly as they are.
//!
//! Guards: shared sessions only, no asking yourself, no loops (B can't ask A back
//! while A is waiting on B), a maximum chain depth, and a rate limit per sender.

use crate::daemon::{BusMsg, Daemon};
use anyhow::{Context, Result, bail};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use styr_core::event::{EventKind, EventSource, Level};
use styr_core::ids::{AgentKind, DeviceId, SessionId};
use styr_core::ipc::{SessionSummary, SessionUpdate};

/// At most this many asks per sender in `RATE_WINDOW`.
const RATE_LIMIT: usize = 10;
const RATE_WINDOW: Duration = Duration::from_secs(600);
/// A asks B, B asks C: fine. A deeper chain is refused.
const MAX_DEPTH: usize = 2;
/// The device id of a message sent by an agent: `agent:<session>` (`agent:mac` when
/// `styr ask` runs outside any agent).
pub const AGENT_DEVICE_PREFIX: &str = "agent:";

#[derive(Default)]
pub struct Peers {
    /// Session answering an ask → the session that asked (and is waiting).
    pending: Mutex<HashMap<SessionId, SessionId>>,
    sent: Mutex<HashMap<String, VecDeque<Instant>>>,
}

#[derive(Debug, PartialEq)]
pub enum AskOutcome {
    /// The other agent's reply (its last message of the turn).
    Reply(String),
    /// Delivered or queued; not waiting for a reply.
    Sent,
    /// No reply within the time limit (the message was delivered).
    TimedOut,
}

pub fn agent_label(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "Claude",
        AgentKind::Codex => "Codex",
        AgentKind::Antigravity => "agy",
        AgentKind::Pty => "Agent",
    }
}

fn repo(s: &SessionSummary) -> String {
    s.cwd
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session".into())
}

fn who(s: &SessionSummary) -> String {
    format!("{} ({})", agent_label(s.agent), repo(s))
}

/// What the receiving agent reads.
pub fn labelled(from: Option<&SessionSummary>, text: &str) -> String {
    let sender = from.map_or_else(|| "the user's Mac".to_owned(), who);
    let back = from.map_or("the user", |s| agent_label(s.agent));
    format!(
        "Message from {sender}, sent through Styrman:\n\n{}\n\n(Reply as you normally would; your reply goes back to {back}.)",
        text.trim()
    )
}

impl Daemon {
    /// Shared sessions other than `exclude`, newest first.
    pub fn peers(&self, exclude: Option<&SessionId>) -> Result<Vec<SessionSummary>> {
        Ok(self
            .summaries(false)?
            .into_iter()
            .filter(|s| s.shared && s.status != styr_core::SessionStatus::Done)
            .filter(|s| Some(&s.id) != exclude)
            .collect())
    }

    /// The shared session the `styr` terminal `launch` belongs to (the asker).
    pub fn session_of_launch(&self, launch: &str) -> Option<SessionSummary> {
        self.terminals
            .sessions_of(launch)
            .into_iter()
            .filter_map(|s| self.summary(&s).ok().flatten())
            .filter(|s| s.shared)
            .max_by_key(|s| s.updated_ms)
    }

    /// Find a peer by id (or a unique prefix/suffix of it) or by agent name.
    pub fn resolve_peer(&self, to: &str, exclude: Option<&SessionId>) -> Result<SessionSummary> {
        let peers = self.peers(exclude)?;
        let t = to.trim().to_lowercase();
        let by_id: Vec<_> = peers
            .iter()
            .filter(|s| {
                let id = s.id.as_str().to_lowercase();
                id == t || (t.len() >= 4 && (id.starts_with(&t) || id.ends_with(&t)))
            })
            .collect();
        if let [one] = by_id.as_slice() {
            return Ok((*one).clone());
        }
        let by_agent: Vec<_> = peers
            .iter()
            .filter(|s| agent_label(s.agent).to_lowercase() == t || s.agent.as_str() == t)
            .collect();
        match by_agent.as_slice() {
            [one] => Ok((*one).clone()),
            [] => bail!("no shared session matches '{to}' (see `styr peers`)"),
            _ => bail!("several {to} sessions are shared; use the id from `styr peers`"),
        }
    }

    fn note(&self, s: &SessionSummary, text: String) {
        let native = styr_core::ids::NativeSessionRef::new(s.agent, s.native_id.clone());
        let _ = self.ingest(
            &SessionUpdate::for_native(native),
            EventSource::Daemon,
            vec![EventKind::Notice {
                level: Level::Info,
                text,
            }],
        );
    }

    /// Ask the peer `to` something. `launch` is the asking terminal (None: the user
    /// running `styr ask` themselves). With `wait`, returns the reply.
    pub async fn ask(
        self: &Arc<Self>,
        launch: Option<&str>,
        to: &str,
        text: &str,
        wait: Option<Duration>,
    ) -> Result<AskOutcome> {
        let text = text.trim();
        anyhow::ensure!(!text.is_empty(), "nothing to ask");
        let from = launch.and_then(|l| self.session_of_launch(l));
        let target = self.resolve_peer(to, from.as_ref().map(|s| &s.id))?;
        let key = from
            .as_ref()
            .map_or_else(|| "mac".to_owned(), |s| s.id.to_string());

        // Guards.
        {
            let pending = self
                .peer_state
                .pending
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(f) = &from {
                if pending.get(&f.id) == Some(&target.id) {
                    bail!(
                        "{} is waiting for your answer: reply to it instead of asking it back",
                        agent_label(target.agent)
                    );
                }
                let mut depth = 0;
                let mut at = f.id.clone();
                while let Some(up) = pending.get(&at) {
                    depth += 1;
                    at = up.clone();
                }
                if depth >= MAX_DEPTH {
                    bail!(
                        "too many agents are already waiting on each other; answer what you were asked first"
                    );
                }
            }
            if pending.contains_key(&target.id) && wait.is_some() {
                bail!(
                    "{} is already answering another agent; try again when it's done",
                    who(&target)
                );
            }
        }
        {
            let mut sent = self
                .peer_state
                .sent
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let q = sent.entry(key.clone()).or_default();
            while q.front().is_some_and(|t| t.elapsed() > RATE_WINDOW) {
                q.pop_front();
            }
            if q.len() >= RATE_LIMIT {
                bail!("rate limit: at most {RATE_LIMIT} messages to other agents per 10 minutes");
            }
            q.push_back(Instant::now());
        }

        // Listen before sending, so the reply can't slip past.
        let mut bus = self.subscribe();
        let message = labelled(from.as_ref(), text);
        let device = DeviceId::from(format!("{AGENT_DEVICE_PREFIX}{key}").as_str());
        self.send_prompt(&target.id, &message, device)
            .with_context(|| format!("couldn't reach {}", who(&target)))?;
        if let Some(f) = &from {
            self.note(f, format!("Asked {}: {}", who(&target), text));
        }
        tracing::info!(to = %target.id, from = %key, "agent asked another agent");
        let Some(limit) = wait else {
            return Ok(AskOutcome::Sent);
        };

        if let Some(f) = &from {
            self.peer_state
                .pending
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(target.id.clone(), f.id.clone());
        }
        let outcome = wait_for_reply(&mut bus, &target.id, &message, limit).await;
        self.peer_state
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&target.id);
        if let Some(f) = &from {
            match &outcome {
                AskOutcome::Reply(r) => {
                    self.note(f, format!("{} replied: {}", who(&target), short(r, 400)))
                }
                AskOutcome::TimedOut => {
                    self.note(f, format!("No reply from {} in time", who(&target)))
                }
                AskOutcome::Sent => {}
            }
        }
        Ok(outcome)
    }
}

fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}

/// Wait for the target to pick up `message` and finish its turn; its last reply.
async fn wait_for_reply(
    bus: &mut tokio::sync::broadcast::Receiver<BusMsg>,
    target: &SessionId,
    message: &str,
    limit: Duration,
) -> AskOutcome {
    let deadline = tokio::time::Instant::now() + limit;
    let mut picked_up = false;
    let mut last_reply: Option<String> = None;
    loop {
        let msg = match tokio::time::timeout_at(deadline, bus.recv()).await {
            Err(_) => return AskOutcome::TimedOut,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) => return AskOutcome::TimedOut,
            Ok(Ok(m)) => m,
        };
        let BusMsg::Event(ev) = msg else { continue };
        if &ev.session != target {
            continue;
        }
        match ev.kind {
            EventKind::UserMessage { text, .. } if text.trim() == message.trim() => {
                picked_up = true
            }
            EventKind::AssistantMessage { text, mode, .. } if picked_up => {
                last_reply = Some(match (mode, last_reply.take()) {
                    (styr_core::event::TextMode::Delta, Some(prev)) => prev + &text,
                    _ => text,
                });
            }
            EventKind::TurnEnded { last_message, .. } if picked_up => {
                return AskOutcome::Reply(
                    last_message
                        .or(last_reply)
                        .unwrap_or_else(|| "(no text in the reply)".into()),
                );
            }
            EventKind::SessionEnded { .. } => return AskOutcome::TimedOut,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_say_who_they_are_from() {
        let m = labelled(None, " hi ");
        assert!(m.starts_with("Message from the user's Mac, sent through Styrman:\n\nhi"));
        assert!(m.contains("your reply goes back to the user"));
    }
}
