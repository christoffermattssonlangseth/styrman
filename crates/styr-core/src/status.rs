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
}

impl Default for StatusTracker {
    fn default() -> Self {
        // A freshly started interactive session sits at its prompt.
        Self {
            phase: Phase::Waiting,
            open_requests: BTreeSet::new(),
            ended: false,
            shared: false,
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
            }
            SessionEnded { .. } => {
                self.ended = true;
                self.open_requests.clear();
            }
            SharingChanged { shared } => self.shared = *shared,
            TurnStarted { .. }
            | UserMessage { .. }
            | Reasoning { .. }
            | Plan { .. }
            | ToolCall { .. } => {
                self.phase = Phase::Working;
            }
            AssistantMessage { .. } => {
                if self.phase != Phase::Waiting {
                    self.phase = Phase::Working;
                }
            }
            PermissionRequested { request, .. } => {
                self.phase = Phase::Working;
                self.open_requests.insert(request.clone());
            }
            PermissionResolved { request, .. } => {
                self.open_requests.remove(request);
            }
            TurnEnded { reason, .. } => {
                self.open_requests.clear();
                self.phase = match reason {
                    TurnEndReason::Failed => Phase::Error,
                    _ => Phase::Waiting,
                };
            }
            Error { fatal: true, .. } => self.phase = Phase::Error,
            Error { fatal: false, .. } | Notice { .. } => {}
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
