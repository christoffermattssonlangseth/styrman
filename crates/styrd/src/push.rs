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
/// Wait this long before pushing a permission request; skip it if answered by then.
pub const PERMISSION_PUSH_DELAY: Duration = Duration::from_secs(8);
/// Only push "finished" for turns at least this long (quick terminal turns stay quiet).
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
        let mut p = serde_json::json!({ "aps": aps });
        if let Some(s) = &self.session {
            p["session"] = s.as_str().into();
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
    key: jsonwebtoken::EncodingKey,
    client: reqwest::Client,
    jwt: Mutex<Option<(String, Instant)>>,
}

impl Pusher {
    /// `Ok(None)` when push isn't set up.
    pub fn load(dir: &Path) -> Result<Option<Self>> {
        let cfg_path = dir.join(CONFIG_FILE);
        if !cfg_path.exists() {
            return Ok(None);
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
            key,
            client,
            jwt: Mutex::new(None),
        }))
    }

    fn token(&self) -> Result<String> {
        let mut g = self.jwt.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((t, at)) = g.as_ref()
            && at.elapsed() < Duration::from_secs(40 * 60)
        {
            return Ok(t.clone());
        }
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        header.kid = Some(self.cfg.key_id.clone());
        let claims = serde_json::json!({"iss": self.cfg.team_id, "iat": now_ms() / 1000});
        let t = jsonwebtoken::encode(&header, &claims, &self.key)?;
        *g = Some((t.clone(), Instant::now()));
        Ok(t)
    }

    pub async fn send(&self, device_token: &str, env: &str, n: &Notification) -> SendResult {
        let host = if env == "production" {
            "api.push.apple.com"
        } else {
            "api.sandbox.push.apple.com"
        };
        let jwt = match self.token() {
            Ok(t) => t,
            Err(e) => return SendResult::Failed(format!("signing: {e}")),
        };
        let mut req = self
            .client
            .post(format!("https://{host}/3/device/{device_token}"))
            .bearer_auth(jwt)
            .header("apns-topic", &self.cfg.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .json(&n.payload());
        if let Some(c) = &n.collapse_id {
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
            category: None,
            collapse_id: Some(format!("share-{session}")),
        }),
        EventKind::PermissionRequested { tool, .. } => Some(Notification {
            title,
            body: if details {
                format!("Needs your approval: {}", tool.title)
            } else {
                "Needs your approval".into()
            },
            session: Some(session.clone()),
            category: Some("PERMISSION"),
            collapse_id: Some(format!("perm-{session}")),
        }),
        EventKind::TurnEnded {
            reason,
            last_message,
            ..
        } => {
            let failed = *reason == TurnEndReason::Failed;
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
                category: None,
                collapse_id: Some(format!("turn-{session}")),
            })
        }
        _ => None,
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
            tokio::spawn(async move {
                tokio::time::sleep(PERMISSION_PUSH_DELAY).await;
                if d.broker.is_pending(&request) {
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
        let jwt = p.token().unwrap();
        assert_eq!(jwt.split('.').count(), 3);
        assert_eq!(p.token().unwrap(), jwt, "cached");
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
