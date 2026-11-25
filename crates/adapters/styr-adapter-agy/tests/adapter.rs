//! agy adapter against payloads captured from Antigravity CLI 1.5.17.

use serde_json::{Value, json};
use styr_adapter_agy::*;
use styr_core::adapter::*;
use styr_core::event::*;
use styr_core::ids::*;

fn dir() -> String {
    format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"))
}

/// Recorded hooks, with `transcriptPath` pointed at the fixture transcript.
fn fixture(name: &str) -> Vec<RawHook> {
    std::fs::read_to_string(format!("{}/{name}", dir()))
        .unwrap()
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            let mut payload = v["payload"].clone();
            payload["transcriptPath"] = json!(format!("{}/transcript_sample.jsonl", dir()));
            RawHook {
                agent: AgentKind::Antigravity,
                event: v["event"].as_str().unwrap().into(),
                payload,
                launched_by_styr: false,
                agent_pid: None,
                launch_id: None,
                agent_version: None,
            }
        })
        .collect()
}

fn view(h: &RawHook) -> SessionView {
    SessionView {
        id: "S".into(),
        native: AntigravityAdapter
            .native_session(h)
            .expect("conversationId"),
        shared: true,
        open_requests: vec![],
    }
}

#[test]
fn pass_through_never_prints_anything() {
    // `{}` for PreToolUse would DENY the tool in agy; failing open = printing nothing.
    for h in fixture("headless_tool.jsonl") {
        assert_eq!(
            AntigravityAdapter.render_reply(&h, &HookDecision::PassThrough),
            None,
            "{}",
            h.event
        );
        assert_eq!(
            AntigravityAdapter.render_exit(&h, &HookDecision::PassThrough),
            None
        );
    }
}

#[test]
fn headless_turn_maps_to_events() {
    let a = AntigravityAdapter;
    let mut names = vec![];
    let mut reply = None;
    let mut output = None;
    for h in fixture("headless_tool.jsonl") {
        let plan = a.on_hook(&h, &view(&h)).unwrap();
        assert!(plan.wait.is_none());
        for e in plan.events {
            match &e {
                EventKind::AssistantMessage { text, .. } => reply = Some(text.clone()),
                EventKind::ToolCall {
                    status: ToolStatus::Completed,
                    output: o,
                    tool,
                    ..
                } => {
                    assert_eq!(tool.title, "Run `echo probe-allow`");
                    assert_eq!(tool.kind, ToolKind::Execute);
                    output = o.clone();
                }
                _ => {}
            }
            names.push(format!("{}:{}", h.event, e.name()));
        }
    }
    assert_eq!(
        names,
        vec![
            "SessionStart:session_started",
            "PreInvocation:turn_started",
            "PreInvocation:user_message",
            "PreToolUse:tool_call",
            "PostToolUse:tool_call",
            "Stop:assistant_message",
            "Stop:turn_ended",
        ],
        "later PreInvocations (invocationNum > 0) are the same turn"
    );
    assert_eq!(
        reply.as_deref(),
        Some("DONE"),
        "reply text comes from the transcript"
    );
    let Some(ToolOutput::Text { text, .. }) = output else {
        panic!("{output:?}")
    };
    assert!(text.contains("probe-allow"), "{text}");
    assert!(
        !text.contains("Created At"),
        "agy's header lines are stripped"
    );
}

#[test]
fn styr_is_intercepted_without_a_model_call() {
    let a = AntigravityAdapter;
    let hooks = fixture("interactive_styr_and_tool.jsonl");
    let pre = hooks
        .iter()
        .find(|h| h.event == "PreInvocation" && h.payload["lastUserInput"] == "/styr")
        .unwrap();
    let plan = a.on_hook(pre, &view(pre)).unwrap();
    assert_eq!(plan.set_sharing, Some(true));
    assert!(
        plan.events.is_empty(),
        "the /styr prompt is not a user message"
    );
    let reply = a.render_reply(pre, plan.reply.as_ref().unwrap()).unwrap();
    assert_eq!(reply["terminationBehavior"], "terminate");
    assert!(
        reply["injectSteps"][0]["errorMessage"]["userMessage"]
            .as_str()
            .unwrap()
            .contains("shared")
    );
    // The Stop that follows our intercept adds no reply.
    let stop = hooks
        .iter()
        .find(|h| h.event == "Stop" && h.payload["terminationReason"] == "TERMINAL_CUSTOM_HOOK")
        .unwrap();
    let plan = a.on_hook(stop, &view(stop)).unwrap();
    assert_eq!(
        plan.events.iter().map(|e| e.name()).collect::<Vec<_>>(),
        vec!["turn_ended"]
    );
}

#[test]
fn tool_titles() {
    let t = |n: &str, a: Value| tool_info(n, a);
    assert_eq!(
        t("run_command", json!({"CommandLine": "cargo test"})).title,
        "Run `cargo test`"
    );
    assert_eq!(
        t("view_file", json!({"AbsolutePath": "/r/a.rs"})).kind,
        ToolKind::Read
    );
    assert_eq!(
        t("replace_file_content", json!({"TargetFile": "/r/a.rs"})).title,
        "Edit /r/a.rs"
    );
    let unknown = t("some_new_tool", json!({"toolSummary": "Do a thing"}));
    assert_eq!(
        (unknown.kind, unknown.title.as_str()),
        (ToolKind::Other, "Do a thing")
    );
}

#[test]
fn transcripts_are_read_defensively() {
    assert!(read_transcript(std::path::Path::new("/nonexistent")).is_empty());
    let r = read_transcript(std::path::Path::new(&format!(
        "{}/transcript_sample.jsonl",
        dir()
    )));
    assert_eq!(last_reply(&r).as_deref(), Some("DONE"));
    assert!(
        step_output(&r, 2)
            .unwrap()
            .contains("Command exited with code 0")
    );
    assert_eq!(step_output(&r, 99), None);
}

#[test]
fn replies_in_consecutive_turns_get_their_own_ids() {
    // agy's Stop.executionNum stays 0 across turns; ids must come from the transcript.
    let a = AntigravityAdapter;
    let d = tempfile::tempdir().unwrap();
    let t = d.path().join("transcript.jsonl");
    let h = |tp: &std::path::Path| RawHook {
        agent: AgentKind::Antigravity,
        event: "Stop".into(),
        payload: json!({"conversationId": "c", "transcriptPath": tp, "executionNum": 0, "terminationReason": "NO_TOOL_CALL", "error": ""}),
        launched_by_styr: true,
        agent_pid: None,
        launch_id: None,
        agent_version: None,
    };
    let id_of = |plan: HookPlan| {
        plan.events
            .into_iter()
            .find_map(|e| match e {
                EventKind::AssistantMessage { message, .. } => Some(message),
                _ => None,
            })
            .unwrap()
    };
    std::fs::write(
        &t,
        "{\"step_index\":1,\"type\":\"PLANNER_RESPONSE\",\"content\":\"Hello!\"}\n",
    )
    .unwrap();
    let first = id_of(a.on_hook(&h(&t), &view(&h(&t))).unwrap());
    std::fs::write(
        &t,
        "{\"step_index\":1,\"type\":\"PLANNER_RESPONSE\",\"content\":\"Hello!\"}\n{\"step_index\":5,\"type\":\"PLANNER_RESPONSE\",\"content\":\"This repo is Styrman.\"}\n",
    )
    .unwrap();
    let second = id_of(a.on_hook(&h(&t), &view(&h(&t))).unwrap());
    assert_ne!(first, second);
}
