//! Replies as they're written: while a shared Claude or Codex session is working,
//! follow its transcript file and pass on each piece of text the agent writes
//! (between tool calls, and the answer itself) as soon as it's in the file, instead
//! of only the last message when the turn ends.
//!
//! Hooks still bring the final message at the end of the turn; `Daemon::ingest`
//! drops a message whose text this turn already brought, from either side.
//! Only text the agent writes is read (no tool output, nothing from subagents), and
//! only from where the file stood when following began: history isn't replayed.

use crate::daemon::Daemon;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use styr_core::event::{EventKind, EventSource, TextMode};
use styr_core::ids::MessageId;
use styr_core::ids::{AgentKind, SessionId};
use styr_core::ipc::SessionUpdate;
use tokio::sync::watch;

const TICK: Duration = Duration::from_millis(500);
/// Most read from a transcript in one go (a huge paste or tool dump in between).
const MAX_READ: u64 = 4 * 1024 * 1024;

/// A session to follow (see `Daemon::follow_targets`).
pub struct Target {
    pub id: SessionId,
    pub native: styr_core::ids::NativeSessionRef,
    pub path: std::path::PathBuf,
    /// Working or asking: read what's new. Otherwise just keep up with the file.
    pub active: bool,
}

/// Where each followed transcript has been read up to.
#[derive(Default)]
pub struct Follower {
    offsets: std::sync::Mutex<HashMap<SessionId, u64>>,
}

impl Follower {
    fn offsets(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, u64>> {
        self.offsets.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Read what's new in one session's transcript and pass it on.
    pub fn step(&self, daemon: &Daemon, t: &Target) {
        let Ok(len) = std::fs::metadata(&t.path).map(|m| m.len()) else {
            return;
        };
        let from = {
            let mut offsets = self.offsets();
            match offsets.get(&t.id).copied() {
                // First sight: start at the end (no replaying history).
                None => {
                    offsets.insert(t.id.clone(), len);
                    return;
                }
                Some(f) if len <= f || !t.active => {
                    offsets.insert(t.id.clone(), len);
                    return;
                }
                Some(f) => f,
            }
        };
        let (texts, ended, consumed) = match read_new(&t.path, from, t.native.agent) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(session = %t.id, "reading transcript: {e:#}");
                return;
            }
        };
        {
            // Another reader (the loop, or a hook catching up) may have gone further.
            let mut offsets = self.offsets();
            let at = offsets.entry(t.id.clone()).or_insert(from);
            if *at != from {
                return;
            }
            *at = from + consumed;
        }
        if texts.is_empty() && ended.is_empty() {
            return;
        }
        let events = texts
            .into_iter()
            .map(|(key, text)| EventKind::AssistantMessage {
                message: MessageId(format!("{}:t:{key}", t.native.native_id)),
                text,
                mode: TextMode::Final,
            })
            .chain(ended)
            .collect();
        if let Err(e) = daemon.ingest(
            &SessionUpdate::for_native(t.native.clone()),
            EventSource::Transcript,
            events,
        ) {
            tracing::warn!(session = %t.id, "passing on transcript text: {e:#}");
        }
    }

    /// Before a hook's events (a tool starting, the turn ending): pass on the text
    /// written before them first, so it shows above them.
    pub fn catch_up(&self, daemon: &Daemon, session: &SessionId) {
        if let Ok(targets) = daemon.follow_targets()
            && let Some(t) = targets.iter().find(|t| &t.id == session)
        {
            self.step(daemon, t);
        }
    }
}

pub async fn run(daemon: Arc<Daemon>, mut stop: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = stop.wait_for(|v| *v) => return,
            _ = tokio::time::sleep(TICK) => {}
        }
        let targets = match daemon.follow_targets() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("listing transcripts to follow: {e:#}");
                continue;
            }
        };
        daemon
            .follower
            .offsets()
            .retain(|id, _| targets.iter().any(|t| &t.id == id));
        for t in &targets {
            daemon.follower.step(&daemon, t);
        }
    }
}

/// What's new in the complete lines after `from`: the agent's text, background tasks
/// that finished, and how many bytes the lines took.
type NewLines = (Vec<(String, String)>, Vec<EventKind>, u64);

fn read_new(path: &Path, from: u64, agent: AgentKind) -> std::io::Result<NewLines> {
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::new();
    f.take(MAX_READ).read_to_end(&mut buf)?;
    // Only whole lines: the last one may still be being written.
    let Some(end) = buf.iter().rposition(|&b| b == b'\n') else {
        // One line longer than MAX_READ: skip it rather than stall.
        return Ok((
            vec![],
            vec![],
            if buf.len() as u64 >= MAX_READ {
                buf.len() as u64
            } else {
                0
            },
        ));
    };
    let lines: Vec<serde_json::Value> = buf[..end]
        .split(|&b| b == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect();
    let texts = lines
        .iter()
        .flat_map(|v| match agent {
            AgentKind::Claude => claude_text(v),
            AgentKind::Codex => codex_text(v),
            _ => vec![],
        })
        .collect();
    let ended = match agent {
        AgentKind::Claude => lines.iter().flat_map(claude_tasks_ended).collect(),
        _ => vec![],
    };
    Ok((texts, ended, end as u64 + 1))
}

/// Claude Code: a background task's notification, queued while the agent works:
/// `{"type":"queue-operation","operation":"enqueue","content":"<task-notification>…"}`.
/// (When it's idle, the notification starts a turn and arrives through the hook.)
fn claude_tasks_ended(v: &serde_json::Value) -> Vec<EventKind> {
    if v["type"] != "queue-operation" || v["operation"] != "enqueue" {
        return vec![];
    }
    v["content"]
        .as_str()
        .map(styr_adapter_claude::task_notifications)
        .unwrap_or_default()
}

/// Claude Code: `{"type":"assistant","uuid":…,"message":{"content":[{"type":"text","text":…}]}}`,
/// one content block per line. Subagents' lines (`isSidechain`) are theirs, not the session's.
fn claude_text(v: &serde_json::Value) -> Vec<(String, String)> {
    if v["type"] != "assistant" || v["isSidechain"] == true {
        return vec![];
    }
    let uuid = v["uuid"].as_str().unwrap_or_default();
    let Some(blocks) = v["message"]["content"].as_array() else {
        return vec![];
    };
    blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| b["type"] == "text")
        .filter_map(|(i, b)| {
            let text = b["text"].as_str()?.trim();
            (!text.is_empty()).then(|| (format!("{uuid}:{i}"), text.to_owned()))
        })
        .collect()
}

/// Codex rollout: `{"type":"response_item","ordinal":…,"payload":{"type":"message",
/// "role":"assistant","content":[{"type":"output_text","text":…}]}}`, both the
/// in-between updates ("commentary") and the final answer.
fn codex_text(v: &serde_json::Value) -> Vec<(String, String)> {
    let p = &v["payload"];
    if v["type"] != "response_item" || p["type"] != "message" || p["role"] != "assistant" {
        return vec![];
    }
    let text: String = p["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "output_text")
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let text = text.trim();
    if text.is_empty() {
        return vec![];
    }
    let key = match &v["ordinal"] {
        serde_json::Value::Number(n) => n.to_string(),
        _ => p["id"].as_str().unwrap_or("msg").to_owned(),
    };
    vec![(key, text.to_owned())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn claude_text_blocks_only_and_not_subagents() {
        let line = json!({"type": "assistant", "uuid": "u1", "message": {"role": "assistant",
            "content": [{"type": "text", "text": "Looking at the parser first."}]}});
        assert_eq!(
            claude_text(&line),
            [("u1:0".to_owned(), "Looking at the parser first.".to_owned())]
        );
        let tool = json!({"type": "assistant", "uuid": "u2", "message": {"content": [{"type": "tool_use", "name": "Bash"}]}});
        assert!(claude_text(&tool).is_empty());
        let thinking = json!({"type": "assistant", "uuid": "u3", "message": {"content": [{"type": "thinking", "thinking": "hm"}]}});
        assert!(claude_text(&thinking).is_empty());
        let sub = json!({"type": "assistant", "uuid": "u4", "isSidechain": true,
            "message": {"content": [{"type": "text", "text": "subagent says"}]}});
        assert!(claude_text(&sub).is_empty());
        let user = json!({"type": "user", "message": {"content": "hi"}});
        assert!(claude_text(&user).is_empty());
    }

    #[test]
    fn codex_commentary_and_answer() {
        let c = json!({"type": "response_item", "ordinal": 41, "payload": {"type": "message", "role": "assistant",
            "phase": "commentary", "id": "msg_1", "content": [{"type": "output_text", "text": "Checking the tests."}]}});
        assert_eq!(
            codex_text(&c),
            [("41".to_owned(), "Checking the tests.".to_owned())]
        );
        let user = json!({"type": "response_item", "payload": {"type": "message", "role": "user",
            "content": [{"type": "input_text", "text": "hi"}]}});
        assert!(codex_text(&user).is_empty());
        let tool = json!({"type": "response_item", "payload": {"type": "custom_tool_call", "name": "apply_patch"}});
        assert!(codex_text(&tool).is_empty());
    }

    #[test]
    fn background_tasks_that_finish() {
        let q = json!({"type": "queue-operation", "operation": "enqueue",
            "content": "<task-notification>\n<task-id>b1</task-id>\n<tool-use-id>toolu_1</tool-use-id>\n<status>completed</status>\n<summary>Background command \"Build\" completed (exit code 0)</summary>\n</task-notification>"});
        assert_eq!(
            claude_tasks_ended(&q),
            vec![EventKind::TaskEnded {
                call: styr_core::ids::ToolCallId("toolu_1".into()),
                status: "completed".into(),
                summary: Some("Background command \"Build\" completed (exit code 0)".into()),
            }]
        );
        // Taken off the queue again: not a second ending.
        let mut removed = q.clone();
        removed["operation"] = json!("remove");
        assert!(claude_tasks_ended(&removed).is_empty());
        assert!(
            claude_tasks_ended(&json!({"type": "user", "message": {"content": "hi"}})).is_empty()
        );
    }

    #[test]
    fn reads_whole_lines_and_resumes_after_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        let a = json!({"type": "assistant", "uuid": "a", "message": {"content": [{"type": "text", "text": "One."}]}});
        let b = json!({"type": "assistant", "uuid": "b", "message": {"content": [{"type": "text", "text": "Two."}]}});
        // The second line isn't finished yet.
        write!(f, "{a}\n{}", &b.to_string()[..10]).unwrap();
        let (texts, _, used) = read_new(&path, 0, AgentKind::Claude).unwrap();
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].1, "One.");
        writeln!(f, "{}", &b.to_string()[10..]).unwrap();
        let (texts, _, _) = read_new(&path, used, AgentKind::Claude).unwrap();
        assert_eq!(
            texts.iter().map(|t| t.1.as_str()).collect::<Vec<_>>(),
            ["Two."]
        );
    }
}
