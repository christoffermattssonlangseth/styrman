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

    let hook = RawHook {
        agent,
        event: event.clone(),
        payload,
        launched_by_styr: std::env::var_os("STYR_LAUNCHED").is_some(),
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
