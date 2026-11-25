//! The adapter seam. Each agent (Claude, Antigravity, Codex, PTY) implements
//! [`AgentAdapter`]. Adapters run inside the daemon; the `styr hook` helper is an
//! agent-agnostic forwarder, so changing adapter logic never requires reinstalling hooks.
//!
//! Two integration styles share the trait:
//! - **hook path** (Claude, agy, Codex hooks): short-lived hook processes deliver a
//!   [`RawHook`]; the adapter normalizes it ([`AgentAdapter::on_hook`]) and renders the
//!   agent-native reply ([`AgentAdapter::render_reply`]).
//! - **protocol path** (Codex app-server, agy ACP): the daemon holds a connection
//!   ([`AgentAdapter::attach`]) that streams events and accepts [`Command`]s.

use crate::command::Command;
use crate::event::{EventKind, PermissionOutcome};
use crate::ids::{AgentKind, NativeSessionRef, SessionId};
use crate::ipc::SessionUpdate;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("not supported by this adapter")]
    Unsupported,
    #[error("malformed hook payload: {0}")]
    BadPayload(String),
    #[error("{0}")]
    Other(String),
}

/// How a prompt typed on the phone reaches the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptDelivery {
    /// Cannot inject prompts.
    None,
    /// Only when the agent has stopped (Stop-hook continuation / asyncRewake).
    WhenIdle,
    /// Any time (Claude Channels, Codex `turn/start`/`turn/steer`, ACP, PTY keystrokes).
    Anytime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// The phone only sees tool calls; approvals stay in the terminal.
    None,
    /// A blocking hook waits for the phone; the terminal prompt appears after a timeout.
    HookBlocking,
    /// The terminal and the phone are both offered the request; the first answer wins.
    Concurrent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub prompt_delivery: PromptDelivery,
    pub permissions: PermissionMode,
    pub streaming_text: bool,
    pub interrupt: bool,
}

/// One hook invocation as `styr hook` forwards it: agent + event name + stdin JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawHook {
    pub agent: AgentKind,
    /// The agent's own event name, e.g. `PreToolUse`.
    pub event: String,
    pub payload: serde_json::Value,
    /// Set when the agent was started through `styr <agent>`.
    #[serde(default)]
    pub launched_by_styr: bool,
    /// The agent's process id, when the hook helper could find it (lets the daemon
    /// notice sessions whose process died without a SessionEnd).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_pid: Option<u32>,
    /// Set when the agent runs inside `styr <agent>`'s terminal wrapper, which can
    /// type phone prompts into it (`STYR_LAUNCH_ID` in the hook's environment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<String>,
}

/// What the daemon currently knows about the session a hook belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionView {
    pub id: SessionId,
    pub native: NativeSessionRef,
    pub shared: bool,
    /// Permission requests still waiting for an answer (oldest first).
    pub open_requests: Vec<crate::ids::RequestId>,
}

/// The adapter's reading of one hook.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HookPlan {
    /// Register or refresh session metadata (cwd, transcript path, model, …).
    pub session: Option<SessionUpdate>,
    pub events: Vec<EventKind>,
    /// Whether the hook process should block for an answer. Only honoured for shared sessions.
    pub wait: Option<WaitFor>,
    /// Sharing toggled from inside the session (`/styr`, `/styr off`).
    pub set_sharing: Option<bool>,
    /// Answer the hook right away with this, whatever the sharing state
    /// (e.g. consume a `/styr` prompt so it never reaches the model).
    pub reply: Option<HookDecision>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WaitFor {
    /// `events` contains the matching `PermissionRequested`.
    Permission {
        request: crate::ids::RequestId,
        timeout: Duration,
    },
    /// Wait for a prompt from the phone (Stop-hook style delivery).
    NextPrompt { timeout: Duration },
}

/// What the daemon decided; the adapter turns it into agent-native stdout.
#[derive(Debug, Clone, PartialEq)]
pub enum HookDecision {
    /// Do nothing: the agent behaves exactly as without Styrman. The fail-open answer.
    PassThrough,
    Permission(PermissionOutcome),
    Prompt(String),
    /// Swallow the user's input with a message (e.g. after handling `/styr`).
    Consume {
        message: String,
    },
}

/// How `styr <agent> …` execs the real agent.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LaunchSpec {
    /// Binary name looked up on PATH, skipping `styr` itself.
    pub program: String,
    pub extra_args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Receives events from protocol-path adapters.
pub trait EventSink: Send + Sync {
    fn session(&self, update: SessionUpdate);
    fn emit(&self, native: &NativeSessionRef, event: EventKind);
}

/// A live connection to one session (protocol path).
pub trait SessionDriver: Send {
    fn send(&mut self, cmd: Command) -> BoxFuture<'_, Result<(), AdapterError>>;
    fn detach(self: Box<Self>) -> BoxFuture<'static, ()>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttachTarget {
    /// An existing session by native id (Codex threadId, ACP sessionId).
    Session(NativeSessionRef),
    /// Start a new one in a directory (phone-initiated sessions, later).
    New { cwd: PathBuf },
}

pub trait AgentAdapter: Send + Sync + 'static {
    fn kind(&self) -> AgentKind;
    fn capabilities(&self) -> Capabilities;

    // ---- hook path ----

    /// Which session a hook belongs to; `None` for payloads that carry no session id.
    fn native_session(&self, _hook: &RawHook) -> Option<NativeSessionRef> {
        None
    }

    /// Normalize one hook. Must be cheap and pure: it runs on every hook of every
    /// session, shared or not.
    fn on_hook(&self, _hook: &RawHook, _session: &SessionView) -> Result<HookPlan, AdapterError> {
        Err(AdapterError::Unsupported)
    }

    /// Agent-native stdout JSON for `decision`; `None` means print nothing.
    /// `PassThrough` must always render as `None`.
    fn render_reply(&self, _hook: &RawHook, _decision: &HookDecision) -> Option<serde_json::Value> {
        None
    }

    /// A non-zero exit code plus stderr for `decision`, for agents whose hook protocol
    /// needs one (Claude's `asyncRewake` wakes on exit 2). `None` = exit 0.
    /// Never return one for `PassThrough`: exit 2 means "block" on many hook events.
    fn render_exit(&self, _hook: &RawHook, _decision: &HookDecision) -> Option<(i32, String)> {
        None
    }

    // ---- protocol path ----

    fn attach(
        &self,
        _target: AttachTarget,
        _sink: std::sync::Arc<dyn EventSink>,
    ) -> BoxFuture<'_, Result<Box<dyn SessionDriver>, AdapterError>> {
        Box::pin(async { Err(AdapterError::Unsupported) })
    }

    // ---- launcher ----

    fn launcher(&self) -> LaunchSpec;
}
