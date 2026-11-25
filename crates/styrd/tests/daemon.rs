//! End-to-end over the real Unix socket: start styrd in-process, talk to it with
//! the same sync client `styr` uses, restart it, and check what survives.

use serde_json::json;
use std::time::Duration;
use styr_core::SessionStatus;
use styr_core::event::*;
use styr_core::ids::*;
use styr_core::ipc::{Client, IpcError, Request, Response, SessionUpdate};
use styr_core::paths::Paths;
use tokio::sync::oneshot;

struct Running {
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn start(paths: &Paths) -> Running {
    let (stop, rx) = oneshot::channel::<()>();
    let p = paths.clone();
    let task = tokio::spawn(async move {
        styrd::run_with(p, styrd::api::ApiConfig::loopback_ephemeral(), async {
            let _ = rx.await;
        })
        .await
    });
    for _ in 0..200 {
        if paths.socket.exists() {
            return Running { stop, task };
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("styrd did not start");
}

impl Running {
    async fn stop(self) {
        let _ = self.stop.send(());
        self.task.await.unwrap().unwrap();
    }
}

async fn call(paths: &Paths, req: Request) -> Result<Response, IpcError> {
    let c = Client::new(&paths.socket);
    tokio::task::spawn_blocking(move || c.call(&req))
        .await
        .unwrap()
}

fn native(id: &str) -> NativeSessionRef {
    NativeSessionRef::new(AgentKind::Claude, id)
}

fn update(id: &str) -> SessionUpdate {
    SessionUpdate {
        native: Some(native(id)),
        cwd: Some("/work/repo".into()),
        ..Default::default()
    }
}

fn started(id: &str) -> EventKind {
    EventKind::SessionStarted {
        agent: AgentKind::Claude,
        native: native(id),
        cwd: Some("/work/repo".into()),
        workspace_roots: vec![],
        model: None,
        title: None,
        start: StartKind::New,
    }
}

fn permission(req: &str) -> EventKind {
    EventKind::PermissionRequested {
        request: req.into(),
        call: Some("toolu_1".into()),
        tool: ToolInfo {
            name: "Bash".into(),
            kind: ToolKind::Execute,
            title: "Run `rm -rf build`".into(),
            input: json!({"command": "rm -rf build"}),
            locations: vec![],
        },
        reason: None,
        options: vec![],
        expires_at_ms: None,
    }
}

async fn ingest(paths: &Paths, id: &str, events: Vec<EventKind>) -> (SessionId, Vec<u64>) {
    match call(
        paths,
        Request::Ingest {
            session: update(id),
            source: EventSource::Hook,
            events,
        },
    )
    .await
    .unwrap()
    {
        Response::Ingested { session, seqs } => (session, seqs),
        other => panic!("unexpected {other:?}"),
    }
}

async fn sessions(paths: &Paths, include_unshared: bool) -> Vec<styr_core::ipc::SessionSummary> {
    match call(paths, Request::ListSessions { include_unshared })
        .await
        .unwrap()
    {
        Response::Sessions { sessions } => sessions,
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn hello_and_fail_open_when_down() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    // Nothing running: the client must report NotRunning (hooks turn this into a no-op).
    let err = call(&paths, Request::Hello).await.unwrap_err();
    assert!(err.is_not_running(), "{err:?}");

    let d = start(&paths).await;
    match call(&paths, Request::Hello).await.unwrap() {
        Response::Hello { protocol, pid, .. } => {
            assert_eq!(protocol, styr_core::ipc::PROTOCOL_VERSION);
            assert_eq!(pid, std::process::id());
        }
        other => panic!("unexpected {other:?}"),
    }
    // Garbage in → an error response, not a crash.
    // (blocking I/O off the runtime thread, or the in-process daemon can't answer)
    let sock = paths.socket.clone();
    let line = tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, Write};
        let raw = std::os::unix::net::UnixStream::connect(sock).unwrap();
        (&raw).write_all(b"not json\n").unwrap();
        let mut line = String::new();
        std::io::BufReader::new(&raw).read_line(&mut line).unwrap();
        line
    })
    .await
    .unwrap();
    assert!(line.contains("\"result\":\"error\""), "{line}");
    d.stop().await;
    assert!(!paths.socket.exists(), "socket removed on shutdown");
}

#[tokio::test]
async fn second_instance_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    let d = start(&paths).await;
    let err = styrd::run_with(
        paths.clone(),
        styrd::api::ApiConfig::loopback_ephemeral(),
        async {},
    )
    .await
    .unwrap_err();
    assert!(format!("{err}").contains("already running"), "{err}");
    // …and the first one still answers.
    assert!(matches!(
        call(&paths, Request::Hello).await.unwrap(),
        Response::Hello { .. }
    ));
    d.stop().await;
}

#[tokio::test]
async fn unshared_sessions_keep_only_lifecycle_and_are_hidden() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    let d = start(&paths).await;

    let user = EventKind::UserMessage {
        text: "secret prompt".into(),
        origin: Origin::Terminal,
    };
    let (sid, seqs) = ingest(&paths, "s1", vec![started("s1"), user.clone()]).await;
    assert_eq!(
        seqs,
        vec![1],
        "only SessionStarted is persisted before sharing"
    );
    assert!(
        sessions(&paths, false).await.is_empty(),
        "hidden from the phone list"
    );
    let all = sessions(&paths, true).await;
    assert_eq!(all.len(), 1);
    assert_eq!(
        all[0].status,
        SessionStatus::Working,
        "status still tracked in memory"
    );

    call(
        &paths,
        Request::SetSharing {
            native: native("s1"),
            shared: true,
        },
    )
    .await
    .unwrap();
    call(
        &paths,
        Request::SetSharing {
            native: native("s1"),
            shared: true,
        },
    )
    .await
    .unwrap(); // idempotent
    let (_, seqs) = ingest(&paths, "s1", vec![user]).await;
    assert_eq!(seqs, vec![3]);
    let listed = sessions(&paths, false).await;
    assert_eq!(listed.len(), 1);
    assert!(listed[0].shared);

    let evs = match call(
        &paths,
        Request::Events {
            session: sid,
            since_seq: 0,
            limit: 100,
        },
    )
    .await
    .unwrap()
    {
        Response::Events { events } => events,
        other => panic!("unexpected {other:?}"),
    };
    let names: Vec<_> = evs.iter().map(|e| e.kind.name()).collect();
    assert_eq!(
        names,
        vec!["session_started", "sharing_changed", "user_message"]
    );
    d.stop().await;
}

#[tokio::test]
async fn restart_keeps_history_and_cancels_open_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    let d = start(&paths).await;
    call(
        &paths,
        Request::SetSharing {
            native: native("s1"),
            shared: true,
        },
    )
    .await
    .unwrap();
    let (sid, _) = ingest(
        &paths,
        "s1",
        vec![
            started("s1"),
            EventKind::UserMessage {
                text: "clean up".into(),
                origin: Origin::Terminal,
            },
            permission("r1"),
        ],
    )
    .await;
    assert_eq!(
        sessions(&paths, false).await[0].status,
        SessionStatus::NeedsApproval
    );
    d.stop().await;

    let d = start(&paths).await;
    let s = &sessions(&paths, false).await[0];
    assert_eq!(s.id, sid, "same session id after restart");
    assert_eq!(
        s.status,
        SessionStatus::Working,
        "dangling request no longer blocks"
    );
    let evs = match call(
        &paths,
        Request::Events {
            session: sid.clone(),
            since_seq: 0,
            limit: 100,
        },
    )
    .await
    .unwrap()
    {
        Response::Events { events } => events,
        other => panic!("unexpected {other:?}"),
    };
    let last = evs.last().unwrap();
    assert_eq!(last.source, EventSource::Daemon);
    assert_eq!(
        last.kind,
        EventKind::PermissionResolved {
            request: "r1".into(),
            outcome: PermissionOutcome::Cancelled,
            by: Resolver::Daemon
        }
    );
    // Seqs stay contiguous across the restart.
    assert_eq!(
        evs.iter().map(|e| e.seq).collect::<Vec<_>>(),
        (1..=evs.len() as u64).collect::<Vec<_>>()
    );
    d.stop().await;
}
