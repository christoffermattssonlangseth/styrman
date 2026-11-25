//! Phone API: `GET /v1/health`, `POST /v1/pair`, `GET /v1/ws`.
//!
//! Bound only to loopback and Tailscale addresses (100.64.0.0/10 and
//! fd7a:115c:a1e0::/48 on a tunnel interface), never to 0.0.0.0. Interfaces are
//! rescanned periodically so Tailscale coming up later is picked up.
//! Tailscale encrypts and authenticates the transport; the bearer token
//! authenticates the device. See `styr_core::phone` for the message protocol.

use crate::daemon::{BusMsg, Daemon};
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use styr_core::ids::SessionId;
use styr_core::phone::{ClientMsg, PHONE_PROTOCOL, PairRequest, ServerMsg};
use tokio::sync::{broadcast, watch};

const PAGE: u32 = 500;

/// Resolves once `stop` is true (the watch guard is dropped before returning, keeping futures `Send`).
async fn until_stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|v| *v).await;
}
const HEARTBEAT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// 0 picks a free port per address (tests).
    pub port: u16,
    pub localhost: bool,
    pub tailscale: bool,
    pub rescan: Duration,
}

impl ApiConfig {
    /// `STYR_PORT` (default 4719), `STYR_TAILSCALE=0` to bind loopback only.
    pub fn from_env() -> Self {
        Self {
            port: std::env::var("STYR_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(styr_core::phone::DEFAULT_PORT),
            localhost: true,
            tailscale: std::env::var("STYR_TAILSCALE")
                .map(|v| v != "0")
                .unwrap_or(true),
            rescan: Duration::from_secs(15),
        }
    }

    pub fn loopback_ephemeral() -> Self {
        Self {
            port: 0,
            localhost: true,
            tailscale: false,
            rescan: Duration::from_secs(3600),
        }
    }
}

/// Addresses Tailscale assigned to this machine.
pub fn tailscale_addrs() -> Vec<IpAddr> {
    let Ok(ifs) = if_addrs::get_if_addrs() else {
        return vec![];
    };
    let mut out: Vec<IpAddr> = ifs
        .into_iter()
        .filter(|i| i.name.starts_with("utun") || i.name.starts_with("tailscale"))
        .map(|i| i.ip())
        .filter(|ip| match ip {
            IpAddr::V4(v4) => {
                let o = v4.octets();
                o[0] == 100 && (o[1] & 0b1100_0000) == 64 // 100.64.0.0/10
            }
            IpAddr::V6(v6) => {
                let s = v6.segments();
                s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0 // fd7a:115c:a1e0::/48
            }
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// This machine's MagicDNS name (`host.tailnet.ts.net`), from the Tailscale CLI.
/// The phone connects by this name: iOS App Transport Security can exempt `ts.net`
/// from TLS, but not raw 100.x addresses.
pub fn tailscale_dns_name() -> Option<String> {
    let candidates = [
        "tailscale",
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/usr/local/bin/tailscale",
        "/opt/homebrew/bin/tailscale",
    ];
    for bin in candidates {
        let Ok(out) = std::process::Command::new(bin)
            .args(["status", "--json"])
            .stderr(std::process::Stdio::null())
            .output()
        else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        let name = v["Self"]["DNSName"]
            .as_str()?
            .trim_end_matches('.')
            .to_owned();
        return (name.ends_with(".ts.net") && !name.contains('/')).then_some(name);
    }
    None
}

fn ws_url(addr: SocketAddr) -> String {
    match addr {
        SocketAddr::V4(a) => format!("ws://{a}/v1/ws"),
        SocketAddr::V6(a) => format!("ws://[{}]:{}/v1/ws", a.ip(), a.port()),
    }
}

#[derive(Clone)]
struct AppState {
    daemon: Arc<Daemon>,
    stop: watch::Receiver<bool>,
}

pub fn router(daemon: Arc<Daemon>, stop: watch::Receiver<bool>) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/pair", post(pair))
        .route("/v1/ws", get(ws))
        .with_state(AppState { daemon, stop })
}

/// Keep listeners in sync with the wanted addresses until `stop` flips.
pub async fn run_listeners(daemon: Arc<Daemon>, cfg: ApiConfig, mut stop: watch::Receiver<bool>) {
    let app = router(daemon.clone(), stop.clone());
    let mut bound: HashMap<IpAddr, (SocketAddr, tokio::task::JoinHandle<()>)> = HashMap::new();
    let mut warned: HashSet<IpAddr> = HashSet::new();
    let mut dns_name: Option<String> = None;
    let mut rounds: u32 = 0;
    loop {
        let mut want: Vec<IpAddr> = Vec::new();
        if cfg.localhost {
            want.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
        }
        if cfg.tailscale {
            want.extend(tailscale_addrs());
        }
        for ip in &want {
            if bound.contains_key(ip) {
                continue;
            }
            match tokio::net::TcpListener::bind((*ip, cfg.port)).await {
                Ok(listener) => {
                    let addr = listener.local_addr().expect("local addr");
                    let mut s = stop.clone();
                    let app = app.clone();
                    let task = tokio::spawn(async move {
                        let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
                            let _ = s.wait_for(|v| *v).await;
                        });
                        if let Err(e) = serve.await {
                            tracing::warn!(%addr, "phone API listener stopped: {e}");
                        }
                    });
                    tracing::info!(url = %ws_url(addr), "phone API listening");
                    warned.remove(ip);
                    bound.insert(*ip, (addr, task));
                }
                Err(e) => {
                    if warned.insert(*ip) {
                        tracing::warn!(%ip, port = cfg.port, "cannot bind phone API: {e}");
                    }
                }
            }
        }
        bound.retain(|ip, (addr, task)| {
            let keep = want.contains(ip);
            if !keep {
                tracing::info!(%addr, "address gone; phone API listener closed");
                task.abort();
            }
            keep
        });
        let mut addrs: Vec<SocketAddr> = bound.values().map(|(a, _)| *a).collect();
        addrs.sort_by_key(|a| (a.ip().is_loopback(), a.is_ipv6(), *a)); // Tailscale v4 first
        let ts_v4 = addrs
            .iter()
            .find(|a| a.is_ipv4() && !a.ip().is_loopback())
            .copied();
        if ts_v4.is_some() && (dns_name.is_none() || rounds.is_multiple_of(20)) {
            dns_name = tokio::task::spawn_blocking(tailscale_dns_name)
                .await
                .ok()
                .flatten();
        }
        rounds = rounds.wrapping_add(1);
        let mut urls: Vec<String> = Vec::new();
        if let (Some(name), Some(a)) = (&dns_name, ts_v4) {
            urls.push(format!("ws://{name}:{}/v1/ws", a.port()));
        }
        urls.extend(addrs.into_iter().map(ws_url));
        daemon.set_api_urls(urls);

        tokio::select! {
            _ = until_stopped(&mut stop) => break,
            _ = tokio::time::sleep(cfg.rescan) => {}
        }
    }
    for (_, (_, task)) in bound {
        if tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .is_err()
        {
            tracing::warn!("phone API listener did not stop in time");
        }
    }
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"ok": true, "service": "styrd", "protocol": PHONE_PROTOCOL}))
}

fn error(status: StatusCode, msg: &str) -> Response {
    (status, Json(serde_json::json!({"error": msg}))).into_response()
}

async fn pair(State(st): State<AppState>, Json(req): Json<PairRequest>) -> Response {
    match st.daemon.pair(&req.code, &req.device_name) {
        Ok(Some(resp)) => Json(resp).into_response(),
        Ok(None) => error(StatusCode::UNAUTHORIZED, "invalid or expired pairing code"),
        Err(e) => {
            tracing::error!("pairing failed: {e:#}");
            error(StatusCode::INTERNAL_SERVER_ERROR, "pairing failed")
        }
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

async fn ws(State(st): State<AppState>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    let Some(device) = bearer(&headers).and_then(|t| st.daemon.authenticate(t)) else {
        return error(StatusCode::UNAUTHORIZED, "missing or invalid device token");
    };
    upgrade
        .max_message_size(1 << 20)
        .on_upgrade(move |socket| async move {
            tracing::info!(%device, "phone connected");
            if let Err(e) = Connection::new(st, device.clone()).run(socket).await {
                tracing::debug!(%device, "phone connection ended: {e:#}");
            }
            tracing::info!(%device, "phone disconnected");
        })
}

struct Connection {
    daemon: Arc<Daemon>,
    stop: watch::Receiver<bool>,
    device: String,
    /// Subscribed sessions → last seq delivered.
    subs: HashMap<SessionId, u64>,
    /// Sessions the client has been told about (so removals are only sent for those).
    known: HashSet<SessionId>,
}

type Tx = futures_util::stream::SplitSink<WebSocket, Message>;

async fn send(tx: &mut Tx, msg: &ServerMsg) -> anyhow::Result<()> {
    tx.send(Message::Text(serde_json::to_string(msg)?.into()))
        .await?;
    Ok(())
}

impl Connection {
    fn new(st: AppState, device: String) -> Self {
        Self {
            daemon: st.daemon,
            stop: st.stop,
            device,
            subs: HashMap::new(),
            known: HashSet::new(),
        }
    }

    async fn run(mut self, socket: WebSocket) -> anyhow::Result<()> {
        // Subscribe before reading any state so nothing falls between snapshot and stream.
        let mut bus = self.daemon.subscribe();
        let mut revoked = self.daemon.revoked.subscribe();
        self.daemon.device_connected(&self.device, true);
        struct Live(Arc<Daemon>, String);
        impl Drop for Live {
            fn drop(&mut self) {
                self.0.device_connected(&self.1, false);
            }
        }
        let _live = Live(self.daemon.clone(), self.device.clone());
        let (mut tx, mut rx) = socket.split();
        send(
            &mut tx,
            &ServerMsg::Hello {
                protocol: PHONE_PROTOCOL,
                daemon_version: styr_core::VERSION.into(),
                device_id: self.device.clone(),
                host: Daemon::host_name(),
            },
        )
        .await?;
        self.send_sessions(&mut tx).await?;
        let mut beat = tokio::time::interval(HEARTBEAT);
        beat.tick().await;
        let mut stop = self.stop.clone();
        loop {
            tokio::select! {
                _ = until_stopped(&mut stop) => {
                    let _ = tx.send(Message::Close(None)).await;
                    return Ok(());
                }
                Ok(id) = revoked.recv() => {
                    if id == self.device {
                        tracing::info!(device = %self.device, "device revoked; closing");
                        let _ = tx.send(Message::Close(None)).await;
                        return Ok(());
                    }
                }
                msg = rx.next() => match msg {
                    Some(Ok(Message::Text(t))) => match serde_json::from_str::<ClientMsg>(t.as_str()) {
                        Ok(m) => self.on_client(&mut tx, m).await?,
                        Err(e) => send(&mut tx, &ServerMsg::Error { message: format!("bad message: {e}") }).await?,
                    },
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(_)) => {} // ping/pong/binary
                    Some(Err(e)) => return Err(e.into()),
                },
                b = bus.recv() => match b {
                    Ok(BusMsg::Event(ev)) => {
                        if let Some(last) = self.subs.get_mut(&ev.session)
                            && ev.seq > *last {
                                *last = ev.seq;
                                send(&mut tx, &ServerMsg::Event { event: ev }).await?;
                            }
                    }
                    Ok(BusMsg::SessionChanged(id)) => self.on_session_changed(&mut tx, id).await?,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(device = %self.device, "phone lagged {n} messages; resyncing");
                        self.send_sessions(&mut tx).await?;
                        let subs: Vec<_> = self.subs.keys().cloned().collect();
                        for s in subs {
                            self.backfill(&mut tx, &s).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
                _ = beat.tick() => tx.send(Message::Ping(Default::default())).await?,
            }
        }
    }

    async fn send_sessions(&mut self, tx: &mut Tx) -> anyhow::Result<()> {
        let sessions = self.daemon.summaries(false)?;
        self.known = sessions.iter().map(|s| s.id.clone()).collect();
        send(tx, &ServerMsg::Sessions { sessions }).await
    }

    async fn on_session_changed(&mut self, tx: &mut Tx, id: SessionId) -> anyhow::Result<()> {
        match self.daemon.summary(&id)? {
            Some(s) if s.shared => {
                self.known.insert(id);
                send(tx, &ServerMsg::SessionUpdated { session: s }).await
            }
            _ => {
                self.subs.remove(&id);
                if self.known.remove(&id) {
                    send(tx, &ServerMsg::SessionRemoved { id }).await?;
                }
                Ok(())
            }
        }
    }

    /// Send everything after the last delivered seq, in pages.
    async fn backfill(&mut self, tx: &mut Tx, session: &SessionId) -> anyhow::Result<()> {
        let mut last = self.subs.get(session).copied().unwrap_or(0);
        loop {
            let events = self.daemon.events(session, last, PAGE)?;
            let n = events.len();
            if let Some(e) = events.last() {
                last = e.seq;
            }
            if n > 0 {
                send(
                    tx,
                    &ServerMsg::Events {
                        session: session.clone(),
                        events,
                    },
                )
                .await?;
            }
            if n < PAGE as usize {
                break;
            }
        }
        self.subs.insert(session.clone(), last);
        Ok(())
    }

    async fn on_client(&mut self, tx: &mut Tx, msg: ClientMsg) -> anyhow::Result<()> {
        match msg {
            ClientMsg::ListSessions => self.send_sessions(tx).await,
            ClientMsg::Subscribe { session, since_seq } => {
                let visible = self.daemon.summary(&session)?.is_some_and(|s| s.shared);
                if !visible {
                    return send(
                        tx,
                        &ServerMsg::Error {
                            message: format!("unknown session {session}"),
                        },
                    )
                    .await;
                }
                self.subs.insert(session.clone(), since_seq);
                self.backfill(tx, &session).await?;
                let last_seq = self.subs[&session];
                send(tx, &ServerMsg::Subscribed { session, last_seq }).await
            }
            ClientMsg::RegisterPush { token, env } => {
                let valid = (32..=200).contains(&token.len())
                    && token.chars().all(|c| c.is_ascii_hexdigit())
                    && (env == "sandbox" || env == "production");
                if !valid {
                    return send(
                        tx,
                        &ServerMsg::Error {
                            message: "invalid push token".into(),
                        },
                    )
                    .await;
                }
                self.daemon.set_push_token(
                    &self.device,
                    Some(&token.to_ascii_lowercase()),
                    Some(&env),
                )?;
                tracing::info!(device = %self.device, %env, "push token registered");
                Ok(())
            }
            ClientMsg::Unsubscribe { session } => {
                self.subs.remove(&session);
                Ok(())
            }
            ClientMsg::Command {
                session,
                command: styr_core::Command::ResolvePermission { request, option },
            } => {
                let visible = self.daemon.summary(&session)?.is_some_and(|s| s.shared);
                let result = if visible {
                    self.daemon
                        .broker
                        .resolve(&session, &request, &option, self.device.as_str().into())
                        .map_err(|e| e.to_string())
                } else {
                    Err(format!("unknown session {session}"))
                };
                match result {
                    Ok(()) => {
                        tracing::info!(device = %self.device, %request, %option, "permission answered from the phone");
                        Ok(())
                    }
                    Err(message) => send(tx, &ServerMsg::Error { message }).await,
                }
            }
            ClientMsg::Command {
                session,
                command: styr_core::Command::SendPrompt { text },
            } => match self
                .daemon
                .send_prompt(&session, &text, self.device.as_str().into())
            {
                Ok(crate::prompts::Delivery::Delivered) => Ok(()),
                Ok(crate::prompts::Delivery::Queued(position)) => {
                    send(
                        tx,
                        &ServerMsg::PromptQueued {
                            session,
                            text: text.trim().to_owned(),
                            position: position as u32,
                        },
                    )
                    .await
                }
                Err(e) => {
                    send(
                        tx,
                        &ServerMsg::Error {
                            message: format!("{e:#}"),
                        },
                    )
                    .await
                }
            },
            ClientMsg::Command {
                session,
                command: styr_core::Command::SetSharing { shared: false },
            } => {
                // "Remove" on the phone: stop sharing; the session itself keeps running.
                match self.daemon.summary(&session)? {
                    Some(s) if s.shared => {
                        let native = styr_core::ids::NativeSessionRef::new(s.agent, s.native_id);
                        self.daemon.set_sharing(&native, false)?;
                        Ok(())
                    }
                    _ => {
                        send(
                            tx,
                            &ServerMsg::Error {
                                message: format!("unknown session {session}"),
                            },
                        )
                        .await
                    }
                }
            }
            ClientMsg::Command { .. } => {
                send(
                    tx,
                    &ServerMsg::Error {
                        message: "commands from the phone arrive in a later version".into(),
                    },
                )
                .await
            }
        }
    }
}
