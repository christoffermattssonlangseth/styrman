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
/// URL scheme of pairing links: `styrman://pair?url=…&code=…&name=…`.
pub const PAIR_SCHEME: &str = "styrman";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    ListSessions,
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
    Folders {
        listing: FolderListing,
    },
    /// A session started from the phone; it opens as soon as the agent runs.
    SessionStarting {
        session: SessionId,
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
