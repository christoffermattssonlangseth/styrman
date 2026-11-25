use serde_json::json;
use styr_core::event::*;
use styr_core::ids::*;
use styr_core::{SessionStatus, StatusTracker};

fn started() -> EventKind {
    EventKind::SessionStarted {
        agent: AgentKind::Claude,
        native: NativeSessionRef::new(AgentKind::Claude, "s"),
        cwd: None,
        workspace_roots: vec![],
        model: None,
        title: None,
        start: StartKind::New,
    }
}

fn tool(status: ToolStatus) -> EventKind {
    EventKind::ToolCall {
        call: "c1".into(),
        tool: ToolInfo {
            name: "Bash".into(),
            kind: ToolKind::Execute,
            title: "ls".into(),
            input: json!({}),
            locations: vec![],
        },
        status,
        output: None,
    }
}

fn perm(id: &str) -> EventKind {
    EventKind::PermissionRequested {
        request: id.into(),
        call: Some("c1".into()),
        tool: ToolInfo {
            name: "Bash".into(),
            kind: ToolKind::Execute,
            title: "ls".into(),
            input: json!({}),
            locations: vec![],
        },
        reason: None,
        options: vec![],
        expires_at_ms: None,
    }
}

fn resolved(id: &str) -> EventKind {
    EventKind::PermissionResolved {
        request: id.into(),
        outcome: PermissionOutcome::Cancelled,
        by: Resolver::Terminal,
    }
}

fn ended(reason: TurnEndReason) -> EventKind {
    EventKind::TurnEnded {
        turn: None,
        reason,
        last_message: None,
    }
}

fn user() -> EventKind {
    EventKind::UserMessage {
        text: "go".into(),
        origin: Origin::Terminal,
    }
}

fn status_after(events: &[EventKind]) -> SessionStatus {
    StatusTracker::replay(events).status()
}

#[test]
fn fresh_session_waits_for_input() {
    assert_eq!(status_after(&[started()]), SessionStatus::Waiting);
}

#[test]
fn typical_turn() {
    let mut evs = vec![started(), user()];
    assert_eq!(status_after(&evs), SessionStatus::Working);
    evs.push(tool(ToolStatus::Running));
    evs.push(perm("r1"));
    assert_eq!(status_after(&evs), SessionStatus::NeedsApproval);
    evs.push(resolved("r1"));
    assert_eq!(status_after(&evs), SessionStatus::Working);
    evs.push(tool(ToolStatus::Completed));
    evs.push(ended(TurnEndReason::EndTurn));
    assert_eq!(status_after(&evs), SessionStatus::Waiting);
    assert!(SessionStatus::Waiting.needs_user());
}

#[test]
fn approval_needs_all_requests_resolved() {
    let s = status_after(&[started(), user(), perm("a"), perm("b"), resolved("a")]);
    assert_eq!(s, SessionStatus::NeedsApproval);
    let t = StatusTracker::replay(&[started(), user(), perm("a"), perm("b"), resolved("a")]);
    assert_eq!(
        t.open_requests().map(|r| r.as_str()).collect::<Vec<_>>(),
        vec!["b"]
    );
}

#[test]
fn turn_end_clears_dangling_requests() {
    assert_eq!(
        status_after(&[
            started(),
            user(),
            perm("a"),
            ended(TurnEndReason::Interrupted)
        ]),
        SessionStatus::Waiting
    );
}

#[test]
fn failed_turn_is_error_until_next_prompt() {
    let mut evs = vec![started(), user(), ended(TurnEndReason::Failed)];
    assert_eq!(status_after(&evs), SessionStatus::Error);
    evs.push(user());
    assert_eq!(status_after(&evs), SessionStatus::Working);
}

#[test]
fn late_assistant_text_does_not_reopen_a_finished_turn() {
    let evs = vec![
        started(),
        user(),
        ended(TurnEndReason::EndTurn),
        EventKind::AssistantMessage {
            message: "m".into(),
            text: "Done".into(),
            mode: TextMode::Final,
        },
    ];
    assert_eq!(status_after(&evs), SessionStatus::Waiting);
}

#[test]
fn ended_session_is_done_and_resume_revives_it() {
    let mut evs = vec![
        started(),
        user(),
        perm("a"),
        EventKind::SessionEnded { reason: None },
    ];
    assert_eq!(status_after(&evs), SessionStatus::Done);
    evs.push(started());
    assert_eq!(status_after(&evs), SessionStatus::Waiting);
}

#[test]
fn sharing_is_tracked() {
    let t = StatusTracker::replay(&[started(), EventKind::SharingChanged { shared: true }]);
    assert!(t.shared());
}
