//! Identifiers shared by the daemon, CLI, adapters and phone.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Which coding agent a session belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Antigravity,
    Codex,
    Pty,
}

impl AgentKind {
    pub const ALL: [AgentKind; 4] = [Self::Claude, Self::Antigravity, Self::Codex, Self::Pty];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Antigravity => "antigravity",
            Self::Codex => "codex",
            Self::Pty => "pty",
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AgentKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude" => Ok(Self::Claude),
            "antigravity" | "agy" => Ok(Self::Antigravity),
            "codex" => Ok(Self::Codex),
            "pty" => Ok(Self::Pty),
            other => Err(format!("unknown agent '{other}'")),
        }
    }
}

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
        }
        impl From<&str> for $name { fn from(s: &str) -> Self { Self(s.to_owned()) } }
        impl From<String> for $name { fn from(s: String) -> Self { Self(s) } }
    };
}

string_id!(
    /// Daemon-assigned stable session id (a ULID string).
    SessionId
);
string_id!(
    /// A turn id, when the agent has one (Codex); otherwise daemon-synthesized.
    TurnId
);
string_id!(
    /// Stable id of an assistant message, used to merge streamed deltas.
    MessageId
);
string_id!(
    /// Stable id of a tool call across its lifecycle (Claude tool_use_id, ACP toolCallId,
    /// Codex itemId; synthesized for agy).
    ToolCallId
);
string_id!(
    /// Id of one permission request; unique within the daemon.
    RequestId
);
string_id!(
    /// A paired phone.
    DeviceId
);

impl SessionId {
    pub fn generate() -> Self {
        Self(ulid::Ulid::generate().to_string())
    }
}

impl RequestId {
    pub fn generate() -> Self {
        Self(ulid::Ulid::generate().to_string())
    }
}

/// An agent's own id for a session (Claude `session_id`, agy `conversationId`, Codex `threadId`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NativeSessionRef {
    pub agent: AgentKind,
    pub native_id: String,
}

impl NativeSessionRef {
    pub fn new(agent: AgentKind, native_id: impl Into<String>) -> Self {
        Self {
            agent,
            native_id: native_id.into(),
        }
    }
}

impl fmt::Display for NativeSessionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.agent, self.native_id)
    }
}
