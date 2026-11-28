//! Local IPC between `styr` (CLI and hook helper) and `styrd` over a Unix socket.
//!
//! Framing: one request per connection. The client writes one JSON line; the daemon
//! answers with one JSON line and closes. A blocking hook simply gets its answer late.
//! The client is synchronous std code so the hook helper stays tiny and every failure
//! mode (no socket, refused, timeout, EOF) is easy to turn into a fail-open no-op.

use crate::event::{Event, EventKind, EventSource, TimestampMs};
use crate::ids::{AgentKind, NativeSessionRef, SessionId};
use crate::status::SessionStatus;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const PROTOCOL_VERSION: u32 = 1;
/// Upper bound for one framed message (hook payloads can carry large tool output).
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// The terminal actions a wrapper understands, sent with every `AttachTerminal`.
/// 1: stop a turn (`Response::Interrupt`) and show the screen (`Response::WatchScreen`).
/// 2: restart with another model or effort (`Response::Restart`).
/// 3: title its terminal tab (`Response::SetTitle`).
pub const WRAPPER_LEVEL: u32 = 3;
/// Below this a wrapper lacks Stop or Terminal: the phone says to restart it.
pub const WRAPPER_LEVEL_FULL: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Hello,
    ListSessions {
        #[serde(default)]
        include_unshared: bool,
    },
    /// Append already-normalized events (protocol adapters, tests, the fake agent).
    Ingest {
        session: SessionUpdate,
        source: EventSource,
        events: Vec<EventKind>,
    },
    SetSharing {
        native: NativeSessionRef,
        shared: bool,
    },
    Events {
        session: SessionId,
        #[serde(default)]
        since_seq: u64,
        #[serde(default = "default_limit")]
        limit: u32,
    },
    /// One hook invocation forwarded by `styr hook`. Answered with `HookReply`,
    /// possibly after a long wait (permission from the phone).
    Hook {
        hook: crate::adapter::RawHook,
    },
    /// Start pairing a phone: a one-time code plus the URLs the phone can reach.
    CreatePairing,
    ListDevices,
    RevokeDevice {
        id: String,
    },
    /// Reload `apns.json` + key after `styr push setup`.
    PushReload,
    PushStatus,
    /// Send a test notification to every phone with a push token.
    PushTest,
    /// Held open by `styr <agent>`'s terminal wrapper: answered with `TypePrompt`
    /// when a phone prompt should be typed into that terminal.
    AttachTerminal {
        launch_id: String,
        /// The wrapper's agent and folder. Codex may run hooks in a shared background
        /// server whose environment names another terminal; the daemon then matches
        /// the hook to the live wrapper in its folder.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<AgentKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<std::path::PathBuf>,
        /// What the wrapper understands ([`WRAPPER_LEVEL`]); 0 for wrappers from
        /// before it was sent. A wrapper started before an update keeps running the
        /// old code, so the daemon must not hand it actions it would ignore.
        #[serde(default)]
        level: u32,
    },
    /// From a terminal wrapper: the agent's own permission menu appeared (`Some`)
    /// or went away (`None`).
    TerminalPrompt {
        launch_id: String,
        prompt: Option<TerminalPrompt>,
    },
    /// From a terminal wrapper: its screen, while a phone watches it.
    TerminalScreen {
        launch_id: String,
        text: String,
        cols: u16,
    },
    /// From a terminal wrapper: the reply its agent is writing, read off the screen.
    TerminalPreview {
        launch_id: String,
        text: Option<String>,
        /// Its status line (absent from older wrappers).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<crate::phone::WorkStatus>,
    },
    /// From a terminal wrapper: the permission mode its agent shows changed.
    TerminalMode {
        launch_id: String,
        mode: Option<String>,
    },
    /// Everything `styr doctor` / `styr setup` need to know about the daemon.
    Diagnostics,
    /// Record that an agent version passed `styr doctor --live`.
    MarkVerified {
        agent: AgentKind,
        version: String,
    },
    /// Delete sessions and their history.
    ForgetSessions {
        ids: Vec<SessionId>,
    },
    /// Name a session, as the phone's Rename does (empty: its own title again).
    RenameSession {
        session: SessionId,
        name: String,
    },
    /// Quit these sessions' agents, as the phone's End does.
    EndSessions {
        ids: Vec<SessionId>,
    },
    /// Queue a prompt for a shared session, as if sent from a phone (CLI, tests).
    SendPrompt {
        session: SessionId,
        text: String,
    },
    /// Shared sessions an agent can talk to (`styr peers`). `launch_id`: the asking
    /// terminal, left out of the list.
    Peers {
        #[serde(default)]
        launch_id: Option<String>,
    },
    /// Send a message to another shared session (`styr ask`); with `wait_secs`, wait
    /// that long for its reply.
    Ask {
        #[serde(default)]
        launch_id: Option<String>,
        to: String,
        text: String,
        #[serde(default)]
        wait_secs: Option<u64>,
    },
    /// Register with a push relay (`styr push relay <url>`).
    PushRelay {
        url: String,
    },
    /// Start `agent` in `cwd` in a new cmux tab, shared with the phone (`styr new`).
    StartSession {
        agent: AgentKind,
        cwd: std::path::PathBuf,
        #[serde(default)]
        prompt: Option<String>,
        /// The cmux workspace `styr new` ran in (an agent starting another): the new
        /// agent opens as a tab there instead of in a workspace of its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cmux_workspace: Option<String>,
    },
    /// `/styr` caught by a `styr <agent>` terminal wrapper (Codex rejects unknown
    /// slash commands before any hook sees them).
    ShareLaunch {
        launch_id: String,
        shared: bool,
        /// With these, a terminal whose agent has no session yet (Codex creates it
        /// with the first prompt) gets a placeholder on the phone right away.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<AgentKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<std::path::PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_pid: Option<u32>,
    },
}

fn default_limit() -> u32 {
    500
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Hello {
        protocol: u32,
        daemon_version: String,
        pid: u32,
    },
    Sessions {
        sessions: Vec<SessionSummary>,
    },
    Ingested {
        session: SessionId,
        seqs: Vec<u64>,
    },
    Events {
        events: Vec<Event>,
    },
    /// What `styr hook` prints on stdout (`None`: print nothing, exit 0).
    HookReply {
        stdout: Option<serde_json::Value>,
        /// Non-zero only on the adapter's explicit request (e.g. 2 to wake Claude
        /// from an `asyncRewake` hook); absent means exit 0.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit: Option<HookExit>,
    },
    Pairing {
        code: String,
        expires_ms: TimestampMs,
        /// Phone-API WebSocket URLs, Tailscale first.
        urls: Vec<String>,
        host: String,
    },
    Devices {
        devices: Vec<DeviceInfo>,
    },
    PushStatus {
        configured: bool,
        details: bool,
        /// Names of paired phones that registered for notifications.
        devices_with_token: Vec<String>,
        /// Notifications go through this relay (no own APNs key).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relay: Option<String>,
    },
    PushResults {
        /// (device id, outcome)
        results: Vec<(String, String)>,
    },
    Diagnostics {
        daemon_version: String,
        pid: u32,
        api_urls: Vec<String>,
        /// The local-network fallback (`wss://`), when Tailscale is down.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        lan_urls: Vec<String>,
        push_configured: bool,
        devices: Vec<DeviceInfo>,
        agents: Vec<AgentVersionInfo>,
        warnings: Vec<String>,
    },
    TypePrompt {
        text: String,
    },
    /// Answer the menu on screen: move to option `index` (0-based) and press Enter.
    SelectOption {
        index: usize,
    },
    /// Switch the agent's permission mode (see `Command::SetMode`).
    SetMode {
        mode: String,
    },
    /// Quit the agent; close the tab if the phone opened it (see `Command::EndSession`).
    EndSession,
    /// Title the terminal's tab (see `Command::Rename`); empty: let the agent title it.
    SetTitle {
        title: String,
    },
    /// Stop the agent's current turn (Esc in its terminal; see `Command::Interrupt`).
    Interrupt,
    /// Send the screen for the next 20 s (a phone is watching).
    WatchScreen,
    /// Quit the agent and start it again in the same terminal, resuming conversation
    /// `resume` with this model and effort (see `Command::SetModel`). `mode`: Claude's
    /// permission mode to keep.
    Restart {
        resume: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
    },
    PromptAccepted {
        /// False: queued until the agent next stops.
        delivered: bool,
    },
    Ok,
    SessionStarting {
        session: SessionId,
    },
    Peers {
        /// The asking terminal's own session, if any.
        me: Option<SessionId>,
        sessions: Vec<SessionSummary>,
    },
    AskReply {
        reply: Option<String>,
        timed_out: bool,
    },
    /// How many sessions changed; 0 means it applies to the terminal's first session.
    LaunchShared {
        sessions: usize,
    },
    Error {
        message: String,
    },
}

/// Metadata an adapter learned about a session; `None` fields leave stored values alone.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SessionUpdate {
    pub native: Option<NativeSessionRef>,
    pub cwd: Option<PathBuf>,
    pub transcript_path: Option<PathBuf>,
    pub model: Option<String>,
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_pid: Option<u32>,
    /// The `styr <agent>` terminal wrapper the session runs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<String>,
}

impl SessionUpdate {
    pub fn for_native(native: NativeSessionRef) -> Self {
        Self {
            native: Some(native),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: SessionId,
    pub agent: AgentKind,
    pub native_id: String,
    pub cwd: Option<PathBuf>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub shared: bool,
    pub status: SessionStatus,
    pub created_ms: TimestampMs,
    pub updated_ms: TimestampMs,
    pub last_seq: u64,
    /// Runs inside `styr <agent>`'s terminal wrapper right now.
    #[serde(default)]
    pub in_terminal_wrapper: bool,
    /// Prompts from the phone can reach it (wrapper, or Claude's hook route).
    #[serde(default)]
    pub accepts_prompts: bool,
    /// One line on what it's doing or last said ("Run `cargo test`", the reply's first line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// The agent's permission mode as its terminal shows it ("default", "acceptEdits",
    /// "plan", "auto", "bypassPermissions"); known only inside `styr <agent>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The session that started this one to ask it something (`styr ask`): it's
    /// that session's helper.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helper_of: Option<SessionId>,
    /// The name the user gave it (`Command::Rename`), shown in place of its title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The reasoning effort chosen from the phone, if any (else the agent's default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Its wrapper was started before Styrman was updated and lacks newer actions
    /// (stop, terminal): starting the session again brings them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub outdated: bool,
}

/// An exit code plus stderr text for `styr hook` to finish with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookExit {
    pub code: i32,
    pub stderr: String,
}

/// What `styr hook` should do: print `stdout` (if any), then exit as told (default 0).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HookOutput {
    pub stdout: Option<serde_json::Value>,
    pub exit: Option<HookExit>,
}

/// A permission menu read off the agent's screen by a terminal wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TerminalPrompt {
    /// The question, e.g. "Run this command?".
    pub title: String,
    /// What it's about, e.g. the command line.
    pub detail: String,
    /// Option labels in order, e.g. "Yes, run command", …, "No, cancel".
    pub options: Vec<String>,
    /// The highlighted option.
    pub selected: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentVersionInfo {
    pub agent: AgentKind,
    /// Most recently seen version.
    pub version: String,
    pub verified: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub created_ms: TimestampMs,
    pub last_seen_ms: Option<TimestampMs>,
    pub revoked: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("daemon not running (socket {0})")]
    NotRunning(PathBuf),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("daemon: {0}")]
    Remote(String),
}

impl IpcError {
    pub fn is_not_running(&self) -> bool {
        matches!(self, Self::NotRunning(_))
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
    timeout: Option<Duration>,
}

impl Client {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            timeout: Some(Duration::from_secs(5)),
        }
    }

    /// `None` waits forever (used by blocking hooks, which have their own agent-side timeout).
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn call(&self, req: &Request) -> Result<Response, IpcError> {
        let stream = UnixStream::connect(&self.socket).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                IpcError::NotRunning(self.socket.clone())
            }
            _ => IpcError::Io(e),
        })?;
        stream.set_read_timeout(self.timeout)?;
        stream.set_write_timeout(self.timeout)?;
        let mut line = serde_json::to_vec(req).map_err(|e| IpcError::Protocol(e.to_string()))?;
        line.push(b'\n');
        (&stream).write_all(&line)?;
        let mut reader = BufReader::new(&stream).take(MAX_FRAME_BYTES as u64);
        let mut buf = String::new();
        if reader.read_line(&mut buf)? == 0 {
            return Err(IpcError::Protocol("daemon closed the connection".into()));
        }
        match serde_json::from_str(&buf).map_err(|e| IpcError::Protocol(e.to_string()))? {
            Response::Error { message } => Err(IpcError::Remote(message)),
            r => Ok(r),
        }
    }
}

use std::io::Read as _;

/// Forward one hook to the daemon and return what the hook should print.
///
/// The caller treats every `Err` as "print nothing, exit 0": that is the fail-open
/// contract that keeps agents unaffected when styrd is down or misbehaves.
pub fn forward_hook(
    client: &Client,
    hook: crate::adapter::RawHook,
) -> Result<HookOutput, IpcError> {
    match client.call(&Request::Hook { hook })? {
        Response::HookReply { stdout, exit } => Ok(HookOutput { stdout, exit }),
        other => Err(IpcError::Protocol(format!("unexpected reply {other:?}"))),
    }
}
