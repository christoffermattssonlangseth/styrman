//! End to end: the real `styr hook` binary talking to an in-process styrd,
//! driven by the fake-agent harness exactly like Claude Code drives it.

use serde_json::json;
use styr_core::SessionStatus;
use styr_core::ids::AgentKind;
use styr_testkit::{ClaudeScript, HookRunner, TestDaemon, load_fixture};

const STYR: &str = env!("CARGO_BIN_EXE_styr");

fn fixture(name: &str) -> Vec<(String, serde_json::Value)> {
    load_fixture(format!(
        "{}/../adapters/styr-adapter-claude/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
}

fn runner(d: &TestDaemon) -> HookRunner {
    HookRunner::new(STYR, &d.paths, AgentKind::Claude)
}

#[test]
fn fails_open_without_a_daemon() {
    let d = TestDaemon::stopped();
    let r = runner(&d);
    for file in ["share_command.jsonl", "tools_and_permission.jsonl"] {
        for (event, payload) in fixture(file) {
            let out = r.run(&event, &payload);
            assert_eq!(out.exit_code, 0, "{event}");
            assert_eq!(out.stdout, "", "{event} must print nothing");
        }
    }
}

#[test]
fn never_exits_nonzero_on_garbage() {
    let d = TestDaemon::start();
    let r = runner(&d);
    for (args, stdin) in [
        (vec![], &b""[..]),
        (vec!["claude"], &b"{}"[..]),
        (vec!["nosuchagent", "PreToolUse"], &b"{}"[..]),
        (vec!["claude", "PreToolUse"], &b"not json"[..]),
        (vec!["claude", "PreToolUse"], &b"[1,2,3]"[..]),
        (vec!["claude", "NoSuchEvent"], &br#"{"session_id":"x"}"#[..]),
        (vec!["claude", "PreToolUse", "--extra", "-x"], &b"{}"[..]),
    ] {
        let out = r.run_raw(&args, stdin);
        assert_eq!(out.exit_code, 0, "{args:?}");
        assert_eq!(out.stdout, "", "{args:?}");
    }
}

#[test]
fn unshared_session_is_tracked_locally_but_keeps_no_content() {
    let d = TestDaemon::start();
    let r = runner(&d);
    for (event, payload) in fixture("tools_and_permission.jsonl") {
        let out = r.run(&event, &payload);
        assert_eq!((out.exit_code, out.stdout.as_str()), (0, ""), "{event}");
    }
    assert!(d.sessions(false).is_empty(), "nothing for the phone");
    let s = &d.sessions(true)[0];
    assert_eq!(
        s.cwd.as_deref(),
        Some(std::path::Path::new("/Users/dev/project"))
    );
    assert_eq!(s.status, SessionStatus::Done);
    assert_eq!(
        d.event_names(&s.native_id),
        vec!["session_started", "session_ended"]
    );
}

#[test]
fn styr_shares_then_mirrors_the_session() {
    let d = TestDaemon::start();
    let r = runner(&d);
    let mut s = ClaudeScript::new("sess-1", "/work/repo");

    let (e, p) = s.start();
    assert_eq!(r.run(&e, &p).stdout, "");

    // `/styr` is consumed by the hook: blocked with a message, no model call.
    let (e, p) = s.prompt("/styr");
    let reply = r.run(&e, &p).json().expect("/styr gets a reply");
    assert_eq!(reply["decision"], "block");
    assert!(d.session_by_native("sess-1").unwrap().shared);

    let mut steps = vec![s.prompt("list the files")];
    steps.extend(s.bash("ls", "Cargo.toml\nsrc"));
    let id = s.next_tool_id();
    let input = json!({"command": "rm -rf target", "description": "clean"});
    steps.push(s.pre_tool(&id, "Bash", input.clone()));
    steps.push(s.permission("Bash", input.clone()));
    for (e, p) in &steps {
        let out = r.run(e, p);
        assert_eq!((out.exit_code, out.stdout.as_str()), (0, ""), "{e}");
    }
    assert_eq!(d.sessions(false)[0].status, SessionStatus::NeedsApproval);

    // Answered in the terminal: the tool runs, the card closes.
    let (e, p) = s.post_tool(&id, "Bash", input, json!({"stdout": "", "stderr": ""}));
    r.run(&e, &p);
    assert_eq!(d.sessions(false)[0].status, SessionStatus::Working);
    let (e, p) = s.stop("Listed the files and cleaned target/.");
    r.run(&e, &p);
    assert_eq!(d.sessions(false)[0].status, SessionStatus::Waiting);

    assert_eq!(
        d.event_names("sess-1"),
        vec![
            "session_started",
            "sharing_changed",
            "turn_started",
            "user_message",
            "tool_call",
            "tool_call",
            "tool_call",
            "permission_requested",
            "permission_resolved",
            "tool_call",
            "assistant_message",
            "turn_ended",
        ]
    );

    // `/styr off` stops mirroring; later content is not kept.
    let (e, p) = s.prompt("/styr off");
    assert_eq!(r.run(&e, &p).json().unwrap()["decision"], "block");
    let (e, p) = s.prompt("secret stuff");
    r.run(&e, &p);
    assert!(d.sessions(false).is_empty());
    let names = d.event_names("sess-1");
    assert_eq!(names.last().map(String::as_str), Some("sharing_changed"));
    assert!(!format!("{:?}", d.events("sess-1")).contains("secret stuff"));
}

#[test]
fn history_survives_a_daemon_restart_mid_session() {
    let mut d = TestDaemon::start();
    let r = runner(&d);
    let mut s = ClaudeScript::new("sess-2", "/work/repo");
    for (e, p) in [s.start(), s.prompt("/styr"), s.prompt("hello")] {
        r.run(&e, &p);
    }
    let before = d.event_names("sess-2");
    d.restart();
    // Hooks keep working against the new daemon; same session, contiguous history.
    for (e, p) in s.bash("pwd", "/work/repo") {
        assert_eq!(r.run(&e, &p).exit_code, 0);
    }
    let after = d.events("sess-2");
    assert_eq!(after.len(), before.len() + 2);
    assert_eq!(
        after.iter().map(|e| e.seq).collect::<Vec<_>>(),
        (1..=after.len() as u64).collect::<Vec<_>>()
    );
}
