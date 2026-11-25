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
    for (e, p) in &steps {
        let out = r.run(e, p);
        assert_eq!((out.exit_code, out.stdout.as_str()), (0, ""), "{e}");
    }
    // A shared session's permission hook blocks, waiting for the phone…
    let (e, p) = s.permission("Bash", input.clone());
    let mut waiting = r.spawn(&e, &p);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while d.sessions(false)[0].status != SessionStatus::NeedsApproval {
        assert!(
            std::time::Instant::now() < deadline,
            "never reached needs-approval"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        waiting.try_wait().unwrap().is_none(),
        "hook is still waiting"
    );
    // …until Claude cancels it because the user answered in the terminal.
    waiting.kill().unwrap();
    waiting.wait().unwrap();
    while d.sessions(false)[0].status == SessionStatus::NeedsApproval {
        assert!(std::time::Instant::now() < deadline, "card never closed");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let last = d.events("sess-1").pop().unwrap();
    assert_eq!(
        last.kind,
        styr_core::event::EventKind::PermissionResolved {
            request: match &d.events("sess-1")[d.events("sess-1").len() - 2].kind {
                styr_core::event::EventKind::PermissionRequested { request, .. } => request.clone(),
                other => panic!("{other:?}"),
            },
            outcome: styr_core::event::PermissionOutcome::Cancelled,
            by: styr_core::event::Resolver::Terminal,
        }
    );

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

#[test]
fn background_stop_hook_exits_2_with_the_phone_prompt() {
    let d = TestDaemon::start();
    let r = runner(&d);
    let mut s = ClaudeScript::new("sess-3", "/work/repo");
    for (e, p) in [s.start(), s.prompt("/styr"), s.stop("Done.")] {
        r.run(&e, &p);
    }
    // The plugin's second Stop hook: `styr hook claude StopWait`, async + asyncRewake.
    let (_, stop) = s.stop("Done.");
    let child = r.spawn("StopWait", &stop);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let sid = d.session_by_native("sess-3").unwrap().id;
    let reply = d
        .client()
        .call(&styr_core::ipc::Request::SendPrompt {
            session: sid,
            text: "add a test for it".into(),
        })
        .unwrap();
    assert_eq!(
        reply,
        styr_core::ipc::Response::PromptAccepted { delivered: true }
    );
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(2), "exit 2 wakes Claude");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("add a test for it"), "{stderr}");
    assert!(out.stdout.is_empty());

    // Without a daemon the same hook exits 0 at once.
    let gone = TestDaemon::stopped();
    let out = runner(&gone).run_raw(&["claude", "StopWait"], stop.to_string().as_bytes());
    assert_eq!(
        (out.exit_code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", "")
    );
}

#[test]
fn wrapper_links_survive_a_daemon_restart() {
    use styr_core::ipc::{Client, Request, Response};
    let mut d = TestDaemon::start();
    let hook = |event: &str, extra: serde_json::Value| {
        let mut payload = json!({"conversationId": "agy-1", "cwd": "/work/repo"});
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        Request::Hook {
            hook: styr_core::adapter::RawHook {
                agent: AgentKind::Antigravity,
                event: event.into(),
                payload,
                launched_by_styr: true,
                agent_pid: None,
                launch_id: Some("L1".into()),
                agent_version: None,
            },
        }
    };
    d.client().call(&hook("SessionStart", json!({}))).unwrap();
    d.client()
        .call(&hook(
            "PreInvocation",
            json!({"invocationNum": 0, "lastUserInput": "/styr"}),
        ))
        .unwrap();
    d.restart();
    // The wrapper reconnects (long-poll in the background); no agy hook fires.
    let sock = d.paths.socket.clone();
    std::thread::spawn(move || {
        Client::new(sock)
            .with_timeout(None)
            .call(&Request::AttachTerminal {
                launch_id: "L1".into(),
            })
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let s = d.session_by_native("agy-1").unwrap();
        if s.in_terminal_wrapper && s.accepts_prompts {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "link lost across restart: {s:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let Response::Sessions { .. } = d
        .client()
        .call(&Request::ListSessions {
            include_unshared: false,
        })
        .unwrap()
    else {
        panic!()
    };
}
