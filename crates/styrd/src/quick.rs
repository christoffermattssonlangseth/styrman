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
