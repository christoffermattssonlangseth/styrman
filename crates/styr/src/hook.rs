//! `styr hook <agent> <event>`: the helper every agent hook runs.
//!
//! Contract (do not break): it ALWAYS exits 0, and prints either nothing or one
//! JSON object from the daemon. Exit code 2 would mean "block" to Claude Code, so
//! bad arguments, a missing daemon, malformed stdin, timeouts and panics all end
//! in a silent exit 0, and the agent behaves exactly as without Styrman.

use std::io::{Read, Write};
use styr_core::adapter::RawHook;
use styr_core::ids::AgentKind;
use styr_core::ipc::{Client, MAX_FRAME_BYTES, forward_hook};
use styr_core::paths::Paths;

pub fn main(args: &[String]) -> ! {
    let debug = std::env::var_os("STYR_DEBUG").is_some();
    let out = std::panic::catch_unwind(|| run(args));
    match out {
        Ok(Ok(Some(stdout))) => {
            let mut lock = std::io::stdout().lock();
            let _ = lock.write_all(stdout.as_bytes());
            let _ = lock.flush();
        }
        Ok(Ok(None)) => {}
        Ok(Err(e)) if debug => eprintln!("styr hook: {e}"),
        _ => {}
    }
    std::process::exit(0)
}

fn run(args: &[String]) -> Result<Option<String>, String> {
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
    match forward_hook(&client, hook) {
        Ok(Some(v)) => Ok(Some(v.to_string())),
        Ok(None) => Ok(None),
        Err(e) => Err(e.to_string()),
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
