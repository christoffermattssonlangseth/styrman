//! Codex mapping against traffic captured from Codex CLI 0.172.0.

use serde_json::{Value, json};
use std::collections::HashMap;
use styr_adapter_codex::*;
use styr_core::event::*;

fn lines(name: &str) -> Vec<String> {
    std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR")))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn live_notifications_map_to_events() {
    let mut names = Vec::new();
    let mut user = Vec::new();
    let mut finals = Vec::new();
    for l in lines("app_server_events.jsonl") {
        let m: Value = serde_json::from_str(&l).unwrap();
        let Some(method) = m["method"].as_str() else { continue };
        if m.get("id").is_some() {
            continue; // server requests are handled separately
        }
        if let Some((thread, evs)) = map_notification(method, &m["params"]) {
            assert!(!thread.is_empty());
            for e in evs {
                match &e {
                    EventKind::UserMessage { text, .. } => user.push(text.clone()),
                    EventKind::AssistantMessage { text, mode: TextMode::Final, .. } => finals.push(text.clone()),
                    _ => {}
                }
                names.push(e.name());
            }
        }
    }
    assert!(user.iter().any(|t| t.contains("touch approved.txt")));
    assert!(user.iter().any(|t| t == "Reply with exactly the word FROMPHONE"), "prompt sent by the second client: {user:?}");
    assert!(finals.iter().any(|t| t == "FROMPHONE"));
    for n in ["turn_started", "turn_ended", "assistant_message", "user_message"] {
        assert!(names.contains(&n), "missing {n}: {names:?}");
    }
}

#[test]
fn approval_requests_become_permission_cards() {
    let req = lines("app_server_events.jsonl")
        .into_iter()
        .map(|l| serde_json::from_str::<Value>(&l).unwrap())
        .find(|m| m["method"] == "item/commandExecution/requestApproval")
        .expect("captured approval request");
    let a = map_server_request(req["method"].as_str().unwrap(), &req["params"]).unwrap();
    assert_eq!(a.tool.title, "Run `touch approved.txt`", "bash -lc wrapper stripped");
    assert_eq!(a.tool.kind, ToolKind::Execute);
    assert!(a.reason.as_deref().unwrap().contains("approved.txt"));
    let labels: Vec<_> = a.options.iter().map(|o| (o.kind, o.label.as_str())).collect();
    assert_eq!(
        labels,
        vec![
            (OptionKind::AllowOnce, "Allow once"),
            (OptionKind::AllowAlways, "Always allow `touch approved.txt`"),
            (OptionKind::RejectOnce, "Deny and stop"),
        ],
        "options are exactly what Codex offered"
    );
    // The answer echoes Codex's own decision value, including the object form.
    assert_eq!(approval_result(&a, "d0").unwrap(), json!({"decision": "accept"}));
    assert_eq!(
        approval_result(&a, "d1").unwrap(),
        json!({"decision": {"acceptWithExecpolicyAmendment": {"execpolicy_amendment": ["touch", "approved.txt"]}}})
    );
    assert_eq!(approval_result(&a, "nope"), None);
    // Without availableDecisions: the documented defaults.
    let plain = map_server_request("item/commandExecution/requestApproval", &json!({"threadId": "t", "command": "rm -rf x"})).unwrap();
    assert_eq!(plain.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), vec!["Allow once", "Allow for this session", "Deny"]);
    assert!(map_server_request("item/tool/call", &json!({"threadId": "t"})).is_none());
}

#[test]
fn code_mode_tool_calls_come_from_the_rollout() {
    let mut known = HashMap::new();
    let mut events = Vec::new();
    for l in lines("rollout_sample.jsonl") {
        if let Some(t) = parse_rollout_line(&l) {
            events.extend(rollout_events(t, &mut known));
        }
    }
    let calls: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            EventKind::ToolCall { tool, status, output, .. } => Some((tool.title.clone(), *status, output.clone())),
            _ => None,
        })
        .collect();
    assert!(calls.iter().any(|(t, s, _)| t == "Run `touch approved.txt`" && *s == ToolStatus::Running));
    let done: Vec<_> = calls.iter().filter(|(_, s, _)| *s != ToolStatus::Running).collect();
    assert!(!done.is_empty());
    assert!(done.iter().any(|(_, s, o)| *s == ToolStatus::Failed
        && matches!(o, Some(ToolOutput::Text { text, .. }) if text.contains("Operation not permitted"))));
}

#[test]
fn commands_are_shown_without_the_shell_wrapper() {
    assert_eq!(display_command("/bin/zsh -lc 'touch approved.txt'"), "touch approved.txt");
    assert_eq!(display_command("cargo test"), "cargo test");
}
