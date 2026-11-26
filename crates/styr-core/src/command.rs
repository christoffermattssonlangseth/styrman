//! Commands flow the other way: phone → daemon → adapter.

use crate::ids::RequestId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Send a new user prompt (delivered as the adapter's `PromptDelivery` allows).
    SendPrompt {
        text: String,
    },
    /// Answer a pending permission with one of its `PermissionOption::id`s.
    ResolvePermission {
        request: RequestId,
        option: String,
    },
    /// Stop the current turn, where the agent supports it.
    Interrupt,
    SetSharing {
        shared: bool,
    },
    /// Switch the agent's permission mode (Claude: "default", "acceptEdits", "plan",
    /// "auto", "bypassPermissions"), as Shift+Tab does in its terminal.
    SetMode {
        mode: String,
    },
    /// Quit the agent (as Ctrl+C would in its terminal) and close the cmux tab if
    /// the phone opened it. Only for sessions running in `styr <agent>`.
    EndSession,
}
