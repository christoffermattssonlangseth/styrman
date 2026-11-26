//! Push notifications through Apple's APNs, straight from the daemon (token auth:
//! an ES256 JWT signed with the team's `.p8` key). Apple's push service is the only
//! third party Styrman talks to, and by default it only sees "agent · repo needs
//! your approval / finished": no commands, code or replies unless `details` is on.
//!
//! Config lives in the data dir: `apns.json` + `apns-key.p8` (0600), written by
//! `styr push setup`.

use crate::daemon::{BusMsg, Daemon};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use styr_core::event::{EventKind, TurnEndReason, now_ms};
use styr_core::ids::SessionId;

pub const CONFIG_FILE: &str = "apns.json";
pub const KEY_FILE: &str = "apns-key.p8";
/// Credentials for a push relay (`styr push relay <url>`), used when there's no own key.
pub const RELAY_FILE: &str = "relay.json";
/// What the relay accepts (it refuses longer text: only short alerts pass through it).
const RELAY_MAX_TITLE: usize = 120;
const RELAY_MAX_BODY: usize = 240;

/// A push relay registration: the relay holds the app publisher's APNs key, so people
/// without an Apple developer key still get notifications. It only ever receives the
/// device token and the short alert text (details are always off through a relay).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayConfig {
    pub url: String,
    pub install_id: String,
    pub secret: String,
}
/// Wait this long before pushing a permission request; skip it if answered by then.
pub const PERMISSION_PUSH_DELAY: Duration = Duration::from_secs(8);
/// Only push "finished" for turns at least this long (quick terminal turns stay quiet).
/// A message another agent sent (`styr ask`), not you.
fn from_agent(origin: &styr_core::event::Origin) -> bool {
    matches!(origin, styr_core::event::Origin::Remote(d)
        if d.as_str().starts_with(crate::peers::AGENT_DEVICE_PREFIX))
}

pub const MIN_TURN_FOR_PUSH: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushConfig {
    pub key_id: String,
    pub team_id: String,
    /// The app's bundle id.
    pub topic: String,
    /// Include command titles and reply excerpts in notifications (off by default).
    #[serde(default)]
    pub details: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    pub session: Option<SessionId>,
    pub category: Option<&'static str>,
    pub collapse_id: Option<String>,
    /// An approval's request id: the phone's notification extension looks it up on
    /// this Mac (over the tailnet) to offer Allow/Deny with the actual command.
    pub request: Option<String>,
}

impl Notification {
    pub fn payload(&self) -> serde_json::Value {
        let mut aps = serde_json::json!({
            "alert": {"title": self.title, "body": self.body},
            "sound": "default",
        });
        if let Some(s) = &self.session {
            aps["thread-id"] = s.as_str().into();
        }
        if let Some(c) = self.category {
            aps["category"] = c.into();
        }
        if self.session.is_some() {
            aps["mutable-content"] = 1.into();
        }
        let mut p = serde_json::json!({ "aps": aps });
        if let Some(s) = &self.session {
            p["session"] = s.as_str().into();
        }
        if let Some(r) = &self.request {
            p["request"] = r.as_str().into();
        }
        p
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendResult {
    Sent,
    /// The token is dead (app deleted, or a sandbox/production mismatch): forget it.
    BadToken(String),
    Failed(String),
}

pub struct Pusher {
    pub cfg: PushConfig,
    mode: Mode,
    client: reqwest::Client,
    jwt: Mutex<Option<(String, Instant)>>,
}

enum Mode {
    /// Our own APNs key (`styr push setup`).
    Direct(jsonwebtoken::EncodingKey),
    Relay(RelayConfig),
}

impl Pusher {
    /// `Ok(None)` when push isn't set up.
    pub fn load(dir: &Path) -> Result<Option<Self>> {
        let cfg_path = dir.join(CONFIG_FILE);
        if !cfg_path.exists() {
            let relay_path = dir.join(RELAY_FILE);
            if !relay_path.exists() {
                return Ok(None);
            }
            let relay: RelayConfig = serde_json::from_slice(&std::fs::read(&relay_path)?)
                .context("reading relay.json")?;
            return Ok(Some(Self {
                cfg: PushConfig {
                    key_id: String::new(),
                    team_id: String::new(),
                    topic: String::new(),
                    details: false,
                },
                mode: Mode::Relay(relay),
                client: reqwest::Client::builder()
                    .timeout(Duration::from_secs(15))
                    .build()?,
                jwt: Mutex::new(None),
            }));
        }
        let cfg: PushConfig =
            serde_json::from_slice(&std::fs::read(&cfg_path)?).context("reading apns.json")?;
        let pem = std::fs::read(dir.join(KEY_FILE)).context("reading the APNs key")?;
        let key = jsonwebtoken::EncodingKey::from_ec_pem(&pem)
            .context("the APNs key is not a valid .p8 (EC) key")?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Some(Self {
            cfg,
            mode: Mode::Direct(key),
            client,
            jwt: Mutex::new(None),
        }))
    }

    /// The relay's URL when notifications go through one.
    pub fn relay_url(&self) -> Option<&str> {
        match &self.mode {
            Mode::Relay(r) => Some(&r.url),
            Mode::Direct(_) => None,
        }
    }

    fn token(&self, key: &jsonwebtoken::EncodingKey) -> Result<String> {
        let mut g = self.jwt.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((t, at)) = g.as_ref()
            && at.elapsed() < Duration::from_secs(40 * 60)
        {
            return Ok(t.clone());
        }
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        header.kid = Some(self.cfg.key_id.clone());
        let claims = serde_json::json!({"iss": self.cfg.team_id, "iat": now_ms() / 1000});
        let t = jsonwebtoken::encode(&header, &claims, key)?;
        *g = Some((t.clone(), Instant::now()));
        Ok(t)
    }

    pub async fn send(&self, device_token: &str, env: &str, n: &Notification) -> SendResult {
        let key = match &self.mode {
            Mode::Direct(k) => k,
            Mode::Relay(r) => return self.send_via_relay(r, device_token, env, n).await,
        };
        self.apns(
            key,
            device_token,
            env,
            "alert",
            &self.cfg.topic,
            10,
            n.collapse_id.as_deref(),
            &n.payload(),
        )
        .await
    }

    /// A Live Activity push (`payload` from `live::payload`).
    pub async fn send_live(
        &self,
        token: &str,
        env: &str,
        payload: &serde_json::Value,
        priority: u8,
    ) -> SendResult {
        match &self.mode {
            Mode::Direct(k) => {
                let topic = format!("{}.push-type.liveactivity", self.cfg.topic);
                self.apns(
                    k,
                    token,
                    env,
                    "liveactivity",
                    &topic,
                    priority,
                    None,
                    payload,
                )
                .await
            }
            Mode::Relay(r) => {
                self.relay_post(
                    r,
                    "live",
                    &relay_live_request(token, env, payload, priority),
                )
                .await
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn apns(
        &self,
        key: &jsonwebtoken::EncodingKey,
        device_token: &str,
        env: &str,
        push_type: &str,
        topic: &str,
        priority: u8,
        collapse_id: Option<&str>,
        payload: &serde_json::Value,
    ) -> SendResult {
        let host = if env == "production" {
            "api.push.apple.com"
        } else {
            "api.sandbox.push.apple.com"
        };
        let jwt = match self.token(key) {
            Ok(t) => t,
            Err(e) => return SendResult::Failed(format!("signing: {e}")),
        };
        let mut req = self
            .client
            .post(format!("https://{host}/3/device/{device_token}"))
            .bearer_auth(jwt)
            .header("apns-topic", topic)
            .header("apns-push-type", push_type)
            .header("apns-priority", priority.to_string())
            .json(payload);
        if let Some(c) = collapse_id {
            req = req.header("apns-collapse-id", c.chars().take(64).collect::<String>());
        }
        match req.send().await {
            Ok(r) if r.status().is_success() => SendResult::Sent,
            Ok(r) => {
                let status = r.status().as_u16();
                let reason = r
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|v| v["reason"].as_str().map(str::to_owned))
                    .unwrap_or_default();
                if status == 410
                    || reason == "BadDeviceToken"
                    || reason == "Unregistered"
                    || reason == "DeviceTokenNotForTopic"
                {
                    SendResult::BadToken(reason)
                } else {
                    SendResult::Failed(format!("{status} {reason}"))
                }
            }
            Err(e) => SendResult::Failed(e.to_string()),
        }
    }
}

impl Pusher {
    async fn send_via_relay(
        &self,
        r: &RelayConfig,
        device_token: &str,
        env: &str,
        n: &Notification,
    ) -> SendResult {
        self.relay_post(r, "push", &relay_request(device_token, env, n))
            .await
    }

    async fn relay_post(
        &self,
        r: &RelayConfig,
        route: &str,
        body: &serde_json::Value,
    ) -> SendResult {
        let resp = self
            .client
            .post(format!("{}/v1/{route}", r.url.trim_end_matches('/')))
            .bearer_auth(format!("{}.{}", r.install_id, r.secret))
            .json(body)
            .send()
            .await;
        match resp {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let v: serde_json::Value = resp.json().await.unwrap_or_default();
                match v["result"].as_str() {
                    Some("sent") => SendResult::Sent,
                    Some("bad_token") => {
                        SendResult::BadToken(v["reason"].as_str().unwrap_or("").to_owned())
                    }
                    _ => SendResult::Failed(format!(
                        "relay: {status} {}",
                        v["reason"].as_str().or(v["error"].as_str()).unwrap_or("")
                    )),
                }
            }
            Err(e) => SendResult::Failed(format!("relay: {e}")),
        }
    }
}

/// What we send the relay: the token and the short alert, nothing else.
pub fn relay_request(device_token: &str, env: &str, n: &Notification) -> serde_json::Value {
    let cut = |s: &str, max: usize| -> String {
        if s.chars().count() <= max {
            s.to_owned()
        } else {
            format!("{}…", s.chars().take(max - 1).collect::<String>())
        }
    };
    let mut v = serde_json::json!({
        "token": device_token,
        "env": if env == "production" { "production" } else { "sandbox" },
        "title": cut(&n.title, RELAY_MAX_TITLE),
        "body": cut(&n.body, RELAY_MAX_BODY),
    });
    if let Some(s) = &n.session {
        v["session"] = s.as_str().into();
    }
    if let Some(c) = n.category {
        v["category"] = c.into();
    }
    if let Some(c) = &n.collapse_id {
        v["collapse_id"] = c.chars().take(64).collect::<String>().into();
    }
    if let Some(r) = &n.request {
        v["request"] = r.chars().take(64).collect::<String>().into();
    }
    v
}

/// What a Live Activity push sends to the relay: the token and the activity's state
/// (counts, and agent · folder · status rows); the relay builds the APNs payload.
fn relay_live_request(
    token: &str,
    env: &str,
    payload: &serde_json::Value,
    priority: u8,
) -> serde_json::Value {
    let aps = &payload["aps"];
    serde_json::json!({
        "token": token,
        "env": if env == "production" { "production" } else { "sandbox" },
        "event": aps["event"],
        "state": aps["content-state"],
        "priority": priority,
    })
}

/// Register this Mac with a push relay and store its credentials (0600).
pub async fn register_relay(dir: &Path, url: &str) -> Result<()> {
    anyhow::ensure!(
        url.starts_with("https://") || url.starts_with("http://127.0.0.1:"),
        "the relay URL must start with https://"
    );
    let url = url.trim_end_matches('/').to_owned();
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{url}/v1/register"))
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .context("couldn't reach the relay")?
        .error_for_status()
        .context("the relay refused to register")?
        .json()
        .await?;
    let cfg = RelayConfig {
        url,
        install_id: v["install_id"]
            .as_str()
            .context("no install_id")?
            .to_owned(),
        secret: v["secret"].as_str().context("no secret")?.to_owned(),
    };
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(dir.join(RELAY_FILE))?;
    std::io::Write::write_all(&mut f, &serde_json::to_vec_pretty(&cfg)?)?;
    Ok(())
}

/// What to push for an event of a shared session (pure; unit-tested).
pub fn notification_for(
    kind: &EventKind,
    session: &SessionId,
    agent: &str,
    repo: &str,
    details: bool,
    turn_took: Option<Duration>,
) -> Option<Notification> {
    let title = format!("{agent} · {repo}");
    match kind {
        EventKind::SharingChanged { shared: true } => Some(Notification {
            title,
            body: "Now on your phone. Tap to follow along.".into(),
            session: Some(session.clone()),
            category: Some("SHARED"),
            collapse_id: Some(format!("share-{session}")),
            request: None,
        }),
        EventKind::PermissionRequested { tool, request, .. } => Some(Notification {
            title,
            body: if details {
                format!("Needs your approval: {}", tool.title)
            } else {
                "Needs your approval".into()
            },
            session: Some(session.clone()),
            category: Some("PERMISSION"),
            collapse_id: Some(format!("perm-{session}")),
            request: Some(request.to_string()),
        }),
        EventKind::TurnEnded {
            reason,
            last_message,
            ..
        } => {
            let failed = *reason == TurnEndReason::Failed;
            // Stopped (from the phone or with Esc): you know already.
            if *reason == TurnEndReason::Interrupted {
                return None;
            }
            if !failed && turn_took.is_none_or(|d| d < MIN_TURN_FOR_PUSH) {
                return None;
            }
            let body = if failed {
                "Stopped with an error".into()
            } else if details && let Some(m) = last_message {
                let line = m
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("Finished");
                line.chars().take(140).collect()
            } else {
                "Finished, waiting for you".into()
            };
            Some(Notification {
                title,
                body,
                session: Some(session.clone()),
                // The phone fills in the reply's first line (fetched from this Mac).
                category: Some(if failed { "TURN_FAILED" } else { "TURN" }),
                collapse_id: Some(format!("turn-{session}")),
                request: None,
            })
        }
        _ => None,
    }
}

/// Whether a phone's settings let `category` through. Approvals and errors need
/// notifications on; finished turns and new sessions also "when a turn finishes".
/// A test push (no category) always goes.
pub fn wants(d: &crate::store::DeviceRecord, category: Option<&str>) -> bool {
    match category {
        None => true,
        Some("PERMISSION" | "TURN_FAILED") => d.notify,
        Some(_) => d.notify && d.notify_finished,
    }
}

impl Daemon {
    /// Send to every paired phone with a push token that isn't connected right now
    /// (a connected app is in the foreground and already shows it).
    pub async fn push_to_devices(
        self: &Arc<Self>,
        n: &Notification,
        include_connected: bool,
    ) -> Vec<(String, SendResult)> {
        let Some(pusher) = self.pusher() else {
            return vec![];
        };
        let devices = match self.lock_store_devices() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("listing devices for push: {e:#}");
                return vec![];
            }
        };
        let mut results = vec![];
        for d in devices {
            let (Some(token), false) = (d.push_token.clone(), d.revoked) else {
                continue;
            };
            if !include_connected && self.is_device_live(&d.id) {
                continue;
            }
            if !wants(&d, n.category) {
                continue;
            }
            let env = d.push_env.clone().unwrap_or_else(|| "sandbox".into());
            let r = pusher.send(&token, &env, n).await;
            match &r {
                SendResult::Sent => tracing::info!(device = %d.id, title = %n.title, "push sent"),
                SendResult::BadToken(why) => {
                    tracing::warn!(device = %d.id, %why, "APNs rejected the token; forgetting it");
                    let _ = self.set_push_token(&d.id, None, None);
                }
                SendResult::Failed(why) => tracing::warn!(device = %d.id, %why, "push failed"),
            }
            results.push((d.id, r));
        }
        results
    }
}

/// Watch the event bus and push for shared sessions.
pub async fn run_notifier(daemon: Arc<Daemon>, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut bus = daemon.subscribe();
    let mut turn_started: HashMap<SessionId, Instant> = HashMap::new();
    // Sessions whose current turn another agent asked for (`styr ask`): its reply goes
    // back to that agent, so its end isn't news for you. Approvals still notify.
    let mut asked_by_agent: std::collections::HashSet<SessionId> = Default::default();
    loop {
        let msg = tokio::select! {
            _ = stop.wait_for(|v| *v) => return,
            m = bus.recv() => m,
        };
        let ev = match msg {
            Ok(BusMsg::Event(ev)) => ev,
            Ok(_) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => return,
        };
        if matches!(ev.kind, EventKind::TurnStarted { .. }) {
            turn_started.insert(ev.session.clone(), Instant::now());
            continue;
        }
        if let EventKind::UserMessage { origin, .. } = &ev.kind {
            if from_agent(origin) {
                asked_by_agent.insert(ev.session.clone());
            } else {
                asked_by_agent.remove(&ev.session);
            }
            continue;
        }
        if !matches!(
            ev.kind,
            EventKind::PermissionRequested { .. }
                | EventKind::TurnEnded { .. }
                | EventKind::SharingChanged { shared: true }
        ) {
            continue;
        }
        let Some(pusher) = daemon.pusher() else {
            continue;
        };
        let Ok(Some(summary)) = daemon.summary(&ev.session) else {
            continue;
        };
        if !summary.shared {
            continue;
        }
        let quiet = match &ev.kind {
            EventKind::TurnEnded { .. } => asked_by_agent.remove(&ev.session),
            // A helper an agent started isn't something you shared.
            EventKind::SharingChanged { .. } => summary.helper_of.is_some(),
            _ => false,
        };
        if quiet {
            continue;
        }
        let repo = summary
            .cwd
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "session".into());
        let agent = match summary.agent {
            styr_core::ids::AgentKind::Claude => "Claude",
            styr_core::ids::AgentKind::Antigravity => "agy",
            styr_core::ids::AgentKind::Codex => "Codex",
            styr_core::ids::AgentKind::Pty => "Agent",
        };
        let took = if matches!(ev.kind, EventKind::TurnEnded { .. }) {
            turn_started.remove(&ev.session).map(|t| t.elapsed())
        } else {
            None
        };
        let Some(n) = notification_for(
            &ev.kind,
            &ev.session,
            agent,
            &repo,
            pusher.cfg.details,
            took,
        ) else {
            continue;
        };
        let d = daemon.clone();
        if let EventKind::PermissionRequested { request, .. } = &ev.kind {
            let request = request.clone();
            let session = ev.session.clone();
            tokio::spawn(async move {
                tokio::time::sleep(PERMISSION_PUSH_DELAY).await;
                // Still unanswered? Hook approvals (Claude) and menus read off the
                // screen (Codex, agy) both show as an open request on the session.
                if d.request_open(&session, &request) {
                    d.push_to_devices(&n, false).await;
                }
            });
        } else {
            tokio::spawn(async move {
                d.push_to_devices(&n, false).await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use styr_core::event::{ToolInfo, ToolKind};

    fn tool() -> ToolInfo {
        ToolInfo {
            name: "Bash".into(),
            kind: ToolKind::Execute,
            title: "Run `rm -rf build`".into(),
            input: serde_json::json!({}),
            locations: vec![],
        }
    }

    fn perm() -> EventKind {
        EventKind::PermissionRequested {
            request: "r".into(),
            call: None,
            tool: tool(),
            reason: None,
            options: vec![],
            expires_at_ms: None,
        }
    }

    fn ended(reason: TurnEndReason) -> EventKind {
        EventKind::TurnEnded {
            turn: None,
            reason,
            last_message: Some("Fixed the flaky test.\nDetails…".into()),
        }
    }

    #[test]
    fn a_phone_chooses_which_notifications_it_gets() {
        let phone = |notify, notify_finished| crate::store::DeviceRecord {
            id: "d".into(),
            name: "Phone".into(),
            secret_hash: vec![],
            created_ms: 0,
            last_seen_ms: None,
            revoked: false,
            push_token: None,
            push_env: None,
            activity_token: None,
            start_token: None,
            notify,
            notify_finished,
        };
        let all = phone(true, true);
        let needs_you = phone(true, false);
        let off = phone(false, false);
        for c in ["PERMISSION", "TURN_FAILED", "TURN", "SHARED"] {
            assert!(wants(&all, Some(c)));
            assert!(!wants(&off, Some(c)));
        }
        assert!(wants(&needs_you, Some("PERMISSION")));
        assert!(wants(&needs_you, Some("TURN_FAILED")));
        assert!(!wants(&needs_you, Some("TURN")));
        assert!(!wants(&needs_you, Some("SHARED")));
        // `styr push test` always arrives.
        assert!(wants(&off, None));
    }

    #[test]
    fn sharing_a_session_notifies() {
        let s = SessionId::from("S");
        let n = notification_for(
            &EventKind::SharingChanged { shared: true },
            &s,
            "agy",
            "styrman",
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            (n.title.as_str(), n.body.as_str()),
            ("agy · styrman", "Now on your phone. Tap to follow along.")
        );
        assert_eq!(n.payload()["session"], "S", "tapping opens the session");
        assert!(
            notification_for(
                &EventKind::SharingChanged { shared: false },
                &s,
                "agy",
                "r",
                false,
                None
            )
            .is_none()
        );
    }

    #[test]
    fn private_by_default() {
        let s = SessionId::from("S");
        let n = notification_for(&perm(), &s, "Claude", "styrman", false, None).unwrap();
        assert_eq!(
            (n.title.as_str(), n.body.as_str()),
            ("Claude · styrman", "Needs your approval")
        );
        assert!(
            !n.payload().to_string().contains("rm -rf"),
            "no command text without details"
        );
        assert_eq!(n.payload()["aps"]["category"], "PERMISSION");
        assert_eq!(n.payload()["session"], "S");
        let n = notification_for(
            &ended(TurnEndReason::EndTurn),
            &s,
            "Claude",
            "styrman",
            false,
            Some(Duration::from_secs(60)),
        )
        .unwrap();
        assert_eq!(n.body, "Finished, waiting for you");
    }

    #[test]
    fn details_when_enabled() {
        let s = SessionId::from("S");
        assert!(
            notification_for(&perm(), &s, "Claude", "r", true, None)
                .unwrap()
                .body
                .contains("rm -rf build")
        );
        let n = notification_for(
            &ended(TurnEndReason::EndTurn),
            &s,
            "Claude",
            "r",
            true,
            Some(Duration::from_secs(60)),
        )
        .unwrap();
        assert_eq!(n.body, "Fixed the flaky test.");
    }

    #[test]
    fn messages_from_agents_are_told_apart_from_yours() {
        use styr_core::event::Origin;
        assert!(from_agent(&Origin::Remote("agent:01ABC".into())));
        assert!(from_agent(&Origin::Remote("agent:mac".into())));
        assert!(!from_agent(&Origin::Remote("01PHONE".into())));
        assert!(!from_agent(&Origin::Terminal));
    }

    #[test]
    fn short_turns_stay_quiet_but_failures_always_notify() {
        let s = SessionId::from("S");
        assert!(
            notification_for(
                &ended(TurnEndReason::EndTurn),
                &s,
                "Claude",
                "r",
                false,
                Some(Duration::from_secs(5))
            )
            .is_none()
        );
        assert!(
            notification_for(
                &ended(TurnEndReason::EndTurn),
                &s,
                "Claude",
                "r",
                false,
                None
            )
            .is_none()
        );
        assert!(
            notification_for(
                &ended(TurnEndReason::Failed),
                &s,
                "Claude",
                "r",
                false,
                Some(Duration::from_secs(1))
            )
            .is_some()
        );
        assert!(
            notification_for(
                &EventKind::SessionEnded { reason: None },
                &s,
                "Claude",
                "r",
                false,
                None
            )
            .is_none()
        );
    }

    #[test]
    fn loads_a_real_p8_and_signs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Pusher::load(dir.path()).unwrap().is_none(),
            "not configured"
        );
        // A throwaway P-256 key in PKCS#8 PEM, the same format Apple's .p8 uses.
        let pem = "-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg2ZaMME9b1YbcA/zc\npw3PUvosUfHYYcpozXNShutIXUqhRANCAAQbSqCMyTopuE7hNgakdW/UZkePBjsb\nm64HsF9Ju2FyqmltNuXBKjmcn+9Typ6Y18Zs9XBsIx60j+TLv/3FR/Bo\n-----END PRIVATE KEY-----\n";
        std::fs::write(dir.path().join(KEY_FILE), pem).unwrap();
        let cfg = PushConfig {
            key_id: "ABC123DEFG".into(),
            team_id: "TEAM123456".into(),
            topic: "x".into(),
            details: false,
        };
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            serde_json::to_vec(&cfg).unwrap(),
        )
        .unwrap();
        let p = Pusher::load(dir.path()).unwrap().unwrap();
        let Mode::Direct(key) = &p.mode else {
            panic!("own key: direct mode")
        };
        let jwt = p.token(key).unwrap();
        assert_eq!(jwt.split('.').count(), 3);
        assert_eq!(p.token(key).unwrap(), jwt, "cached");
        let header: serde_json::Value = serde_json::from_slice(
            &base64::Engine::decode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                jwt.split('.').next().unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            (header["alg"].as_str(), header["kid"].as_str()),
            (Some("ES256"), Some("ABC123DEFG"))
        );
    }
}

#[cfg(test)]
mod relay_tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex as AsyncMutex;

    fn note(body: &str) -> Notification {
        Notification {
            title: "Codex · styrman".into(),
            body: body.into(),
            session: Some("01SESSION".into()),
            category: Some("PERMISSION"),
            collapse_id: Some("perm-01SESSION".into()),
            request: Some("r1".into()),
        }
    }

    #[test]
    fn only_the_short_alert_goes_to_the_relay() {
        let v = relay_request(&"ab".repeat(32), "production", &note(&"x".repeat(500)));
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            [
                "body",
                "category",
                "collapse_id",
                "env",
                "request",
                "session",
                "title",
                "token"
            ]
        );
        assert_eq!(v["body"].as_str().unwrap().chars().count(), RELAY_MAX_BODY);
        assert_eq!(v["env"], "production");
        assert_eq!(relay_request("aa", "weird", &note("b"))["env"], "sandbox");
    }

    #[tokio::test]
    async fn register_then_push_through_a_relay() {
        use axum::{Json, Router, http::HeaderMap, routing::post};
        let seen: Arc<AsyncMutex<Vec<(String, serde_json::Value)>>> = Arc::default();
        let s = seen.clone();
        let app = Router::new()
            .route(
                "/v1/register",
                post(|| async { Json(serde_json::json!({"install_id": "inst", "secret": "sec"})) }),
            )
            .route(
                "/v1/push",
                post(move |h: HeaderMap, Json(v): Json<serde_json::Value>| {
                    let s = s.clone();
                    async move {
                        let auth = h["authorization"].to_str().unwrap().to_owned();
                        let dead = v["token"] == "dead";
                        s.lock().await.push((auth, v));
                        Json(if dead {
                            serde_json::json!({"result": "bad_token", "reason": "Unregistered"})
                        } else {
                            serde_json::json!({"result": "sent"})
                        })
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let dir = tempfile::tempdir().unwrap();
        register_relay(dir.path(), &url).await.unwrap();
        let mode = std::fs::metadata(dir.path().join(RELAY_FILE))
            .unwrap()
            .permissions();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            mode.mode() & 0o777,
            0o600,
            "credentials readable only by the user"
        );

        let p = Pusher::load(dir.path()).unwrap().expect("relay mode");
        assert_eq!(p.relay_url(), Some(url.as_str()));
        assert!(!p.cfg.details, "details are always off through a relay");
        assert_eq!(
            p.send("abcd", "sandbox", &note("Needs your approval"))
                .await,
            SendResult::Sent
        );
        assert_eq!(
            p.send("dead", "sandbox", &note("x")).await,
            SendResult::BadToken("Unregistered".into())
        );
        let seen = seen.lock().await;
        assert_eq!(seen[0].0, "Bearer inst.sec");
        assert_eq!(seen[0].1["body"], "Needs your approval");
        // An own key takes precedence over a relay.
        assert!(
            register_relay(dir.path(), "http://example.com")
                .await
                .is_err(),
            "https only"
        );
    }
}
