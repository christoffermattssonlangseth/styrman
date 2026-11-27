//! Phone API: `GET /v1/health`, `POST /v1/pair`, `GET /v1/ws`.
//!
//! Bound to loopback and Tailscale addresses (100.64.0.0/10 and
//! fd7a:115c:a1e0::/48 on a tunnel interface), never to 0.0.0.0. Interfaces are
//! rescanned periodically so Tailscale coming up later is picked up.
//! Tailscale encrypts and authenticates the transport; the bearer token
//! authenticates the device. As a fallback for when Tailscale is down, the same
//! API (without pairing) is served over TLS on private local-network addresses;
//! see `lan`. See `styr_core::phone` for the message protocol.

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
use styr_core::event::Event;
use styr_core::ids::SessionId;
use styr_core::phone::{ClientMsg, PHONE_PROTOCOL, PairRequest, ServerMsg};
use tokio::sync::{broadcast, watch};

const PAGE: u32 = 500;
/// The most history one message carries: the phone's WebSocket drops a message
/// over 1 MiB (and with it the connection, again on every reconnect).
const PAGE_BYTES: usize = 256 * 1024;

/// `events` in runs of at most about `limit` bytes of JSON (at least one event each).
fn by_size(events: Vec<Event>, limit: usize) -> Vec<Vec<Event>> {
    let mut out: Vec<Vec<Event>> = vec![];
    let mut size = 0;
    for e in events {
        let n = serde_json::to_string(&e).map_or(0, |j| j.len());
        match out.last_mut() {
            Some(run) if size + n <= limit => run.push(e),
            _ => {
                out.push(vec![e]);
                size = 0;
            }
        }
        size += n;
    }
    out
}

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
    /// Also serve TLS on the local network (`lan_port`), for when Tailscale is down.
    pub lan: bool,
    pub lan_port: u16,
    /// Restart Tailscale when it claims to run but this Mac has no Tailscale address.
    pub heal_tailscale: bool,
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
            lan: std::env::var("STYR_LAN").map(|v| v != "0").unwrap_or(true),
            lan_port: std::env::var("STYR_LAN_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(styr_core::phone::LAN_PORT),
            heal_tailscale: std::env::var("STYR_TAILSCALE_HEAL")
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
            lan: false,
            lan_port: 0,
            heal_tailscale: false,
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
    // Reverse DNS of our Tailscale IPv4 through the system resolver (the CLI
    // needs `TERM` set under launchd; see `styr_core::tailscale::command`).
    for ip in tailscale_addrs() {
        if let IpAddr::V4(v4) = ip
            && let Some(name) = reverse_dns(v4)
            && name.ends_with(".ts.net")
        {
            return Some(name);
        }
    }
    tailscale_dns_name_from_cli()
}

fn reverse_dns(ip: Ipv4Addr) -> Option<String> {
    // SAFETY: getnameinfo with a sockaddr_in we build and a buffer we own.
    unsafe {
        let mut sa: libc::sockaddr_in = std::mem::zeroed();
        sa.sin_family = libc::AF_INET as libc::sa_family_t;
        #[cfg(target_os = "macos")]
        {
            sa.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
        }
        sa.sin_addr = libc::in_addr {
            s_addr: u32::from_ne_bytes(ip.octets()),
        };
        let mut host = [0 as libc::c_char; 1025];
        let rc = libc::getnameinfo(
            &sa as *const libc::sockaddr_in as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            host.as_mut_ptr(),
            host.len() as libc::socklen_t,
            std::ptr::null_mut(),
            0,
            libc::NI_NAMEREQD,
        );
        if rc != 0 {
            return None;
        }
        let name = std::ffi::CStr::from_ptr(host.as_ptr())
            .to_string_lossy()
            .trim_end_matches('.')
            .to_owned();
        (!name.is_empty() && !name.contains('/')).then_some(name)
    }
}

fn tailscale_dns_name_from_cli() -> Option<String> {
    let candidates = [
        "tailscale",
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/usr/local/bin/tailscale",
        "/opt/homebrew/bin/tailscale",
    ];
    for bin in candidates {
        let Ok(out) = styr_core::tailscale::command(bin)
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

/// Close listeners whose address is gone.
fn close_gone(
    bound: &mut HashMap<IpAddr, (SocketAddr, tokio::task::JoinHandle<()>)>,
    want: &[IpAddr],
) {
    bound.retain(|ip, (addr, task)| {
        let keep = want.contains(ip);
        if !keep {
            tracing::info!(%addr, "address gone; phone API listener closed");
            task.abort();
        }
        keep
    });
}

/// How long Tailscale may claim to run without giving this Mac an address
/// before it's restarted, and how often at most.
const HEAL_AFTER: Duration = Duration::from_secs(30);
const HEAL_EVERY: Duration = Duration::from_secs(10 * 60);

/// Restarts Tailscale when it says Running but this Mac has no Tailscale
/// address (seen on macOS: the tunnel goes, the app keeps saying Running).
#[derive(Default)]
struct Healer {
    missing_since: Option<std::time::Instant>,
    last_try: Option<std::time::Instant>,
}

impl Healer {
    /// `missing`: no Tailscale address now. `online`: the Mac has a network at all
    /// (with none, Tailscale can't work and a restart would not help).
    fn check(&mut self, missing: bool, online: bool) {
        if !missing {
            self.missing_since = None;
            return;
        }
        let now = std::time::Instant::now();
        let since = *self.missing_since.get_or_insert(now);
        if !should_heal(now, since, self.last_try, online) {
            return;
        }
        self.last_try = Some(now);
        tokio::task::spawn_blocking(heal_tailscale);
    }
}

fn should_heal(
    now: std::time::Instant,
    missing_since: std::time::Instant,
    last_try: Option<std::time::Instant>,
    online: bool,
) -> bool {
    online
        && now.duration_since(missing_since) >= HEAL_AFTER
        && last_try.is_none_or(|t| now.duration_since(t) >= HEAL_EVERY)
}

fn heal_tailscale() {
    use styr_core::tailscale;
    let Some(cli) = tailscale::cli() else { return };
    // Stopped or logged out on purpose: leave it alone.
    match tailscale::state(&cli) {
        Some(st) if st.backend == "Running" => {}
        _ => return,
    }
    tracing::warn!("Tailscale says Running but this Mac has no Tailscale address; restarting it");
    match tailscale::restart(&cli) {
        Ok(()) => tracing::info!("Tailscale restarted"),
        Err(e) => tracing::warn!("could not restart Tailscale: {e}"),
    }
}

fn wss_url(addr: SocketAddr) -> String {
    format!("wss://{addr}/v1/ws")
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
    router_with(daemon, stop, true)
}

/// `pairing`: offer `/v1/pair` (not on the local-network fallback, where the
/// phone has no pinned certificate yet).
pub fn router_with(daemon: Arc<Daemon>, stop: watch::Receiver<bool>, pairing: bool) -> Router {
    let mut r = Router::new().route("/v1/health", get(health));
    if pairing {
        r = r.route(
            "/v1/pair",
            post(pair).layer(axum::extract::DefaultBodyLimit::max(16 * 1024)),
        );
    }
    r.route("/v1/ws", get(ws))
        .route("/v1/live", get(live_snapshot))
        .route("/v1/brief/{session}", get(session_brief))
        .route(
            "/v1/upload/{session}",
            post(upload).layer(axum::extract::DefaultBodyLimit::max(
                crate::uploads::MAX_BYTES,
            )),
        )
        .route(
            "/v1/permission/{session}/{request}",
            get(permission_get)
                .post(permission_answer)
                .layer(axum::extract::DefaultBodyLimit::max(4 * 1024)),
        )
        .layer(axum::middleware::from_fn(reject_browsers))
        .with_state(AppState { daemon, stop })
}

/// The phone app never sends `Origin`; browsers always do on cross-site requests
/// and WebSocket upgrades. Refusing them stops a web page open on this Mac (or a
/// DNS-rebinding page) from talking to the daemon at all.
async fn reject_browsers(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    if req.headers().contains_key(header::ORIGIN) {
        return error(StatusCode::FORBIDDEN, "browsers are not allowed");
    }
    next.run(req).await
}

/// Global brake on bad tokens: after `MAX_AUTH_FAILURES` within a minute,
/// WebSocket auth is refused for the rest of that minute.
pub const MAX_AUTH_FAILURES: u32 = 20;

#[derive(Default)]
pub struct AuthThrottle {
    state: std::sync::Mutex<(Option<std::time::Instant>, u32)>,
}

impl AuthThrottle {
    pub fn blocked(&self) -> bool {
        let g = self.state.lock().unwrap_or_else(|p| p.into_inner());
        matches!(g.0, Some(start) if start.elapsed() < Duration::from_secs(60) && g.1 >= MAX_AUTH_FAILURES)
    }

    pub fn failed(&self) {
        let mut g = self.state.lock().unwrap_or_else(|p| p.into_inner());
        match g.0 {
            Some(start) if start.elapsed() < Duration::from_secs(60) => g.1 += 1,
            _ => *g = (Some(std::time::Instant::now()), 1),
        }
        if g.1 == MAX_AUTH_FAILURES {
            tracing::warn!(
                "{MAX_AUTH_FAILURES} failed phone logins in a minute; refusing logins for a minute"
            );
        }
    }
}

/// Keep listeners in sync with the wanted addresses until `stop` flips.
pub async fn run_listeners(daemon: Arc<Daemon>, cfg: ApiConfig, mut stop: watch::Receiver<bool>) {
    let app = router(daemon.clone(), stop.clone());
    let lan_app = router_with(daemon.clone(), stop.clone(), false);
    let mut bound: HashMap<IpAddr, (SocketAddr, tokio::task::JoinHandle<()>)> = HashMap::new();
    let mut lan_bound: HashMap<IpAddr, (SocketAddr, tokio::task::JoinHandle<()>)> = HashMap::new();
    let mut warned: HashSet<IpAddr> = HashSet::new();
    let mut lan_id: Option<crate::lan::Identity> = None;
    let mut lan_failed = false;
    let mut heal = Healer::default();
    if cfg.tailscale && cfg.heal_tailscale {
        tokio::task::spawn_blocking(|| {
            match styr_core::tailscale::cli()
                .and_then(|c| styr_core::tailscale::state(&c).map(|s| (c, s)))
            {
                Some((cli, st)) => {
                    tracing::info!(cli = %cli.display(), state = %st.backend, "Tailscale CLI usable; will restart Tailscale if its tunnel goes")
                }
                None => tracing::info!(
                    "Tailscale CLI not usable here; Tailscale won't be restarted automatically"
                ),
            }
        });
    }
    let mut dns_name: Option<String> = None;
    let mut rounds: u32 = 0;
    loop {
        let mut want: Vec<IpAddr> = Vec::new();
        if cfg.localhost {
            want.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
        }
        let ts = if cfg.tailscale {
            tailscale_addrs()
        } else {
            vec![]
        };
        want.extend(ts.iter().copied());
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
        close_gone(&mut bound, &want);

        // Local-network fallback (TLS).
        let lan_want = if cfg.lan && !lan_failed {
            crate::lan::lan_addrs()
        } else {
            vec![]
        };
        if lan_id.is_none() && !lan_want.is_empty() {
            match daemon.data_dir_path().map(|d| crate::lan::identity(&d)) {
                Some(Ok(id)) => lan_id = Some(id),
                Some(Err(e)) => {
                    tracing::warn!("local-network fallback off: {e:#}");
                    lan_failed = true;
                }
                None => {}
            }
        }
        if let Some(id) = &lan_id {
            for ip in &lan_want {
                if lan_bound.contains_key(ip) {
                    continue;
                }
                let tcp = match tokio::net::TcpListener::bind((*ip, cfg.lan_port)).await {
                    Ok(t) => t,
                    Err(e) => {
                        if warned.insert(*ip) {
                            tracing::warn!(%ip, port = cfg.lan_port, "cannot bind local-network API: {e}");
                        }
                        continue;
                    }
                };
                let Ok(listener) = crate::lan::TlsListener::new(tcp, id.acceptor.clone()) else {
                    continue;
                };
                let addr = axum::serve::Listener::local_addr(&listener).expect("local addr");
                let mut s = stop.clone();
                let app = lan_app.clone();
                let task = tokio::spawn(async move {
                    let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
                        let _ = s.wait_for(|v| *v).await;
                    });
                    if let Err(e) = serve.await {
                        tracing::warn!(%addr, "local-network listener stopped: {e}");
                    }
                });
                tracing::info!(url = %wss_url(addr), "local-network fallback listening");
                warned.remove(ip);
                lan_bound.insert(*ip, (addr, task));
            }
        }
        close_gone(&mut lan_bound, &lan_want);
        let mut lan_urls: Vec<String> = lan_bound.values().map(|(a, _)| wss_url(*a)).collect();
        lan_urls.sort();
        daemon.set_lan(lan_urls, lan_id.as_ref().map(|i| i.fingerprint.clone()));

        if cfg.tailscale && cfg.heal_tailscale {
            heal.check(ts.is_empty(), !crate::lan::lan_addrs().is_empty());
        }

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
    bound.extend(lan_bound);
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

/// The device behind the request's bearer token (with the same brake on bad tokens as the WebSocket).
#[allow(clippy::result_large_err)] // the Err is the ready response, used once
fn authed(st: &AppState, headers: &HeaderMap) -> Result<String, Response> {
    if st.daemon.auth_throttle.blocked() {
        return Err(error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many failed logins; try again in a minute",
        ));
    }
    match bearer(headers).and_then(|t| st.daemon.authenticate(t)) {
        Some(d) => Ok(d.to_string()),
        None => {
            st.daemon.auth_throttle.failed();
            Err(error(
                StatusCode::UNAUTHORIZED,
                "missing or invalid device token",
            ))
        }
    }
}

/// Refresh the widget over the paired connection. The push itself carries no snapshot.
async fn live_snapshot(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = authed(&st, &headers) {
        return response;
    }
    match st.daemon.summaries(false) {
        Ok(sessions) => Json(crate::live::LiveState::of(&sessions, false)).into_response(),
        Err(e) => {
            tracing::warn!("widget snapshot failed: {e:#}");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not load session status",
            )
        }
    }
}

/// A session in a few words, for the phone's notification (see `quick::SessionBrief`).
async fn session_brief(
    State(st): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(session): axum::extract::Path<String>,
) -> Response {
    if let Err(r) = authed(&st, &headers) {
        return r;
    }
    match st.daemon.session_brief(&SessionId::from(session.as_str())) {
        Ok(Some(b)) => Json(b).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "no such session"),
        Err(e) => {
            tracing::error!("session brief: {e:#}");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "couldn't read the session",
            )
        }
    }
}

/// What an approval asks, for the phone's notification (see `quick`).
async fn permission_get(
    State(st): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path((session, request)): axum::extract::Path<(String, String)>,
) -> Response {
    if let Err(r) = authed(&st, &headers) {
        return r;
    }
    match st
        .daemon
        .permission_details(&SessionId::from(session.as_str()), &request.as_str().into())
    {
        Ok(Some(d)) => Json(d).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "no such request"),
        Err(e) => {
            tracing::error!("permission details: {e:#}");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "couldn't read the request",
            )
        }
    }
}

#[derive(serde::Deserialize)]
struct UploadQuery {
    name: String,
}

/// An image or file for a message: saved on this Mac, its path returned.
async fn upload(
    State(st): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(session): axum::extract::Path<String>,
    axum::extract::Query(q): axum::extract::Query<UploadQuery>,
    body: axum::body::Bytes,
) -> Response {
    let device = match authed(&st, &headers) {
        Ok(d) => d,
        Err(r) => return r,
    };
    match st
        .daemon
        .save_upload(&SessionId::from(session.as_str()), &q.name, &body)
    {
        Ok(path) => {
            tracing::info!(%device, bytes = body.len(), path = %path.display(), "upload saved");
            Json(serde_json::json!({"path": path})).into_response()
        }
        Err(e) => error(StatusCode::BAD_REQUEST, &format!("{e:#}")),
    }
}

/// Answer an approval from the phone's notification.
async fn permission_answer(
    State(st): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path((session, request)): axum::extract::Path<(String, String)>,
    Json(body): Json<crate::quick::AnswerBody>,
) -> Response {
    let device = match authed(&st, &headers) {
        Ok(d) => d,
        Err(r) => return r,
    };
    match st.daemon.answer_permission(
        &SessionId::from(session.as_str()),
        &request.as_str().into(),
        &body.option,
        device.as_str().into(),
    ) {
        Ok(()) => {
            tracing::info!(%device, %request, option = %body.option, "permission answered from a notification");
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Err(message) => error(StatusCode::CONFLICT, &message),
    }
}

async fn ws(State(st): State<AppState>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    if st.daemon.auth_throttle.blocked() {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many failed logins; try again in a minute",
        );
    }
    let Some(device) = bearer(&headers).and_then(|t| st.daemon.authenticate(t)) else {
        st.daemon.auth_throttle.failed();
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
        let (lan_urls, lan_cert) = self.daemon.lan();
        send(
            &mut tx,
            &ServerMsg::Hello {
                protocol: PHONE_PROTOCOL,
                daemon_version: styr_core::VERSION.into(),
                device_id: self.device.clone(),
                host: Daemon::host_name(),
                warnings: self.daemon.warnings(),
                usage_supported: true,
                lan_urls,
                lan_cert,
            },
        )
        .await?;
        self.send_sessions(&mut tx).await?;
        let mut beat = tokio::time::interval(HEARTBEAT);
        beat.tick().await;
        let mut stop = self.stop.clone();
        // Anything from the phone (incl. pongs to our pings) proves it's still there. A
        // phone that vanished (network switch, locked) would otherwise count as "app
        // open" and its pushes would be skipped.
        let mut last_heard = tokio::time::Instant::now();
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
                msg = rx.next() => {
                    last_heard = tokio::time::Instant::now();
                    match msg {
                    Some(Ok(Message::Text(t))) => match serde_json::from_str::<ClientMsg>(t.as_str()) {
                        Ok(m) => self.on_client(&mut tx, m).await?,
                        Err(e) => send(&mut tx, &ServerMsg::Error { message: format!("bad message: {e}") }).await?,
                    },
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(_)) => {} // ping/pong/binary
                    Some(Err(e)) => return Err(e.into()),
                    }
                }
                b = bus.recv() => match b {
                    Ok(BusMsg::Event(ev)) => {
                        if let Some(last) = self.subs.get_mut(&ev.session)
                            && ev.seq > *last {
                                *last = ev.seq;
                                send(&mut tx, &ServerMsg::Event { event: ev }).await?;
                            }
                    }
                    Ok(BusMsg::SessionChanged(id)) => self.on_session_changed(&mut tx, id).await?,
                    Ok(BusMsg::LiveTokens) => {}
                    Ok(BusMsg::Screen(session, text, cols)) => {
                        if self.subs.contains_key(&session) {
                            send(&mut tx, &ServerMsg::Screen { session, text, cols }).await?;
                        }
                    }
                    Ok(BusMsg::Preview(session, text)) => {
                        if self.subs.contains_key(&session) {
                            send(&mut tx, &ServerMsg::Preview { session, text }).await?;
                        }
                    }
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
                _ = beat.tick() => {
                    if last_heard.elapsed() > HEARTBEAT * 3 {
                        tracing::info!(device = %self.device, "phone silent for {}s; closing", last_heard.elapsed().as_secs());
                        return Ok(());
                    }
                    tx.send(Message::Ping(Default::default())).await?
                }
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
            for events in by_size(events, PAGE_BYTES) {
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
            ClientMsg::SetNotifications { on, finished } => {
                self.daemon.set_notify(&self.device, on, finished)?;
                tracing::info!(device = %self.device, on, finished, "notification settings");
                Ok(())
            }
            ClientMsg::RegisterActivityToken { kind, token } => {
                let valid = token.as_deref().is_none_or(|t| {
                    (32..=400).contains(&t.len()) && t.chars().all(|c| c.is_ascii_hexdigit())
                });
                if !valid || (kind != "update" && kind != "start") {
                    return send(
                        tx,
                        &ServerMsg::Error {
                            message: "invalid Live Activity token".into(),
                        },
                    )
                    .await;
                }
                let token = token.map(|t| t.to_ascii_lowercase());
                self.daemon
                    .set_activity_token(&self.device, kind == "start", token.as_deref())?;
                tracing::info!(device = %self.device, %kind, set = token.is_some(), "Live Activity token");
                Ok(())
            }
            ClientMsg::GetUsage => {
                let providers = tokio::task::spawn_blocking(crate::usage::read)
                    .await
                    .unwrap_or_default();
                send(tx, &ServerMsg::Usage { providers }).await
            }

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
            ClientMsg::ListFolders { path } => match self.daemon.list_folders(path.as_deref()) {
                Ok(listing) => send(tx, &ServerMsg::Folders { listing }).await,
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
            ClientMsg::CreateFolder { parent, name } => {
                match self.daemon.create_folder(&parent, &name) {
                    Ok(listing) => send(tx, &ServerMsg::Folders { listing }).await,
                    Err(e) => {
                        send(
                            tx,
                            &ServerMsg::Error {
                                message: format!("{e:#}"),
                            },
                        )
                        .await
                    }
                }
            }
            ClientMsg::StartSession { agent, cwd, prompt } => {
                match self
                    .daemon
                    .start_session(agent, &cwd, prompt, self.device.as_str().into())
                {
                    Ok(session) => send(tx, &ServerMsg::SessionStarting { session }).await,
                    Err(e) => {
                        send(
                            tx,
                            &ServerMsg::Error {
                                message: format!("{e:#}"),
                            },
                        )
                        .await
                    }
                }
            }
            ClientMsg::Unsubscribe { session } => {
                self.subs.remove(&session);
                Ok(())
            }
            ClientMsg::Command {
                session,
                command: styr_core::Command::ResolvePermission { request, option },
            } => {
                let result = self.daemon.answer_permission(
                    &session,
                    &request,
                    &option,
                    self.device.as_str().into(),
                );
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
                Ok(crate::prompts::Delivery::Delivered) => {
                    send(
                        tx,
                        &ServerMsg::PromptDelivered {
                            session,
                            text: text.trim().to_owned(),
                        },
                    )
                    .await
                }
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
            ClientMsg::GetChanges { session } => {
                let cwd = self
                    .daemon
                    .summary(&session)?
                    .filter(|s| s.shared)
                    .and_then(|s| s.cwd);
                let Some(cwd) = cwd else {
                    return send(
                        tx,
                        &ServerMsg::Error {
                            message: "This session has no folder to look at.".into(),
                        },
                    )
                    .await;
                };
                let read = tokio::task::spawn_blocking(move || crate::changes::read(&cwd)).await;
                match read {
                    Ok(Ok(c)) => {
                        send(
                            tx,
                            &ServerMsg::Changes {
                                session,
                                branch: c.branch,
                                diff: c.diff,
                                truncated: c.truncated,
                                ahead: c.ahead,
                            },
                        )
                        .await
                    }
                    Ok(Err(e)) => {
                        send(
                            tx,
                            &ServerMsg::Error {
                                message: format!("{e:#}"),
                            },
                        )
                        .await
                    }
                    Err(e) => Err(e.into()),
                }
            }
            ClientMsg::GetModels { session } => {
                let Some(agent) = self
                    .daemon
                    .summary(&session)?
                    .filter(|s| s.shared)
                    .map(|s| s.agent)
                else {
                    return Ok(());
                };
                let models =
                    tokio::task::spawn_blocking(move || crate::models::choices(agent)).await?;
                send(tx, &ServerMsg::Models { session, models }).await
            }
            ClientMsg::Commit {
                session,
                message,
                push,
            } => {
                let summary = self.daemon.summary(&session)?.filter(|s| s.shared);
                let fail = |message: String| ServerMsg::Error { message };
                let Some(cwd) = summary.as_ref().and_then(|s| s.cwd.clone()) else {
                    return send(tx, &fail("This session has no folder to commit in.".into()))
                        .await;
                };
                if summary.is_some_and(|s| s.status == styr_core::SessionStatus::Working) {
                    // It may be halfway through an edit.
                    return send(
                        tx,
                        &fail("It's still working: commit once it's done.".into()),
                    )
                    .await;
                }
                tracing::info!(%session, push, "commit from the phone");
                let done = tokio::task::spawn_blocking(move || {
                    let r = crate::changes::commit(&cwd, &message, push);
                    (r, crate::changes::read(&cwd))
                })
                .await?;
                match done.0 {
                    Ok(c) => {
                        send(
                            tx,
                            &ServerMsg::Committed {
                                session: session.clone(),
                                commit: c.commit,
                                pushed: c.pushed,
                            },
                        )
                        .await?
                    }
                    Err(e) => send(tx, &fail(format!("{e:#}"))).await?,
                }
                // What's left (or the error's state) either way.
                match done.1 {
                    Ok(c) => {
                        send(
                            tx,
                            &ServerMsg::Changes {
                                session,
                                branch: c.branch,
                                diff: c.diff,
                                truncated: c.truncated,
                                ahead: c.ahead,
                            },
                        )
                        .await
                    }
                    Err(_) => Ok(()),
                }
            }
            ClientMsg::WatchScreen { session } => match self.daemon.watch_screen(&session) {
                Ok(()) => Ok(()),
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
                command: styr_core::Command::Interrupt,
            } => match self.daemon.interrupt(&session) {
                Ok(()) => Ok(()),
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
                command: styr_core::Command::EndSession,
            } => match self.daemon.end_session(&session) {
                Ok(()) => Ok(()),
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
                command: styr_core::Command::SetMode { mode },
            } => match self.daemon.set_mode(&session, &mode) {
                Ok(()) => Ok(()),
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
                command: styr_core::Command::SetModel { model, effort },
            } => match self.daemon.set_model(&session, model, effort) {
                Ok(()) => Ok(()),
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

#[cfg(test)]
mod page_tests {
    use super::*;

    #[test]
    fn a_long_history_goes_in_messages_the_phone_can_take() {
        let events: Vec<Event> = (1..=500)
            .map(|seq| {
                serde_json::from_value(serde_json::json!({
                    "session": "01M8C3A705H249464MDSQVZGTA",
                    "seq": seq,
                    "at_ms": 0,
                    "source": "daemon",
                    "kind": {"type": "error", "message": "x".repeat(3000), "fatal": false},
                }))
                .unwrap()
            })
            .collect();
        let runs = by_size(events, PAGE_BYTES);
        assert!(runs.len() > 1);
        assert_eq!(runs.iter().map(Vec::len).sum::<usize>(), 500);
        for run in &runs {
            assert!(serde_json::to_string(run).unwrap().len() < 1024 * 1024);
        }
        assert_eq!(runs[0][0].seq, 1);
        // Still in order, and a lone event bigger than the limit still goes.
        assert_eq!(runs.last().unwrap().last().unwrap().seq, 500);
        assert_eq!(by_size(runs[0].clone(), 10).len(), runs[0].len());
    }
}

#[cfg(test)]
mod heal_tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn restarts_tailscale_only_after_a_while_and_not_too_often() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert!(!should_heal(at(10), t0, None, true), "a blip");
        assert!(should_heal(at(30), t0, None, true));
        assert!(!should_heal(at(30), t0, None, false), "no network at all");
        assert!(
            !should_heal(at(300), t0, Some(at(30)), true),
            "tried recently"
        );
        assert!(should_heal(at(631), t0, Some(at(30)), true));
    }
}
