//! Claude Code adapter: normalizes Claude Code hook payloads into Styrman events and
//! renders Styrman decisions back into Claude Code's hook output format.
//!
//! Payload shapes were captured from Claude Code 2.1.332 (`tests/fixtures/`) and checked
//! against <https://code.claude.com/docs/en/hooks>. Notable facts:
//! - `UserPromptSubmit` sees the raw `/styr` before slash-command expansion, and
//!   `{"decision":"block"}` stops it without a model call.
//! - `PermissionRequest` fires only when Claude would show a prompt; it carries no
//!   `tool_use_id` but does carry `permission_suggestions` (the "don't ask again" rules).
//! - `PostToolUse.tool_response` is an object for Bash (`stdout`, `stderr`, …).

mod tools;

use serde_json::{Value, json};
use std::time::Duration;
use styr_core::adapter::*;
use styr_core::event::*;
use styr_core::ids::*;
use styr_core::ipc::SessionUpdate;

pub use tools::tool_info;

/// Longest tool output kept in an event; the full output stays in Claude's transcript.
pub const MAX_OUTPUT_CHARS: usize = 8 * 1024;

#[derive(Debug, Clone)]
pub struct ClaudeConfig {
    /// How long the phone can answer a permission request. Claude Code shows its own
    /// prompt in the terminal at the same time (verified with 2.1.332), and whichever
    /// answer comes first wins, so this never delays the terminal. Must stay below the
    /// plugin's 600 s hook timeout.
    pub permission_timeout: Duration,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            permission_timeout: Duration::from_secs(540),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ClaudeAdapter {
    pub config: ClaudeConfig,
}

pub use styr_core::adapter::{StyrCommand, parse_styr_command};

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn truncate(text: String) -> (String, bool) {
    if text.chars().count() <= MAX_OUTPUT_CHARS {
        return (text, false);
    }
    (text.chars().take(MAX_OUTPUT_CHARS).collect(), true)
}

fn tool_output(resp: &Value) -> Option<ToolOutput> {
    let text = match resp {
        Value::Null => return None,
        Value::String(s) => s.clone(),
        Value::Object(o) if o.contains_key("stdout") || o.contains_key("stderr") => {
            let out = o.get("stdout").and_then(Value::as_str).unwrap_or_default();
            let err = o.get("stderr").and_then(Value::as_str).unwrap_or_default();
            match (out.is_empty(), err.is_empty()) {
                (_, true) => out.to_owned(),
                (true, false) => err.to_owned(),
                (false, false) => format!("{out}\n{err}"),
            }
        }
        other => {
            let compact = serde_json::to_string(other).unwrap_or_default();
            if compact.len() <= MAX_OUTPUT_CHARS {
                return Some(ToolOutput::Json {
                    value: other.clone(),
                });
            }
            compact
        }
    };
    let (text, truncated) = truncate(text);
    Some(ToolOutput::Text { text, truncated })
}

fn start_kind(source: Option<&str>) -> StartKind {
    match source {
        Some("resume") => StartKind::Resume,
        Some("clear") => StartKind::Clear,
        Some("compact") => StartKind::Compact,
        Some("fork") => StartKind::Fork,
        _ => StartKind::New,
    }
}

/// Close any permission requests still open: if Claude moved on (the tool ran, failed,
/// was denied, or the turn ended), they were answered in the terminal.
fn close_open_requests(view: &SessionView, events: &mut Vec<EventKind>) {
    for request in &view.open_requests {
        events.push(EventKind::PermissionResolved {
            request: request.clone(),
            outcome: PermissionOutcome::Cancelled,
            by: Resolver::Terminal,
        });
    }
}

/// Prompts Claude Code submits on its own, such as the wake-up turn after an
/// `asyncRewake` hook (`<task-notification>…Stop hook feedback…`).
pub fn is_internal_prompt(prompt: &str) -> bool {
    prompt.trim_start().starts_with("<task-notification>")
}

/// Event name the plugin passes for its background (asyncRewake) Stop hook.
pub const STOP_WAIT: &str = "StopWait";
/// How long that hook waits for a phone prompt; below the plugin's 86 400 s timeout.
pub const STOP_WAIT_TIMEOUT: Duration = Duration::from_secs(23 * 3600);

/// How a phone prompt reads to Claude when it is delivered through the Stop hook.
pub fn frame_phone_prompt(text: &str) -> String {
    format!(
        "The user sent this message from their phone via Styrman. Treat it as their next prompt and respond to it:\n\n{text}"
    )
}

pub const OPT_ALLOW: &str = "allow";
pub const OPT_ALWAYS: &str = "always";
pub const OPT_DENY: &str = "deny";

fn permission_options(payload: &Value) -> Vec<PermissionOption> {
    let mut opts = vec![PermissionOption {
        id: OPT_ALLOW.into(),
        kind: OptionKind::AllowOnce,
        label: "Allow once".into(),
    }];
    let has_suggestions = payload
        .get("permission_suggestions")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty());
    if has_suggestions {
        opts.push(PermissionOption {
            id: OPT_ALWAYS.into(),
            kind: OptionKind::AllowAlways,
            label: "Always allow".into(),
        });
    }
    opts.push(PermissionOption {
        id: OPT_DENY.into(),
        kind: OptionKind::RejectOnce,
        label: "Deny".into(),
    });
    opts
}

impl AgentAdapter for ClaudeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            prompt_delivery: PromptDelivery::WhenIdle,
            permissions: PermissionMode::Concurrent,
            streaming_text: false,
            interrupt: false,
        }
    }

    fn native_session(&self, hook: &RawHook) -> Option<NativeSessionRef> {
        s(&hook.payload, "session_id")
            .filter(|id| !id.is_empty())
            .map(|id| NativeSessionRef::new(AgentKind::Claude, id))
    }

    fn on_hook(&self, hook: &RawHook, view: &SessionView) -> Result<HookPlan, AdapterError> {
        let p = &hook.payload;
        if !p.is_object() {
            return Err(AdapterError::BadPayload("payload is not an object".into()));
        }
        let mut plan = HookPlan {
            session: Some(SessionUpdate {
                native: Some(view.native.clone()),
                cwd: s(p, "cwd").map(Into::into),
                transcript_path: s(p, "transcript_path").map(Into::into),
                model: s(p, "model"),
                title: s(p, "session_title"),
                agent_pid: hook.agent_pid,
                launch_id: hook.launch_id.clone(),
            }),
            ..Default::default()
        };
        let ev = &mut plan.events;
        match hook.event.as_str() {
            "SessionStart" => ev.push(EventKind::SessionStarted {
                agent: AgentKind::Claude,
                native: view.native.clone(),
                cwd: s(p, "cwd").map(Into::into),
                workspace_roots: vec![],
                model: s(p, "model"),
                title: s(p, "session_title"),
                start: start_kind(p.get("source").and_then(Value::as_str)),
            }),
            "UserPromptSubmit" => {
                let prompt = s(p, "prompt").unwrap_or_default();
                if let Some(cmd) = parse_styr_command(&prompt) {
                    let on = cmd == StyrCommand::On;
                    plan.set_sharing = Some(on);
                    plan.reply = Some(HookDecision::Consume {
                        message: if on {
                            "Styrman: this session is now shared with your phone. `/styr off` stops sharing.".into()
                        } else {
                            "Styrman: this session is no longer shared.".into()
                        },
                    });
                } else if is_internal_prompt(&prompt) {
                    // Claude Code's own wake-up turn (e.g. after our asyncRewake hook
                    // delivered a phone prompt): not something the user typed.
                    ev.push(EventKind::TurnStarted {
                        turn: s(p, "prompt_id").map(TurnId),
                    });
                } else {
                    close_open_requests(view, ev);
                    ev.push(EventKind::TurnStarted {
                        turn: s(p, "prompt_id").map(TurnId),
                    });
                    ev.push(EventKind::UserMessage {
                        text: prompt,
                        origin: Origin::Terminal,
                    });
                }
            }
            "PreToolUse" => {
                let (name, input) = (
                    s(p, "tool_name").unwrap_or_default(),
                    p.get("tool_input").cloned().unwrap_or(Value::Null),
                );
                if name == "TodoWrite"
                    && let Some(entries) = tools::todo_plan(&input)
                {
                    ev.push(EventKind::Plan { entries });
                }
                ev.push(EventKind::ToolCall {
                    call: ToolCallId(s(p, "tool_use_id").unwrap_or_default()),
                    tool: tool_info(&name, input),
                    status: ToolStatus::Pending,
                    output: None,
                });
            }
            "PermissionRequest" => {
                let request = RequestId::generate();
                let tool = tool_info(
                    &s(p, "tool_name").unwrap_or_default(),
                    p.get("tool_input").cloned().unwrap_or(Value::Null),
                );
                let timeout = self.config.permission_timeout;
                ev.push(EventKind::PermissionRequested {
                    request: request.clone(),
                    call: None,
                    tool,
                    reason: None,
                    options: permission_options(p),
                    expires_at_ms: Some(now_ms() + timeout.as_millis() as i64),
                });
                plan.wait = Some(WaitFor::Permission { request, timeout });
            }
            "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" => {
                close_open_requests(view, ev);
                let failed = hook.event != "PostToolUse";
                let output = if hook.event == "PostToolUse" {
                    p.get("tool_response").and_then(tool_output)
                } else {
                    let msg = s(p, "error")
                        .or_else(|| s(p, "reason"))
                        .unwrap_or_else(|| "denied".into());
                    Some(ToolOutput::Text {
                        text: msg,
                        truncated: false,
                    })
                };
                ev.push(EventKind::ToolCall {
                    call: ToolCallId(s(p, "tool_use_id").unwrap_or_default()),
                    tool: tool_info(
                        &s(p, "tool_name").unwrap_or_default(),
                        p.get("tool_input").cloned().unwrap_or(Value::Null),
                    ),
                    status: if failed {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Completed
                    },
                    output,
                });
            }
            "Notification" => {
                // "Waiting for your input" and "needs your permission" are already
                // conveyed by the session status and the permission card.
                let msg = s(p, "message").unwrap_or_default();
                let redundant = matches!(
                    p.get("notification_type").and_then(Value::as_str),
                    Some("idle_prompt" | "permission_prompt")
                ) || msg.contains("waiting for your input")
                    || msg.contains("needs your permission");
                if let (false, Some(text)) = (redundant, s(p, "message")) {
                    ev.push(EventKind::Notice {
                        level: Level::Info,
                        text,
                    });
                }
            }
            "Stop" => {
                close_open_requests(view, ev);
                let last = s(p, "last_assistant_message").filter(|m| !m.is_empty());
                if let Some(text) = &last {
                    let id = format!(
                        "{}:{}",
                        view.native.native_id,
                        s(p, "prompt_id").unwrap_or_default()
                    );
                    ev.push(EventKind::AssistantMessage {
                        message: MessageId(id),
                        text: text.clone(),
                        mode: TextMode::Final,
                    });
                }
                ev.push(EventKind::TurnEnded {
                    turn: s(p, "prompt_id").map(TurnId),
                    reason: TurnEndReason::EndTurn,
                    last_message: last,
                });
            }
            "StopFailure" => {
                close_open_requests(view, ev);
                let msg = s(p, "error")
                    .or_else(|| s(p, "message"))
                    .unwrap_or_else(|| "the turn failed".into());
                ev.push(EventKind::Error {
                    message: msg,
                    fatal: false,
                });
                ev.push(EventKind::TurnEnded {
                    turn: s(p, "prompt_id").map(TurnId),
                    reason: TurnEndReason::Failed,
                    last_message: None,
                });
            }
            "SessionEnd" => ev.push(EventKind::SessionEnded {
                reason: s(p, "reason"),
            }),
            // The plugin's second Stop hook (async + asyncRewake): wait in the
            // background for a prompt from the phone; exit 2 delivers it and wakes Claude.
            STOP_WAIT => {
                plan.session = None;
                plan.wait = Some(WaitFor::NextPrompt {
                    timeout: STOP_WAIT_TIMEOUT,
                });
            }
            _ => {}
        }
        Ok(plan)
    }

    fn render_reply(&self, hook: &RawHook, decision: &HookDecision) -> Option<Value> {
        match (hook.event.as_str(), decision) {
            (_, HookDecision::PassThrough) => None,
            ("UserPromptSubmit", HookDecision::Consume { message }) => {
                Some(json!({"decision": "block", "reason": message}))
            }
            ("PermissionRequest", HookDecision::Permission(outcome)) => match outcome {
                PermissionOutcome::Selected { kind, .. } if kind.is_allow() => {
                    let mut d = json!({"behavior": "allow"});
                    if *kind == OptionKind::AllowAlways
                        && let Some(sugg) = hook
                            .payload
                            .get("permission_suggestions")
                            .filter(|v| v.is_array())
                    {
                        d["updatedPermissions"] = sugg.clone();
                    }
                    Some(
                        json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": d}}),
                    )
                }
                PermissionOutcome::Selected { .. } => Some(json!({"hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {"behavior": "deny", "message": "Denied from the Styrman app."}
                }})),
                // No answer from the phone: let Claude show its own prompt.
                PermissionOutcome::Cancelled | PermissionOutcome::TimedOut => None,
            },
            ("Stop", HookDecision::Prompt(text)) => {
                Some(json!({"decision": "block", "reason": text}))
            }
            _ => None,
        }
    }

    fn verified_versions(&self) -> &'static [&'static str] {
        // Hook payloads, PermissionRequest concurrency, asyncRewake and terminal
        // typing were checked by hand against these (docs/design.md §8).
        &["2.1.332"]
    }

    fn render_exit(&self, hook: &RawHook, decision: &HookDecision) -> Option<(i32, String)> {
        match (hook.event.as_str(), decision) {
            (STOP_WAIT, HookDecision::Prompt(text)) => Some((2, frame_phone_prompt(text))),
            _ => None,
        }
    }

    fn launcher(&self) -> LaunchSpec {
        LaunchSpec {
            program: "claude".into(),
            extra_args: vec![],
            env: vec![
                ("STYR_LAUNCHED".into(), "1".into()),
                ("STYR_WRAPPED".into(), "1".into()),
            ],
        }
    }
}
