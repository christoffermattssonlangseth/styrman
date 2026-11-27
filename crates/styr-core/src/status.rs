//! Derived session status: a pure reducer over events. The session list shows this;
//! it is recomputed from history on daemon start, never trusted from storage alone.

use crate::event::{EventKind, TurnEndReason};
use crate::ids::RequestId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// The agent is doing something.
    Working,
    /// At least one permission request is waiting for an answer.
    NeedsApproval,
    /// The turn is over and the agent waits for the next prompt.
    Waiting,
    /// The last turn failed or the agent reported a fatal error.
    Error,
    /// The session has ended.
    Done,
}

impl SessionStatus {
    /// Whether the phone should treat this as "needs you".
    pub fn needs_user(self) -> bool {
        matches!(self, Self::NeedsApproval | Self::Waiting)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Working,
    Waiting,
    Error,
}

#[derive(Debug, Clone)]
pub struct StatusTracker {
    phase: Phase,
    open_requests: BTreeSet<RequestId>,
    ended: bool,
    shared: bool,
    /// One line on what the session is doing or last said, for the phone's list.
    activity: Option<String>,
    /// While a permission is open: what it asks to do (wins over `activity`).
    asking: Option<String>,
}

/// The first non-empty line of `text`, at most 120 characters.
fn first_line(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.trim_start_matches(['#', '*', '-', '>', ' ']).trim();
    if line.is_empty() {
        return None;
    }
    Some(if line.chars().count() > 120 {
        format!("{}…", line.chars().take(119).collect::<String>())
    } else {
        line.to_owned()
    })
}

impl Default for StatusTracker {
    fn default() -> Self {
        // A freshly started interactive session sits at its prompt.
        Self {
            phase: Phase::Waiting,
            open_requests: BTreeSet::new(),
            ended: false,
            shared: false,
            activity: None,
            asking: None,
        }
    }
}

impl StatusTracker {
    pub fn apply(&mut self, ev: &EventKind) {
        use EventKind::*;
        match ev {
            SessionStarted { .. } => {
                // A resume/clear starts a fresh lifecycle.
                self.ended = false;
                self.phase = Phase::Waiting;
                self.open_requests.clear();
                self.asking = None;
            }
            SessionEnded { .. } => {
                self.ended = true;
                self.open_requests.clear();
                self.asking = None;
            }
            SharingChanged { shared } => self.shared = *shared,
            UserMessage { .. } | TurnStarted { .. } => {
                self.phase = Phase::Working;
                self.activity = None;
            }
            Reasoning { .. } | Plan { .. } => self.phase = Phase::Working,
            ToolCall { tool, .. } => {
                self.phase = Phase::Working;
                self.activity = first_line(&tool.title);
            }
            AssistantMessage { text, mode, .. } => {
                if self.phase != Phase::Waiting {
                    self.phase = Phase::Working;
                }
                if *mode == crate::event::TextMode::Final
                    && let Some(l) = first_line(text)
                {
                    self.activity = Some(l);
                }
            }
            PermissionRequested { request, tool, .. } => {
                self.phase = Phase::Working;
                self.open_requests.insert(request.clone());
                self.asking = first_line(&tool.title);
            }
            PermissionResolved { request, .. } => {
                self.open_requests.remove(request);
                if self.open_requests.is_empty() {
                    self.asking = None;
                }
            }
            TurnEnded {
                reason,
                last_message,
                ..
            } => {
                self.open_requests.clear();
                self.asking = None;
                if let Some(l) = last_message.as_deref().and_then(first_line) {
                    self.activity = Some(l);
                }
                self.phase = match reason {
                    TurnEndReason::Failed => Phase::Error,
                    _ => Phase::Waiting,
                };
            }
            Error { fatal: true, .. } => self.phase = Phase::Error,
            Error { fatal: false, .. } | Notice { .. } | TaskEnded { .. } => {}
        }
    }

    pub fn status(&self) -> SessionStatus {
        if self.ended {
            SessionStatus::Done
        } else if !self.open_requests.is_empty() {
            SessionStatus::NeedsApproval
        } else {
            match self.phase {
                Phase::Working => SessionStatus::Working,
                Phase::Waiting => SessionStatus::Waiting,
                Phase::Error => SessionStatus::Error,
            }
        }
    }

    /// What the session is doing (the open approval first), for the phone's list.
    pub fn activity(&self) -> Option<String> {
        if self.ended {
            return None;
        }
        self.asking.clone().or_else(|| self.activity.clone())
    }

    pub fn shared(&self) -> bool {
        self.shared
    }

    /// Permission requests that never got a `PermissionResolved`.
    pub fn open_requests(&self) -> impl Iterator<Item = &RequestId> {
        self.open_requests.iter()
    }

    pub fn replay<'a>(events: impl IntoIterator<Item = &'a EventKind>) -> Self {
        let mut t = Self::default();
        for e in events {
            t.apply(e);
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::*;
    use crate::ids::*;

    fn tool(title: &str) -> ToolInfo {
        serde_json::from_value(
            serde_json::json!({"name": "Bash", "kind": "execute", "title": title, "input": null}),
        )
        .unwrap()
    }

    #[test]
    fn activity_follows_what_the_agent_is_doing() {
        let mut t = StatusTracker::default();
        t.apply(&EventKind::UserMessage {
            text: "go".into(),
            origin: Origin::Terminal,
        });
        assert_eq!(t.activity(), None);
        t.apply(&EventKind::ToolCall {
            call: ToolCallId("c".into()),
            tool: tool("Run `cargo test`"),
            status: ToolStatus::Pending,
            output: None,
        });
        assert_eq!(t.activity().as_deref(), Some("Run `cargo test`"));
        t.apply(&EventKind::PermissionRequested {
            request: RequestId("r".into()),
            call: None,
            tool: tool("Run `rm -rf build`"),
            reason: None,
            options: vec![],
            expires_at_ms: None,
        });
        assert_eq!(t.activity().as_deref(), Some("Run `rm -rf build`"));
        t.apply(&EventKind::PermissionResolved {
            request: RequestId("r".into()),
            outcome: serde_json::from_value(serde_json::json!({"outcome": "cancelled"})).unwrap(),
            by: serde_json::from_value(serde_json::json!({"by": "terminal"})).unwrap(),
        });
        assert_eq!(t.activity().as_deref(), Some("Run `cargo test`"));
        t.apply(&EventKind::TurnEnded {
            turn: None,
            reason: TurnEndReason::EndTurn,
            last_message: Some("\n## Done. All 41 tests pass\nmore".into()),
        });
        assert_eq!(t.activity().as_deref(), Some("Done. All 41 tests pass"));
        t.apply(&EventKind::SessionEnded { reason: None });
        assert_eq!(t.activity(), None);
    }
}
