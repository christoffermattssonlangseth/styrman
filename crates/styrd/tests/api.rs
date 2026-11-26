//! The phone API over real TCP + WebSocket: pairing, auth, snapshot, backfill,
//! live events, list updates, unsharing and revocation.

mod common;
use common::*;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::time::Duration;
use styr_core::event::*;
use styr_core::ids::*;
use styr_core::ipc::{Request, Response};
use styr_core::phone::{PairResponse, ServerMsg};
use tokio_tungstenite::tungstenite;

fn user(text: &str) -> EventKind {
    EventKind::UserMessage {
        text: text.into(),
        origin: Origin::Terminal,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phone_api_end_to_end() {
    let d = Daemon::start().await;
    let (code, host, urls) = d.pairing().await;
    let ws_url = urls[0].clone();
    assert!(ws_url.starts_with("ws://127.0.0.1:"), "{ws_url}");
    let base = http_base(&ws_url);

    // Health needs no token.
    let h: Value = reqwest::get(format!("{base}/v1/health"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(h["ok"], true);

    // Wrong code → 401; the right one works exactly once.
    assert_eq!(pair(&d, &base, "WRONGCODE1").await.status(), 401);
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/pair"))
        .json(&json!({"code": code.to_lowercase(), "device_name": "Test iPhone"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let paired: PairResponse = resp.json().await.unwrap();
    assert_eq!(paired.host, host);
    assert!(paired.token.starts_with("sd1."));
    assert_eq!(
        pair(&d, &base, &code).await.status(),
        401,
        "codes are one-time"
    );

    // No token / bad token → refused before the upgrade.
    assert!(connect(&ws_url, "sd1.nope.nope").await.is_err());
    let forged = format!("{}x", paired.token);
    assert!(connect(&ws_url, &forged).await.is_err());

    // A shared session with some history, and an unshared one.
    d.call(Request::SetSharing {
        native: NativeSessionRef::new(AgentKind::Claude, "s1"),
        shared: true,
    })
    .await;
    d.ingest("s1", vec![user("one"), user("two")]).await;
    d.ingest("hidden", vec![user("private")]).await;

    let mut ws = connect(&ws_url, &paired.token)
        .await
        .expect("connect with token");
    let ServerMsg::Hello {
        device_id,
        protocol,
        usage_supported,
        ..
    } = recv(&mut ws).await
    else {
        panic!()
    };
    assert_eq!(
        (device_id.as_str(), protocol),
        (paired.device_id.as_str(), 1)
    );
    let ServerMsg::Sessions { sessions } = recv(&mut ws).await else {
        panic!()
    };
    assert_eq!(sessions.len(), 1, "unshared sessions are invisible");
    let sid = sessions[0].id.clone();

    assert!(usage_supported);
    send(&mut ws, json!({"type": "get_usage"})).await;
    assert!(matches!(recv(&mut ws).await, ServerMsg::Usage { .. }));

    let http = reqwest::Client::new();
    let live_url = format!("{base}/v1/live");
    assert_eq!(http.get(&live_url).send().await.unwrap().status(), 401);
    let snapshot: serde_json::Value = http
        .get(&live_url)
        .bearer_auth(&paired.token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        snapshot["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| { row["session"] == sid.as_str() && row.get("line").is_none() })
    );

    // A session in a few words, for the notification extension.
    let brief_url = format!("{base}/v1/brief/{sid}");
    assert_eq!(http.get(&brief_url).send().await.unwrap().status(), 401);
    let brief: serde_json::Value = http
        .get(&brief_url)
        .bearer_auth(&paired.token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        brief["agent"].is_string() && brief["repo"].is_string(),
        "{brief}"
    );
    assert!(brief["status"].is_string(), "{brief}");
    let missing = http
        .get(format!("{base}/v1/brief/nope"))
        .bearer_auth(&paired.token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);

    // Unknown/unshared sessions cannot be subscribed to.
    send(&mut ws, json!({"type": "subscribe", "session": "nope"})).await;
    assert!(matches!(recv(&mut ws).await, ServerMsg::Error { .. }));

    // Subscribe from seq 1: backfill the rest, then live events.
    send(
        &mut ws,
        json!({"type": "subscribe", "session": sid, "since_seq": 1}),
    )
    .await;
    let ServerMsg::Events { events, .. } = recv(&mut ws).await else {
        panic!()
    };
    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2, 3]);
    let ServerMsg::Subscribed { last_seq, .. } = recv(&mut ws).await else {
        panic!()
    };
    assert_eq!(last_seq, 3);

    d.ingest("s1", vec![user("three")]).await;
    let mut got_event = false;
    let mut got_update = false;
    while !(got_event && got_update) {
        match recv(&mut ws).await {
            ServerMsg::Event { event } => {
                assert_eq!(event.seq, 4);
                assert_eq!(event.kind, user("three"));
                got_event = true;
            }
            ServerMsg::SessionUpdated { session } => {
                assert_eq!(session.id, sid);
                got_update = true;
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    // Activity in an unshared session leaks nothing.
    d.ingest("hidden", vec![user("still private")]).await;
    // Unsharing removes the session from the phone.
    d.call(Request::SetSharing {
        native: NativeSessionRef::new(AgentKind::Claude, "s1"),
        shared: false,
    })
    .await;
    loop {
        match recv(&mut ws).await {
            ServerMsg::SessionRemoved { id } => {
                assert_eq!(id, sid);
                break;
            }
            ServerMsg::SessionUpdated { session } => panic!("leaked {session:?}"),
            ServerMsg::Event { event } => panic!("leaked {event:?}"),
            _ => {}
        }
    }

    // Revoking the device closes its live connection and kills the token.
    let suffix = &paired.device_id;
    assert!(matches!(
        d.call(Request::RevokeDevice { id: suffix.clone() }).await,
        Response::Ok
    ));
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(tungstenite::Message::Close(_))) => return,
                _ => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "revoked connection must close");
    assert!(connect(&ws_url, &paired.token).await.is_err());

    match d.call(Request::ListDevices).await {
        Response::Devices { devices } => {
            assert_eq!(devices.len(), 1);
            assert!(devices[0].revoked);
            assert!(devices[0].last_seen_ms.is_some());
        }
        other => panic!("{other:?}"),
    }
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_closes_open_phone_connections() {
    let d = Daemon::start().await;
    let (code, _, urls) = d.pairing().await;
    let base = http_base(&urls[0]);
    let paired: PairResponse = reqwest::Client::new()
        .post(format!("{base}/v1/pair"))
        .json(&json!({"code": code, "device_name": "x"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut ws = connect(&urls[0], &paired.token).await.unwrap();
    recv(&mut ws).await;
    // Must not hang on the open WebSocket.
    tokio::time::timeout(Duration::from_secs(10), d.stop())
        .await
        .expect("daemon stopped with a phone connected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browsers_and_token_guessing_are_refused() {
    let d = Daemon::start().await;
    let (code, _, urls) = d.pairing().await;
    let base = http_base(&urls[0]);
    let http = reqwest::Client::new();

    // Anything carrying an Origin header (a web page) is refused, even with a valid code.
    let r = http
        .post(format!("{base}/v1/pair"))
        .header("origin", "https://evil.example")
        .json(&json!({"code": code, "device_name": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let r = http
        .get(format!("{base}/v1/health"))
        .header("origin", "http://localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    // The code was not consumed by the refused request.
    let (_, token) = (
        (),
        http.post(format!("{base}/v1/pair"))
            .json(&json!({"code": code, "device_name": "x"}))
            .send()
            .await
            .unwrap()
            .json::<PairResponse>()
            .await
            .unwrap()
            .token,
    );
    let mut req =
        tungstenite::client::IntoClientRequest::into_client_request(urls[0].as_str()).unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    req.headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    assert!(
        tokio_tungstenite::connect_async(req).await.is_err(),
        "browser WebSocket refused"
    );

    // Oversized pairing bodies are refused.
    let big = "x".repeat(64 * 1024);
    let r = http
        .post(format!("{base}/v1/pair"))
        .json(&json!({"code": big, "device_name": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);

    // Guessing tokens: after 20 failures even the right token is refused for a minute.
    for _ in 0..20 {
        assert!(connect(&urls[0], "sd1.guess.guess").await.is_err());
    }
    let mut req =
        tungstenite::client::IntoClientRequest::into_client_request(urls[0].as_str()).unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    match tokio_tungstenite::connect_async(req).await {
        Err(tungstenite::Error::Http(resp)) => assert_eq!(resp.status(), 429),
        other => panic!("expected 429, got {:?}", other.map(|_| ())),
    }
    d.stop().await;
}

/// A menu read off a Codex terminal reaches a phone that is already watching the
/// session (live, not only after reopening it), and the phone's answer goes back
/// to the terminal as a menu selection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn screen_permission_reaches_a_watching_phone_live() {
    let d = Daemon::start().await;
    let (code, _host, urls) = d.pairing().await;
    let ws_url = urls[0].clone();
    let base = http_base(&ws_url);
    let paired: PairResponse = pair(&d, &base, &code).await.json().await.unwrap();

    // A shared Codex session running in `styr codex` (terminal "L").
    let hook = |event: &str, extra: Value| {
        let mut payload = json!({"session_id": "cx1", "cwd": "/work/repo", "hook_event_name": event,
            "model": "m", "permission_mode": "default", "transcript_path": null});
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        styr_core::adapter::RawHook {
            agent: AgentKind::Codex,
            event: event.into(),
            payload,
            launched_by_styr: true,
            agent_pid: None,
            launch_id: Some("L".into()),
            agent_version: None,
        }
    };
    d.call(Request::Hook {
        hook: hook("SessionStart", json!({"source": "startup"})),
    })
    .await;
    d.call(Request::ShareLaunch {
        launch_id: "L".into(),
        shared: true,
        agent: None,
        cwd: None,
        agent_pid: None,
    })
    .await;
    d.call(Request::Hook {
        hook: hook(
            "PreToolUse",
            json!({"turn_id": "t", "tool_name": "Bash", "tool_input": {"command": "touch x"}, "tool_use_id": "c1"}),
        ),
    })
    .await;
    // The wrapper is attached and waiting for something to do.
    let sock = d.paths.socket.clone();
    let wrapper = tokio::task::spawn_blocking(move || {
        styr_core::ipc::Client::new(sock)
            .with_timeout(None)
            .call(&Request::AttachTerminal {
                launch_id: "L".into(),
                agent: Some(AgentKind::Codex),
                cwd: Some("/work/repo".into()),
            })
    });

    // The phone opens the session and watches it.
    let mut ws = connect(&ws_url, &paired.token).await.unwrap();
    let ServerMsg::Hello { .. } = recv(&mut ws).await else {
        panic!()
    };
    let ServerMsg::Sessions { sessions } = recv(&mut ws).await else {
        panic!()
    };
    let sid = sessions
        .iter()
        .find(|s| s.native_id == "cx1")
        .unwrap()
        .id
        .clone();
    send(
        &mut ws,
        json!({"type": "subscribe", "session": sid, "since_seq": 0}),
    )
    .await;
    let ServerMsg::Events { .. } = recv(&mut ws).await else {
        panic!()
    };
    let ServerMsg::Subscribed { .. } = recv(&mut ws).await else {
        panic!()
    };

    // Codex shows its approval menu; the wrapper reports it.
    let prompt = styr_core::ipc::TerminalPrompt {
        title: "Would you like to run the following command?".into(),
        detail: "$ touch x".into(),
        options: vec![
            "Yes, proceed".into(),
            "No, and tell Codex what to do differently".into(),
        ],
        selected: 0,
    };
    d.call(Request::TerminalPrompt {
        launch_id: "L".into(),
        prompt: Some(prompt),
    })
    .await;

    // Live on the phone, without reopening the session.
    let request = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ServerMsg::Event { event } = recv(&mut ws).await
                && let EventKind::PermissionRequested {
                    request, options, ..
                } = event.kind
            {
                assert_eq!(options.len(), 2);
                return request;
            }
        }
    })
    .await
    .expect("the card arrives live");

    // Answering on the phone selects the option in the terminal.
    send(
        &mut ws,
        json!({"type": "command", "session": sid, "command": {"type": "resolve_permission", "request": request, "option": "opt-0"}}),
    )
    .await;
    let r = tokio::time::timeout(Duration::from_secs(5), wrapper)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(r, Response::SelectOption { index: 0 });
    d.stop().await;
}
