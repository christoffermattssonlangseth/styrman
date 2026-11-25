//! Antigravity CLI (`agy`) adapter.
//!
//! Hook payloads were captured from agy 1.5.17 (`tests/fixtures/`). Facts that shape it:
//! - Events that fire: SessionStart, PreInvocation / PostInvocation (around each model
//!   call; `invocationNum` 0 = a new user turn), PreToolUse, PostToolUse (only when a
//!   tool actually runs), Stop. Payloads are camelCase.
//! - A typed `/styr` must exist as a skill, else the TUI rejects it; it then reaches
//!   `PreInvocation` as the raw text, and `{"terminationBehavior":"terminate",
//!   "injectSteps":[{"errorMessage":…}]}` ends the turn without a model call.
//! - **Fail-open differs from Claude:** a PreToolUse reply of `{}` (no `decision`)
//!   DENIES the tool. Empty output is fine, so `PassThrough` always renders as nothing.
//! - Hooks carry no reply text or tool output; those come from the transcript JSONL
//!   (`PLANNER_RESPONSE.content`, and the step at a tool's `stepIdx`).
//! - Permission prompts have no hook; `styr agy`'s terminal wrapper sees agy's own
//!   prompt on screen and relays it (see `styr::pty`).

use serde_json::{Value, json};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use styr_core::adapter::*;
use styr_core::event::*;
use styr_core::ids::*;
use styr_core::ipc::SessionUpdate;

/// Longest tool output kept in an event.
pub const MAX_OUTPUT_CHARS: usize = 8 * 1024;
/// Only the tail of a long transcript is read.
const TRANSCRIPT_TAIL_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct AntigravityAdapter;

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// The transcript's records (tail only for long transcripts). Missing file → empty.
pub fn read_transcript(path: &Path) -> Vec<Value> {
    let Ok(mut f) = std::fs::File::open(path) else {
        return vec![];
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut skip_first = false;
    if len > TRANSCRIPT_TAIL_BYTES {
        let _ = f.seek(SeekFrom::Start(len - TRANSCRIPT_TAIL_BYTES));
        skip_first = true;
    }
    let mut text = String::new();
    if f.read_to_string(&mut text).is_err() {
        return vec![];
    }
    text.lines()
        .skip(usize::from(skip_first))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// agy writes the transcript next to `transcriptPath`, sometimes under a sibling name.
fn transcript_path(p: &Value) -> Option<PathBuf> {
    let path = PathBuf::from(s(p, "transcriptPath")?);
    if path.exists() {
        return Some(path);
    }
    let alt = path.with_file_name("transcript.jsonl");
    alt.exists().then_some(alt)
}

/// The model's last reply text in a transcript.
pub fn last_reply(records: &[Value]) -> Option<String> {
    records
        .iter()
        .rev()
        .find(|r| {
            r["type"] == "PLANNER_RESPONSE"
                && r["content"].as_str().is_some_and(|c| !c.trim().is_empty())
        })
        .and_then(|r| r["content"].as_str())
        .map(|c| c.trim().to_owned())
}

/// A tool step's output, without agy's "Created At / Completed At" header lines.
pub fn step_output(records: &[Value], step: i64) -> Option<String> {
    let r = records
        .iter()
        .find(|r| r["step_index"].as_i64() == Some(step))?;
    let raw = r["content"].as_str().or_else(|| r["error"].as_str())?;
    let body: Vec<&str> = raw
        .lines()
        .filter(|l| !l.starts_with("Created At:") && !l.starts_with("Completed At:"))
        .collect();
    let text = body.join("\n").trim().replace('\r', "");
    (!text.is_empty()).then_some(text)
}

fn short(text: &str) -> String {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= 120 {
        one
    } else {
        format!("{}…", one.chars().take(119).collect::<String>())
    }
}

/// Human summary and category for agy's tools.
pub fn tool_info(name: &str, args: Value) -> ToolInfo {
    let arg = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| args.get(*k).and_then(Value::as_str))
            .map(str::to_owned)
    };
    let path = arg(&[
        "TargetFile",
        "AbsolutePath",
        "Path",
        "File",
        "DirectoryPath",
    ]);
    let summary = arg(&["toolSummary"]);
    let kind = match name {
        "run_command" | "send_command_input" => ToolKind::Execute,
        n if n.contains("view") || n.starts_with("read") => ToolKind::Read,
        n if n.contains("write") || n.contains("replace") || n.contains("edit") => ToolKind::Edit,
        n if n.contains("search")
            || n.contains("find")
            || n.contains("list")
            || n.contains("grep") =>
        {
            ToolKind::Search
        }
        n if n.contains("url") || n.contains("web") || n.contains("browser") => ToolKind::Fetch,
        n if n.starts_with("mcp") => ToolKind::Mcp,
        _ => ToolKind::Other,
    };
    let title = match (kind, arg(&["CommandLine", "Command"]), &path) {
        (ToolKind::Execute, Some(cmd), _) => format!("Run `{}`", short(&cmd)),
        (ToolKind::Read, _, Some(p)) => format!("Read {p}"),
        (ToolKind::Edit, _, Some(p)) => format!("Edit {p}"),
        _ => summary
            .map(|s| short(&s))
            .unwrap_or_else(|| name.to_owned()),
    };
    ToolInfo {
        name: name.to_owned(),
        kind,
        title,
        input: args,
        locations: path.map(PathBuf::from).into_iter().collect(),
    }
}

/// Clear, model-proof wording for the reply that consumes `/styr`.
fn styr_message(on: bool) -> String {
    if on {
        "Styrman: this session is now shared with your phone. `/styr off` stops sharing.".into()
    } else {
        "Styrman: this session is no longer shared.".into()
    }
}

impl AgentAdapter for AntigravityAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Antigravity
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // Both through `styr agy`'s terminal wrapper.
            prompt_delivery: PromptDelivery::Anytime,
            permissions: PermissionMode::Concurrent,
            streaming_text: false,
            interrupt: false,
        }
    }

    fn native_session(&self, hook: &RawHook) -> Option<NativeSessionRef> {
        s(&hook.payload, "conversationId")
            .filter(|id| !id.is_empty())
            .map(|id| NativeSessionRef::new(AgentKind::Antigravity, id))
    }

    fn on_hook(&self, hook: &RawHook, view: &SessionView) -> Result<HookPlan, AdapterError> {
        let p = &hook.payload;
        if !p.is_object() {
            return Err(AdapterError::BadPayload("payload is not an object".into()));
        }
        let cwd = s(p, "cwd").or_else(|| {
            p["workspacePaths"]
                .get(0)
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        let mut plan = HookPlan {
            session: Some(SessionUpdate {
                native: Some(view.native.clone()),
                cwd: cwd.clone().map(Into::into),
                transcript_path: s(p, "transcriptPath").map(Into::into),
                model: s(p, "modelName"),
                title: None,
                agent_pid: hook.agent_pid,
            }),
            ..Default::default()
        };
        let ev = &mut plan.events;
        match hook.event.as_str() {
            "SessionStart" => ev.push(EventKind::SessionStarted {
                agent: AgentKind::Antigravity,
                native: view.native.clone(),
                cwd: cwd.map(Into::into),
                workspace_roots: p["workspacePaths"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(PathBuf::from)
                            .collect()
                    })
                    .unwrap_or_default(),
                model: s(p, "modelName"),
                title: None,
                start: StartKind::New,
            }),
            "PreInvocation" if p["invocationNum"].as_i64() == Some(0) => {
                let prompt = s(p, "lastUserInput").unwrap_or_default();
                if let Some(cmd) = parse_styr_command(&prompt) {
                    let on = cmd == StyrCommand::On;
                    plan.set_sharing = Some(on);
                    let message = if on && hook.launch_id.is_none() {
                        "Styrman: shared with your phone, read-only. To approve and send messages from the phone, start agy in a new terminal tab (after `styr setup`) or with `styr agy`.".to_owned()
                    } else {
                        styr_message(on)
                    };
                    plan.reply = Some(HookDecision::Consume { message });
                } else {
                    ev.push(EventKind::TurnStarted { turn: None });
                    ev.push(EventKind::UserMessage {
                        text: prompt,
                        origin: Origin::Terminal,
                    });
                }
            }
            "PreToolUse" | "PostToolUse" => {
                let call = &p["toolCall"];
                let name = call["name"].as_str().unwrap_or("tool").to_owned();
                let id = call["id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{}:{}", view.native.native_id, p["stepIdx"]));
                let tool = tool_info(&name, call["args"].clone());
                if hook.event == "PreToolUse" {
                    ev.push(EventKind::ToolCall {
                        call: ToolCallId(id),
                        tool,
                        status: ToolStatus::Pending,
                        output: None,
                    });
                } else {
                    let error = s(p, "error").filter(|e| !e.is_empty());
                    let output = match &error {
                        Some(e) => Some(e.clone()),
                        None => transcript_path(p)
                            .map(|t| read_transcript(&t))
                            .and_then(|r| p["stepIdx"].as_i64().and_then(|i| step_output(&r, i))),
                    }
                    .map(|text| {
                        let truncated = text.chars().count() > MAX_OUTPUT_CHARS;
                        ToolOutput::Text {
                            text: text.chars().take(MAX_OUTPUT_CHARS).collect(),
                            truncated,
                        }
                    });
                    ev.push(EventKind::ToolCall {
                        call: ToolCallId(id),
                        tool,
                        status: if error.is_some() {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Completed
                        },
                        output,
                    });
                }
            }
            "Stop" => {
                let reason = s(p, "terminationReason").unwrap_or_default();
                let error = s(p, "error").filter(|e| !e.is_empty());
                let our_intercept = reason == "TERMINAL_CUSTOM_HOOK"
                    && parse_styr_command(&s(p, "lastUserInput").unwrap_or_default()).is_some();
                let last = if our_intercept {
                    None
                } else {
                    transcript_path(p)
                        .map(|t| read_transcript(&t))
                        .and_then(|r| last_reply(&r))
                };
                if let Some(text) = &last {
                    let id = format!("{}:{}", view.native.native_id, p["executionNum"]);
                    ev.push(EventKind::AssistantMessage {
                        message: MessageId(id),
                        text: text.clone(),
                        mode: TextMode::Final,
                    });
                }
                if let Some(e) = &error {
                    ev.push(EventKind::Error {
                        message: e.clone(),
                        fatal: false,
                    });
                }
                ev.push(EventKind::TurnEnded {
                    turn: None,
                    reason: match (error.is_some(), reason.as_str()) {
                        (true, _) => TurnEndReason::Failed,
                        (_, r) if r.contains("MAX") => TurnEndReason::MaxSteps,
                        _ => TurnEndReason::EndTurn,
                    },
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
            // Never `{}` for PreToolUse: agy reads a reply without `decision` as deny.
            (_, HookDecision::PassThrough) => None,
            ("PreInvocation", HookDecision::Consume { message }) => Some(json!({
                "terminationBehavior": "terminate",
                "injectSteps": [{"errorMessage": {"userMessage": message}}]
            })),
            _ => None,
        }
    }

    fn verified_versions(&self) -> &'static [&'static str] {
        &["1.5.17"]
    }

    fn launcher(&self) -> LaunchSpec {
        LaunchSpec {
            program: "agy".into(),
            extra_args: vec![],
            env: vec![
                ("STYR_LAUNCHED".into(), "1".into()),
                ("STYR_WRAPPED".into(), "1".into()),
            ],
        }
    }
}
