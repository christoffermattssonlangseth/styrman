//! Fake-agent test harness.
//!
//! - [`TestDaemon`]: a real `styrd` running in-process on its own thread with a
//!   throwaway home directory; dropped → stopped.
//! - [`ClaudeScript`]: builds realistic Claude Code hook payloads for one session
//!   (shapes captured from Claude Code 2.1.332).
//! - [`HookRunner`]: delivers hooks exactly as an agent does, by spawning the
//!   `styr hook <agent> <event>` binary with the payload on stdin, and reports exit
//!   code and stdout.
//! - [`load_fixture`]: reads recorded `{"event", "payload"}` JSONL fixtures.

use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use styr_core::ids::AgentKind;
use styr_core::ipc::{Client, Request, Response, SessionSummary};
use styr_core::paths::Paths;

pub struct TestDaemon {
    pub paths: Paths,
    _dir: tempfile::TempDir,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
}

impl TestDaemon {
    pub fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::in_dir(dir.path());
        let mut d = Self {
            paths,
            _dir: dir,
            stop: None,
            thread: None,
        };
        d.spawn();
        d
    }

    /// A home directory for a daemon that is not running (for fail-open tests).
    pub fn stopped() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        Self {
            paths: Paths::in_dir(dir.path()),
            _dir: dir,
            stop: None,
            thread: None,
        }
    }

    fn spawn(&mut self) {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let paths = self.paths.clone();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?;
            rt.block_on(styrd::run_with(
                paths,
                styrd::api::ApiConfig::loopback_ephemeral(),
                async {
                    let _ = rx.await;
                },
            ))
        });
        for _ in 0..500 {
            if self.paths.socket.exists() {
                self.stop = Some(tx);
                self.thread = Some(thread);
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("styrd did not start");
    }

    /// Stop and start again on the same database.
    pub fn restart(&mut self) {
        self.shutdown();
        self.spawn();
    }

    pub fn shutdown(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            t.join()
                .expect("daemon thread panicked")
                .expect("daemon failed");
        }
    }

    pub fn client(&self) -> Client {
        Client::new(&self.paths.socket)
    }

    pub fn sessions(&self, include_unshared: bool) -> Vec<SessionSummary> {
        match self
            .client()
            .call(&Request::ListSessions { include_unshared })
            .expect("list")
        {
            Response::Sessions { sessions } => sessions,
            other => panic!("unexpected {other:?}"),
        }
    }

    pub fn session_by_native(&self, native_id: &str) -> Option<SessionSummary> {
        self.sessions(true)
            .into_iter()
            .find(|s| s.native_id == native_id)
    }

    /// Names of all persisted events of a session, in order.
    pub fn event_names(&self, native_id: &str) -> Vec<String> {
        self.events(native_id)
            .iter()
            .map(|e| e.kind.name().to_owned())
            .collect()
    }

    pub fn events(&self, native_id: &str) -> Vec<styr_core::Event> {
        let Some(s) = self.session_by_native(native_id) else {
            return vec![];
        };
        match self
            .client()
            .call(&Request::Events {
                session: s.id,
                since_seq: 0,
                limit: 5000,
            })
            .expect("events")
        {
            Response::Events { events } => events,
            other => panic!("unexpected {other:?}"),
        }
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Result of one hook process.
#[derive(Debug, Clone)]
pub struct HookOutcome {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl HookOutcome {
    pub fn json(&self) -> Option<Value> {
        let t = self.stdout.trim();
        (!t.is_empty()).then(|| serde_json::from_str(t).expect("hook stdout is not JSON"))
    }
}

/// Runs `styr hook` the way an agent would.
#[derive(Debug, Clone)]
pub struct HookRunner {
    pub styr_bin: PathBuf,
    pub paths: Paths,
    pub agent: AgentKind,
}

impl HookRunner {
    pub fn new(styr_bin: impl Into<PathBuf>, paths: &Paths, agent: AgentKind) -> Self {
        Self {
            styr_bin: styr_bin.into(),
            paths: paths.clone(),
            agent,
        }
    }

    pub fn run(&self, event: &str, payload: &Value) -> HookOutcome {
        self.run_raw(
            &[self.agent.as_str(), event],
            payload.to_string().as_bytes(),
        )
    }

    /// Run with arbitrary arguments after `hook` and arbitrary stdin bytes.
    pub fn run_raw(&self, args: &[&str], stdin: &[u8]) -> HookOutcome {
        let mut child = Command::new(&self.styr_bin)
            .arg("hook")
            .args(args)
            .env("STYR_HOME", &self.paths.data_dir)
            .env("STYR_SOCKET", &self.paths.socket)
            .env_remove("STYR_HOOK_RECORD")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn styr");
        child.stdin.take().unwrap().write_all(stdin).ok();
        let out = child.wait_with_output().expect("wait styr");
        HookOutcome {
            exit_code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

/// One recorded hook: `{"event": "...", "payload": {...}}`.
pub fn load_fixture(path: impl AsRef<Path>) -> Vec<(String, Value)> {
    let text = std::fs::read_to_string(path.as_ref()).expect("read fixture");
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("fixture line");
            (
                v["event"].as_str().expect("event").to_owned(),
                v["payload"].clone(),
            )
        })
        .collect()
}

/// Builder for one Claude Code session's hook payloads.
#[derive(Debug, Clone)]
pub struct ClaudeScript {
    pub session_id: String,
    pub cwd: String,
    prompt_n: u32,
    tool_n: u32,
}

impl ClaudeScript {
    pub fn new(session_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            cwd: cwd.into(),
            prompt_n: 0,
            tool_n: 0,
        }
    }

    fn base(&self, event: &str) -> Value {
        json!({
            "session_id": self.session_id,
            "transcript_path": format!("/Users/dev/.claude/projects/-x/{}.jsonl", self.session_id),
            "cwd": self.cwd,
            "hook_event_name": event,
            "permission_mode": "default",
            "prompt_id": format!("prompt-{}", self.prompt_n),
        })
    }

    fn with(&self, event: &str, extra: Value) -> (String, Value) {
        let mut v = self.base(event);
        if let (Some(o), Value::Object(e)) = (v.as_object_mut(), extra) {
            o.extend(e);
        }
        (event.to_owned(), v)
    }

    pub fn start(&self) -> (String, Value) {
        self.with(
            "SessionStart",
            json!({"source": "startup", "model": "claude-opus-5-5"}),
        )
    }

    pub fn prompt(&mut self, text: &str) -> (String, Value) {
        self.prompt_n += 1;
        self.with("UserPromptSubmit", json!({"prompt": text}))
    }

    pub fn next_tool_id(&mut self) -> String {
        self.tool_n += 1;
        format!("toolu_{:04}", self.tool_n)
    }

    pub fn pre_tool(&self, id: &str, name: &str, input: Value) -> (String, Value) {
        self.with(
            "PreToolUse",
            json!({"tool_name": name, "tool_input": input, "tool_use_id": id}),
        )
    }

    pub fn post_tool(
        &self,
        id: &str,
        name: &str,
        input: Value,
        response: Value,
    ) -> (String, Value) {
        self.with(
            "PostToolUse",
            json!({"tool_name": name, "tool_input": input, "tool_response": response, "tool_use_id": id, "duration_ms": 42}),
        )
    }

    pub fn permission(&self, name: &str, input: Value) -> (String, Value) {
        self.with(
            "PermissionRequest",
            json!({"tool_name": name, "tool_input": input, "permission_suggestions": [
                {"type": "addRules", "rules": [{"toolName": name}], "behavior": "allow", "destination": "localSettings"}
            ]}),
        )
    }

    pub fn notification(&self, message: &str, kind: &str) -> (String, Value) {
        self.with(
            "Notification",
            json!({"message": message, "notification_type": kind}),
        )
    }

    pub fn stop(&self, last_message: &str) -> (String, Value) {
        self.with(
            "Stop",
            json!({"stop_hook_active": false, "last_assistant_message": last_message}),
        )
    }

    pub fn end(&self) -> (String, Value) {
        self.with("SessionEnd", json!({"reason": "prompt_input_exit"}))
    }

    /// Bash tool call: PreToolUse + PostToolUse.
    pub fn bash(&mut self, command: &str, stdout: &str) -> Vec<(String, Value)> {
        let id = self.next_tool_id();
        let input = json!({"command": command, "description": command});
        vec![
            self.pre_tool(&id, "Bash", input.clone()),
            self.post_tool(
                &id,
                "Bash",
                input,
                json!({"stdout": stdout, "stderr": "", "interrupted": false}),
            ),
        ]
    }
}
