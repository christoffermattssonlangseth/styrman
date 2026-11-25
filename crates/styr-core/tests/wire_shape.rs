//! The event JSON is a contract with the iOS app and the SQLite history.
//! These tests pin its exact shape; changing one is a protocol change.

use serde_json::json;
use styr_core::Command;
use styr_core::event::*;
use styr_core::ids::*;
use styr_core::ipc::{Request, Response};

fn roundtrip(kind: &EventKind, expected: serde_json::Value) {
    let v = serde_json::to_value(kind).unwrap();
    assert_eq!(v, expected, "serialized shape changed");
    let back: EventKind = serde_json::from_value(v).unwrap();
    assert_eq!(&back, kind);
}

fn bash_tool() -> ToolInfo {
    ToolInfo {
        name: "Bash".into(),
        kind: ToolKind::Execute,
        title: "Run `cargo test`".into(),
        input: json!({"command": "cargo test"}),
        locations: vec![],
    }
}

#[test]
fn session_started_shape() {
    roundtrip(
        &EventKind::SessionStarted {
            agent: AgentKind::Claude,
            native: NativeSessionRef::new(AgentKind::Claude, "abc"),
            cwd: Some("/repo".into()),
            workspace_roots: vec![],
            model: Some("claude-opus-5-5".into()),
            title: None,
            start: StartKind::New,
        },
        json!({
            "type": "session_started", "agent": "claude",
            "native": {"agent": "claude", "native_id": "abc"},
            "cwd": "/repo", "model": "claude-opus-5-5", "title": null, "start": "new"
        }),
    );
}

#[test]
fn tool_call_shape() {
    roundtrip(
        &EventKind::ToolCall {
            call: "toolu_1".into(),
            tool: bash_tool(),
            status: ToolStatus::Completed,
            output: Some(ToolOutput::Text {
                text: "ok".into(),
                truncated: false,
            }),
        },
        json!({
            "type": "tool_call", "call": "toolu_1",
            "tool": {"name": "Bash", "kind": "execute", "title": "Run `cargo test`",
                     "input": {"command": "cargo test"}},
            "status": "completed",
            "output": {"type": "text", "text": "ok", "truncated": false}
        }),
    );
}

#[test]
fn permission_shapes() {
    roundtrip(
        &EventKind::PermissionRequested {
            request: "r1".into(),
            call: Some("toolu_1".into()),
            tool: bash_tool(),
            reason: None,
            options: vec![
                PermissionOption {
                    id: "allow".into(),
                    kind: OptionKind::AllowOnce,
                    label: "Allow once".into(),
                },
                PermissionOption {
                    id: "deny".into(),
                    kind: OptionKind::RejectOnce,
                    label: "Deny".into(),
                },
            ],
            expires_at_ms: Some(1000),
        },
        json!({
            "type": "permission_requested", "request": "r1", "call": "toolu_1",
            "tool": {"name": "Bash", "kind": "execute", "title": "Run `cargo test`",
                     "input": {"command": "cargo test"}},
            "reason": null,
            "options": [
                {"id": "allow", "kind": "allow_once", "label": "Allow once"},
                {"id": "deny", "kind": "reject_once", "label": "Deny"}
            ],
            "expires_at_ms": 1000
        }),
    );
    roundtrip(
        &EventKind::PermissionResolved {
            request: "r1".into(),
            outcome: PermissionOutcome::Selected {
                option: "allow".into(),
                kind: OptionKind::AllowOnce,
            },
            by: Resolver::Phone("iphone".into()),
        },
        json!({
            "type": "permission_resolved", "request": "r1",
            "outcome": {"outcome": "selected", "option": "allow", "kind": "allow_once"},
            "by": {"by": "phone", "device": "iphone"}
        }),
    );
    roundtrip(
        &EventKind::PermissionResolved {
            request: "r1".into(),
            outcome: PermissionOutcome::Cancelled,
            by: Resolver::Terminal,
        },
        json!({
            "type": "permission_resolved", "request": "r1",
            "outcome": {"outcome": "cancelled"}, "by": {"by": "terminal"}
        }),
    );
}

#[test]
fn message_shapes() {
    roundtrip(
        &EventKind::UserMessage {
            text: "hi".into(),
            origin: Origin::Remote("iphone".into()),
        },
        json!({"type": "user_message", "text": "hi", "origin": {"from": "remote", "device": "iphone"}}),
    );
    roundtrip(
        &EventKind::AssistantMessage {
            message: "m1".into(),
            text: "Done.".into(),
            mode: TextMode::Final,
        },
        json!({"type": "assistant_message", "message": "m1", "text": "Done.", "mode": "final"}),
    );
    roundtrip(
        &EventKind::TurnEnded {
            turn: None,
            reason: TurnEndReason::EndTurn,
            last_message: Some("Done.".into()),
        },
        json!({"type": "turn_ended", "turn": null, "reason": "end_turn", "last_message": "Done."}),
    );
}

#[test]
fn every_variant_roundtrips_and_names_match_tag() {
    let samples = vec![
        EventKind::SessionEnded {
            reason: Some("exit".into()),
        },
        EventKind::SharingChanged { shared: true },
        EventKind::TurnStarted {
            turn: Some("t1".into()),
        },
        EventKind::Reasoning {
            message: "m".into(),
            text: "hmm".into(),
            mode: TextMode::Delta,
        },
        EventKind::Plan {
            entries: vec![PlanEntry {
                content: "x".into(),
                status: PlanStatus::InProgress,
            }],
        },
        EventKind::Notice {
            level: Level::Warning,
            text: "w".into(),
        },
        EventKind::Error {
            message: "boom".into(),
            fatal: true,
        },
    ];
    for k in samples {
        let v = serde_json::to_value(&k).unwrap();
        assert_eq!(v["type"], k.name());
        assert_eq!(serde_json::from_value::<EventKind>(v).unwrap(), k);
    }
}

#[test]
fn command_and_ipc_shapes() {
    let c = Command::ResolvePermission {
        request: "r1".into(),
        option: "allow".into(),
    };
    assert_eq!(
        serde_json::to_value(&c).unwrap(),
        json!({"type": "resolve_permission", "request": "r1", "option": "allow"})
    );
    let r: Request = serde_json::from_value(json!({"op": "list_sessions"})).unwrap();
    assert_eq!(
        r,
        Request::ListSessions {
            include_unshared: false
        }
    );
    let r: Request = serde_json::from_value(json!({"op": "events", "session": "S"})).unwrap();
    assert_eq!(
        r,
        Request::Events {
            session: "S".into(),
            since_seq: 0,
            limit: 500
        }
    );
    assert_eq!(
        serde_json::to_value(Response::Ok).unwrap(),
        json!({"result": "ok"})
    );
}

#[test]
fn new_session_shapes() {
    use styr_core::phone::{ClientMsg, FolderEntry, FolderListing, ServerMsg};
    let m: ClientMsg = serde_json::from_value(json!({"type": "list_folders"})).unwrap();
    assert_eq!(m, ClientMsg::ListFolders { path: None });
    let m: ClientMsg = serde_json::from_value(
        json!({"type": "start_session", "agent": "codex", "cwd": "/Users/me/work/x", "prompt": "hi"}),
    )
    .unwrap();
    assert_eq!(
        m,
        ClientMsg::StartSession {
            agent: styr_core::ids::AgentKind::Codex,
            cwd: "/Users/me/work/x".into(),
            prompt: Some("hi".into())
        }
    );
    let listing = FolderListing {
        path: "/Users/me/work".into(),
        parent: Some("/Users/me".into()),
        home: "/Users/me".into(),
        entries: vec![FolderEntry {
            name: "x".into(),
            path: "/Users/me/work/x".into(),
            is_repo: true,
        }],
        recent: vec!["/Users/me/work/x".into()],
        agents: vec![
            styr_core::ids::AgentKind::Claude,
            styr_core::ids::AgentKind::Codex,
        ],
        files: vec!["README.md".into()],
        more_files: false,
        git_branch: Some("main".into()),
    };
    assert_eq!(
        serde_json::to_value(ServerMsg::Folders { listing }).unwrap(),
        json!({"type": "folders", "listing": {
            "path": "/Users/me/work", "parent": "/Users/me", "home": "/Users/me",
            "entries": [{"name": "x", "path": "/Users/me/work/x", "is_repo": true}],
            "recent": ["/Users/me/work/x"], "agents": ["claude", "codex"],
            "files": ["README.md"], "more_files": false, "git_branch": "main"}})
    );
    let m: ClientMsg = serde_json::from_value(
        json!({"type": "create_folder", "parent": "/Users/me/work", "name": "new app"}),
    )
    .unwrap();
    assert_eq!(
        m,
        ClientMsg::CreateFolder {
            parent: "/Users/me/work".into(),
            name: "new app".into()
        }
    );
    assert_eq!(
        serde_json::to_value(ServerMsg::SessionStarting {
            session: "S".into()
        })
        .unwrap(),
        json!({"type": "session_starting", "session": "S"})
    );
}
