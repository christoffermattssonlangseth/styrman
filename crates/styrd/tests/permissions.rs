//! The permission round-trip: a blocking `PermissionRequest` hook, answered from
//! the phone, by timeout, or abandoned (answered in the terminal).
//! Own test binary so the short timeout below doesn't affect other tests.

mod common;
use common::*;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};
use styr_core::adapter::RawHook;
use styr_core::event::*;
use styr_core::ids::*;
use styr_core::ipc::{Client, Request, Response};
use styr_core::phone::ServerMsg;

const TIMEOUT_SECS: u64 = 2;

fn init() {
    // SAFETY: set before any daemon (or other thread reading env) starts in this binary.
    unsafe { std::env::set_var("STYR_PERMISSION_TIMEOUT", TIMEOUT_SECS.to_string()) };
}

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

fn permission_hook(session: &str) -> RawHook {
    hook(
        session,
        "PermissionRequest",
        json!({"tool_name": "Bash", "tool_input": {"command": "rm -rf build"},
               "permission_suggestions": [{"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "rm -rf build"}],
                                           "behavior": "allow", "destination": "localSettings"}]}),
    )
}

/// Send a hook the way `styr hook` does, on a blocking thread; returns its stdout JSON.
fn spawn_hook(d: &Daemon, h: RawHook) -> tokio::task::JoinHandle<Option<Value>> {
    let c = Client::new(&d.paths.socket).with_timeout(None);
    tokio::task::spawn_blocking(move || {
        styr_core::ipc::forward_hook(&c, h)
            .expect("hook call")
            .stdout
    })
}

async fn share(d: &Daemon, session: &str) {
    d.call(Request::Hook {
        hook: hook(session, "SessionStart", json!({"source": "startup"})),
    })
    .await;
    let r = d
        .call(Request::Hook {
            hook: hook(session, "UserPromptSubmit", json!({"prompt": "/styr"})),
        })
        .await;
    assert!(matches!(
        r,
        Response::HookReply {
            stdout: Some(_),
            ..
        }
    ));
}

/// Wait for the next `permission_requested` event on the phone; returns its request id.
async fn next_request(ws: &mut Ws) -> String {
    loop {
        if let ServerMsg::Event { event } = recv(ws).await
            && let EventKind::PermissionRequested {
                request, options, ..
            } = event.kind
        {
            assert_eq!(
                options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
                vec!["allow", "always", "deny"]
            );
            return request.to_string();
        }
    }
}

async fn next_resolution(ws: &mut Ws) -> (PermissionOutcome, Resolver) {
    loop {
        if let ServerMsg::Event { event } = recv(ws).await
            && let EventKind::PermissionResolved { outcome, by, .. } = event.kind
        {
            return (outcome, by);
        }
    }
}

async fn phone_on_session(d: &Daemon, session_native: &str) -> (Ws, String) {
    let (url, token) = paired_phone(d).await;
    let mut ws = connect(&url, &token).await.unwrap();
    recv(&mut ws).await; // hello
    let ServerMsg::Sessions { sessions } = recv(&mut ws).await else {
        panic!()
    };
    let s = sessions
        .iter()
        .find(|s| s.native_id == session_native)
        .expect("shared session listed");
    let sid = s.id.to_string();
    send(
        &mut ws,
        json!({"type": "subscribe", "session": sid, "since_seq": s.last_seq}),
    )
    .await;
    loop {
        if let ServerMsg::Subscribed { .. } = recv(&mut ws).await {
            break;
        }
    }
    (ws, sid)
}

/// Skip events and list updates until an error message arrives.
async fn expect_error(ws: &mut Ws) -> String {
    loop {
        match recv(ws).await {
            ServerMsg::Error { message } => return message,
            ServerMsg::Event { .. } | ServerMsg::SessionUpdated { .. } => continue,
            other => panic!("unexpected {other:?}"),
        }
    }
}

async fn answer(ws: &mut Ws, sid: &str, request: &str, option: &str) {
    send(ws, json!({"type": "command", "session": sid,
                    "command": {"type": "resolve_permission", "request": request, "option": option}}))
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phone_allows_once_and_always() {
    init();
    let d = Daemon::start().await;
    share(&d, "s1").await;
    let (mut ws, sid) = phone_on_session(&d, "s1").await;

    // Allow once.
    let pending = spawn_hook(&d, permission_hook("s1"));
    let req = next_request(&mut ws).await;
    answer(&mut ws, &sid, &req, "allow").await;
    let out = pending.await.unwrap().expect("allow reply");
    assert_eq!(
        out,
        json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "allow"}}})
    );
    let (outcome, by) = next_resolution(&mut ws).await;
    assert_eq!(
        outcome,
        PermissionOutcome::Selected {
            option: "allow".into(),
            kind: OptionKind::AllowOnce
        }
    );
    assert!(matches!(by, Resolver::Phone(_)));

    // Always: Claude's own suggestion comes back as updatedPermissions.
    let pending = spawn_hook(&d, permission_hook("s1"));
    let req = next_request(&mut ws).await;
    answer(&mut ws, &sid, &req, "always").await;
    let out = pending.await.unwrap().unwrap();
    let decision = &out["hookSpecificOutput"]["decision"];
    assert_eq!(decision["behavior"], "allow");
    assert_eq!(decision["updatedPermissions"][0]["type"], "addRules");

    // Deny, and a second answer to the same request is refused.
    let pending = spawn_hook(&d, permission_hook("s1"));
    let req = next_request(&mut ws).await;
    answer(&mut ws, &sid, &req, "nonsense").await;
    let msg = expect_error(&mut ws).await;
    assert!(
        msg.contains("not an option"),
        "unknown option is rejected: {msg}"
    );
    answer(&mut ws, &sid, &req, "deny").await;
    let out = pending.await.unwrap().unwrap();
    assert_eq!(out["hookSpecificOutput"]["decision"]["behavior"], "deny");
    answer(&mut ws, &sid, &req, "allow").await;
    let msg = expect_error(&mut ws).await;
    assert!(msg.contains("no longer"), "{msg}");
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_answer_falls_back_to_the_terminal() {
    init();
    let d = Daemon::start().await;
    share(&d, "s2").await;
    let (mut ws, _) = phone_on_session(&d, "s2").await;
    let started = Instant::now();
    let out = spawn_hook(&d, permission_hook("s2")).await.unwrap();
    let waited = started.elapsed();
    assert_eq!(
        out, None,
        "timeout prints nothing: Claude shows its own prompt"
    );
    assert!(
        waited >= Duration::from_secs(TIMEOUT_SECS)
            && waited < Duration::from_secs(TIMEOUT_SECS + 3),
        "{waited:?}"
    );
    next_request(&mut ws).await;
    assert_eq!(
        next_resolution(&mut ws).await,
        (PermissionOutcome::TimedOut, Resolver::Daemon)
    );
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hook_going_away_means_answered_in_the_terminal() {
    init();
    let d = Daemon::start().await;
    share(&d, "s3").await;
    let (mut ws, sid) = phone_on_session(&d, "s3").await;

    // Like a hook process Claude killed: send the request, then hang up.
    let line = serde_json::to_string(&Request::Hook {
        hook: permission_hook("s3"),
    })
    .unwrap();
    let sock = d.paths.socket.clone();
    let stream = tokio::task::spawn_blocking(move || {
        let s = std::os::unix::net::UnixStream::connect(sock).unwrap();
        (&s).write_all(format!("{line}\n").as_bytes()).unwrap();
        s
    })
    .await
    .unwrap();
    let req = next_request(&mut ws).await;
    drop(stream);
    assert_eq!(
        next_resolution(&mut ws).await,
        (PermissionOutcome::Cancelled, Resolver::Terminal)
    );
    // The phone can no longer answer it.
    answer(&mut ws, &sid, &req, "allow").await;
    assert!(expect_error(&mut ws).await.contains("no longer"));
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unshared_sessions_never_wait() {
    init();
    let d = Daemon::start().await;
    d.call(Request::Hook {
        hook: hook("s4", "SessionStart", json!({"source": "startup"})),
    })
    .await;
    let started = Instant::now();
    let out = spawn_hook(&d, permission_hook("s4")).await.unwrap();
    assert_eq!(out, None);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "no waiting without /styr"
    );
    // And the raw IPC answer arrives right away too.
    let sock = d.paths.socket.clone();
    let line = serde_json::to_string(&Request::Hook {
        hook: permission_hook("s4"),
    })
    .unwrap();
    let reply = tokio::task::spawn_blocking(move || {
        let s = std::os::unix::net::UnixStream::connect(sock).unwrap();
        (&s).write_all(format!("{line}\n").as_bytes()).unwrap();
        let mut l = String::new();
        BufReader::new(&s).read_line(&mut l).unwrap();
        l
    })
    .await
    .unwrap();
    assert!(reply.contains("hook_reply"), "{reply}");
    d.stop().await;
}
