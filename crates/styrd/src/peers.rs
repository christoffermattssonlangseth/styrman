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
/// At most this many sessions started by asking, per sender in `RATE_WINDOW`.
const START_LIMIT: usize = 3;
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
    /// The helper each session started per agent (asker, agent) → helper, so follow-up
    /// questions go to the same one.
    helpers: Mutex<HashMap<(SessionId, AgentKind), SessionId>>,
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
        let key = from
            .as_ref()
            .map_or_else(|| "mac".to_owned(), |s| s.id.to_string());
        let device = DeviceId::from(format!("{AGENT_DEVICE_PREFIX}{key}").as_str());
        let target = match self.peer_for(to, from.as_ref())? {
            Peer::Running(s) => s,
            Peer::Start(agent, cwd) => {
                // None of that agent in the asker's folder: start one there, with
                // the message as its first prompt.
                self.count_send(&key, true)?;
                let message = labelled(from.as_ref(), text);
                let mut bus = self.subscribe();
                let id = self.start_session(
                    agent,
                    &cwd.to_string_lossy(),
                    Some(message.clone()),
                    device,
                )?;
                if let Some(f) = &from {
                    self.peer_state
                        .helpers
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert((f.id.clone(), agent), id.clone());
                    // Phones show it under the session that asked.
                    self.notify_session_changed(&id);
                }
                let target = self.summary(&id)?.context("the new session vanished")?;
                if let Some(f) = &from {
                    self.note(
                        f,
                        format!("Started {} for this and asked: {}", who(&target), text),
                    );
                }
                tracing::info!(to = %id, from = %key, "agent started another agent to ask it");
                let Some(limit) = wait else {
                    return Ok(AskOutcome::Sent);
                };
                return Ok(self
                    .await_reply(&mut bus, from.as_ref(), &target, &message, limit)
                    .await);
            }
        };

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
        self.count_send(&key, false)?;

        // Listen before sending, so the reply can't slip past.
        let mut bus = self.subscribe();
        let message = labelled(from.as_ref(), text);
        self.send_prompt(&target.id, &message, device)
            .with_context(|| format!("couldn't reach {}", who(&target)))?;
        if let Some(f) = &from {
            self.note(f, format!("Asked {}: {}", who(&target), text));
        }
        tracing::info!(to = %target.id, from = %key, "agent asked another agent");
        let Some(limit) = wait else {
            return Ok(AskOutcome::Sent);
        };
        Ok(self
            .await_reply(&mut bus, from.as_ref(), &target, &message, limit)
            .await)
    }

    /// Rate limits per sender: messages, and fewer new sessions started.
    fn count_send(&self, key: &str, starts: bool) -> Result<()> {
        let mut sent = self
            .peer_state
            .sent
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut check = |k: String, limit: usize, what: &str| -> Result<()> {
            let q = sent.entry(k).or_default();
            while q.front().is_some_and(|t| t.elapsed() > RATE_WINDOW) {
                q.pop_front();
            }
            if q.len() >= limit {
                bail!("rate limit: at most {limit} {what} per 10 minutes");
            }
            q.push_back(Instant::now());
            Ok(())
        };
        if starts {
            check(format!("{key}:starts"), START_LIMIT, "new agent sessions")?;
        }
        check(key.to_owned(), RATE_LIMIT, "messages to other agents")
    }

    /// Wait for `target`'s answer to `message`, noting it in the asker's session.
    async fn await_reply(
        &self,
        bus: &mut tokio::sync::broadcast::Receiver<BusMsg>,
        from: Option<&SessionSummary>,
        target: &SessionSummary,
        message: &str,
        limit: Duration,
    ) -> AskOutcome {
        if let Some(f) = from {
            self.peer_state
                .pending
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(target.id.clone(), f.id.clone());
        }
        let outcome = wait_for_reply(bus, &target.id, message, limit).await;
        self.peer_state
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&target.id);
        if let Some(f) = from {
            match &outcome {
                AskOutcome::Reply(r) => {
                    // Whole (up to a long answer's worth): phones fold it.
                    self.note(f, format!("{} replied: {}", who(target), short(r, 6000)))
                }
                AskOutcome::TimedOut => {
                    self.note(f, format!("No reply from {} in time", who(target)))
                }
                AskOutcome::Sent => {}
            }
        }
        outcome
    }

    /// The session `helper` is working for: the one that started it to ask it
    /// something, or else the agent that sent its latest prompt (a session you
    /// started can become another agent's reviewer). From the history, so it
    /// survives a restart; a prompt of your own ends it.
    pub fn helper_of(&self, helper: &SessionId) -> Option<SessionId> {
        let started = self
            .peer_state
            .helpers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find(|(_, h)| *h == helper)
            .map(|((asker, _), _)| asker.clone());
        started.or_else(|| self.prompted_by_agent(helper))
    }

    /// Who `to` means. An agent name, asked from a session in a folder, means a
    /// helper of its own in that folder: the one it started before (follow-ups), or a
    /// new one. Never someone else's session that happens to be open there (it may
    /// be in the middle of the user's own work). An id reaches any session on purpose.
    fn peer_for(&self, to: &str, from: Option<&SessionSummary>) -> Result<Peer> {
        let t = to.trim().to_lowercase();
        let agent = [AgentKind::Claude, AgentKind::Codex, AgentKind::Antigravity]
            .into_iter()
            .find(|a| agent_label(*a).to_lowercase() == t || a.as_str() == t);
        let (Some(agent), Some(f)) = (agent, from) else {
            return self
                .resolve_peer(to, from.map(|s| &s.id))
                .map(Peer::Running);
        };
        let Some(cwd) = f.cwd.clone() else {
            return self.resolve_peer(to, Some(&f.id)).map(Peer::Running);
        };
        let helper = self
            .peer_state
            .helpers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&(f.id.clone(), agent))
            .cloned();
        if let Some(h) = helper
            && let Some(s) = self.peers(Some(&f.id))?.into_iter().find(|s| s.id == h)
        {
            return Ok(Peer::Running(s));
        }
        Ok(Peer::Start(agent, cwd))
    }
}

// Short-lived, one at a time: boxing the summary would buy nothing.
#[allow(clippy::large_enum_variant)]
enum Peer {
    Running(SessionSummary),
    /// Start this agent in this folder.
    Start(AgentKind, std::path::PathBuf),
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
