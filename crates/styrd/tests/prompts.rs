//! Prompts from the phone: delivered to the background (asyncRewake) Stop hook,
//! queued while the agent works, and stood down when the user types in the terminal.

mod common;
use common::*;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use styr_core::adapter::RawHook;
use styr_core::event::*;
use styr_core::ids::*;
use styr_core::ipc::{Client, HookOutput, Request, Response};
use styr_core::phone::ServerMsg;

fn hook(session: &str, event: &str, extra: Value) -> RawHook {
    let mut payload = json!({"session_id": session, "cwd": "/work/repo", "hook_event_name": event});
    payload
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    RawHook {
        agent: AgentKind::Claude,
        event: event.into(),
        payload,
        launched_by_styr: false,
        agent_pid: None,
        launch_id: None,
        agent_version: None,
    }
}

fn stop_wait(session: &str) -> RawHook {
    let mut h = hook(session, "Stop", json!({"stop_hook_active": false}));
    h.event = "StopWait".into();
    h
}

fn spawn_hook(d: &Daemon, h: RawHook) -> tokio::task::JoinHandle<HookOutput> {
    let c = Client::new(&d.paths.socket).with_timeout(None);
    tokio::task::spawn_blocking(move || styr_core::ipc::forward_hook(&c, h).expect("hook call"))
}

async fn share(d: &Daemon, session: &str) {
    d.call(Request::Hook {
        hook: hook(session, "SessionStart", json!({"source": "startup"})),
    })
    .await;
    d.call(Request::Hook {
        hook: hook(session, "UserPromptSubmit", json!({"prompt": "/styr"})),
    })
    .await;
}

async fn phone(d: &Daemon, native: &str) -> (Ws, String) {
    let (url, token) = paired_phone(d).await;
    let mut ws = connect(&url, &token).await.unwrap();
    recv(&mut ws).await;
    let ServerMsg::Sessions { sessions } = recv(&mut ws).await else {
        panic!()
    };
    let s = sessions.iter().find(|s| s.native_id == native).unwrap();
    let sid = s.id.to_string();
    send(
        &mut ws,
        json!({"type": "subscribe", "session": sid, "since_seq": s.last_seq}),
    )
    .await;
    while !matches!(recv(&mut ws).await, ServerMsg::Subscribed { .. }) {}
    (ws, sid)
}

async fn prompt(ws: &mut Ws, sid: &str, text: &str) {
    send(ws, json!({"type": "command", "session": sid, "command": {"type": "send_prompt", "text": text}})).await;
}

/// Next error sent to the phone, skipping everything else.
async fn next_error(ws: &mut Ws) -> String {
    loop {
        if let ServerMsg::Error { message } = recv(ws).await {
            return message;
        }
    }
}

/// Next `user_message` from the phone, skipping everything else.
async fn next_remote_message(ws: &mut Ws) -> String {
    loop {
        if let ServerMsg::Event { event } = recv(ws).await
            && let EventKind::UserMessage {
                text,
                origin: Origin::Remote(_),
            } = event.kind
        {
            return text;
        }
    }
}

fn assert_wakes(out: &HookOutput, text: &str) {
    let exit = out.exit.as_ref().expect("exit 2 to wake Claude");
    assert_eq!(exit.code, 2);
    assert!(exit.stderr.contains(text), "{}", exit.stderr);
    assert!(exit.stderr.contains("from their phone"));
    assert_eq!(out.stdout, None);
}

/// Give a spawned hook a moment to reach the daemon and start waiting.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_session_wakes_with_the_phone_prompt() {
    let d = Daemon::start().await;
    share(&d, "s1").await;
    let (mut ws, sid) = phone(&d, "s1").await;
    let waiting = spawn_hook(&d, stop_wait("s1"));
    settle().await;
    assert!(!waiting.is_finished(), "the background Stop hook waits");
    prompt(&mut ws, &sid, "  now run the tests  ").await;
    let out = waiting.await.unwrap();
    assert_wakes(&out, "now run the tests");
    assert_eq!(next_remote_message(&mut ws).await, "now run the tests");
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prompts_sent_while_busy_are_queued_for_the_next_stop() {
    let d = Daemon::start().await;
    share(&d, "s2").await;
    let (mut ws, sid) = phone(&d, "s2").await;
    prompt(&mut ws, &sid, "first").await;
    prompt(&mut ws, &sid, "second").await;
    let mut positions = vec![];
    while positions.len() < 2 {
        if let ServerMsg::PromptQueued { position, .. } = recv(&mut ws).await {
            positions.push(position);
        }
    }
    assert_eq!(positions, vec![1, 2]);
    // Next stop: the queued prompt goes out immediately, oldest first.
    let started = Instant::now();
    assert_wakes(&spawn_hook(&d, stop_wait("s2")).await.unwrap(), "first");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_wakes(&spawn_hook(&d, stop_wait("s2")).await.unwrap(), "second");
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typing_in_the_terminal_stands_the_wait_down() {
    let d = Daemon::start().await;
    share(&d, "s3").await;
    let waiting = spawn_hook(&d, stop_wait("s3"));
    settle().await;
    d.call(Request::Hook {
        hook: hook(
            "s3",
            "UserPromptSubmit",
            json!({"prompt": "typed at the keyboard"}),
        ),
    })
    .await;
    let out = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("released")
        .unwrap();
    assert_eq!(out, HookOutput::default(), "exits 0, prints nothing");

    // A newer wait also replaces an older one.
    let older = spawn_hook(&d, stop_wait("s3"));
    settle().await;
    let newer = spawn_hook(&d, stop_wait("s3"));
    let out = tokio::time::timeout(Duration::from_secs(5), older)
        .await
        .expect("released")
        .unwrap();
    assert_eq!(out, HookOutput::default());
    // CLI path delivers to the newer one.
    settle().await;
    let sid = d
        .call(Request::ListSessions {
            include_unshared: false,
        })
        .await;
    let Response::Sessions { sessions } = sid else {
        panic!()
    };
    let r = d
        .call(Request::SendPrompt {
            session: sessions[0].id.clone(),
            text: "from the cli".into(),
        })
        .await;
    assert_eq!(r, Response::PromptAccepted { delivered: true });
    assert_wakes(&newer.await.unwrap(), "from the cli");
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unshared_or_ended_sessions_never_wait() {
    let d = Daemon::start().await;
    d.call(Request::Hook {
        hook: hook("s4", "SessionStart", json!({"source": "startup"})),
    })
    .await;
    let started = Instant::now();
    assert_eq!(
        spawn_hook(&d, stop_wait("s4")).await.unwrap(),
        HookOutput::default()
    );
    assert!(started.elapsed() < Duration::from_millis(500));

    share(&d, "s5").await;
    let waiting = spawn_hook(&d, stop_wait("s5"));
    settle().await;
    d.call(Request::Hook {
        hook: hook("s5", "UserPromptSubmit", json!({"prompt": "/styr off"})),
    })
    .await;
    let out = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("released on unshare")
        .unwrap();
    assert_eq!(out, HookOutput::default());
    d.stop().await;
}

fn with_launch(mut h: RawHook, launch: &str) -> RawHook {
    h.launch_id = Some(launch.into());
    h
}

/// Play `styr claude`'s terminal wrapper: wait for the next prompt to type.
fn attach(d: &Daemon, launch: &str) -> tokio::task::JoinHandle<Response> {
    attach_at(d, launch, styr_core::ipc::WRAPPER_LEVEL)
}

/// A wrapper that understands actions up to `level` (0: from before levels).
fn attach_at(d: &Daemon, launch: &str, level: u32) -> tokio::task::JoinHandle<Response> {
    let c = Client::new(&d.paths.socket).with_timeout(None);
    let req = Request::AttachTerminal {
        launch_id: launch.into(),
        agent: None,
        cwd: None,
        level,
    };
    tokio::task::spawn_blocking(move || c.call(&req).expect("attach"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inside_styr_claude_prompts_are_typed_into_the_terminal() {
    let d = Daemon::start().await;
    let l = "launch-1";
    d.call(Request::Hook {
        hook: with_launch(hook("t1", "SessionStart", json!({"source": "startup"})), l),
    })
    .await;
    d.call(Request::Hook {
        hook: with_launch(
            hook("t1", "UserPromptSubmit", json!({"prompt": "/styr"})),
            l,
        ),
    })
    .await;
    let (mut ws, sid) = phone(&d, "t1").await;

    let wrapper = attach(&d, l);
    settle().await;
    prompt(&mut ws, &sid, "summarise the README").await;
    let typed = tokio::time::timeout(Duration::from_secs(5), wrapper)
        .await
        .expect("typed")
        .unwrap();
    assert_eq!(
        typed,
        Response::TypePrompt {
            text: "summarise the README".into()
        }
    );

    // Claude reports the typed line as a normal prompt; it's credited to the phone.
    d.call(Request::Hook {
        hook: with_launch(
            hook(
                "t1",
                "UserPromptSubmit",
                json!({"prompt": "summarise the README"}),
            ),
            l,
        ),
    })
    .await;
    assert_eq!(next_remote_message(&mut ws).await, "summarise the README");

    // While a permission prompt is open nothing is typed (keys could answer it).
    let perm = with_launch(
        hook(
            "t1",
            "PermissionRequest",
            json!({"tool_name": "Bash", "tool_input": {"command": "rm x"}}),
        ),
        l,
    );
    let sock = d.paths.socket.clone();
    let line = serde_json::to_string(&Request::Hook { hook: perm }).unwrap();
    let perm_conn = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let s = std::os::unix::net::UnixStream::connect(sock).unwrap();
        (&s).write_all(format!("{line}\n").as_bytes()).unwrap();
        s
    })
    .await
    .unwrap();
    settle().await;
    let wrapper = attach(&d, l);
    prompt(&mut ws, &sid, "and then the tests").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        !wrapper.is_finished(),
        "held while the permission prompt is open"
    );
    drop(perm_conn); // answered in the terminal: the hook goes away
    let typed = tokio::time::timeout(Duration::from_secs(5), wrapper)
        .await
        .expect("typed after")
        .unwrap();
    assert_eq!(
        typed,
        Response::TypePrompt {
            text: "and then the tests".into()
        }
    );
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_reaches_the_terminal_only_if_its_wrapper_knows_it() {
    let d = Daemon::start().await;
    let l = "launch-stop";
    for (event, extra) in [
        ("SessionStart", json!({"source": "startup"})),
        ("UserPromptSubmit", json!({"prompt": "/styr"})),
    ] {
        d.call(Request::Hook {
            hook: with_launch(hook("t1", event, extra), l),
        })
        .await;
    }
    let (mut ws, sid) = phone(&d, "t1").await;
    d.call(Request::Hook {
        hook: with_launch(
            hook("t1", "UserPromptSubmit", json!({"prompt": "long job"})),
            l,
        ),
    })
    .await;
    let stop = json!({"type": "command", "session": sid, "command": {"type": "interrupt"}});

    // Started before an update: its wrapper would ignore the Esc. Say so, and don't
    // pretend the turn stopped.
    let _old = attach_at(&d, l, 0);
    settle().await;
    let outdated = |r: Response| match r {
        Response::Sessions { sessions } => {
            sessions
                .iter()
                .find(|s| s.native_id == "t1")
                .unwrap()
                .outdated
        }
        r => panic!("{r:?}"),
    };
    assert!(outdated(
        d.call(Request::ListSessions {
            include_unshared: true
        })
        .await
    ));
    send(&mut ws, stop.clone()).await;
    loop {
        match recv(&mut ws).await {
            ServerMsg::Error { message } => {
                assert!(
                    message.contains("started before Styrman was updated"),
                    "{message}"
                );
                break;
            }
            ServerMsg::Event { event } => assert!(
                !matches!(event.kind, EventKind::TurnEnded { .. }),
                "not marked stopped"
            ),
            _ => {}
        }
    }

    // A current wrapper gets the Esc.
    let wrapper = attach(&d, l);
    settle().await;
    assert!(!outdated(
        d.call(Request::ListSessions {
            include_unshared: true
        })
        .await
    ));
    send(&mut ws, stop).await;
    let got = tokio::time::timeout(Duration::from_secs(5), wrapper)
        .await
        .expect("told")
        .unwrap();
    assert_eq!(got, Response::Interrupt);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_model_restarts_the_agent_as_the_same_session() {
    let d = Daemon::start().await;
    let l = "launch-model";
    for (event, extra) in [
        ("SessionStart", json!({"source": "startup"})),
        ("UserPromptSubmit", json!({"prompt": "/styr"})),
        ("UserPromptSubmit", json!({"prompt": "long job"})),
    ] {
        d.call(Request::Hook {
            hook: with_launch(hook("t1", event, extra), l),
        })
        .await;
    }
    let (mut ws, sid) = phone(&d, "t1").await;
    let set = json!({"type": "command", "session": sid,
        "command": {"type": "set_model", "model": "sonnet", "effort": "low"}});
    // Old wrappers can't; nor mid-turn.
    let _old = attach_at(&d, l, 1);
    settle().await;
    send(&mut ws, set.clone()).await;
    assert!(
        next_error(&mut ws)
            .await
            .contains("started before Styrman was updated")
    );
    let wrapper = attach(&d, l);
    settle().await;
    send(&mut ws, set.clone()).await;
    assert!(next_error(&mut ws).await.contains("working"));

    d.call(Request::Hook {
        hook: with_launch(hook("t1", "Stop", json!({"stop_hook_active": false})), l),
    })
    .await;
    send(&mut ws, set).await;
    let got = tokio::time::timeout(Duration::from_secs(5), wrapper)
        .await
        .expect("told")
        .unwrap();
    assert_eq!(
        got,
        Response::Restart {
            resume: "t1".into(),
            model: Some("sonnet".into()),
            effort: Some("low".into()),
            mode: None,
        }
    );

    // The agent quits and resumes: no end, still shared, model shown.
    for (event, extra) in [
        ("SessionEnd", json!({"reason": "other"})),
        ("SessionStart", json!({"source": "resume"})),
    ] {
        d.call(Request::Hook {
            hook: with_launch(hook("t1", event, extra), l),
        })
        .await;
    }
    let _again = attach(&d, l);
    settle().await;
    let Response::Sessions { sessions } = d
        .call(Request::ListSessions {
            include_unshared: true,
        })
        .await
    else {
        panic!()
    };
    let s = sessions.iter().find(|s| s.native_id == "t1").unwrap();
    assert!(s.shared);
    assert_ne!(s.status, styr_core::SessionStatus::Done);
    assert_eq!(
        (s.model.as_deref(), s.effort.as_deref()),
        (Some("sonnet"), Some("low"))
    );
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn asking_an_agent_by_name_means_the_one_in_your_folder() {
    let d = Daemon::start().await;
    // Claude in /work/repo; Codex only in another project.
    for (event, extra) in [
        ("SessionStart", json!({"source": "startup"})),
        ("UserPromptSubmit", json!({"prompt": "/styr"})),
    ] {
        d.call(Request::Hook {
            hook: with_launch(hook("a1", event, extra.clone()), "LA"),
        })
        .await;
        let mut h = codex_hook("x1", event, extra, "LX");
        h.payload["cwd"] = json!("/work/other");
        d.call(Request::Hook { hook: h }).await;
    }
    let elsewhere = attach(&d, "LX");
    settle().await;
    let ask = |to: &'static str| {
        let c = Client::new(&d.paths.socket).with_timeout(None);
        tokio::task::spawn_blocking(move || {
            c.call(&Request::Ask {
                launch_id: Some("LA".into()),
                to: to.into(),
                text: "review this".into(),
                wait_secs: None,
            })
        })
    };
    // No Codex in /work/repo: it would start one there (not possible here: the
    // folder doesn't exist), never use the one busy with another project.
    let r = ask("codex").await.unwrap();
    assert!(r.is_err(), "{r:?}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !elsewhere.is_finished(),
        "the Codex in /work/other got nothing"
    );
    // Its id still reaches it on purpose.
    let Response::Sessions { sessions } = d
        .call(Request::ListSessions {
            include_unshared: false,
        })
        .await
    else {
        panic!()
    };
    let other = sessions
        .iter()
        .find(|s| s.native_id == "x1")
        .unwrap()
        .id
        .to_string();
    let c = Client::new(&d.paths.socket).with_timeout(None);
    let by_id = tokio::task::spawn_blocking(move || {
        c.call(&Request::Ask {
            launch_id: Some("LA".into()),
            to: other,
            text: "review this".into(),
            wait_secs: None,
        })
    });
    let typed = tokio::time::timeout(Duration::from_secs(5), elsewhere)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(typed, Response::TypePrompt { .. }), "{typed:?}");
    assert!(by_id.await.unwrap().is_ok());
    d.stop().await;
}

fn agy_hook(conv: &str, event: &str, extra: Value, launch: &str) -> RawHook {
    let mut payload = json!({"conversationId": conv, "cwd": "/work/repo", "hookEventName": event});
    payload
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    RawHook {
        agent: AgentKind::Antigravity,
        event: event.into(),
        payload,
        launched_by_styr: true,
        agent_pid: None,
        launch_id: Some(launch.into()),
        agent_version: None,
    }
}

fn menu() -> styr_core::ipc::TerminalPrompt {
    styr_core::ipc::TerminalPrompt {
        title: "Run this command?".into(),
        detail: "rm -rf build".into(),
        options: vec![
            "Yes, run command".into(),
            "Yes, and always allow in this conversation".into(),
            "No, cancel".into(),
        ],
        selected: 0,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agy_permission_menus_are_answered_from_the_phone_or_the_terminal() {
    let d = Daemon::start().await;
    let l = "agy-launch";
    let r = d
        .call(Request::Hook {
            hook: agy_hook("c1", "SessionStart", json!({}), l),
        })
        .await;
    assert_eq!(
        r,
        Response::HookReply {
            stdout: None,
            exit: None
        },
        "agy hooks print nothing by default"
    );
    let r = d
        .call(Request::Hook {
            hook: agy_hook(
                "c1",
                "PreInvocation",
                json!({"invocationNum": 0, "lastUserInput": "/styr"}),
                l,
            ),
        })
        .await;
    let Response::HookReply {
        stdout: Some(out), ..
    } = r
    else {
        panic!("{r:?}")
    };
    assert_eq!(out["terminationBehavior"], "terminate");
    let (mut ws, sid) = phone(&d, "c1").await;
    d.call(Request::Hook {
        hook: agy_hook("c1", "PreToolUse", json!({"toolCall": {"id": "call_1", "name": "run_command", "args": {"CommandLine": "rm -rf build"}}, "stepIdx": 2}), l),
    })
    .await;

    // The wrapper reports agy's menu; the phone gets a card with its options.
    d.call(Request::TerminalPrompt {
        launch_id: l.into(),
        prompt: Some(menu()),
    })
    .await;
    let (request, options, tool) = loop {
        if let ServerMsg::Event { event } = recv(&mut ws).await
            && let EventKind::PermissionRequested {
                request,
                options,
                tool,
                ..
            } = event.kind
        {
            break (request, options, tool);
        }
    };
    assert_eq!(
        tool.title, "Run `rm -rf build`",
        "described by the last announced tool"
    );
    assert_eq!(
        options.iter().map(|o| o.kind).collect::<Vec<_>>(),
        vec![
            OptionKind::AllowOnce,
            OptionKind::AllowAlways,
            OptionKind::RejectOnce
        ]
    );
    // Repeated reports of the same menu don't create new requests.
    d.call(Request::TerminalPrompt {
        launch_id: l.into(),
        prompt: Some(styr_core::ipc::TerminalPrompt {
            selected: 1,
            ..menu()
        }),
    })
    .await;

    // Phone picks "always" → the wrapper is told to select option 1.
    let wrapper = attach(&d, l);
    settle().await;
    send(&mut ws, json!({"type": "command", "session": sid, "command": {"type": "resolve_permission", "request": request, "option": "opt-1"}})).await;
    let action = tokio::time::timeout(Duration::from_secs(5), wrapper)
        .await
        .expect("select")
        .unwrap();
    assert_eq!(action, Response::SelectOption { index: 1 });
    loop {
        if let ServerMsg::Event { event } = recv(&mut ws).await
            && let EventKind::PermissionResolved { by, outcome, .. } = event.kind
        {
            assert!(matches!(by, Resolver::Phone(_)));
            assert!(matches!(
                outcome,
                PermissionOutcome::Selected {
                    kind: OptionKind::AllowAlways,
                    ..
                }
            ));
            break;
        }
    }
    d.call(Request::TerminalPrompt {
        launch_id: l.into(),
        prompt: None,
    })
    .await;

    // Next menu answered at the keyboard: it disappears → resolved by the terminal.
    d.call(Request::TerminalPrompt {
        launch_id: l.into(),
        prompt: Some(styr_core::ipc::TerminalPrompt {
            detail: "rm -rf dist".into(),
            ..menu()
        }),
    })
    .await;
    loop {
        if let ServerMsg::Event { event } = recv(&mut ws).await
            && let EventKind::PermissionRequested { .. } = event.kind
        {
            break;
        }
    }
    d.call(Request::TerminalPrompt {
        launch_id: l.into(),
        prompt: None,
    })
    .await;
    loop {
        if let ServerMsg::Event { event } = recv(&mut ws).await
            && let EventKind::PermissionResolved { by, .. } = event.kind
        {
            assert_eq!(by, Resolver::Terminal);
            break;
        }
    }
    d.stop().await;
}

fn codex_hook(session: &str, event: &str, extra: Value, launch: &str) -> RawHook {
    let mut payload = json!({"session_id": session, "cwd": "/work/repo", "hook_event_name": event,
        "model": "gpt-5.6-luna", "permission_mode": "default", "transcript_path": null});
    payload
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    RawHook {
        agent: AgentKind::Codex,
        event: event.into(),
        payload,
        launched_by_styr: true,
        agent_pid: None,
        launch_id: Some(launch.into()),
        agent_version: None,
    }
}

async fn shared_codex(d: &Daemon) -> Vec<bool> {
    let Response::Sessions { sessions } = d
        .call(Request::ListSessions {
            include_unshared: true,
        })
        .await
    else {
        panic!()
    };
    sessions
        .into_iter()
        .filter(|s| s.agent == AgentKind::Codex)
        .map(|s| s.shared)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codex_styr_caught_by_the_terminal_shares_its_session() {
    let d = Daemon::start().await;
    let l = "codex-launch";
    // Codex fires SessionStart only with the first prompt: `/styr` comes first.
    let r = d
        .call(Request::ShareLaunch {
            launch_id: l.into(),
            shared: true,
            agent: None,
            cwd: None,
            agent_pid: None,
        })
        .await;
    assert_eq!(r, Response::LaunchShared { sessions: 0 }, "deferred");
    assert!(shared_codex(&d).await.is_empty());
    for (event, extra) in [
        ("SessionStart", json!({"source": "startup"})),
        (
            "UserPromptSubmit",
            json!({"prompt": "fix the build", "turn_id": "t1"}),
        ),
    ] {
        let r = d
            .call(Request::Hook {
                hook: codex_hook("s1", event, extra, l),
            })
            .await;
        assert_eq!(
            r,
            Response::HookReply {
                stdout: None,
                exit: None
            },
            "Codex hooks print nothing"
        );
    }
    assert_eq!(
        shared_codex(&d).await,
        vec![true],
        "applied on the first hook"
    );
    // Later `/styr off` in the same terminal applies right away.
    let r = d
        .call(Request::ShareLaunch {
            launch_id: l.into(),
            shared: false,
            agent: None,
            cwd: None,
            agent_pid: None,
        })
        .await;
    assert_eq!(r, Response::LaunchShared { sessions: 1 });
    assert_eq!(shared_codex(&d).await, vec![false]);
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codex_placeholder_appears_at_once_and_is_adopted_by_the_real_session() {
    let d = Daemon::start().await;
    let l = "codex-fresh";
    let r = d
        .call(Request::ShareLaunch {
            launch_id: l.into(),
            shared: true,
            agent: Some(AgentKind::Codex),
            cwd: Some("/work/repo".into()),
            agent_pid: None,
        })
        .await;
    assert_eq!(
        r,
        Response::LaunchShared { sessions: 1 },
        "placeholder created now"
    );
    let Response::Sessions { sessions } = d
        .call(Request::ListSessions {
            include_unshared: false,
        })
        .await
    else {
        panic!()
    };
    let placeholder: Vec<_> = sessions
        .iter()
        .filter(|s| s.agent == AgentKind::Codex)
        .collect();
    assert_eq!(placeholder.len(), 1, "on the phone before any prompt");
    let id = placeholder[0].id.clone();
    assert_eq!(
        placeholder[0].cwd.as_deref(),
        Some(std::path::Path::new("/work/repo"))
    );
    // The first prompt makes Codex create its session; its hooks adopt the placeholder.
    for (event, extra) in [
        ("SessionStart", json!({"source": "startup"})),
        (
            "UserPromptSubmit",
            json!({"prompt": "hello from the phone", "turn_id": "t1"}),
        ),
    ] {
        d.call(Request::Hook {
            hook: codex_hook("real-1", event, extra, l),
        })
        .await;
    }
    let Response::Sessions { sessions } = d
        .call(Request::ListSessions {
            include_unshared: true,
        })
        .await
    else {
        panic!()
    };
    let codex: Vec<_> = sessions
        .iter()
        .filter(|s| s.agent == AgentKind::Codex)
        .collect();
    assert_eq!(codex.len(), 1, "one session, not two");
    assert_eq!(
        (
            codex[0].id.clone(),
            codex[0].native_id.as_str(),
            codex[0].shared
        ),
        (id.clone(), "real-1", true)
    );
    let Response::Events { events } = d
        .call(Request::Events {
            session: id,
            since_seq: 0,
            limit: 100,
        })
        .await
    else {
        panic!()
    };
    let names: Vec<_> = events.iter().map(|e| e.kind.name()).collect();
    assert_eq!(
        names.iter().filter(|n| **n == "session_started").count(),
        1,
        "{names:?}"
    );
    assert!(names.contains(&"user_message"));
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codex_hooks_with_a_stale_terminal_id_find_their_terminal_by_folder() {
    let d = Daemon::start().await;
    // `styr codex` in /work/repo, attached and waiting for prompts.
    let c = Client::new(&d.paths.socket).with_timeout(None);
    let req = Request::AttachTerminal {
        launch_id: "live".into(),
        agent: Some(AgentKind::Codex),
        cwd: Some("/work/repo".into()),
        level: styr_core::ipc::WRAPPER_LEVEL,
    };
    let _wrapper = tokio::task::spawn_blocking(move || c.call(&req));
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Codex's background server runs the hooks with another terminal's id.
    for (event, extra) in [
        ("SessionStart", json!({"source": "startup"})),
        ("UserPromptSubmit", json!({"prompt": "hi", "turn_id": "t1"})),
    ] {
        d.call(Request::Hook {
            hook: codex_hook("s9", event, extra, "stale-from-another-terminal"),
        })
        .await;
    }
    let Response::Sessions { sessions } = d
        .call(Request::ListSessions {
            include_unshared: true,
        })
        .await
    else {
        panic!()
    };
    let s = sessions.iter().find(|s| s.native_id == "s9").unwrap();
    assert!(
        s.in_terminal_wrapper,
        "linked to the live wrapper in its folder"
    );
    // Sharing from that terminal now reaches the session.
    let r = d
        .call(Request::ShareLaunch {
            launch_id: "live".into(),
            shared: true,
            agent: None,
            cwd: None,
            agent_pid: None,
        })
        .await;
    assert_eq!(r, Response::LaunchShared { sessions: 1 });
    d.stop().await;
}

/// Agent to agent: a Claude session (terminal "LA") asks another shared session
/// (terminal "LB"); B's terminal gets the labelled message, B answers, and the reply
/// comes back to A. B can't ask A back while A waits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agents_ask_each_other_and_get_the_reply() {
    let d = Daemon::start().await;
    for (s, l) in [("a1", "LA"), ("b1", "LB")] {
        d.call(Request::Hook {
            hook: with_launch(hook(s, "SessionStart", json!({"source": "startup"})), l),
        })
        .await;
        d.call(Request::Hook {
            hook: with_launch(hook(s, "UserPromptSubmit", json!({"prompt": "/styr"})), l),
        })
        .await;
    }
    // B's terminal wrapper is waiting for something to type.
    let wrapper_b = attach(&d, "LB");
    tokio::time::sleep(Duration::from_millis(100)).await;

    // A asks B (as `styr ask b1 …` run inside A's terminal would).
    let c = Client::new(&d.paths.socket).with_timeout(None);
    let ask = tokio::task::spawn_blocking(move || {
        c.call(&Request::Ask {
            launch_id: Some("LA".into()),
            to: "claude".into(), // the only *other* shared Claude session
            text: "what does the parser do with tabs?".into(),
            wait_secs: Some(10),
        })
    });

    let typed = tokio::time::timeout(Duration::from_secs(5), wrapper_b)
        .await
        .unwrap()
        .unwrap();
    let Response::TypePrompt { text } = typed else {
        panic!("{typed:?}")
    };
    assert!(
        text.starts_with("Message from Claude (repo), sent through Styrman:"),
        "{text}"
    );
    assert!(text.contains("what does the parser do with tabs?"));

    // While A waits, B may not ask A back.
    let c = Client::new(&d.paths.socket);
    let back = tokio::task::spawn_blocking(move || {
        c.call(&Request::Ask {
            launch_id: Some("LB".into()),
            to: "claude".into(),
            text: "and you?".into(),
            wait_secs: Some(1),
        })
    })
    .await
    .unwrap();
    let refusal = format!("{back:?}");
    assert!(
        back.is_err() && refusal.contains("waiting for your answer"),
        "{refusal}"
    );

    // B gets it as a prompt and answers.
    d.call(Request::Hook {
        hook: with_launch(
            hook("b1", "UserPromptSubmit", json!({"prompt": text})),
            "LB",
        ),
    })
    .await;
    d.call(Request::Hook {
        hook: with_launch(
            hook(
                "b1",
                "Stop",
                json!({"last_assistant_message": "It expands them to 4 spaces."}),
            ),
            "LB",
        ),
    })
    .await;
    let r = tokio::time::timeout(Duration::from_secs(5), ask)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        r,
        Response::AskReply {
            reply: Some("It expands them to 4 spaces.".into()),
            timed_out: false
        }
    );

    // Both timelines show it: B's message came from agent A; A has notes of the exchange.
    let events = |native: &'static str| {
        let d = &d;
        async move {
            let Response::Sessions { sessions } = d
                .call(Request::ListSessions {
                    include_unshared: false,
                })
                .await
            else {
                panic!()
            };
            let id = sessions
                .into_iter()
                .find(|s| s.native_id == native)
                .unwrap()
                .id;
            let Response::Events { events } = d
                .call(Request::Events {
                    session: id,
                    since_seq: 0,
                    limit: 100,
                })
                .await
            else {
                panic!()
            };
            events
        }
    };
    let b = events("b1").await;
    assert!(b.iter().any(|e| matches!(&e.kind, EventKind::UserMessage { origin: Origin::Remote(dev), .. } if dev.as_str().starts_with("agent:"))));
    let notes: Vec<String> = events("a1")
        .await
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Notice { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("Asked Claude (repo): what does the parser")),
        "{notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|n| n.contains("replied: It expands them to 4 spaces.")),
        "{notes:?}"
    );

    // Peers: A sees B but not itself.
    let Response::Peers { me, sessions } = d
        .call(Request::Peers {
            launch_id: Some("LA".into()),
        })
        .await
    else {
        panic!()
    };
    assert!(me.is_some());
    assert_eq!(
        sessions
            .iter()
            .map(|s| s.native_id.as_str())
            .collect::<Vec<_>>(),
        vec!["b1"]
    );
    d.stop().await;
}
