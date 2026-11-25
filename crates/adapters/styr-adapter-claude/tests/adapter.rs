//! Claude adapter against payloads captured from Claude Code 2.1.332.

use serde_json::{Value, json};
use styr_adapter_claude::*;
use styr_core::adapter::*;
use styr_core::event::*;
use styr_core::ids::*;

fn fixture(name: &str) -> Vec<RawHook> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            RawHook {
                agent: AgentKind::Claude,
                event: v["event"].as_str().unwrap().into(),
                payload: v["payload"].clone(),
                launched_by_styr: false,
                agent_pid: None,
            }
        })
        .collect()
}

fn view(hook: &RawHook, shared: bool, open: &[&str]) -> SessionView {
    let native = ClaudeAdapter::default()
        .native_session(hook)
        .expect("session id");
    SessionView {
        id: "S".into(),
        native,
        shared,
        open_requests: open.iter().map(|r| RequestId::from(*r)).collect(),
    }
}

#[test]
fn styr_command_parsing() {
    assert_eq!(parse_styr_command("/styr"), Some(StyrCommand::On));
    assert_eq!(parse_styr_command("  /styr on \n"), Some(StyrCommand::On));
    assert_eq!(parse_styr_command("/styr off"), Some(StyrCommand::Off));
    assert_eq!(
        parse_styr_command("/styrman:styr off"),
        Some(StyrCommand::Off)
    );
    assert_eq!(parse_styr_command("/styr please"), None);
    assert_eq!(parse_styr_command("/styrx"), None);
    assert_eq!(parse_styr_command("explain /styr"), None);
}

#[test]
fn share_command_fixture_is_consumed() {
    let a = ClaudeAdapter::default();
    let hooks = fixture("share_command.jsonl");
    let submit = hooks
        .iter()
        .find(|h| h.event == "UserPromptSubmit")
        .unwrap();
    let plan = a.on_hook(submit, &view(submit, false, &[])).unwrap();
    assert_eq!(plan.set_sharing, Some(true));
    assert!(
        plan.events.is_empty(),
        "the /styr prompt itself is not a user message"
    );
    let reply = a
        .render_reply(submit, plan.reply.as_ref().unwrap())
        .unwrap();
    assert_eq!(reply["decision"], "block");
    assert!(reply["reason"].as_str().unwrap().contains("shared"));
}

#[test]
fn tools_and_permission_fixture_maps_to_events() {
    let a = ClaudeAdapter::default();
    let hooks = fixture("tools_and_permission.jsonl");
    let mut names = vec![];
    let mut open: Vec<String> = vec![];
    for h in &hooks {
        let opens: Vec<&str> = open.iter().map(String::as_str).collect();
        let plan = a.on_hook(h, &view(h, true, &opens)).unwrap();
        assert!(
            plan.session.as_ref().unwrap().cwd.is_some(),
            "{} carries cwd",
            h.event
        );
        for e in &plan.events {
            match e {
                EventKind::PermissionRequested { request, .. } => open.push(request.to_string()),
                EventKind::PermissionResolved { request, .. } => {
                    open.retain(|r| r != request.as_str())
                }
                _ => {}
            }
            names.push(format!("{}:{}", h.event, e.name()));
        }
        if h.event == "PermissionRequest" {
            assert!(matches!(plan.wait, Some(WaitFor::Permission { .. })));
        } else {
            assert!(plan.wait.is_none());
        }
        // Nothing decided yet → nothing printed.
        assert_eq!(a.render_reply(h, &HookDecision::PassThrough), None);
    }
    assert_eq!(
        names,
        vec![
            "SessionStart:session_started",
            "UserPromptSubmit:turn_started",
            "UserPromptSubmit:user_message",
            "PreToolUse:tool_call",
            "PostToolUse:tool_call",
            "PreToolUse:tool_call",
            "PermissionRequest:permission_requested",
            // The turn ended without the tool running: the request was answered in the terminal.
            "Stop:permission_resolved",
            "Stop:assistant_message",
            "Stop:turn_ended",
            "SessionEnd:session_ended",
        ]
    );
    assert!(open.is_empty());
}

#[test]
fn bash_tool_details() {
    let a = ClaudeAdapter::default();
    let hooks = fixture("tools_and_permission.jsonl");
    let post = hooks.iter().find(|h| h.event == "PostToolUse").unwrap();
    let plan = a.on_hook(post, &view(post, true, &[])).unwrap();
    let EventKind::ToolCall {
        call,
        tool,
        status,
        output,
    } = &plan.events[0]
    else {
        panic!()
    };
    assert!(call.as_str().starts_with("toolu_"));
    assert_eq!(tool.kind, ToolKind::Execute);
    assert_eq!(tool.title, "Run `ls -la`");
    assert_eq!(*status, ToolStatus::Completed);
    let Some(ToolOutput::Text {
        text,
        truncated: false,
    }) = output
    else {
        panic!("{output:?}")
    };
    assert!(text.starts_with("total 0"));
}

#[test]
fn permission_request_options_and_replies() {
    let a = ClaudeAdapter::default();
    let hooks = fixture("tools_and_permission.jsonl");
    let req = hooks
        .iter()
        .find(|h| h.event == "PermissionRequest")
        .unwrap();
    let plan = a.on_hook(req, &view(req, true, &[])).unwrap();
    let EventKind::PermissionRequested { tool, options, .. } = &plan.events[0] else {
        panic!()
    };
    assert_eq!(tool.title, "Run `touch probe.txt`");
    let kinds: Vec<_> = options.iter().map(|o| o.kind).collect();
    assert_eq!(
        kinds,
        vec![
            OptionKind::AllowOnce,
            OptionKind::AllowAlways,
            OptionKind::RejectOnce
        ]
    );

    let allow = |kind, option: &str| {
        HookDecision::Permission(PermissionOutcome::Selected {
            option: option.into(),
            kind,
        })
    };
    assert_eq!(
        a.render_reply(req, &allow(OptionKind::AllowOnce, OPT_ALLOW))
            .unwrap(),
        json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "allow"}}})
    );
    let always = a
        .render_reply(req, &allow(OptionKind::AllowAlways, OPT_ALWAYS))
        .unwrap();
    assert_eq!(
        always["hookSpecificOutput"]["decision"]["behavior"],
        "allow"
    );
    assert_eq!(
        always["hookSpecificOutput"]["decision"]["updatedPermissions"],
        req.payload["permission_suggestions"],
        "Always = Claude's own 'don't ask again' suggestion"
    );
    let deny = a
        .render_reply(req, &allow(OptionKind::RejectOnce, OPT_DENY))
        .unwrap();
    assert_eq!(deny["hookSpecificOutput"]["decision"]["behavior"], "deny");
    // No phone answer → Claude's normal terminal prompt.
    assert_eq!(
        a.render_reply(req, &HookDecision::Permission(PermissionOutcome::TimedOut)),
        None
    );
    assert_eq!(
        a.render_reply(req, &HookDecision::Permission(PermissionOutcome::Cancelled)),
        None
    );
}

#[test]
fn stop_can_carry_a_phone_prompt() {
    let a = ClaudeAdapter::default();
    let hooks = fixture("tools_and_permission.jsonl");
    let stop = hooks.iter().find(|h| h.event == "Stop").unwrap();
    assert_eq!(
        a.render_reply(stop, &HookDecision::Prompt("now run the tests".into()))
            .unwrap(),
        json!({"decision": "block", "reason": "now run the tests"})
    );
}

#[test]
fn tool_titles() {
    let t = |name: &str, input: Value| tool_info(name, input);
    assert_eq!(
        t("Edit", json!({"file_path": "/r/src/main.rs"})).title,
        "Edit /r/src/main.rs"
    );
    assert_eq!(
        t("Edit", json!({"file_path": "/r/a.rs"})).locations,
        vec![std::path::PathBuf::from("/r/a.rs")]
    );
    assert_eq!(
        t("Grep", json!({"pattern": "fn main"})).kind,
        ToolKind::Search
    );
    let mcp = t("mcp__github__create_issue", json!({}));
    assert_eq!(
        (mcp.kind, mcp.title.as_str()),
        (ToolKind::Mcp, "github: create_issue")
    );
    let long = "x".repeat(500);
    assert!(t("Bash", json!({"command": long})).title.chars().count() < 140);
}

#[test]
fn malformed_payloads_are_rejected_not_panicking() {
    let a = ClaudeAdapter::default();
    let h = RawHook {
        agent: AgentKind::Claude,
        event: "PreToolUse".into(),
        payload: json!("nope"),
        launched_by_styr: false,
        agent_pid: None,
    };
    assert_eq!(a.native_session(&h), None);
    let v = SessionView {
        id: "S".into(),
        native: NativeSessionRef::new(AgentKind::Claude, "x"),
        shared: true,
        open_requests: vec![],
    };
    assert!(a.on_hook(&h, &v).is_err());
    let h = RawHook {
        payload: json!({"session_id": "x"}),
        ..h
    };
    let plan = a.on_hook(&h, &v).unwrap(); // missing fields degrade gracefully
    assert_eq!(plan.events.len(), 1);
}

#[test]
fn rewake_turns_are_not_user_messages_and_phone_prompts_exit_2() {
    let a = ClaudeAdapter::default();
    // Captured from Claude Code 2.1.332 after our StopWait hook exited 2.
    let internal = "<task-notification>\n<summary>Stop hook feedback</summary>\n</task-notification>\n<system-reminder>\nStop hook blocking error from command \"Stop\": The user sent this message from their phone via Styrman. Treat it as their next prompt and respond to it:\n\nReply with exactly the word PHONE-ONE\n</system-reminder>";
    let h = RawHook {
        agent: AgentKind::Claude,
        event: "UserPromptSubmit".into(),
        payload: json!({"session_id": "x", "prompt": internal}),
        launched_by_styr: false,
        agent_pid: None,
    };
    let v = SessionView {
        id: "S".into(),
        native: NativeSessionRef::new(AgentKind::Claude, "x"),
        shared: true,
        open_requests: vec!["r".into()],
    };
    let plan = a.on_hook(&h, &v).unwrap();
    assert_eq!(
        plan.events.iter().map(|e| e.name()).collect::<Vec<_>>(),
        vec!["turn_started"]
    );

    let wait = RawHook {
        event: STOP_WAIT.into(),
        payload: json!({"session_id": "x"}),
        ..h
    };
    let plan = a.on_hook(&wait, &v).unwrap();
    assert!(plan.events.is_empty());
    assert!(matches!(plan.wait, Some(WaitFor::NextPrompt { .. })));
    let (code, stderr) = a
        .render_exit(&wait, &HookDecision::Prompt("run it".into()))
        .unwrap();
    assert_eq!(code, 2);
    assert!(stderr.ends_with("run it"));
    assert_eq!(a.render_exit(&wait, &HookDecision::PassThrough), None);
    assert_eq!(
        a.render_reply(&wait, &HookDecision::Prompt("x".into())),
        None
    );
}
