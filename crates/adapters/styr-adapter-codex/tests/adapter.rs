//! Codex hook mapping against payloads captured from Codex CLI 0.172.0.

use serde_json::{Value, json};
use styr_adapter_codex::CodexAdapter;
use styr_core::adapter::*;
use styr_core::event::*;
use styr_core::ids::*;

fn hooks() -> Vec<Value> {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/hooks_approval_turn.jsonl",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
    .lines()
    .map(|l| serde_json::from_str(l).unwrap())
    .collect()
}

fn raw(payload: Value) -> RawHook {
    RawHook {
        agent: AgentKind::Codex,
        event: payload["hook_event_name"].as_str().unwrap().to_owned(),
        payload,
        launched_by_styr: true,
        agent_pid: None,
        launch_id: Some("L1".into()),
        agent_version: None,
    }
}

fn view(native: NativeSessionRef) -> SessionView {
    SessionView {
        id: "S".into(),
        native,
        shared: true,
        open_requests: vec![],
    }
}

#[test]
fn a_real_turn_maps_to_events() {
    let a = CodexAdapter;
    let mut all = Vec::new();
    for p in hooks() {
        let h = raw(p);
        let native = a.native_session(&h).expect("session_id");
        let plan = a.on_hook(&h, &view(native)).unwrap();
        assert_eq!(
            plan.session.as_ref().unwrap().launch_id.as_deref(),
            Some("L1")
        );
        assert_eq!(
            a.render_reply(&h, &HookDecision::PassThrough),
            None,
            "{}: never blocks",
            h.event
        );
        all.extend(plan.events);
    }
    let names: Vec<_> = all.iter().map(|e| e.name()).collect();
    assert_eq!(
        names,
        vec![
            "session_started",
            "turn_started",
            "user_message",
            "tool_call",
            "tool_call",
            "assistant_message",
            "turn_ended"
        ]
    );
    let EventKind::ToolCall {
        tool,
        status: ToolStatus::Pending,
        call,
        ..
    } = &all[3]
    else {
        panic!()
    };
    assert_eq!(tool.title, "Run `touch approved.txt`");
    assert!(call.0.starts_with("exec-"));
    let EventKind::ToolCall {
        status, call: done, ..
    } = &all[4]
    else {
        panic!()
    };
    assert_eq!(*status, ToolStatus::Completed);
    assert_eq!(done, call, "the same call id before and after");
    let EventKind::AssistantMessage {
        text,
        mode: TextMode::Final,
        ..
    } = &all[5]
    else {
        panic!()
    };
    assert_eq!(text, "Created `approved.txt` successfully.");
}

#[test]
fn styr_via_prompt_is_consumed() {
    let a = CodexAdapter;
    let h = raw(
        json!({"hook_event_name": "UserPromptSubmit", "session_id": "x", "prompt": "/styr off", "turn_id": "t"}),
    );
    let plan = a
        .on_hook(&h, &view(NativeSessionRef::new(AgentKind::Codex, "x")))
        .unwrap();
    assert_eq!(plan.set_sharing, Some(false));
    assert!(plan.events.is_empty());
    let reply = a.render_reply(&h, plan.reply.as_ref().unwrap()).unwrap();
    assert_eq!(reply["decision"], "block");
}

#[test]
fn apply_patch_is_an_edit() {
    let a = CodexAdapter;
    let h = raw(json!({
        "hook_event_name": "PreToolUse", "session_id": "x", "turn_id": "t", "tool_use_id": "c1",
        "tool_name": "apply_patch",
        "tool_input": {"patch": "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-a\n+b\n*** End Patch"}
    }));
    let plan = a
        .on_hook(&h, &view(NativeSessionRef::new(AgentKind::Codex, "x")))
        .unwrap();
    let EventKind::ToolCall { tool, .. } = &plan.events[0] else {
        panic!()
    };
    assert_eq!(
        (tool.kind, tool.title.as_str()),
        (ToolKind::Edit, "Edit src/main.rs")
    );
}
