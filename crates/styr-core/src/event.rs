//! The normalized event model. Every adapter translates its agent's native signals
//! (hooks, JSON-RPC notifications, transcript lines, PTY output) into these events.
//! They are persisted in SQLite and streamed to the phone, so their JSON shape is a
//! public, versioned contract (see `tests/wire_shape.rs`).

use crate::ids::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Milliseconds since the Unix epoch.
pub type TimestampMs = i64;

pub fn now_ms() -> TimestampMs {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A persisted, sequenced event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub session: SessionId,
    /// Per-session, monotonic, daemon-assigned, starting at 1. The phone resumes with `since_seq`.
    pub seq: u64,
    pub at_ms: TimestampMs,
    pub source: EventSource,
    pub kind: EventKind,
}

/// Where the daemon learned about an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    Hook,
    Protocol,
    Transcript,
    Pty,
    Daemon,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    SessionStarted {
        agent: AgentKind,
        native: NativeSessionRef,
        cwd: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        workspace_roots: Vec<PathBuf>,
        model: Option<String>,
        title: Option<String>,
        start: StartKind,
    },
    SessionEnded {
        reason: Option<String>,
    },
    SharingChanged {
        shared: bool,
    },

    TurnStarted {
        turn: Option<TurnId>,
    },
    UserMessage {
        text: String,
        origin: Origin,
    },
    AssistantMessage {
        message: MessageId,
        text: String,
        mode: TextMode,
    },
    Reasoning {
        message: MessageId,
        text: String,
        mode: TextMode,
    },
    Plan {
        entries: Vec<PlanEntry>,
    },

    /// Emitted on every status change of a tool call; the latest one per `call` wins.
    ToolCall {
        call: ToolCallId,
        tool: ToolInfo,
        status: ToolStatus,
        output: Option<ToolOutput>,
    },

    PermissionRequested {
        request: RequestId,
        call: Option<ToolCallId>,
        tool: ToolInfo,
        reason: Option<String>,
        options: Vec<PermissionOption>,
        expires_at_ms: Option<TimestampMs>,
    },
    PermissionResolved {
        request: RequestId,
        outcome: PermissionOutcome,
        by: Resolver,
    },

    TurnEnded {
        turn: Option<TurnId>,
        reason: TurnEndReason,
        last_message: Option<String>,
    },
    Notice {
        level: Level,
        text: String,
    },
    /// A tool call left running in the background (a shell command, an agent)
    /// finished: Claude Code's `<task-notification>`.
    TaskEnded {
        call: ToolCallId,
        /// `completed`, `failed`, `killed`, …
        status: String,
        summary: Option<String>,
    },
    Error {
        message: String,
        fatal: bool,
    },
}

impl EventKind {
    /// Short machine name of the variant (`"tool_call"`, …), for logs and SQL.
    pub fn name(&self) -> &'static str {
        match self {
            Self::SessionStarted { .. } => "session_started",
            Self::SessionEnded { .. } => "session_ended",
            Self::SharingChanged { .. } => "sharing_changed",
            Self::TurnStarted { .. } => "turn_started",
            Self::UserMessage { .. } => "user_message",
            Self::AssistantMessage { .. } => "assistant_message",
            Self::Reasoning { .. } => "reasoning",
            Self::Plan { .. } => "plan",
            Self::ToolCall { .. } => "tool_call",
            Self::PermissionRequested { .. } => "permission_requested",
            Self::PermissionResolved { .. } => "permission_resolved",
            Self::TurnEnded { .. } => "turn_ended",
            Self::Notice { .. } => "notice",
            Self::TaskEnded { .. } => "task_ended",
            Self::Error { .. } => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartKind {
    New,
    Resume,
    Fork,
    Clear,
    Compact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", content = "device", rename_all = "snake_case")]
pub enum Origin {
    Terminal,
    Remote(DeviceId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextMode {
    /// Append to the message with the same id.
    Delta,
    /// Replace the message with the same id (authoritative).
    Final,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    pub status: PlanStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInfo {
    /// The agent's own tool name: `Bash`, `run_command`, `commandExecution`, ….
    pub name: String,
    pub kind: ToolKind,
    /// One-line human summary: "Run `cargo test`", "Edit src/main.rs".
    pub title: String,
    /// Raw arguments as the agent gave them.
    pub input: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<PathBuf>,
}

/// Tool categories, aligned with ACP's `ToolKind` plus `Mcp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Mcp,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolOutput {
    Text { text: String, truncated: bool },
    Diff { path: PathBuf, diff: String },
    Json { value: serde_json::Value },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionOption {
    /// Adapter-defined id echoed back in `Command::ResolvePermission`.
    pub id: String,
    pub kind: OptionKind,
    pub label: String,
}

/// ACP's permission option kinds; Claude and Codex decisions map onto these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

impl OptionKind {
    pub fn is_allow(self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowAlways)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PermissionOutcome {
    Selected {
        option: String,
        kind: OptionKind,
    },
    /// Withdrawn: answered in the terminal, turn ended, hook gave up, daemon restarted.
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", content = "device", rename_all = "snake_case")]
pub enum Resolver {
    Phone(DeviceId),
    Terminal,
    OtherClient,
    Daemon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEndReason {
    EndTurn,
    Interrupted,
    Failed,
    MaxSteps,
    Refusal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Info,
    Warning,
}
