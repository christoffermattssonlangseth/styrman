//! The phone protocol: JSON text frames over `GET /v1/ws`
//! (`Authorization: Bearer <device token>`), plus `POST /v1/pair`.
//!
//! Flow: the server sends `hello` and a `sessions` snapshot; the client
//! `subscribe`s to a session with the last seq it has (0 for none), receives the
//! missing history as `events` pages and a `subscribed` marker, then live `event`s.
//! `session_updated` / `session_removed` keep the list current without polling.
//! Only shared sessions are ever visible here.

use crate::Command;
use crate::event::Event;
use crate::ids::AgentKind;
use crate::ids::SessionId;
use crate::ipc::SessionSummary;
use serde::{Deserialize, Serialize};

pub const PHONE_PROTOCOL: u32 = 1;
/// Default TCP port of the phone API.
pub const DEFAULT_PORT: u16 = 4719;
/// The local-network fallback (TLS with a self-signed certificate the phone pins).
pub const LAN_PORT: u16 = 4720;
/// URL scheme of pairing links: `styrman://pair?url=…&code=…&name=…`.
pub const PAIR_SCHEME: &str = "styrman";

/// A model the phone can switch a session to (`ServerMsg::Models`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelChoice {
    /// What the agent takes (`--model <id>`).
    pub id: String,
    pub label: String,
    /// Reasoning efforts this model takes; empty: none to choose.
    #[serde(default)]
    pub efforts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    ListSessions,
    GetUsage,
    Subscribe {
        session: SessionId,
        #[serde(default)]
        since_seq: u64,
    },
    Unsubscribe {
        session: SessionId,
    },
    Command {
        session: SessionId,
        command: Command,
    },
    /// The app's APNs device token (hex) and environment ("sandbox" | "production").
    RegisterPush {
        token: String,
        env: String,
    },
    /// Which notifications this phone wants: none (`on` false), or approvals and
    /// errors only (`finished` false), or also finished turns and new sessions.
    SetNotifications {
        on: bool,
        finished: bool,
    },
    /// A Live Activity push token: `kind` "update" (the running activity's) or "start"
    /// (push-to-start). No token clears it.
    RegisterActivityToken {
        kind: String,
        #[serde(default)]
        token: Option<String>,
    },
    /// Folders for the new-session picker (default: the home directory).
    ListFolders {
        #[serde(default)]
        path: Option<String>,
    },
    /// Make a folder `name` inside `parent` (both inside the home directory); the
    /// answer is the new folder's listing.
    CreateFolder {
        parent: String,
        name: String,
    },
    /// The session folder's uncommitted git changes (answered with `Changes`).
    GetChanges {
        session: SessionId,
    },
    /// The models this session's agent can be switched to (answered with `Models`).
    GetModels {
        session: SessionId,
    },
    /// Commit everything in the session's folder with `message` (skipped if empty),
    /// then push if `push`. Answered with `Committed` and fresh `Changes`, or `Error`.
    Commit {
        session: SessionId,
        #[serde(default)]
        message: String,
        #[serde(default)]
        push: bool,
    },
    /// Show me this session's terminal for the next 20 s (sent again to keep it up):
    /// the screen comes as `ServerMsg::Screen`. Only for sessions in `styr <agent>`.
    WatchScreen {
        session: SessionId,
    },
    /// Start `agent` in `cwd` in a new cmux tab on the Mac, shared with the phone.
    StartSession {
        agent: AgentKind,
        cwd: String,
        #[serde(default)]
        prompt: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderEntry {
    pub name: String,
    pub path: String,
    /// Has a `.git` folder.
    #[serde(default)]
    pub is_repo: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderListing {
    pub path: String,
    /// `None` at the home directory (the top of what can be browsed).
    pub parent: Option<String>,
    pub home: String,
    pub entries: Vec<FolderEntry>,
    /// Folders recent sessions ran in, newest first.
    pub recent: Vec<String>,
    /// Agents that can be started (set up on the Mac).
    pub agents: Vec<AgentKind>,
    /// Files directly in `path` (names; hidden ones left out), to see what's there.
    #[serde(default)]
    pub files: Vec<String>,
    /// More files than listed.
    #[serde(default)]
    pub more_files: bool,
    /// The checked-out branch, when `path` is a git repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
}

/// Read-only quota information exported by KarvMac.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageProvider {
    pub id: String,
    pub estimated: bool,
    pub observed_at_ms: i64,
    pub windows: Vec<UsageWindow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    pub title: String,
    pub used_fraction: f64,
    pub resets_at_ms: i64,
    pub rolling: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Hello {
        protocol: u32,
        daemon_version: String,
        device_id: String,
        host: String,
        /// Things the user should know (e.g. an unverified agent version).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<String>,
        #[serde(default)]
        usage_supported: bool,
        /// Other ways to reach this Mac when Tailscale is down: `wss://` URLs on
        /// the local network, served with the certificate whose SHA-256 is `lan_cert`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        lan_urls: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lan_cert: Option<String>,
    },
    Sessions {
        sessions: Vec<SessionSummary>,
    },
    SessionUpdated {
        session: SessionSummary,
    },
    SessionRemoved {
        id: SessionId,
    },
    /// A page of history (oldest first) in answer to `subscribe`.
    Events {
        session: SessionId,
        events: Vec<Event>,
    },
    /// History is complete up to `last_seq`; live `event`s follow.
    Subscribed {
        session: SessionId,
        last_seq: u64,
    },
    Event {
        event: Event,
    },
    /// A prompt from the phone was handed to the agent (typed into its terminal,
    /// or given to its waiting hook); the agent may still queue it while busy.
    PromptDelivered {
        session: SessionId,
        text: String,
    },
    /// A prompt from the phone is waiting until the agent next stops.
    PromptQueued {
        session: SessionId,
        text: String,
        position: u32,
    },
    Error {
        message: String,
    },
    Usage {
        providers: Vec<UsageProvider>,
    },
    Folders {
        listing: FolderListing,
    },
    /// A session started from the phone; it opens as soon as the agent runs.
    SessionStarting {
        session: SessionId,
    },
    /// A session folder's uncommitted changes, as a unified diff (new files as
    /// additions). `truncated`: it was too long and was cut.
    Changes {
        session: SessionId,
        #[serde(default)]
        branch: Option<String>,
        diff: String,
        #[serde(default)]
        truncated: bool,
        /// Commits not yet pushed; absent when the branch has no upstream.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ahead: Option<u32>,
    },
    /// What a session's agent can be switched to (`Command::SetModel`).
    Models {
        session: SessionId,
        models: Vec<ModelChoice>,
    },
    /// A `Commit` went through: the new commit ("abc1234 Subject"), and whether it
    /// was pushed.
    Committed {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit: Option<String>,
        pushed: bool,
    },
    /// The session's terminal as it looks now (plain text, `cols` wide), while a
    /// phone watches it (`ClientMsg::WatchScreen`). Never stored.
    Screen {
        session: SessionId,
        text: String,
        cols: u16,
    },
    /// The reply the agent is writing right now, as its terminal shows it (only to
    /// phones viewing the session; never stored). `None`: no reply in progress.
    Preview {
        session: SessionId,
        #[serde(default)]
        text: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairRequest {
    pub code: String,
    pub device_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairResponse {
    pub device_id: String,
    /// Opaque bearer token; store it in the Keychain. Shown exactly once.
    pub token: String,
    pub host: String,
}

/// Build a pairing link for the QR code.
pub fn pair_link(ws_url: &str, code: &str, host: &str) -> String {
    format!(
        "{PAIR_SCHEME}://pair?v={PHONE_PROTOCOL}&url={}&code={}&name={}",
        pct(ws_url),
        pct(code),
        pct(host)
    )
}

/// Percent-encode everything but RFC 3986 unreserved characters.
fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_is_url_safe() {
        assert_eq!(
            pair_link("ws://100.1.2.3:4719", "AB CD", "Mac mini"),
            "styrman://pair?v=1&url=ws%3A%2F%2F100.1.2.3%3A4719&code=AB%20CD&name=Mac%20mini"
        );
    }

    #[test]
    fn notification_settings_from_the_phone() {
        let m: ClientMsg =
            serde_json::from_str(r#"{"type":"set_notifications","on":true,"finished":false}"#)
                .unwrap();
        assert!(matches!(
            m,
            ClientMsg::SetNotifications {
                on: true,
                finished: false
            }
        ));
    }

    #[test]
    fn commit_shapes() {
        let m: ClientMsg = serde_json::from_str(
            r#"{"type":"commit","session":"S","message":"Fix it","push":true}"#,
        )
        .unwrap();
        assert_eq!(
            m,
            ClientMsg::Commit {
                session: "S".into(),
                message: "Fix it".into(),
                push: true
            }
        );
        let push_only: ClientMsg =
            serde_json::from_str(r#"{"type":"commit","session":"S","push":true}"#).unwrap();
        assert!(
            matches!(push_only, ClientMsg::Commit { message, push: true, .. } if message.is_empty())
        );
        assert_eq!(
            serde_json::to_value(ServerMsg::Committed {
                session: "S".into(),
                commit: Some("abc Fix".into()),
                pushed: false
            })
            .unwrap(),
            serde_json::json!({"type": "committed", "session": "S", "commit": "abc Fix", "pushed": false})
        );
    }

    #[test]
    fn client_msg_shapes() {
        let m: ClientMsg = serde_json::from_str(r#"{"type":"subscribe","session":"S"}"#).unwrap();
        assert_eq!(
            m,
            ClientMsg::Subscribe {
                session: "S".into(),
                since_seq: 0
            }
        );
        let m: ClientMsg = serde_json::from_str(
            r#"{"type":"command","session":"S","command":{"type":"resolve_permission","request":"r1","option":"allow"}}"#,
        )
        .unwrap();
        assert_eq!(
            m,
            ClientMsg::Command {
                session: "S".into(),
                command: Command::ResolvePermission {
                    request: "r1".into(),
                    option: "allow".into()
                }
            }
        );
        let m: ClientMsg = serde_json::from_str(r#"{"type":"list_sessions"}"#).unwrap();
        assert_eq!(m, ClientMsg::ListSessions);
        assert_eq!(
            serde_json::to_value(ServerMsg::SessionRemoved { id: "S".into() }).unwrap(),
            serde_json::json!({"type": "session_removed", "id": "S"})
        );
    }
}
