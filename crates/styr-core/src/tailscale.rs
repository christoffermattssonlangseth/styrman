//! The Tailscale CLI: its state and a restart (`tailscale down`, then `up`).
//!
//! Tailscale can break silently: the app reports Running while the Mac has lost
//! its tunnel address, so the phone can't reach styrd. A restart brings it back.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

const CANDIDATES: [&str; 4] = [
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    "/opt/homebrew/bin/tailscale",
    "/usr/local/bin/tailscale",
    "tailscale",
];

/// A command running the Tailscale CLI. The Mac app's binary acts as the CLI
/// only when it sees a terminal-ish environment (`TERM`): under launchd it
/// would otherwise try to start the app and fail ("The Tailscale GUI failed to start").
pub fn command(cli: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(cli);
    if std::env::var_os("TERM").is_none() {
        c.env("TERM", "dumb");
    }
    c
}

/// The Tailscale CLI, if one is installed.
pub fn cli() -> Option<PathBuf> {
    CANDIDATES.iter().find_map(|c| {
        let ok = command(c)
            .arg("version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        ok.then(|| PathBuf::from(c))
    })
}

/// What `tailscale status --json` says about this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// `Running`, `Stopped`, `NeedsLogin`, `Starting`, …
    pub backend: String,
    pub ips: Vec<String>,
    /// MagicDNS name without the trailing dot.
    pub dns_name: Option<String>,
    /// Tailscale's own health warnings.
    pub health: Vec<String>,
}

pub fn state(cli: &PathBuf) -> Option<State> {
    let out = command(cli)
        .args(["status", "--json"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    parse_state(&out.stdout)
}

fn parse_state(json: &[u8]) -> Option<State> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let strings = |v: &serde_json::Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(State {
        backend: v["BackendState"].as_str()?.to_owned(),
        ips: strings(&v["TailscaleIPs"]),
        dns_name: v["Self"]["DNSName"]
            .as_str()
            .map(|s| s.trim_end_matches('.').to_owned())
            .filter(|s| !s.is_empty()),
        health: strings(&v["Health"]),
    })
}

/// `tailscale down`, then `tailscale up`. Err carries what the CLI said.
pub fn restart(cli: &PathBuf) -> Result<(), String> {
    run(cli, "down")?;
    std::thread::sleep(Duration::from_secs(2));
    run(cli, "up")
}

fn run(cli: &PathBuf, verb: &str) -> Result<(), String> {
    let out = command(cli)
        .arg(verb)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("tailscale {verb}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let msg = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    Err(format!("tailscale {verb} failed: {msg}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_state() {
        let s = parse_state(
            br#"{"BackendState":"Running","TailscaleIPs":["100.1.2.3","fd7a::1"],
                "Self":{"DNSName":"mac.tail1.ts.net."},"Health":["relay down"]}"#,
        )
        .unwrap();
        assert_eq!(s.backend, "Running");
        assert_eq!(s.ips, ["100.1.2.3", "fd7a::1"]);
        assert_eq!(s.dns_name.as_deref(), Some("mac.tail1.ts.net"));
        assert_eq!(s.health, ["relay down"]);
        assert!(parse_state(b"{}").is_none());
    }
}
