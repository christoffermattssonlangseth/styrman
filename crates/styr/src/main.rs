//! `styr`: the Styrman CLI. Agent launcher (`styr claude …`), hook helper
//! (`styr hook <agent> <event>`) and inspection commands.

mod hook;
mod launch;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::ffi::OsString;
use styr_core::SessionStatus;
use styr_core::event::now_ms;
use styr_core::ids::{AgentKind, NativeSessionRef, SessionId};
use styr_core::ipc::{Client, Request, Response, SessionSummary};
use styr_core::paths::Paths;

#[derive(Parser)]
#[command(
    name = "styr",
    version,
    about = "Styrman: steer your coding agents from anywhere"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check that styrd is running.
    Ping,
    /// List sessions known to styrd.
    Status {
        /// Include sessions that are not shared with the phone.
        #[arg(short, long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print a session's event history.
    Events {
        /// Session id (or a unique prefix/suffix, e.g. the ID column of `styr status`).
        session: String,
        #[arg(long, default_value_t = 0)]
        since: u64,
        #[arg(long, default_value_t = 200)]
        limit: u32,
    },
    /// Share (or stop sharing) a session with the phone by its agent-native id.
    Share {
        agent: AgentKind,
        native_id: String,
        #[arg(long)]
        off: bool,
    },
    /// Pair a phone: shows a QR code with a one-time code (valid 5 minutes).
    Pair {
        /// Use this phone-API URL instead of the detected Tailscale one,
        /// e.g. ws://my-mac.tailnet.ts.net:4719/v1/ws
        #[arg(long)]
        url: Option<String>,
        /// Print the pairing details as JSON instead of a QR code.
        #[arg(long)]
        json: bool,
    },
    /// List paired phones, or revoke one.
    Devices {
        #[command(subcommand)]
        action: Option<DeviceCmd>,
    },
    /// Hook helper called by agent hooks: `styr hook <agent> <event>` (stdin: payload).
    /// Always exits 0; prints nothing unless the daemon has an answer.
    Hook {
        #[arg(num_args = 0.., trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Launch an agent through Styrman: `styr claude …`, `styr agy …`, `styr codex …`.
    #[command(external_subcommand)]
    Launch(Vec<OsString>),
}

#[derive(Subcommand)]
enum DeviceCmd {
    /// Revoke a device (its connection closes at once; it must pair again).
    Revoke {
        /// Device id, or a unique suffix of it (the ID column of `styr devices`).
        id: String,
    },
}

fn client() -> Client {
    Client::new(Paths::resolve().socket)
}

fn call(req: Request) -> Result<Response> {
    client().call(&req).map_err(|e| {
        if e.is_not_running() {
            anyhow::anyhow!("{e}\nstart it with `styrd` (or load the launchd agent, see README)")
        } else {
            e.into()
        }
    })
}

fn main() -> Result<()> {
    // `styr hook` must never exit non-zero (exit 2 means "block" to Claude Code),
    // so it is dispatched before clap can reject anything.
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("hook") {
        hook::main(&argv[2..]);
    }
    match Cli::parse().cmd {
        Cmd::Hook { .. } => unreachable!("handled above"),
        Cmd::Launch(mut args) => {
            let name = args.remove(0);
            let agent: AgentKind = name
                .to_str()
                .and_then(|n| n.parse().ok())
                .filter(|a| *a != AgentKind::Pty)
                .with_context(|| {
                    format!("unknown command {name:?} (agents: claude, agy, codex)")
                })?;
            launch::exec(agent, args)?;
        }
        Cmd::Ping => match call(Request::Hello)? {
            Response::Hello {
                protocol,
                daemon_version,
                pid,
            } => {
                println!(
                    "styrd {daemon_version} (pid {pid}, protocol v{protocol}) at {}",
                    client().socket().display()
                );
            }
            other => bail!("unexpected response: {other:?}"),
        },
        Cmd::Status { all, json } => {
            let Response::Sessions { sessions } = call(Request::ListSessions {
                include_unshared: all,
            })?
            else {
                bail!("unexpected response");
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&sessions)?);
            } else {
                print_sessions(&sessions, all);
            }
        }
        Cmd::Events {
            session,
            since,
            limit,
        } => {
            let id = resolve_session(&session)?;
            let Response::Events { events } = call(Request::Events {
                session: id,
                since_seq: since,
                limit,
            })?
            else {
                bail!("unexpected response");
            };
            for e in events {
                println!("{:>5}  {}", e.seq, serde_json::to_string(&e.kind)?);
            }
        }
        Cmd::Pair { url, json } => pair(url, json)?,
        Cmd::Devices { action: None } => {
            let Response::Devices { devices } = call(Request::ListDevices)? else {
                bail!("unexpected response");
            };
            if devices.is_empty() {
                println!("no paired devices (run `styr pair`)");
            }
            let now = now_ms();
            for d in devices {
                println!(
                    "{:<10} {:<24} {:<8} last seen {}",
                    &d.id[d.id.len().saturating_sub(8)..],
                    d.name,
                    if d.revoked { "revoked" } else { "active" },
                    d.last_seen_ms
                        .map(|t| format!("{} ago", ago(now - t)))
                        .unwrap_or_else(|| "never".into())
                );
            }
        }
        Cmd::Devices {
            action: Some(DeviceCmd::Revoke { id }),
        } => {
            let Response::Devices { devices } = call(Request::ListDevices)? else {
                bail!("unexpected response");
            };
            let up = id.to_ascii_uppercase();
            let hits: Vec<_> = devices
                .iter()
                .filter(|d| !d.revoked && d.id.ends_with(&up))
                .collect();
            let [d] = hits.as_slice() else {
                bail!(
                    "'{id}' must match exactly one active device ({} matched)",
                    hits.len()
                );
            };
            call(Request::RevokeDevice { id: d.id.clone() })?;
            println!("revoked {} ({})", d.name, d.id);
        }
        Cmd::Share {
            agent,
            native_id,
            off,
        } => {
            call(Request::SetSharing {
                native: NativeSessionRef::new(agent, native_id),
                shared: !off,
            })?;
            println!(
                "{}",
                if off {
                    "sharing off"
                } else {
                    "shared with the phone"
                }
            );
        }
    }
    Ok(())
}

fn resolve_session(prefix: &str) -> Result<SessionId> {
    let Response::Sessions { sessions } = call(Request::ListSessions {
        include_unshared: true,
    })?
    else {
        bail!("unexpected response");
    };
    let p = prefix.to_ascii_uppercase();
    let mut hits = sessions.into_iter().filter(|s| {
        s.id.as_str().starts_with(&p)
            || s.id.as_str().ends_with(&p)
            || s.native_id.starts_with(prefix)
    });
    let first = hits
        .next()
        .with_context(|| format!("no session matches '{prefix}'"))?;
    if hits.next().is_some() {
        bail!("'{prefix}' matches more than one session; use more characters");
    }
    Ok(first.id)
}

#[allow(clippy::print_literal)] // header row mirrors the data row format
fn print_sessions(sessions: &[SessionSummary], all: bool) {
    if sessions.is_empty() {
        println!(
            "{}",
            if all {
                "no sessions yet"
            } else {
                "no shared sessions (run /styr in an agent session; `styr status --all` lists all)"
            }
        );
        return;
    }
    let now = now_ms();
    println!(
        "{:<10} {:<12} {:<15} {:<7} {:>8}  {}",
        "ID", "AGENT", "STATUS", "SHARED", "UPDATED", "CWD"
    );
    for s in sessions {
        let status = match s.status {
            SessionStatus::Working => "working",
            SessionStatus::NeedsApproval => "needs approval",
            SessionStatus::Waiting => "needs you",
            SessionStatus::Error => "error",
            SessionStatus::Done => "done",
        };
        let cwd = s.cwd.as_ref().map(|p| shorten_home(p)).unwrap_or_default();
        println!(
            "{:<10} {:<12} {:<15} {:<7} {:>8}  {}",
            &s.id.as_str()[s.id.as_str().len().saturating_sub(8)..],
            s.agent.to_string(),
            status,
            if s.shared { "yes" } else { "no" },
            ago(now - s.updated_ms),
            cwd
        );
    }
}

fn shorten_home(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

fn ago(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

fn pair(url_override: Option<String>, json: bool) -> Result<()> {
    let Response::Pairing {
        code,
        expires_ms,
        urls,
        host,
    } = call(Request::CreatePairing)?
    else {
        bail!("unexpected response");
    };
    let url = match url_override.or_else(|| urls.first().cloned()) {
        Some(u) => u,
        None => bail!("styrd has no phone API listener (is the port in use? see the daemon log)"),
    };
    let link = styr_core::phone::pair_link(&url, &code, &host);
    if json {
        println!(
            "{}",
            serde_json::json!({"link": link, "url": url, "code": code, "host": host, "expires_ms": expires_ms})
        );
        return Ok(());
    }
    let qr = qrcode::QrCode::new(link.as_bytes())
        .map_err(|e| anyhow::anyhow!("encoding QR code: {e:?}"))?;
    let art = qr
        .render::<qrcode::render::unicode::Dense1x2>()
        .dark_color(qrcode::render::unicode::Dense1x2::Light)
        .light_color(qrcode::render::unicode::Dense1x2::Dark)
        .quiet_zone(true)
        .build();
    println!("\nScan with the iPhone camera to pair Styrman with {host}:\n");
    println!("{art}");
    println!("Or enter manually in the app:");
    println!("  address  {url}");
    println!("  code     {}-{}", &code[..5], &code[5..]);
    println!("\nThe code works once and expires in 5 minutes.");
    if url.contains("127.0.0.1") {
        println!(
            "Note: Tailscale was not detected, so this address only works on this Mac (e.g. the iOS Simulator)."
        );
    }
    Ok(())
}
