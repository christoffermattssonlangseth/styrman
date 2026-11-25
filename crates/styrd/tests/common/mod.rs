//! Shared helpers for the API tests: an in-process daemon, pairing, a WS client.
#![allow(dead_code)]

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use styr_core::event::*;
use styr_core::ids::*;
use styr_core::ipc::{Client, Request, Response, SessionUpdate};
use styr_core::paths::Paths;
use styr_core::phone::{PairResponse, ServerMsg};
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};

pub struct Daemon {
    pub paths: Paths,
    _dir: tempfile::TempDir,
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
}

impl Daemon {
    pub async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path());
        let (stop, rx) = oneshot::channel::<()>();
        let p = paths.clone();
        let task = tokio::spawn(async move {
            styrd::run_with(p, styrd::api::ApiConfig::loopback_ephemeral(), async {
                let _ = rx.await;
            })
            .await
        });
        let d = Self {
            paths,
            _dir: dir,
            stop: Some(stop),
            task: Some(task),
        };
        for _ in 0..300 {
            if d.paths.socket.exists() && !d.pairing().await.2.is_empty() {
                return d;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("daemon/API did not start");
    }

    pub async fn call(&self, req: Request) -> Response {
        let c = Client::new(&self.paths.socket);
        tokio::task::spawn_blocking(move || c.call(&req))
            .await
            .unwrap()
            .unwrap()
    }

    /// (code, host, urls)
    pub async fn pairing(&self) -> (String, String, Vec<String>) {
        match self.call(Request::CreatePairing).await {
            Response::Pairing {
                code, host, urls, ..
            } => (code, host, urls),
            other => panic!("{other:?}"),
        }
    }

    pub async fn ingest(&self, native: &str, events: Vec<EventKind>) {
        let update = SessionUpdate {
            native: Some(NativeSessionRef::new(AgentKind::Claude, native)),
            cwd: Some("/work/repo".into()),
            ..Default::default()
        };
        self.call(Request::Ingest {
            session: update,
            source: EventSource::Hook,
            events,
        })
        .await;
    }

    pub async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.task.take().unwrap().await.unwrap().unwrap();
    }
}

pub fn http_base(ws_url: &str) -> String {
    ws_url.replace("ws://", "http://").replace("/v1/ws", "")
}

pub async fn pair(_d: &Daemon, base: &str, code: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/v1/pair"))
        .json(&json!({"code": code, "device_name": "Test iPhone"}))
        .send()
        .await
        .unwrap()
}

pub type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub async fn connect(url: &str, token: &str) -> Result<Ws, tungstenite::Error> {
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
}

pub async fn recv(ws: &mut Ws) -> ServerMsg {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out")
            .expect("closed")
            .unwrap();
        if let tungstenite::Message::Text(t) = msg {
            return serde_json::from_str(t.as_str()).unwrap_or_else(|e| panic!("{e}: {t}"));
        }
    }
}

pub async fn send(ws: &mut Ws, v: Value) {
    ws.send(tungstenite::Message::Text(v.to_string().into()))
        .await
        .unwrap();
}

/// Pair a phone and return its token.
pub async fn paired_phone(d: &Daemon) -> (String, String) {
    let (code, _, urls) = d.pairing().await;
    let base = http_base(&urls[0]);
    let r: PairResponse = reqwest::Client::new()
        .post(format!("{base}/v1/pair"))
        .json(&json!({"code": code, "device_name": "Test iPhone"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    (urls[0].clone(), r.token)
}
