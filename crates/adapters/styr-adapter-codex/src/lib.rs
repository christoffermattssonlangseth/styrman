//! Codex adapter (protocol path).
//!
//! Measured on Codex CLI 0.172.0 (`tests/fixtures/`):
//! - Every TUI session runs through a local app-server daemon (`daemon_auto_start`,
//!   `tui_app_server`). Its control socket (`~/.codex/app-server-control/
//!   app-server-control.sock`) speaks JSON-RPC over **WebSocket over a Unix socket**.
//! - A second client can `thread/resume` a loaded thread and receives its live
//!   notifications (`turn/*`, `item/*`, deltas). `turn/start` from that client shows
//!   up in the TUI as a normal prompt.
//! - Approvals are server requests sent to every subscribed client
//!   (`item/commandExecution/requestApproval`, …); the first answer wins and
//!   `serverRequest/resolved` is broadcast.
//! - In "code mode" (default for current models) tool runs are `custom_tool_call`s
//!   named `exec` whose input is a JS snippet calling `tools.exec_command`. They are
//!   **not** emitted as app-server items, only written to the rollout JSONL, so tool
//!   calls come from tailing the rollout file.
//! - The TUI rejects unknown slash commands, so `/styr` is caught by `styr codex`'s
//!   terminal wrapper as you type it.
//!
//! This crate holds the pure mapping; `styrd::codex` owns the connection.

use serde_json::{Value, json};
use std::path::PathBuf;
use styr_core::adapter::*;
use styr_core::event::*;
use styr_core::ids::*;

pub const MAX_OUTPUT_CHARS: usize = 8 * 1024;

#[derive(Debug, Clone, Default)]
pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            prompt_delivery: PromptDelivery::Anytime,
            permissions: PermissionMode::Concurrent,
            streaming_text: true,
            interrupt: true,
        }
    }

    fn verified_versions(&self) -> &'static [&'static str] {
        &["0.172.0"]
    }

    fn launcher(&self) -> LaunchSpec {
        LaunchSpec {
            program: "codex".into(),
            extra_args: vec![],
            env: vec![("STYR_LAUNCHED".into(), "1".into()), ("STYR_WRAPPED".into(), "1".into())],
        }
    }
}

/// The default control socket of the Codex app-server daemon.
pub fn control_socket() -> Option<PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")))?;
    Some(home.join("app-server-control/app-server-control.sock"))
}

pub fn initialize_request(id: u64) -> Value {
    json!({"method": "initialize", "id": id, "params": {
        "clientInfo": {"name": "styrman", "title": "Styrman", "version": styr_core::VERSION},
        "capabilities": {"experimentalApi": true}
    }})
}

fn text_of_user_message(item: &Value) -> String {
    item["content"]
        .as_array()
        .map(|parts| parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}

fn short(text: &str) -> String {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= 120 { one } else { format!("{}…", one.chars().take(119).collect::<String>()) }
}

fn truncate_output(text: String) -> ToolOutput {
    let truncated = text.chars().count() > MAX_OUTPUT_CHARS;
    ToolOutput::Text { text: text.chars().take(MAX_OUTPUT_CHARS).collect(), truncated }
}

/// Strip the `bash -lc '…'` wrapper Codex puts around commands.
pub fn display_command(cmd: &str) -> String {
    let c = cmd.trim();
    for sh in ["/bin/zsh -lc ", "/bin/bash -lc ", "bash -lc ", "zsh -lc ", "sh -c "] {
        if let Some(rest) = c.strip_prefix(sh) {
            let rest = rest.trim();
            let inner = rest.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')).unwrap_or(rest);
            return inner.to_owned();
        }
    }
    c.to_owned()
}

/// Map one app-server notification to (thread id, events).
pub fn map_notification(method: &str, params: &Value) -> Option<(String, Vec<EventKind>)> {
    let thread = params["threadId"].as_str().map(str::to_owned).or_else(|| params["thread"]["id"].as_str().map(str::to_owned))?;
    let mut ev = Vec::new();
    match method {
        "turn/started" => ev.push(EventKind::TurnStarted { turn: params["turn"]["id"].as_str().map(TurnId::from) }),
        "turn/completed" => {
            let turn = &params["turn"];
            let reason = match turn["status"].as_str() {
                Some("interrupted") => TurnEndReason::Interrupted,
                Some("failed") => TurnEndReason::Failed,
                _ => TurnEndReason::EndTurn,
            };
            if let Some(msg) = turn["error"]["message"].as_str() {
                ev.push(EventKind::Error { message: msg.to_owned(), fatal: false });
            }
            ev.push(EventKind::TurnEnded { turn: turn["id"].as_str().map(TurnId::from), reason, last_message: None });
        }
        "item/agentMessage/delta" => {
            let delta = params["delta"].as_str().unwrap_or_default();
            if !delta.is_empty() {
                ev.push(EventKind::AssistantMessage {
                    message: MessageId(params["itemId"].as_str().unwrap_or_default().to_owned()),
                    text: delta.to_owned(),
                    mode: TextMode::Delta,
                });
            }
        }
        "item/started" | "item/completed" => {
            let item = &params["item"];
            let done = method == "item/completed";
            match item["type"].as_str() {
                Some("userMessage") if done => {
                    ev.push(EventKind::UserMessage { text: text_of_user_message(item), origin: Origin::Terminal })
                }
                Some("agentMessage") if done => {
                    let text = item["text"].as_str().unwrap_or_default();
                    if !text.is_empty() {
                        ev.push(EventKind::AssistantMessage {
                            message: MessageId(item["id"].as_str().unwrap_or_default().to_owned()),
                            text: text.to_owned(),
                            mode: TextMode::Final,
                        });
                    }
                }
                Some("commandExecution") => {
                    let cmd = display_command(item["command"].as_str().unwrap_or_default());
                    let failed = item["exitCode"].as_i64().is_some_and(|c| c != 0) || item["status"] == "failed";
                    ev.push(EventKind::ToolCall {
                        call: ToolCallId(item["id"].as_str().unwrap_or_default().to_owned()),
                        tool: ToolInfo {
                            name: "commandExecution".into(),
                            kind: ToolKind::Execute,
                            title: format!("Run `{}`", short(&cmd)),
                            input: json!({"command": cmd, "cwd": item["cwd"]}),
                            locations: vec![],
                        },
                        status: match (done, failed) {
                            (false, _) => ToolStatus::Running,
                            (true, true) => ToolStatus::Failed,
                            (true, false) => ToolStatus::Completed,
                        },
                        output: item["aggregatedOutput"].as_str().filter(|_| done).map(|s| truncate_output(s.to_owned())),
                    });
                }
                Some("fileChange") => {
                    let paths: Vec<String> = item["changes"]
                        .as_array()
                        .map(|cs| cs.iter().filter_map(|c| c["path"].as_str().map(str::to_owned)).collect())
                        .unwrap_or_default();
                    ev.push(EventKind::ToolCall {
                        call: ToolCallId(item["id"].as_str().unwrap_or_default().to_owned()),
                        tool: ToolInfo {
                            name: "fileChange".into(),
                            kind: ToolKind::Edit,
                            title: match paths.as_slice() {
                                [one] => format!("Edit {one}"),
                                many => format!("Edit {} files", many.len()),
                            },
                            input: item["changes"].clone(),
                            locations: paths.iter().map(PathBuf::from).collect(),
                        },
                        status: if done { ToolStatus::Completed } else { ToolStatus::Running },
                        output: None,
                    });
                }
                _ => {}
            }
        }
        "turn/plan/updated" => {
            let entries = params["plan"]
                .as_array()
                .map(|ps| {
                    ps.iter()
                        .filter_map(|p| {
                            Some(PlanEntry {
                                content: p["step"].as_str().or(p["content"].as_str())?.to_owned(),
                                status: match p["status"].as_str() {
                                    Some("completed") => PlanStatus::Completed,
                                    Some("inProgress" | "in_progress") => PlanStatus::InProgress,
                                    _ => PlanStatus::Pending,
                                },
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            ev.push(EventKind::Plan { entries });
        }
        "thread/closed" => ev.push(EventKind::SessionEnded { reason: Some("closed".into()) }),
        _ => return None,
    }
    Some((thread, ev))
}

/// An approval the app-server asked for, normalized.
#[derive(Debug, Clone, PartialEq)]
pub struct Approval {
    pub thread: String,
    pub tool: ToolInfo,
    pub reason: Option<String>,
    pub options: Vec<PermissionOption>,
    /// The exact `decision` value to send back for each option, by option id.
    pub decisions: Vec<(String, Value)>,
}

/// One of Codex's `availableDecisions` as a phone option.
fn option_for(i: usize, d: &Value, command: &str) -> Option<PermissionOption> {
    let id = format!("d{i}");
    let (kind, label) = match d {
        Value::String(s) => match s.as_str() {
            "accept" | "approved" => (OptionKind::AllowOnce, "Allow once".to_owned()),
            "acceptForSession" | "approved_for_session" => (OptionKind::AllowAlways, "Allow for this session".to_owned()),
            "decline" | "denied" => (OptionKind::RejectOnce, "Deny".to_owned()),
            "cancel" | "abort" => (OptionKind::RejectOnce, "Deny and stop".to_owned()),
            _ => return None,
        },
        Value::Object(o) if o.contains_key("acceptWithExecpolicyAmendment") => {
            let rule = o["acceptWithExecpolicyAmendment"]["execpolicy_amendment"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| command.to_owned());
            (OptionKind::AllowAlways, format!("Always allow `{}`", short(&rule)))
        }
        Value::Object(o) if o.contains_key("applyNetworkPolicyAmendment") => {
            (OptionKind::AllowAlways, "Always allow this network access".to_owned())
        }
        _ => return None,
    };
    Some(PermissionOption { id, kind, label })
}

/// Map a server request (JSON-RPC with an `id`) to an approval, if it is one.
pub fn map_server_request(method: &str, params: &Value) -> Option<Approval> {
    let thread = params["threadId"].as_str()?.to_owned();
    let (tool, available) = match method {
        "item/commandExecution/requestApproval" | "execCommandApproval" => {
            let raw = params["command"].as_str().map(str::to_owned).unwrap_or_else(|| {
                params["command"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default()
            });
            let cmd = display_command(&raw);
            (
                ToolInfo {
                    name: "commandExecution".into(),
                    kind: ToolKind::Execute,
                    title: format!("Run `{}`", short(&cmd)),
                    input: json!({"command": cmd, "cwd": params["cwd"]}),
                    locations: vec![],
                },
                params["availableDecisions"].as_array().cloned(),
            )
        }
        "item/fileChange/requestApproval" | "applyPatchApproval" => (
            ToolInfo {
                name: "fileChange".into(),
                kind: ToolKind::Edit,
                title: params["grantRoot"].as_str().map(|r| format!("Write files under {r}")).unwrap_or_else(|| "Apply file changes".into()),
                input: params.clone(),
                locations: vec![],
            },
            None,
        ),
        _ => return None,
    };
    let available = available.unwrap_or_else(|| vec![json!("accept"), json!("acceptForSession"), json!("decline")]);
    let command = tool.input["command"].as_str().unwrap_or_default().to_owned();
    let mut options = Vec::new();
    let mut decisions = Vec::new();
    for (i, d) in available.iter().enumerate() {
        if let Some(o) = option_for(i, d, &command) {
            decisions.push((o.id.clone(), d.clone()));
            options.push(o);
        }
    }
    Some(Approval { thread, tool, reason: params["reason"].as_str().map(str::to_owned), options, decisions })
}

/// The JSON-RPC result answering an approval with the chosen option.
pub fn approval_result(approval: &Approval, option: &str) -> Option<Value> {
    approval.decisions.iter().find(|(id, _)| id == option).map(|(_, d)| json!({"decision": d}))
}

/// A tool call read from the rollout file (code mode).
#[derive(Debug, Clone, PartialEq)]
pub enum RolloutTool {
    Call { call: String, tool: ToolInfo },
    Output { call: String, text: String, failed: bool },
}

/// `tools.exec_command({cmd:"touch x", …})` → `touch x`.
fn command_in_script(script: &str) -> Option<String> {
    let i = script.find("cmd:")? + 4;
    let rest = script[i..].trim_start();
    let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\'' | '`'))?;
    let mut out = String::new();
    let mut escaped = false;
    for ch in rest[1..].chars() {
        match (escaped, ch) {
            (true, c) => {
                out.push(c);
                escaped = false;
            }
            (false, '\\') => escaped = true,
            (false, c) if c == quote => return Some(out),
            (false, c) => out.push(c),
        }
    }
    None
}

/// Parse one rollout JSONL line into a tool call or its output.
pub fn parse_rollout_line(line: &str) -> Option<RolloutTool> {
    let d: Value = serde_json::from_str(line).ok()?;
    let p = &d["payload"];
    let call = p["call_id"].as_str()?.to_owned();
    match p["type"].as_str()? {
        "custom_tool_call" | "function_call" => {
            let name = p["name"].as_str().unwrap_or("tool");
            let input = p["input"].as_str().or(p["arguments"].as_str()).unwrap_or_default();
            let tool = match command_in_script(input)
                .or_else(|| serde_json::from_str::<Value>(input).ok().and_then(|v| v["cmd"].as_str().or(v["command"].as_str()).map(str::to_owned)))
            {
                Some(cmd) => ToolInfo {
                    name: name.to_owned(),
                    kind: ToolKind::Execute,
                    title: format!("Run `{}`", short(&display_command(&cmd))),
                    input: json!({"command": cmd}),
                    locations: vec![],
                },
                None => ToolInfo { name: name.to_owned(), kind: ToolKind::Other, title: format!("Run {name}"), input: json!({"input": input}), locations: vec![] },
            };
            Some(RolloutTool::Call { call, tool })
        }
        "custom_tool_call_output" | "function_call_output" => {
            let text = match &p["output"] {
                Value::String(s) => s.clone(),
                Value::Array(parts) => parts.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join(""),
                other => other.to_string(),
            };
            let failed = text.contains("Operation not permitted") || text.contains("exit code: 1") || text.contains("Process exited with code 1");
            Some(RolloutTool::Output { call, text, failed })
        }
        _ => None,
    }
}

/// Rollout tool records as events (the call's title is remembered by the caller).
pub fn rollout_events(tool: RolloutTool, known: &mut std::collections::HashMap<String, ToolInfo>) -> Vec<EventKind> {
    match tool {
        RolloutTool::Call { call, tool } => {
            known.insert(call.clone(), tool.clone());
            vec![EventKind::ToolCall { call: ToolCallId(call), tool, status: ToolStatus::Running, output: None }]
        }
        RolloutTool::Output { call, text, failed } => {
            let tool = known.get(&call).cloned().unwrap_or(ToolInfo {
                name: "exec".into(),
                kind: ToolKind::Execute,
                title: "Run a command".into(),
                input: Value::Null,
                locations: vec![],
            });
            let body = text
                .lines()
                .filter(|l| !l.starts_with("Script completed") && !l.starts_with("Wall time") && l.trim() != "Output:")
                .collect::<Vec<_>>()
                .join("\n");
            vec![EventKind::ToolCall {
                call: ToolCallId(call),
                tool,
                status: if failed { ToolStatus::Failed } else { ToolStatus::Completed },
                output: Some(truncate_output(body.trim().to_owned())),
            }]
        }
    }
}
