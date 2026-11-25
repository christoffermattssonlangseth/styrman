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
    let c = Client::new(&d.paths.socket).with_timeout(None);
    let req = Request::AttachTerminal {
        launch_id: launch.into(),
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
