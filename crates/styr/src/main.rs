//! `styr`: the Styrman CLI. Agent launcher (`styr claude …`), hook helper
//! (`styr hook <agent> <event>`) and inspection commands.

mod cmux_access;
mod codex_hooks;
pub(crate) mod hook;
pub(crate) mod launch;
mod net;
mod pty;
mod restart;
mod service;
mod setup;

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
    /// Guided install: background service, Claude plugin, shell alias, pairing.
    Setup {
        /// Answer yes to every question.
        #[arg(short, long)]
        yes: bool,
    },
    /// Check every part of the setup; `--live` runs an end-to-end check with Claude Code.
    Doctor {
        #[arg(long)]
        live: bool,
    },
    /// Check that styrd is running.
    Ping,
    /// How the phone reaches this Mac (Tailscale, local network); `restart` restarts Tailscale.
    Net {
        #[command(subcommand)]
        action: Option<NetAction>,
    },
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
    /// Name a session (on every phone, and its cmux tab); no name: its own title again
    Rename {
        /// Session id (the ID column of `styr status`).
        id: String,
        /// The name; leave out to clear it.
        name: Vec<String>,
    },
    /// End sessions: quit their agents, as the phone's End does (history is kept)
    End {
        /// Session ids (the ID column of `styr status`).
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Delete sessions and their history (from this Mac and every phone).
    Forget {
        /// Session ids (the ID column of `styr status --all`).
        ids: Vec<String>,
        /// Forget every session that has ended.
        #[arg(long)]
        ended: bool,
    },
    /// Send a prompt to a shared session (delivered when the agent next stops).
    Send {
        /// Session id (or the ID column of `styr status`).
        session: String,
        /// The prompt.
        text: Vec<String>,
    },
    /// Start an agent in a folder in a new cmux tab (in this workspace when run inside cmux), shared with the phone
    /// (what the phone's New session does).
    New {
        /// claude, codex or agy.
        agent: String,
        /// The folder (inside your home directory). Default: the current one.
        #[arg(long, short = 'C')]
        dir: Option<std::path::PathBuf>,
        /// An optional first message, typed in once the agent is ready.
        prompt: Vec<String>,
    },
    /// List the other shared sessions an agent can talk to.
    Peers {
        #[arg(long)]
        json: bool,
    },
    /// Send a message to another shared agent session and print its reply.
    /// Meant for agents, when the user asks them to consult another agent.
    Ask {
        /// The session: an id from `styr peers` (or a unique part of it), or an agent
        /// name (claude, codex, agy) when only one such session is shared.
        to: String,
        /// The message.
        text: Vec<String>,
        /// Don't wait for the reply.
        #[arg(long)]
        no_wait: bool,
        /// How long to wait for the reply, in seconds.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Give Styrman the password of cmux's socket (if you set password mode yourself),
    /// so sessions started from the phone can open cmux tabs.
    CmuxPassword { password: String },
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
    /// Run styrd as a launchd agent (starts at login, restarted if it exits).
    Service {
        #[command(subcommand)]
        action: ServiceCmd,
    },
    /// Push notifications (APNs): setup, status, test.
    Push {
        #[command(subcommand)]
        action: PushCmd,
    },
    /// List paired phones, or revoke one.
    Devices {
        #[command(subcommand)]
        action: Option<DeviceCmd>,
    },
    /// Hook helper called by agent hooks: `styr hook <agent> <event>` (stdin: payload).
    /// Exits 0 and prints nothing unless the daemon has an answer (exit 2 only to wake Claude).
    Hook {
        #[arg(num_args = 0.., trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Launch an agent through Styrman: `styr claude …`, `styr agy …`, `styr codex …`.
    #[command(external_subcommand)]
    Launch(Vec<OsString>),
}

#[derive(Subcommand)]
enum NetAction {
    /// Restart Tailscale (`tailscale down`, then `up`) and wait for the phone API to come back.
    Restart,
}

#[derive(Subcommand)]
enum ServiceCmd {
    Install,
    Uninstall,
    Status,
    Restart,
}

#[derive(Subcommand)]
enum PushCmd {
    /// Install your APNs key (.p8 from developer.apple.com › Keys) and turn push on.
    Setup {
        /// Path to AuthKey_XXXXXXXXXX.p8
        key: std::path::PathBuf,
        /// Key ID (default: from the file name).
        #[arg(long)]
        key_id: Option<String>,
        /// Apple Developer team id.
        #[arg(long, default_value = "7R6QL53THM")]
        team: String,
        /// App bundle id.
        #[arg(long, default_value = "com.christoffermattssonlangseth.styrman")]
        topic: String,
    },
    /// Show whether push is on and which phones registered.
    Status,
    /// Send a test notification to every registered phone.
    Test,
    /// Include command titles and reply excerpts in notifications (`on`/`off`).
    Details { state: String },
    /// Get notifications through a push relay instead of your own Apple key
    /// (for people without an Apple developer account). Only the phone's push token
    /// and the short alert ("Claude · repo needs your approval") go to the relay.
    Relay { url: String },
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
        Cmd::Net { action } => match action {
            None => net::status(call(Request::Diagnostics).ok().as_ref()),
            Some(NetAction::Restart) => net::restart(|| call(Request::Diagnostics).ok())?,
        },
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
                if let Ok(Response::Diagnostics { warnings, .. }) =
                    client().call(&Request::Diagnostics)
                {
                    for w in warnings {
                        println!("\nnote: {w}");
                    }
                }
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
        Cmd::Setup { yes } => setup::setup(yes)?,
        Cmd::Doctor { live } => {
            if !setup::doctor(live)? {
                std::process::exit(1);
            }
        }
        Cmd::Rename { id, name } => {
            let session = resolve_session(&id)?;
            let name = name.join(" ");
            call(Request::RenameSession {
                session,
                name: name.clone(),
            })?;
            println!(
                "{}",
                if name.is_empty() {
                    "name cleared".to_owned()
                } else {
                    format!("named \"{name}\"")
                }
            );
        }
        Cmd::End { ids } => {
            let mut targets = Vec::new();
            for id in &ids {
                targets.push(resolve_session(id)?);
            }
            let n = targets.len();
            call(Request::EndSessions { ids: targets })?;
            println!("ended {n} session{}", if n == 1 { "" } else { "s" });
        }
        Cmd::Forget { ids, ended } => {
            let mut targets = Vec::new();
            for id in &ids {
                targets.push(resolve_session(id)?);
            }
            if ended {
                let Response::Sessions { sessions } = call(Request::ListSessions {
                    include_unshared: true,
                })?
                else {
                    bail!("unexpected response");
                };
                targets.extend(
                    sessions
                        .into_iter()
                        .filter(|s| s.status == SessionStatus::Done)
                        .map(|s| s.id),
                );
            }
            if targets.is_empty() {
                bail!("nothing to forget (give session ids or --ended)");
            }
            let n = targets.len();
            call(Request::ForgetSessions { ids: targets })?;
            println!("forgot {n} session{}", if n == 1 { "" } else { "s" });
        }
        Cmd::Push { action } => push(action)?,
        Cmd::Service { action } => match action {
            ServiceCmd::Install => service::install()?,
            ServiceCmd::Uninstall => service::uninstall()?,
            ServiceCmd::Status => {
                service::status()?;
                match client().call(&Request::Hello) {
                    Ok(Response::Hello {
                        pid,
                        daemon_version,
                        ..
                    }) => println!("styrd:  running (pid {pid}, {daemon_version})"),
                    _ => println!("styrd:  not answering"),
                }
            }
            ServiceCmd::Restart => service::restart()?,
        },
        Cmd::Peers { json } => {
            let launch_id = std::env::var("STYR_LAUNCH_ID")
                .ok()
                .filter(|s| !s.is_empty());
            let Response::Peers { sessions, .. } = call(Request::Peers { launch_id })? else {
                bail!("unexpected response");
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&sessions)?);
            } else if sessions.is_empty() {
                println!("no other shared sessions (type /styr in an agent to share it)");
            } else {
                println!(
                    "{:<8}  {:<7}  {:<14}  {:<16}  FIRST MESSAGE",
                    "ID", "AGENT", "STATUS", "FOLDER"
                );
                for s in sessions {
                    let id = s.id.as_str();
                    let folder = s
                        .cwd
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    println!(
                        "{:<8}  {:<7}  {:<14}  {:<16}  {}",
                        &id[id.len().saturating_sub(8)..],
                        s.agent.to_string(),
                        format!("{:?}", s.status).to_lowercase(),
                        folder,
                        s.title.unwrap_or_default()
                    );
                }
            }
        }
        Cmd::Ask {
            to,
            text,
            no_wait,
            timeout,
        } => {
            let text = text.join(" ");
            let launch_id = std::env::var("STYR_LAUNCH_ID")
                .ok()
                .filter(|s| !s.is_empty());
            let wait_secs = (!no_wait).then_some(timeout);
            let r = styr_core::ipc::Client::new(styr_core::paths::Paths::resolve().socket)
                .with_timeout(None)
                .call(&Request::Ask {
                    launch_id,
                    to,
                    text,
                    wait_secs,
                })?;
            match r {
                Response::AskReply {
                    reply: Some(reply), ..
                } => println!("{reply}"),
                Response::AskReply {
                    timed_out: true, ..
                } => {
                    bail!(
                        "no reply within {timeout} s (the message was delivered; the reply will show in that session)"
                    )
                }
                Response::AskReply { .. } => println!("sent"),
                other => bail!("unexpected response: {other:?}"),
            }
        }
        Cmd::CmuxPassword { password } => {
            cmux_access::store_password(&password)?;
            println!("saved; sessions started from your phone can now open cmux tabs");
        }
        Cmd::New { agent, dir, prompt } => {
            let agent: AgentKind = match agent.as_str() {
                "agy" => AgentKind::Antigravity,
                other => other.parse().map_err(|e| anyhow::anyhow!("{e}"))?,
            };
            let cwd = std::fs::canonicalize(dir.unwrap_or(std::env::current_dir()?))?;
            let prompt = (!prompt.is_empty()).then(|| prompt.join(" "));
            let cmux_workspace = std::env::var("CMUX_WORKSPACE_ID").ok();
            let here = cmux_workspace.is_some();
            match call(Request::StartSession {
                agent,
                cwd,
                prompt,
                cmux_workspace,
            })? {
                Response::SessionStarting { session } if here => {
                    println!(
                        "started {session} in a new tab in this cmux workspace (shared with your phone)"
                    )
                }
                Response::SessionStarting { session } => {
                    println!("started {session} in a new cmux tab (shared with your phone)")
                }
                other => bail!("unexpected response: {other:?}"),
            }
        }
        Cmd::Send { session, text } => {
            let id = resolve_session(&session)?;
            let text = text.join(" ");
            match call(Request::SendPrompt { session: id, text })? {
                Response::PromptAccepted { delivered: true } => println!("delivered"),
                Response::PromptAccepted { delivered: false } => {
                    println!("queued: delivered when the agent next stops")
                }
                other => bail!("unexpected response: {other:?}"),
            }
        }
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

pub(crate) fn pair(url_override: Option<String>, json: bool) -> Result<()> {
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

fn push(action: PushCmd) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let paths = Paths::resolve();
    let cfg_path = paths.data_dir.join("apns.json");
    let print_status = |r: Response| -> Result<()> {
        let Response::PushStatus {
            configured,
            details,
            devices_with_token,
            relay,
        } = r
        else {
            bail!("unexpected response");
        };
        println!(
            "push: {}",
            if configured {
                "on"
            } else {
                "off (run `styr push setup <key.p8>`)"
            }
        );
        if let Some(url) = &relay {
            println!("via relay: {url} (only short alerts; details are always off)");
        } else if configured {
            println!(
                "details in notifications: {}",
                if details { "on" } else { "off" }
            );
        }
        if devices_with_token.is_empty() {
            println!("no phone has registered yet (open the app once after updating it)");
        } else {
            println!("registered phones: {}", devices_with_token.join(", "));
        }
        Ok(())
    };
    match action {
        PushCmd::Setup {
            key,
            key_id,
            team,
            topic,
        } => {
            let pem = std::fs::read_to_string(&key)
                .with_context(|| format!("reading {}", key.display()))?;
            anyhow::ensure!(
                pem.contains("BEGIN PRIVATE KEY"),
                "{} is not a .p8 key",
                key.display()
            );
            let key_id = match key_id {
                Some(k) => k,
                None => key
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.strip_prefix("AuthKey_"))
                    .map(str::to_owned)
                    .context("can't read the key id from the file name; pass --key-id")?,
            };
            anyhow::ensure!(
                key_id.len() == 10 && key_id.chars().all(|c| c.is_ascii_alphanumeric()),
                "key id should be 10 letters/digits"
            );
            std::fs::create_dir_all(&paths.data_dir)?;
            let dest = paths.data_dir.join("apns-key.p8");
            std::fs::write(&dest, pem)?;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600))?;
            let details = std::fs::read(&cfg_path)
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|v| v["details"].as_bool())
                .unwrap_or(false);
            let cfg = serde_json::json!({"key_id": key_id, "team_id": team, "topic": topic, "details": details});
            std::fs::write(&cfg_path, serde_json::to_vec_pretty(&cfg)?)?;
            std::fs::set_permissions(&cfg_path, std::fs::Permissions::from_mode(0o600))?;
            println!(
                "installed key {key_id} for team {team} ({})",
                dest.display()
            );
            println!(
                "you can delete {} now if you keep a backup elsewhere",
                key.display()
            );
            print_status(call(Request::PushReload)?)?;
        }
        PushCmd::Status => print_status(call(Request::PushStatus)?)?,
        PushCmd::Relay { url } => {
            anyhow::ensure!(
                !cfg_path.exists(),
                "you have your own APNs key set up (styr push setup), which takes precedence; remove {} to use a relay",
                cfg_path.display()
            );
            print_status(call(Request::PushRelay { url })?)?;
        }
        PushCmd::Test => {
            let Response::PushResults { results } = call(Request::PushTest)? else {
                bail!("unexpected response");
            };
            if results.is_empty() {
                println!("nothing sent: push is off or no phone has registered");
            }
            for (id, r) in results {
                println!("{}  {r}", &id[id.len().saturating_sub(8)..]);
            }
        }
        PushCmd::Details { state } => {
            let on = match state.as_str() {
                "on" => true,
                "off" => false,
                _ => bail!("use `on` or `off`"),
            };
            let mut cfg: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&cfg_path).context("push is not set up")?)?;
            cfg["details"] = on.into();
            std::fs::write(&cfg_path, serde_json::to_vec_pretty(&cfg)?)?;
            print_status(call(Request::PushReload)?)?;
        }
    }
    Ok(())
}
