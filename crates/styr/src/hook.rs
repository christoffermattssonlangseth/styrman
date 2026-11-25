//! `styr hook <agent> <event>`: the helper every agent hook runs.
//!
//! Contract (do not break): it exits 0 and prints either nothing or one JSON object
//! from the daemon. Exit code 2 means "block" to Claude Code, so bad arguments, a
//! missing daemon, malformed stdin, timeouts and panics all end in a silent exit 0,
//! and the agent behaves exactly as without Styrman.
//!
//! The one exception: when the daemon explicitly answers with exit code 2 (an
//! `asyncRewake` hook delivering a prompt from the phone), it exits 2 with that stderr.

use std::io::{Read, Write};
use std::time::{Duration, Instant};
use styr_core::adapter::RawHook;
use styr_core::ids::AgentKind;
use styr_core::ipc::{Client, HookOutput, IpcError, MAX_FRAME_BYTES, forward_hook};
use styr_core::paths::Paths;

/// Hooks that wait in the background for the phone (`asyncRewake`); they reconnect
/// if the daemon restarts mid-wait instead of giving up.
const WAIT_EVENTS: &[&str] = &["StopWait"];
const RECONNECT_FOR: Duration = Duration::from_secs(3600);

pub fn main(args: &[String]) -> ! {
    let debug = std::env::var_os("STYR_DEBUG").is_some();
    let out = std::panic::catch_unwind(|| run(args));
    let mut code = 0;
    match out {
        Ok(Ok(out)) => {
            if let Some(v) = out.stdout {
                let mut lock = std::io::stdout().lock();
                let _ = lock.write_all(v.to_string().as_bytes());
                let _ = lock.flush();
            }
            // Only exit 2 is ever passed through, and only on the daemon's say-so.
            if let Some(exit) = out.exit.filter(|e| e.code == 2) {
                let _ = std::io::stderr().write_all(exit.stderr.as_bytes());
                code = 2;
            }
        }
        Ok(Err(e)) if debug => eprintln!("styr hook: {e}"),
        _ => {}
    }
    std::process::exit(code)
}

fn run(args: &[String]) -> Result<HookOutput, String> {
    let [agent, event, ..] = args else {
        return Err("usage: styr hook <agent> <event>".into());
    };
    let agent: AgentKind = agent.parse()?;
    let mut buf = Vec::new();
    std::io::stdin()
        .take(MAX_FRAME_BYTES as u64)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    let payload: serde_json::Value =
        serde_json::from_slice(&buf).map_err(|e| format!("stdin: {e}"))?;

    if let Some(path) = std::env::var_os("STYR_HOOK_RECORD") {
        record(&path, agent, event, &payload);
    }

    // Who is the agent process (and which version)? Only looked up where it can change.
    let lookup = matches!(event.as_str(), "SessionStart" | "UserPromptSubmit");
    let agent_pid = if lookup {
        // CLAUDE_PID is inherited by anything started from a Claude session (e.g. agy
        // run from Claude's shell), so only trust it for Claude's own hooks.
        let claude_pid = (agent == AgentKind::Claude)
            .then(|| {
                std::env::var("CLAUDE_PID")
                    .ok()
                    .and_then(|p| p.parse().ok())
            })
            .flatten();
        claude_pid.or_else(find_agent_pid)
    } else {
        None
    };
    let agent_version = (event == "SessionStart")
        .then(|| agent_pid.and_then(version_of_pid))
        .flatten();
    let hook = RawHook {
        agent,
        event: event.clone(),
        payload,
        launched_by_styr: std::env::var_os("STYR_LAUNCHED").is_some(),
        agent_pid,
        launch_id: std::env::var("STYR_LAUNCH_ID")
            .ok()
            .filter(|s| !s.is_empty()),
        agent_version,
    };
    // No client-side timeout: the agent's own per-hook timeout bounds us, and a
    // permission hook may legitimately wait for the phone.
    let client = Client::new(Paths::resolve().socket).with_timeout(None);
    if !WAIT_EVENTS.contains(&event.as_str()) {
        return forward_hook(&client, hook).map_err(|e| e.to_string());
    }
    // A background wait: if the daemon goes away mid-wait (restart), reconnect.
    // A daemon that is not running at the start means "no Styrman": give up at once.
    let started = Instant::now();
    loop {
        match forward_hook(&client, hook.clone()) {
            Ok(out) => return Ok(out),
            Err(IpcError::NotRunning(_)) if started.elapsed() < Duration::from_millis(500) => {
                return Err("daemon not running".into());
            }
            Err(e) if started.elapsed() > RECONNECT_FOR => return Err(e.to_string()),
            Err(_) => std::thread::sleep(Duration::from_secs(2)),
        }
    }
}

/// The agent process that ran this hook: our parent, skipping shell wrappers
/// (`sh -c`, the plugin's `styr-hook` shim).
fn find_agent_pid() -> Option<u32> {
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..4 {
        let out = std::process::Command::new("ps")
            .args(["-o", "ppid=,comm=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let line = String::from_utf8_lossy(&out.stdout);
        let (ppid, comm) = line.trim().split_once(char::is_whitespace)?;
        let name = comm.trim().rsplit('/').next()?.trim_start_matches('-');
        if matches!(name, "sh" | "bash" | "zsh" | "dash" | "styr-hook" | "env") {
            pid = ppid.trim().parse().ok()?;
            continue;
        }
        return (pid > 1).then_some(pid);
    }
    None
}

/// The agent's version from its executable's real path: Claude Code installs to
/// `…/claude/versions/<version>` and `claude` is a symlink to it. No process is run.
fn version_of_pid(pid: u32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .args(["-o", "comm=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let exe = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let real = std::fs::canonicalize(&exe).ok()?;
    version_from_path(&real).or_else(|| version_from_cli(&real))
}

/// `<agent> --version` (agy's binary isn't versioned by path); first dotted number.
fn version_from_cli(exe: &std::path::Path) -> Option<String> {
    if exe.file_name()? != "agy" {
        return None;
    }
    let out = std::process::Command::new(exe)
        .arg("--version")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .find(|w| {
            w.split('.').count() >= 2
                && w.split('.')
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
        .map(str::to_owned)
}

pub fn version_from_path(p: &std::path::Path) -> Option<String> {
    let name = p.file_name()?.to_str()?;
    let looks_like_version = name.split('.').count() >= 2
        && name
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
    looks_like_version.then(|| name.to_owned())
}

/// Append the raw invocation to a JSONL file (for building fixtures / debugging).
fn record(path: &std::ffi::OsStr, agent: AgentKind, event: &str, payload: &serde_json::Value) {
    let line = serde_json::json!({"agent": agent, "event": event, "payload": payload});
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}
