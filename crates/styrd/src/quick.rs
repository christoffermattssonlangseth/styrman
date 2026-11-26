//! Answering an approval from the notification, without opening the app.
//!
//! The push itself carries no command (only "Claude · repo needs your approval" and
//! the request id). The phone's notification extension then asks this Mac directly,
//! over the tailnet, what the request is (`GET /v1/permission/{session}/{request}`), shows
//! it with Allow/Deny, and the answer comes back as `POST` to the same path. Both
//! need the device's bearer token, like the WebSocket.

use crate::daemon::Daemon;
use serde::{Deserialize, Serialize};
use styr_core::event::{EventKind, PermissionOption, ToolKind};
use styr_core::ids::{DeviceId, RequestId, SessionId};

#[derive(Debug, Serialize)]
pub struct PermissionDetails {
    pub open: bool,
    pub agent: String,
    pub repo: String,
    /// One line for the notification: the command for shell tools, else the tool's title.
    pub text: String,
    pub options: Vec<PermissionOption>,
}

/// A session in a few words, for a notification the phone rewrites when it arrives
/// (`GET /v1/brief/{session}`, over the tailnet: the push itself stays generic).
#[derive(Debug, Serialize, PartialEq)]
pub struct SessionBrief {
    pub agent: String,
    pub repo: String,
    /// Which session: your latest message to it, first line (slash commands skipped).
    pub asked: Option<String>,
    /// What it's doing or last said, first line.
    pub line: Option<String>,
    pub status: styr_core::SessionStatus,
}

/// One line, at most `max` characters.
fn one_line(text: &str, max: usize) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        out.push('…');
    }
    Some(out)
}

#[derive(Debug, Deserialize)]
pub struct AnswerBody {
    pub option: String,
}

impl Daemon {
    /// The open (or closed) request `request` of shared session `session`.
    pub fn permission_details(
        &self,
        session: &SessionId,
        request: &RequestId,
    ) -> anyhow::Result<Option<PermissionDetails>> {
        let Some(s) = self.summary(session)?.filter(|s| s.shared) else {
            return Ok(None);
        };
        let from = s.last_seq.saturating_sub(400);
        let found = self
            .events(session, from, 1000)?
            .into_iter()
            .rev()
            .find_map(|e| match e.kind {
                EventKind::PermissionRequested {
                    request: r,
                    tool,
                    options,
                    ..
                } if &r == request => Some((tool, options)),
                _ => None,
            });
        let Some((tool, options)) = found else {
            return Ok(None);
        };
        let text = match (
            tool.kind,
            tool.input.get("command").and_then(|c| c.as_str()),
        ) {
            (ToolKind::Execute, Some(cmd)) => cmd.to_owned(),
            _ => tool.title.clone(),
        };
        Ok(Some(PermissionDetails {
            open: self.is_open_request(session, request),
            agent: crate::peers::agent_label(s.agent).to_owned(),
            repo: s
                .cwd
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "session".into()),
            text,
            options,
        }))
    }

    /// A shared session in a few words (see `SessionBrief`).
    pub fn session_brief(&self, session: &SessionId) -> anyhow::Result<Option<SessionBrief>> {
        let Some(s) = self.summary(session)?.filter(|s| s.shared) else {
            return Ok(None);
        };
        let from = s.last_seq.saturating_sub(400);
        let asked = self
            .events(session, from, 1000)?
            .into_iter()
            .rev()
            .find_map(|e| match e.kind {
                EventKind::UserMessage { text, .. } if !text.trim_start().starts_with('/') => {
                    one_line(&text, 80)
                }
                _ => None,
            });
        let cwd = s.cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
        let line = s.activity.as_deref().and_then(|a| {
            let a = match &cwd {
                Some(c) => a.replace(&format!("{}/", c.trim_end_matches('/')), ""),
                None => a.to_owned(),
            };
            one_line(&a, 160)
        });
        Ok(Some(SessionBrief {
            agent: crate::peers::agent_label(s.agent).to_owned(),
            repo: s
                .cwd
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "session".into()),
            asked,
            line,
            status: s.status,
        }))
    }

    /// Answer a permission request for `device`: the terminal menu if the agent shows one,
    /// else the hook broker. Shared sessions only.
    pub fn answer_permission(
        &self,
        session: &SessionId,
        request: &RequestId,
        option: &str,
        device: DeviceId,
    ) -> Result<(), String> {
        let visible = self
            .summary(session)
            .map_err(|e| format!("{e:#}"))?
            .is_some_and(|s| s.shared);
        if !visible {
            return Err(format!("unknown session {session}"));
        }
        if let Some(r) = self.resolve_screen_permission(session, request, option, device.clone()) {
            return r.map_err(|e| format!("{e:#}"));
        }
        self.broker
            .resolve(session, request, option, device)
            .map_err(|e| e.to_string())
    }
}
