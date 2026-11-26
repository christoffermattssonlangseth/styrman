//! Codex CLI adapter (hooks + the `styr codex` terminal wrapper).
//!
//! Measured on Codex CLI 0.172.0 (`tests/fixtures/`):
//! - Codex has Claude-style command hooks (`~/.codex/hooks.json`, feature `hooks`, on
//!   by default): SessionStart, UserPromptSubmit, PreToolUse, PermissionRequest,
//!   PostToolUse, Stop, SessionEnd. Payloads are snake_case and close to Claude's
//!   (`tool_name: "Bash"`, `tool_use_id`, `turn_id`, `last_assistant_message`).
//! - New or changed hooks must be trusted once in Codex ("Trust all and continue");
//!   cmux passes its own hooks with `-c` and still runs ours from hooks.json.
//! - **PermissionRequest runs before Codex's own approval menu, not beside it**: while
//!   a hook waits, the terminal shows nothing. So the hook never waits; `styr codex`
//!   reads the menu off the screen and the phone answers it with keystrokes, as
//!   with agy. The first answer wins.
//! - The app-server protocol isn't used: cmux's `-c` hook options make Codex run in
//!   embedded mode, where no second client can attach.
//! - The TUI rejects unknown slash commands before any hook, so `/styr` is caught by
//!   `styr codex` as you type it.

use serde_json::{Value, json};
use styr_adapter_claude::tools::tool_info;
use styr_core::adapter::*;
use styr_core::event::*;
use styr_core::ids::*;
use styr_core::ipc::SessionUpdate;

/// Longest tool output kept in an event.
pub const MAX_OUTPUT_CHARS: usize = 8 * 1024;

#[derive(Debug, Clone, Default)]
pub struct CodexAdapter;

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Codex calls its file editor `apply_patch`; everything else maps like Claude's tools.
fn codex_tool(name: &str, input: Value) -> ToolInfo {
    match name {
        "apply_patch" | "ApplyPatch" => {
            let patch = input["patch"]
                .as_str()
                .or(input["input"].as_str())
                .or(input["command"].as_str())
                .or(input.as_str())
                .unwrap_or("");
            let files: Vec<String> = patch
                .lines()
                .filter_map(|l| {
                    ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
                        .iter()
                        .find_map(|p| l.strip_prefix(p))
                })
                .map(str::to_owned)
                .collect();
            ToolInfo {
                name: name.to_owned(),
                kind: ToolKind::Edit,
                title: match files.as_slice() {
                    [one] => format!("Edit {one}"),
                    [] => "Edit files".to_owned(),
                    many => format!("Edit {} files", many.len()),
                },
                locations: files.iter().map(Into::into).collect(),
                input,
            }
        }
        _ => tool_info(name, input),
    }
}

fn output_text(v: &Value) -> Option<String> {
    let text = match v {
        Value::Null => return None,
        Value::String(s) => s.clone(),
        Value::Object(o) => o
            .get("output")
            .or_else(|| o.get("stdout"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| v.to_string()),
        other => other.to_string(),
    };
    Some(text)
}

impl AgentAdapter for CodexAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // Both through `styr codex`'s terminal wrapper.
            prompt_delivery: PromptDelivery::Anytime,
            permissions: PermissionMode::Concurrent,
            streaming_text: false,
            interrupt: false,
        }
    }

    fn native_session(&self, hook: &RawHook) -> Option<NativeSessionRef> {
        s(&hook.payload, "session_id")
            .filter(|id| !id.is_empty())
            .map(|id| NativeSessionRef::new(AgentKind::Codex, id))
    }

    fn on_hook(&self, hook: &RawHook, view: &SessionView) -> Result<HookPlan, AdapterError> {
        let p = &hook.payload;
        if !p.is_object() {
            return Err(AdapterError::BadPayload("payload is not an object".into()));
        }
        let cwd = s(p, "cwd");
        let mut plan = HookPlan {
            session: Some(SessionUpdate {
                native: Some(view.native.clone()),
                cwd: cwd.clone().map(Into::into),
                transcript_path: s(p, "transcript_path").map(Into::into),
                model: s(p, "model"),
                title: None,
                agent_pid: hook.agent_pid,
                launch_id: hook.launch_id.clone(),
            }),
            ..Default::default()
        };
        let turn = s(p, "turn_id").map(TurnId);
        let ev = &mut plan.events;
        match hook.event.as_str() {
            "SessionStart" => ev.push(EventKind::SessionStarted {
                agent: AgentKind::Codex,
                native: view.native.clone(),
                cwd: cwd.map(Into::into),
                workspace_roots: vec![],
                model: s(p, "model"),
                title: None,
                start: match p["source"].as_str() {
                    Some("resume") => StartKind::Resume,
                    Some("fork") => StartKind::Fork,
                    Some("clear") => StartKind::Clear,
                    Some("compact") => StartKind::Compact,
                    _ => StartKind::New,
                },
            }),
            "UserPromptSubmit" => {
                let prompt = s(p, "prompt").unwrap_or_default();
                // The TUI rejects `/styr` itself; `codex exec "/styr"` would get here.
                if let Some(cmd) = parse_styr_command(&prompt) {
                    let on = cmd == StyrCommand::On;
                    plan.set_sharing = Some(on);
                    plan.reply = Some(HookDecision::Consume {
                        message: if on {
                            "Styrman: shared with your phone.".into()
                        } else {
                            "Styrman: stopped sharing with your phone.".into()
                        },
                    });
                } else {
                    ev.push(EventKind::TurnStarted { turn });
                    ev.push(EventKind::UserMessage {
                        text: prompt,
                        origin: Origin::Terminal,
                    });
                }
            }
            "PreToolUse" | "PostToolUse" => {
                let name = s(p, "tool_name").unwrap_or_else(|| "tool".into());
                let id = s(p, "tool_use_id")
                    .unwrap_or_else(|| format!("{}:{name}", view.native.native_id));
                let tool = codex_tool(&name, p["tool_input"].clone());
                if hook.event == "PreToolUse" {
                    ev.push(EventKind::ToolCall {
                        call: ToolCallId(id),
                        tool,
                        status: ToolStatus::Pending,
                        output: None,
                    });
                } else {
                    let output = output_text(&p["tool_response"])
                        .filter(|t| !t.is_empty())
                        .map(|text| {
                            let truncated = text.chars().count() > MAX_OUTPUT_CHARS;
                            ToolOutput::Text {
                                text: text.chars().take(MAX_OUTPUT_CHARS).collect(),
                                truncated,
                            }
                        });
                    let failed = p["tool_response"]["exit_code"]
                        .as_i64()
                        .is_some_and(|c| c != 0)
                        || p["tool_response"]["success"] == false;
                    ev.push(EventKind::ToolCall {
                        call: ToolCallId(id),
                        tool,
                        status: if failed {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Completed
                        },
                        output,
                    });
                }
            }
            // Codex shows its own menu only after this hook returns, so it never
            // waits; `styr codex` relays the menu from the screen.
            "PermissionRequest" => {}
            "Stop" => {
                let last = s(p, "last_assistant_message").filter(|t| !t.trim().is_empty());
                if let Some(text) = &last {
                    let id = match &turn {
                        Some(t) => format!("{}:{}", view.native.native_id, t.0),
                        None => format!("{}:last", view.native.native_id),
                    };
                    ev.push(EventKind::AssistantMessage {
                        message: MessageId(id),
                        text: text.clone(),
                        mode: TextMode::Final,
                    });
                }
                ev.push(EventKind::TurnEnded {
                    turn,
                    reason: TurnEndReason::EndTurn,
                    last_message: last,
                });
            }
            "SessionEnd" => ev.push(EventKind::SessionEnded {
                reason: s(p, "reason"),
            }),
            _ => {}
        }
        Ok(plan)
    }

    fn render_reply(&self, hook: &RawHook, decision: &HookDecision) -> Option<Value> {
        match (hook.event.as_str(), decision) {
            ("UserPromptSubmit", HookDecision::Consume { message }) => {
                Some(json!({"decision": "block", "reason": message}))
            }
            // Empty output: Codex carries on as if there were no hook.
            _ => None,
        }
    }

    fn verified_versions(&self) -> &'static [&'static str] {
        &["0.172.0"]
    }

    fn launcher(&self) -> LaunchSpec {
        LaunchSpec {
            program: "codex".into(),
            extra_args: vec![],
            env: vec![
                ("STYR_LAUNCHED".into(), "1".into()),
                ("STYR_WRAPPED".into(), "1".into()),
            ],
        }
    }
}
