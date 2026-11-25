//! The phone's Live Activity (Lock Screen and Dynamic Island): how many agents are
//! working or waiting for you, and the top few by agent · folder · status.
//!
//! While the app is open it runs the activity itself. When it's closed, this sends
//! the changes as APNs `liveactivity` pushes, to the running activity's token, or
//! starts one with the push-to-start token. Like notifications, these carry only
//! agent names, folder names and statuses: never commands, code or replies.

use crate::daemon::{BusMsg, Daemon};
use crate::push::SendResult;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use styr_core::SessionStatus;
use styr_core::ipc::SessionSummary;

/// Wait this long after a change before pushing, so a burst becomes one update.
const DEBOUNCE: Duration = Duration::from_millis(1500);
/// At most this many rows on the activity.
const MAX_ROWS: usize = 3;

/// Must match `StyrmanActivityAttributes.ContentState` in the iOS app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct LiveState {
    pub working: u32,
    pub needs: u32,
    pub rows: Vec<LiveRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiveRow {
    pub agent: String,
    pub repo: String,
    /// "needs_approval", "waiting", "working" or "error".
    pub status: String,
}

impl LiveState {
    pub fn is_idle(&self) -> bool {
        self.working == 0 && self.needs == 0
    }

    pub fn of(sessions: &[SessionSummary]) -> Self {
        let mut live: Vec<&SessionSummary> = sessions
            .iter()
            .filter(|s| s.shared && s.status != SessionStatus::Done)
            .collect();
        // Waiting for you first, then working; newest first within each.
        live.sort_by_key(|s| (!s.status.needs_user(), std::cmp::Reverse(s.updated_ms)));
        let needs = live.iter().filter(|s| s.status.needs_user()).count() as u32;
        let working = live
            .iter()
            .filter(|s| s.status == SessionStatus::Working)
            .count() as u32;
        let rows = live
            .iter()
            .filter(|s| s.status.needs_user() || s.status == SessionStatus::Working)
            .take(MAX_ROWS)
            .map(|s| LiveRow {
                agent: crate::peers::agent_label(s.agent).to_owned(),
                repo: s
                    .cwd
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().chars().take(40).collect())
                    .unwrap_or_else(|| "session".into()),
                status: match s.status {
                    SessionStatus::NeedsApproval => "needs_approval",
                    SessionStatus::Waiting => "waiting",
                    SessionStatus::Error => "error",
                    _ => "working",
                }
                .into(),
            })
            .collect();
        Self {
            working,
            needs,
            rows,
        }
    }

    /// The headline for a push-to-start alert.
    fn headline(&self) -> String {
        match self.rows.first() {
            Some(r) if r.status == "working" => format!("{} is working in {}", r.agent, r.repo),
            Some(r) => format!("{} · {} needs you", r.agent, r.repo),
            None => "Your agents".into(),
        }
    }
}

/// The `aps` payload for a Live Activity push.
pub fn payload(event: &str, state: &LiveState, now_secs: u64) -> serde_json::Value {
    let mut aps = serde_json::json!({
        "timestamp": now_secs,
        "event": event,
        "content-state": state,
    });
    match event {
        "start" => {
            aps["attributes-type"] = "StyrmanActivityAttributes".into();
            aps["attributes"] = serde_json::json!({});
            aps["alert"] = serde_json::json!({"title": "Styrman", "body": state.headline()});
        }
        "end" => aps["dismissal-date"] = now_secs.into(),
        _ => {
            // Something waiting for you lights the activity up.
            if state.needs > 0 {
                aps["alert"] = serde_json::json!({"title": "Styrman", "body": state.headline()});
            }
        }
    }
    serde_json::json!({ "aps": aps })
}

/// Keep closed apps' Live Activities in step with the sessions.
pub async fn run(daemon: Arc<Daemon>, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut bus = daemon.subscribe();
    // What each device's activity last showed.
    let mut shown: HashMap<String, LiveState> = HashMap::new();
    loop {
        tokio::select! {
            _ = stop.wait_for(|v| *v) => return,
            m = bus.recv() => match m {
                Ok(BusMsg::SessionChanged(_) | BusMsg::LiveTokens) => {}
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            },
        }
        // Collect the rest of the burst.
        let quiet = tokio::time::sleep(DEBOUNCE);
        tokio::pin!(quiet);
        loop {
            tokio::select! {
                _ = &mut quiet => break,
                _ = stop.wait_for(|v| *v) => return,
                m = bus.recv() => if matches!(m, Err(tokio::sync::broadcast::error::RecvError::Closed)) { return },
            }
        }
        let Some(pusher) = daemon.pusher() else {
            continue;
        };
        let Ok(sessions) = daemon.summaries(false) else {
            continue;
        };
        let state = LiveState::of(&sessions);
        let Ok(devices) = daemon.lock_store_devices() else {
            continue;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        for d in devices.into_iter().filter(|d| !d.revoked) {
            // An open app runs its own activity.
            if daemon.is_device_live(&d.id) {
                shown.remove(&d.id);
                continue;
            }
            let env = d.push_env.clone().unwrap_or_else(|| "sandbox".into());
            let (token, event, start) = match (&d.activity_token, &d.start_token) {
                (Some(t), _) if state.is_idle() => (t.clone(), "end", false),
                (Some(t), _) => (t.clone(), "update", false),
                (None, Some(t)) if !state.is_idle() => (t.clone(), "start", true),
                _ => continue,
            };
            if shown.get(&d.id) == Some(&state) {
                continue;
            }
            let priority = if event == "update" && state.needs == 0 {
                5
            } else {
                10
            };
            let r = pusher
                .send_live(&token, &env, &payload(event, &state, now), priority)
                .await;
            match &r {
                SendResult::Sent => {
                    tracing::info!(device = %d.id, event, working = state.working, needs = state.needs, "live activity push");
                    shown.insert(d.id.clone(), state.clone());
                    if event == "end" {
                        let _ = daemon.set_activity_token_quiet(&d.id, false, None);
                    }
                }
                SendResult::BadToken(why) => {
                    tracing::info!(device = %d.id, %why, "live activity token gone; forgetting it");
                    let _ = daemon.set_activity_token_quiet(&d.id, start, None);
                }
                SendResult::Failed(why) => {
                    tracing::warn!(device = %d.id, %why, "live activity push failed")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(
        agent: styr_core::ids::AgentKind,
        repo: &str,
        status: SessionStatus,
        updated: i64,
    ) -> SessionSummary {
        serde_json::from_value(serde_json::json!({
            "id": format!("{repo}-{updated}"), "agent": agent, "native_id": repo, "cwd": format!("/Users/me/{repo}"),
            "title": "secret prompt text", "model": null, "shared": true, "status": status,
            "created_ms": 0, "updated_ms": updated, "last_seq": 1,
            "activity": "Run `rm -rf secret`"
        }))
        .unwrap()
    }

    #[test]
    fn summarises_without_any_session_content() {
        use styr_core::ids::AgentKind::*;
        let st = LiveState::of(&[
            s(Claude, "harbor", SessionStatus::Working, 3),
            s(Codex, "lighthouse", SessionStatus::NeedsApproval, 1),
            s(Antigravity, "beacon", SessionStatus::Done, 9),
            s(Claude, "dotfiles", SessionStatus::Working, 5),
        ]);
        assert_eq!((st.working, st.needs), (2, 1));
        let rows: Vec<_> = st
            .rows
            .iter()
            .map(|r| (r.agent.as_str(), r.repo.as_str(), r.status.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("Codex", "lighthouse", "needs_approval"),
                ("Claude", "dotfiles", "working"),
                ("Claude", "harbor", "working")
            ]
        );
        let p = payload("update", &st, 100).to_string();
        assert!(
            !p.contains("secret"),
            "no titles or activity in the push: {p}"
        );
        assert!(p.contains("\"alert\""), "something needs you: it lights up");
        let start = payload("start", &st, 100);
        assert_eq!(start["aps"]["attributes-type"], "StyrmanActivityAttributes");
        assert_eq!(
            start["aps"]["alert"]["body"],
            "Codex · lighthouse needs you"
        );
        assert!(LiveState::of(&[]).is_idle());
    }
}
