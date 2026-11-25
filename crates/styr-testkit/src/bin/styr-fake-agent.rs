//! Plays a scripted Claude Code session against a running styrd through the real
//! `styr hook` path. Useful for developing the phone app without a live agent.
//!
//!     styr-fake-agent [--styr PATH] [--delay-ms N] [--no-share] [--session ID] [--cwd DIR]
//!     styr-fake-agent --replay fixture.jsonl

use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use styr_core::ids::AgentKind;
use styr_core::paths::Paths;
use styr_testkit::{ClaudeScript, HookRunner, load_fixture};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut styr = None;
    let mut delay = 700u64;
    let mut share = true;
    let mut session = format!("fake-{}", std::process::id());
    let mut cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "/tmp".into());
    let mut replay: Option<PathBuf> = None;
    let mut wait_for_phone = true;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--styr" => styr = args.next().map(PathBuf::from),
            "--delay-ms" => delay = args.next().and_then(|v| v.parse().ok()).unwrap_or(delay),
            "--no-share" => share = false,
            "--session" => session = args.next().unwrap_or(session),
            "--cwd" => cwd = args.next().unwrap_or(cwd),
            "--replay" => replay = args.next().map(PathBuf::from),
            "--no-wait" => wait_for_phone = false,
            "-h" | "--help" => {
                eprintln!(
                    "usage: styr-fake-agent [--styr PATH] [--delay-ms N] [--no-share] [--session ID] [--cwd DIR] [--replay FILE]"
                );
                return;
            }
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let styr = styr.unwrap_or_else(|| {
        // Next to this binary (target/debug) or on PATH.
        let sibling = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("styr")));
        sibling
            .filter(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("styr"))
    });
    let runner = HookRunner::new(styr, &Paths::resolve(), AgentKind::Claude);

    let steps = match replay {
        Some(path) => load_fixture(path),
        None => demo(&session, &cwd, share),
    };
    for (event, payload) in steps {
        if event == "PermissionRequest" {
            permission_step(&runner, &payload, wait_for_phone);
            std::thread::sleep(Duration::from_millis(delay));
            continue;
        }
        let out = runner.run(&event, &payload);
        let reply = out.stdout.trim();
        println!(
            "{event:<18} exit={} {}",
            out.exit_code,
            if reply.is_empty() { "-" } else { reply }
        );
        std::thread::sleep(Duration::from_millis(delay));
    }
}

fn demo(session: &str, cwd: &str, share: bool) -> Vec<(String, serde_json::Value)> {
    let mut s = ClaudeScript::new(session, cwd);
    let mut steps = vec![s.start()];
    if share {
        steps.push(s.prompt("/styr"));
    }
    steps.push(s.prompt("The tests in crates/styrd are flaky, can you find out why?"));
    let todo = json!({"todos": [
        {"content": "Run the test suite", "status": "in_progress", "activeForm": "Running tests"},
        {"content": "Find the flaky test", "status": "pending", "activeForm": "Finding it"},
        {"content": "Fix it", "status": "pending", "activeForm": "Fixing it"}
    ]});
    let id = s.next_tool_id();
    steps.push(s.pre_tool(&id, "TodoWrite", todo.clone()));
    steps.push(s.post_tool(&id, "TodoWrite", todo, json!({"ok": true})));
    steps.extend(s.bash(
        "cargo test -p styrd",
        "test result: FAILED. 3 passed; 1 failed",
    ));
    let id = s.next_tool_id();
    let read = json!({"file_path": format!("{cwd}/crates/styrd/tests/daemon.rs")});
    steps.push(s.pre_tool(&id, "Read", read.clone()));
    steps.push(s.post_tool(
        &id,
        "Read",
        read,
        json!({"type": "text", "file": {"numLines": 180}}),
    ));
    let id = s.next_tool_id();
    let edit = json!({"file_path": format!("{cwd}/crates/styrd/tests/daemon.rs"), "old_string": "read_line", "new_string": "spawn_blocking"});
    steps.push(s.pre_tool(&id, "Edit", edit.clone()));
    steps.push(s.permission("Edit", edit.clone()));
    steps.push(s.notification(
        "Claude needs your permission to use Edit",
        "permission_prompt",
    ));
    steps.push(s.post_tool(
        &id,
        "Edit",
        edit,
        json!({"filePath": "crates/styrd/tests/daemon.rs"}),
    ));
    steps.extend(s.bash("cargo test -p styrd", "test result: ok. 4 passed; 0 failed"));
    steps.push(s.stop(
        "Found it: one test did a blocking socket read on the single-threaded test runtime, \
         starving the in-process daemon. I moved the read into spawn_blocking; all 4 tests pass.",
    ));
    steps
}

/// A permission hook blocks while the session is shared. Either wait for the phone to
/// answer (up to 2 minutes), or after a moment act like Claude does when the user
/// answers in the terminal: kill the hook.
fn permission_step(runner: &HookRunner, payload: &serde_json::Value, wait_for_phone: bool) {
    use std::io::Read;
    let mut child = runner.spawn("PermissionRequest", payload);
    let limit = if wait_for_phone {
        Duration::from_secs(120)
    } else {
        Duration::from_millis(1500)
    };
    if wait_for_phone {
        println!("PermissionRequest  waiting for an answer on the phone (2 min)…");
    }
    let start = std::time::Instant::now();
    while start.elapsed() < limit {
        if let Ok(Some(_)) = child.try_wait() {
            let mut out = String::new();
            if let Some(mut s) = child.stdout.take() {
                let _ = s.read_to_string(&mut out);
            }
            let reply = out.trim();
            println!(
                "PermissionRequest  answered: {}",
                if reply.is_empty() {
                    "(no answer: terminal prompt)"
                } else {
                    reply
                }
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    println!("PermissionRequest  answered in the (simulated) terminal");
}
