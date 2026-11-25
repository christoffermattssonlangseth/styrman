//! `styr setup` (guided, re-runnable install) and `styr doctor` (checks, optionally
//! a live end-to-end run against the installed Claude Code).

use anyhow::{Context, Result};
use std::io::{BufRead, IsTerminal, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use styr_core::ids::AgentKind;
use styr_core::ipc::{Client, Request, Response};
use styr_core::paths::Paths;

/// The Claude Code plugin, built into the binary so installs don't need a checkout.
const PLUGIN_FILES: &[(&str, &str, bool)] = &[
    (
        ".claude-plugin/marketplace.json",
        include_str!("../../../plugins/.claude-plugin/marketplace.json"),
        false,
    ),
    (
        "claude/styrman/.claude-plugin/plugin.json",
        include_str!("../../../plugins/claude/styrman/.claude-plugin/plugin.json"),
        false,
    ),
    (
        "claude/styrman/hooks/hooks.json",
        include_str!("../../../plugins/claude/styrman/hooks/hooks.json"),
        false,
    ),
    (
        "claude/styrman/commands/styr.md",
        include_str!("../../../plugins/claude/styrman/commands/styr.md"),
        false,
    ),
    (
        "claude/styrman/bin/styr-hook",
        include_str!("../../../plugins/claude/styrman/bin/styr-hook"),
        true,
    ),
    (
        "claude/styrman/skills/peers/SKILL.md",
        include_str!("../../../plugins/claude/styrman/skills/peers/SKILL.md"),
        false,
    ),
];
const ALIAS_LINE: &str = "alias claude='styr claude'";

/// The Antigravity (agy) plugin, also built in.
const AGY_PLUGIN_FILES: &[(&str, &str, bool)] = &[
    (
        "plugin.json",
        include_str!("../../../plugins/agy/styrman/plugin.json"),
        false,
    ),
    (
        "hooks.json",
        include_str!("../../../plugins/agy/styrman/hooks.json"),
        false,
    ),
    (
        "skills/styr/SKILL.md",
        include_str!("../../../plugins/agy/styrman/skills/styr/SKILL.md"),
        false,
    ),
    (
        "bin/styr-hook",
        include_str!("../../../plugins/agy/styrman/bin/styr-hook"),
        true,
    ),
    (
        "skills/peers/SKILL.md",
        include_str!("../../../plugins/agy/styrman/skills/peers/SKILL.md"),
        false,
    ),
    (
        "assets/logo.svg",
        include_str!("../../../plugins/agy/styrman/assets/logo.svg"),
        false,
    ),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Ok,
    Warn,
    Fail,
}

fn line(mark: Mark, what: &str, detail: impl AsRef<str>) {
    let m = match mark {
        Mark::Ok => "✓",
        Mark::Warn => "!",
        Mark::Fail => "✗",
    };
    println!("{m} {what:<20} {}", detail.as_ref());
}

fn client() -> Client {
    Client::new(Paths::resolve().socket).with_timeout(Some(Duration::from_secs(5)))
}

fn ask(question: &str, yes: bool) -> bool {
    if yes {
        return true;
    }
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("  {question} [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().lock().read_line(&mut s);
    !matches!(s.trim().to_lowercase().as_str(), "n" | "no")
}

fn claude_bin() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    crate::launch::find_on_path("claude", &path, std::env::current_exe().ok().as_deref())
}

/// The real Claude Code binary's version (from its install path), following wrappers.
fn claude_version() -> Option<String> {
    // `claude` may be a shell wrapper (cmux); the installer's symlink is the reliable source.
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let candidates = [
        home.join(".local/bin/claude"),
        home.join(".claude/local/claude"),
    ];
    candidates
        .iter()
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .chain(claude_bin().and_then(|p| std::fs::canonicalize(p).ok()))
        .find_map(|p| crate::hook::version_from_path(&p))
}

struct PluginState {
    installed: bool,
    enabled: bool,
    path: Option<PathBuf>,
    current: bool,
}

fn plugin_state() -> PluginState {
    let out = Command::new("claude")
        .args(["plugin", "list"])
        .stderr(Stdio::null())
        .output();
    let text = out
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let Some(start) = text.find("styrman@") else {
        return PluginState {
            installed: false,
            enabled: false,
            path: None,
            current: false,
        };
    };
    let block: String = text[start..].lines().take(6).collect::<Vec<_>>().join("\n");
    let path = block
        .lines()
        .find_map(|l| l.trim().strip_prefix("Read from:"))
        .map(|p| PathBuf::from(p.trim()));
    let current = path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p.join("hooks/hooks.json")).ok())
        .is_some_and(|h| h.contains("StopWait"))
        && path
            .as_ref()
            .is_some_and(|p| p.join("skills/peers/SKILL.md").exists());
    PluginState {
        installed: true,
        enabled: block.contains("enabled"),
        path,
        current,
    }
}

fn write_embedded_plugin(dir: &Path) -> Result<()> {
    for (rel, body, exec) in PLUGIN_FILES {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().expect("has parent"))?;
        std::fs::write(&p, body)?;
        if *exec {
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}

fn codex_bin() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let exe = std::env::current_exe().ok();
    crate::launch::find_on_path("codex", &path, exe.as_deref())
}

/// `codex-cli 0.172.0` → `0.172.0`.
fn codex_version(bin: &Path) -> Option<String> {
    let out = Command::new(bin)
        .arg("--version")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .last()
        .map(str::to_owned)
}

fn agy_bin() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    crate::launch::find_on_path("agy", &path, std::env::current_exe().ok().as_deref()).or_else(
        || {
            let p = PathBuf::from(std::env::var_os("HOME")?).join(".local/bin/agy");
            p.is_file().then_some(p)
        },
    )
}

fn agy_version() -> Option<String> {
    let out = Command::new(agy_bin()?)
        .arg("--version")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let t = String::from_utf8_lossy(&out.stdout);
    t.split_whitespace()
        .find(|w| w.contains('.') && w.chars().all(|c| c.is_ascii_digit() || c == '.'))
        .map(str::to_owned)
}

/// The agy plugin is installed and matches the one built into this `styr`.
fn agy_plugin_current() -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let dir = PathBuf::from(home).join(".gemini/config/plugins/styrman");
    let embedded = AGY_PLUGIN_FILES
        .iter()
        .find(|f| f.0 == "hooks.json")
        .map(|f| f.1)
        .unwrap_or_default();
    std::fs::read_to_string(dir.join("hooks.json")).is_ok_and(|h| h == embedded)
        && dir.join("skills/styr/SKILL.md").exists()
        && dir.join("skills/peers/SKILL.md").exists()
        && global_agy_skill().is_some_and(|p| p.exists())
}

/// A user-level `styr` skill so plain `/styr` works in agy (plugin skills are
/// namespaced as `/styrman:styr`).
fn global_agy_skill() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".gemini/config/skills/styr/SKILL.md"))
}

fn install_agy_plugin() -> Result<bool> {
    let src = Paths::resolve().data_dir.join("agy-plugin/styrman");
    for (rel, body, exec) in AGY_PLUGIN_FILES {
        let p = src.join(rel);
        std::fs::create_dir_all(p.parent().expect("has parent"))?;
        std::fs::write(&p, body)?;
        if *exec {
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    if let Some(skill) = global_agy_skill() {
        std::fs::create_dir_all(skill.parent().expect("has parent"))?;
        let body = AGY_PLUGIN_FILES
            .iter()
            .find(|f| f.0 == "skills/styr/SKILL.md")
            .map(|f| f.1)
            .unwrap_or_default();
        std::fs::write(&skill, body)?;
    }
    let agy = agy_bin().context("agy not found")?;
    let _ = Command::new(&agy)
        .args(["plugin", "uninstall", "styrman"])
        .output();
    let ok = Command::new(&agy)
        .args(["plugin", "install", &src.display().to_string()])
        .output()
        .is_ok_and(|o| o.status.success());
    Ok(ok && agy_plugin_current())
}

fn rc_file() -> Option<(PathBuf, &'static str)> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let shell = std::env::var("SHELL").unwrap_or_default();
    if shell.ends_with("fish") {
        Some((
            home.join(".config/fish/config.fish"),
            "alias claude 'styr claude'",
        ))
    } else if shell.ends_with("bash") {
        Some((home.join(".bashrc"), ALIAS_LINE))
    } else {
        Some((home.join(".zshrc"), ALIAS_LINE))
    }
}

const SHIM_PATH_MARK: &str =
    "# Styrman: route claude/agy through Styrman (phone approvals and prompts)";

fn shim_path_line(rc: &Path) -> &'static str {
    if rc.extension().is_some_and(|e| e == "fish") {
        "fish_add_path --prepend --move $HOME/.styrman/bin"
    } else {
        "export PATH=\"$HOME/.styrman/bin:$PATH\""
    }
}

/// The agents routed through `styr` (Claude always; agy and Codex when installed).
fn routed_agents() -> Vec<&'static str> {
    std::iter::once("claude")
        .chain(agy_bin().map(|_| "agy"))
        .chain(codex_bin().map(|_| "codex"))
        .collect()
}

fn install_shims(agents: &[&str]) -> Result<()> {
    let dir = crate::launch::shim_dir().context("HOME is not set")?;
    std::fs::create_dir_all(&dir)?;
    for a in agents {
        let p = dir.join(a);
        std::fs::write(&p, crate::launch::SHIM)?;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Routing {
    /// Shims installed and first on PATH in a new interactive shell.
    Active,
    NotInPath,
    Missing,
}

/// What a new interactive shell would run for each agent.
fn routing_state(agents: &[&str]) -> Routing {
    let Some(dir) = crate::launch::shim_dir() else {
        return Routing::Missing;
    };
    let installed = agents
        .iter()
        .all(|a| std::fs::read_to_string(dir.join(a)).is_ok_and(|s| s == crate::launch::SHIM));
    if !installed {
        return Routing::Missing;
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let out = Command::new(&shell)
        .args(["-ic", "printf %s \"$PATH\""])
        .stderr(Stdio::null())
        .output();
    let path = out
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    let shim_pos = dirs.iter().position(|d| d == &dir);
    let first_real = |name: &str| {
        dirs.iter().position(|d| {
            d != &dir && d.join(name).is_file() && !d.to_string_lossy().contains("cmux")
        })
    };
    match shim_pos {
        Some(s) if agents.iter().all(|a| first_real(a).is_none_or(|r| s < r)) => Routing::Active,
        _ => Routing::NotInPath,
    }
}

fn diagnostics() -> Option<Response> {
    client().call(&Request::Diagnostics).ok()
}

fn wait_for_daemon() -> bool {
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if client().call(&Request::Hello).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

// ---------------------------------------------------------------- setup

pub fn setup(yes: bool) -> Result<()> {
    println!("Setting up Styrman. Safe to run again at any time.\n");

    // 1. Daemon as a background service.
    if cfg!(target_os = "macos") {
        if crate::service::is_loaded() && wait_for_daemon() {
            line(Mark::Ok, "background service", "styrd runs at login");
        } else {
            crate::service::install_quiet()?;
            let up = wait_for_daemon();
            line(
                if up { Mark::Ok } else { Mark::Fail },
                "background service",
                if up {
                    "installed; styrd runs at login"
                } else {
                    "installed, but styrd isn't answering (see ~/Library/Logs/styrd.log)"
                },
            );
        }
    } else if wait_for_daemon() {
        line(Mark::Ok, "styrd", "running");
    } else {
        line(
            Mark::Fail,
            "styrd",
            "not running: start `styrd` (e.g. from a systemd user unit)",
        );
    }

    // 2. Claude Code + plugin.
    match (claude_bin(), claude_version()) {
        (Some(_), v) => line(
            Mark::Ok,
            "Claude Code",
            v.map(|v| format!("version {v}"))
                .unwrap_or_else(|| "found".into()),
        ),
        (None, _) => line(
            Mark::Fail,
            "Claude Code",
            "not found on PATH; install it, then run `styr setup` again",
        ),
    }
    if claude_bin().is_some() {
        let st = plugin_state();
        let embedded_dir = Paths::resolve().data_dir.join("claude-plugin");
        let ours = st
            .path
            .as_ref()
            .is_some_and(|p| p.starts_with(&embedded_dir));
        if st.installed && st.enabled && st.current {
            if ours {
                write_embedded_plugin(&embedded_dir)?; // keep it in step with this styr
            }
            line(
                Mark::Ok,
                "Claude plugin",
                "styrman installed (hooks + /styr)",
            );
        } else {
            write_embedded_plugin(&embedded_dir)?;
            let dir = embedded_dir.display().to_string();
            let _ = Command::new("claude")
                .args(["plugin", "marketplace", "add", &dir])
                .output();
            let ok = Command::new("claude")
                .args(["plugin", "install", "styrman@styrman"])
                .output()
                .is_ok_and(|o| o.status.success());
            let st = plugin_state();
            let good = ok && st.installed && st.current;
            line(
                if good { Mark::Ok } else { Mark::Fail },
                "Claude plugin",
                if good {
                    "installed; start a new Claude session to load it".to_owned()
                } else {
                    format!(
                        "couldn't install; try `claude plugin marketplace add {dir}` then `claude plugin install styrman@styrman`"
                    )
                },
            );
        }
    }

    // 2b. Antigravity (agy), if installed.
    if agy_bin().is_some() {
        let v = agy_version()
            .map(|v| format!("version {v}, "))
            .unwrap_or_default();
        if agy_plugin_current() {
            line(
                Mark::Ok,
                "agy plugin",
                format!("{v}styrman installed (hooks + /styr)"),
            );
        } else if install_agy_plugin()? {
            line(
                Mark::Ok,
                "agy plugin",
                format!("{v}installed; start a new agy session to load it"),
            );
        } else {
            line(
                Mark::Fail,
                "agy plugin",
                "couldn't install; run `styr setup` again or see `agy plugin list`",
            );
        }
    }

    // 2c. Codex, if installed: hooks in ~/.codex/hooks.json.
    if let Some(codex) = codex_bin() {
        let v = codex_version(&codex)
            .map(|v| format!("version {v}, "))
            .unwrap_or_default();
        match crate::codex_hooks::install() {
            Ok(false) if crate::codex_hooks::trusted() => {
                line(Mark::Ok, "Codex hooks", format!("{v}installed and trusted"))
            }
            Ok(_) if !crate::codex_hooks::trusted() => line(
                Mark::Warn,
                "Codex hooks",
                format!(
                    "{v}installed; the next time Codex starts, choose \"Trust all and continue\""
                ),
            ),
            Ok(_) => line(Mark::Ok, "Codex hooks", format!("{v}installed")),
            Err(e) => line(
                Mark::Fail,
                "Codex hooks",
                format!("couldn't install: {e:#}"),
            ),
        }
    }

    // 2d. cmux: let sessions started from the phone open cmux tabs.
    match crate::cmux_access::state() {
        crate::cmux_access::State::NoCmux => {}
        crate::cmux_access::State::Ready => line(
            Mark::Ok,
            "cmux",
            "Styrman can open tabs (New session on the phone)",
        ),
        crate::cmux_access::State::Locked => {
            if ask(
                "Let Styrman open cmux tabs for sessions you start from your phone? This switches cmux's socket access to password mode with a random password only Styrman (and cmux) know; cmux.json is backed up first.",
                yes,
            ) {
                match crate::cmux_access::enable() {
                    Ok(()) => line(Mark::Ok, "cmux", "password mode on; Styrman can open tabs"),
                    Err(e) => line(
                        Mark::Warn,
                        "cmux",
                        format!("couldn't change cmux's settings: {e:#}"),
                    ),
                }
            } else {
                line(
                    Mark::Warn,
                    "cmux",
                    "New session on the phone can't open cmux tabs (cmux only lets its own processes in)",
                );
            }
        }
        crate::cmux_access::State::Other(why) => line(
            Mark::Warn,
            "cmux",
            format!(
                "{why}: to start sessions from the phone, use password mode and run `styr cmux-password <password>`"
            ),
        ),
    }

    // 3. Route `claude`/`agy`/`codex` through Styrman with PATH shims (aliases don't
    //    survive cmux's shell integration, which unaliases them).
    let agents = routed_agents();
    match routing_state(&agents) {
        Routing::Active => line(
            Mark::Ok,
            "routing",
            format!("{} → styr (phone approvals and prompts)", agents.join(", ")),
        ),
        state => {
            install_shims(&agents)?;
            let rc = rc_file().map(|(rc, _)| rc);
            let has_path = rc.as_ref().is_some_and(|rc| {
                std::fs::read_to_string(rc).is_ok_and(|t| t.contains(SHIM_PATH_MARK))
            });
            if has_path {
                line(
                    Mark::Ok,
                    "routing",
                    format!("{} → styr; open a new terminal tab", agents.join(", ")),
                );
            } else if let Some(rc) = rc {
                if ask(
                    &format!(
                        "Route {} through Styrman? Adds ~/.styrman/bin to PATH in {} (needed for approvals and prompts from the phone).",
                        agents.join(" and "),
                        rc.display()
                    ),
                    yes,
                ) {
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&rc)?;
                    writeln!(f, "\n{SHIM_PATH_MARK}\n{}", shim_path_line(&rc))?;
                    line(
                        Mark::Ok,
                        "routing",
                        format!("added to {}; open a new terminal tab", rc.display()),
                    );
                } else {
                    line(
                        Mark::Warn,
                        "routing",
                        "skipped: sessions are read-only on the phone unless started with `styr claude` / `styr agy`",
                    );
                }
            }
            let _ = state;
        }
    }

    // 4. Tailscale, 5. phone, 6. notifications.
    let diag = diagnostics();
    report_network_phone_push(diag.as_ref());
    if let Some(Response::Diagnostics { devices, .. }) = &diag
        && !devices.iter().any(|d| !d.revoked)
        && ask("Pair your iPhone now?", yes)
    {
        println!();
        crate::pair(None, false)?;
    }
    println!("\nDone. In Claude Code, type /styr to share a session with your phone.");
    Ok(())
}

fn report_network_phone_push(diag: Option<&Response>) {
    let Some(Response::Diagnostics {
        api_urls,
        devices,
        push_configured,
        ..
    }) = diag
    else {
        line(Mark::Fail, "styrd", "not answering");
        return;
    };
    match api_urls.iter().find(|u| u.contains(".ts.net")) {
        Some(u) => line(
            Mark::Ok,
            "Tailscale",
            u.trim_start_matches("ws://").trim_end_matches("/v1/ws"),
        ),
        None if api_urls.iter().any(|u| !u.contains("127.0.0.1")) => line(
            Mark::Warn,
            "Tailscale",
            "connected, but MagicDNS is off; turn it on in the Tailscale admin console",
        ),
        None => line(
            Mark::Warn,
            "Tailscale",
            "not found; install Tailscale on this Mac and your iPhone and sign in to both",
        ),
    }
    let active: Vec<_> = devices.iter().filter(|d| !d.revoked).collect();
    match active.as_slice() {
        [] => line(Mark::Warn, "phone", "none paired yet: run `styr pair`"),
        list => line(
            Mark::Ok,
            "phone",
            list.iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
                + " (on the iPhone, set Tailscale › VPN On Demand to stay connected)",
        ),
    }
    if *push_configured {
        line(Mark::Ok, "notifications", "on");
    } else {
        line(
            Mark::Warn,
            "notifications",
            "off (optional): `styr push setup <AuthKey.p8>`",
        );
    }
}

// ---------------------------------------------------------------- doctor

pub fn doctor(live: bool) -> Result<bool> {
    let mut healthy = true;
    let diag = diagnostics();
    match &diag {
        Some(Response::Diagnostics {
            daemon_version,
            pid,
            ..
        }) => {
            if daemon_version == styr_core::VERSION {
                line(
                    Mark::Ok,
                    "styrd",
                    format!("running (pid {pid}, {daemon_version})"),
                );
            } else {
                healthy = false;
                line(
                    Mark::Warn,
                    "styrd",
                    format!(
                        "is {daemon_version} but styr is {}: run `styr service restart`",
                        styr_core::VERSION
                    ),
                );
            }
        }
        _ => {
            healthy = false;
            line(
                Mark::Fail,
                "styrd",
                "not answering: `styr service restart` (or `styr setup`)",
            );
        }
    }
    if cfg!(target_os = "macos") {
        if crate::service::is_loaded() {
            line(Mark::Ok, "background service", "loaded");
        } else {
            line(
                Mark::Warn,
                "background service",
                "not installed: `styr service install`",
            );
        }
    }
    match claude_bin() {
        Some(_) => line(
            Mark::Ok,
            "Claude Code",
            claude_version()
                .map(|v| format!("version {v}"))
                .unwrap_or_else(|| "found".into()),
        ),
        None => {
            healthy = false;
            line(Mark::Fail, "Claude Code", "not found on PATH");
        }
    }
    let st = plugin_state();
    match (st.installed, st.enabled, st.current) {
        (true, true, true) => line(Mark::Ok, "Claude plugin", "styrman, enabled, up to date"),
        (true, false, _) => {
            healthy = false;
            line(
                Mark::Fail,
                "Claude plugin",
                "installed but disabled: `claude plugin enable styrman@styrman`",
            )
        }
        (true, true, false) => {
            healthy = false;
            line(Mark::Warn, "Claude plugin", "outdated: run `styr setup`")
        }
        _ => {
            healthy = false;
            line(
                Mark::Fail,
                "Claude plugin",
                "not installed: run `styr setup`",
            )
        }
    }
    if let Some(codex) = codex_bin() {
        let v = codex_version(&codex).unwrap_or_else(|| "?".into());
        match (
            crate::codex_hooks::installed(),
            crate::codex_hooks::trusted(),
        ) {
            (true, true) => line(
                Mark::Ok,
                "Codex",
                format!("version {v}, hooks installed and trusted"),
            ),
            (true, false) => line(
                Mark::Warn,
                "Codex",
                format!(
                    "version {v}: start Codex and choose \"Trust all and continue\" for Styrman's hooks"
                ),
            ),
            (false, _) => {
                healthy = false;
                line(
                    Mark::Warn,
                    "Codex",
                    format!("version {v}: hooks missing or outdated, run `styr setup`"),
                )
            }
        }
    }
    match crate::cmux_access::state() {
        crate::cmux_access::State::NoCmux => {}
        crate::cmux_access::State::Ready => line(Mark::Ok, "cmux", "Styrman can open tabs"),
        crate::cmux_access::State::Locked => line(
            Mark::Warn,
            "cmux",
            "New session on the phone can't open tabs: run `styr setup` to allow it",
        ),
        crate::cmux_access::State::Other(why) => line(Mark::Warn, "cmux", why),
    }
    let agents = routed_agents();
    match routing_state(&agents) {
        Routing::Active => line(Mark::Ok, "routing", format!("{} → styr", agents.join(", "))),
        Routing::NotInPath => line(
            Mark::Warn,
            "routing",
            "~/.styrman/bin isn't first on PATH in new shells: run `styr setup`",
        ),
        Routing::Missing => line(
            Mark::Warn,
            "routing",
            "not set up: sessions are read-only on the phone; run `styr setup`",
        ),
    }
    if agy_bin().is_some() {
        let v = agy_version().unwrap_or_else(|| "?".into());
        if agy_plugin_current() {
            line(Mark::Ok, "agy", format!("version {v}, plugin up to date"));
        } else {
            healthy = false;
            line(
                Mark::Warn,
                "agy",
                format!("version {v}: plugin missing or outdated, run `styr setup`"),
            );
        }
    }
    report_network_phone_push(diag.as_ref());
    if let Some(Response::Diagnostics {
        agents, warnings, ..
    }) = &diag
    {
        for a in agents {
            line(
                if a.verified { Mark::Ok } else { Mark::Warn },
                "agent version",
                format!(
                    "{} {} {}",
                    a.agent,
                    a.version,
                    if a.verified {
                        "(verified)"
                    } else {
                        "(not verified yet)"
                    }
                ),
            );
        }
        for w in warnings {
            println!("  note: {w}");
        }
    }
    if live {
        println!();
        healthy &= live_check()?;
    } else {
        println!(
            "\nRun `styr doctor --live` for an end-to-end check with your Claude Code (one small Haiku call)."
        );
    }
    Ok(healthy)
}

fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut b);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

fn run_claude(dir: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    let mut child = Command::new("claude")
        .args(args)
        .current_dir(dir)
        .env_remove("STYR_LAUNCH_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting claude")?;
    let until = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() > until {
            let _ = child.kill();
            anyhow::bail!("claude didn't finish within {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let out = child.wait_with_output()?;
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// Share a throwaway headless session, have it answer, and check that every piece
/// arrived in the daemon intact. Marks the Claude Code version as verified on success.
fn live_check() -> Result<bool> {
    let dir = std::env::temp_dir().join(format!("styr-doctor-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let session = uuid_v4();
    let model = "claude-haiku-5-5";
    let mut ok = true;
    let c = client();
    let find = || -> Option<styr_core::ipc::SessionSummary> {
        let canon = std::fs::canonicalize(&dir).ok()?;
        match c
            .call(&Request::ListSessions {
                include_unshared: true,
            })
            .ok()?
        {
            Response::Sessions { sessions } => sessions.into_iter().find(|s| {
                s.cwd
                    .as_ref()
                    .and_then(|p| std::fs::canonicalize(p).ok())
                    .as_ref()
                    == Some(&canon)
            }),
            _ => None,
        }
    };

    // 1. `/styr` is intercepted by the hook (no model call) and shares the session.
    let out = run_claude(
        &dir,
        &["-p", "/styr", "--session-id", &session, "--model", model],
        Duration::from_secs(60),
    )?;
    let shared = out.contains("shared with your phone") && find().is_some_and(|s| s.shared);
    line(
        if shared { Mark::Ok } else { Mark::Fail },
        "live: /styr",
        if shared {
            "hook intercepted it and shared the session"
        } else {
            "the hook didn't answer; is the plugin loaded?"
        },
    );
    ok &= shared;

    // 2. A real turn: prompt, reply and turn end all reach the daemon.
    if ok {
        let out = run_claude(
            &dir,
            &[
                "-p",
                "--resume",
                &session,
                "--model",
                model,
                "Reply with exactly the word DOCTOR-OK and nothing else.",
            ],
            Duration::from_secs(120),
        )?;
        let replied = out.contains("DOCTOR-OK");
        let events = find()
            .and_then(|s| {
                match c
                    .call(&Request::Events {
                        session: s.id,
                        since_seq: 0,
                        limit: 500,
                    })
                    .ok()?
                {
                    Response::Events { events } => Some(events),
                    _ => None,
                }
            })
            .unwrap_or_default();
        let names: Vec<&str> = events.iter().map(|e| e.kind.name()).collect();
        let mirrored = ["user_message", "assistant_message", "turn_ended"].iter().all(|n| names.contains(n))
            && events.iter().any(|e| matches!(&e.kind, styr_core::EventKind::AssistantMessage { text, .. } if text.contains("DOCTOR-OK")));
        line(
            if replied { Mark::Ok } else { Mark::Fail },
            "live: Claude",
            if replied {
                "answered"
            } else {
                "didn't answer (login? model access?)"
            },
        );
        line(
            if mirrored { Mark::Ok } else { Mark::Fail },
            "live: mirroring",
            if mirrored {
                "prompt, reply and turn end all reached styrd"
            } else {
                "events missing: hook payloads may have changed in this Claude version"
            },
        );
        ok &= replied && mirrored;
    }

    // Clean up the throwaway session.
    if let Some(s) = find() {
        let _ = c.call(&Request::ForgetSessions { ids: vec![s.id] });
    }
    let _ = std::fs::remove_dir_all(&dir);

    if agy_bin().is_some() {
        ok &= live_check_agy()?;
    }
    if ok && let Some(v) = claude_version() {
        let _ = c.call(&Request::MarkVerified {
            agent: AgentKind::Claude,
            version: v.clone(),
        });
        line(
            Mark::Ok,
            "live: result",
            format!("Claude Code {v} works with Styrman"),
        );
    } else if !ok {
        line(
            Mark::Fail,
            "live: result",
            "something is off; details above",
        );
    }
    Ok(ok)
}

/// The same end-to-end check for agy (headless, Gemini Flash).
fn live_check_agy() -> Result<bool> {
    let agy = agy_bin().context("agy not found")?;
    let dir = std::env::temp_dir().join(format!("styr-doctor-agy-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let c = client();
    let canon = std::fs::canonicalize(&dir)?;
    let find = || -> Option<styr_core::ipc::SessionSummary> {
        match c
            .call(&Request::ListSessions {
                include_unshared: true,
            })
            .ok()?
        {
            Response::Sessions { sessions } => sessions.into_iter().find(|s| {
                s.agent == AgentKind::Antigravity
                    && s.cwd
                        .as_ref()
                        .and_then(|p| std::fs::canonicalize(p).ok())
                        .as_ref()
                        == Some(&canon)
            }),
            _ => None,
        }
    };
    let run = |args: &[&str]| -> Result<String> {
        let out = Command::new(&agy)
            .args(args)
            .current_dir(&dir)
            .env_remove("STYR_LAUNCH_ID")
            .stdin(Stdio::null())
            .output()
            .context("running agy")?;
        Ok(format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ))
    };
    let model = "gemini-3.8-flash-medium";
    let _ = run(&["--model", model, "-p", "/styr"])?;
    let shared = find().is_some_and(|s| s.shared);
    line(
        if shared { Mark::Ok } else { Mark::Fail },
        "live: agy /styr",
        if shared {
            "hook intercepted it and shared the session"
        } else {
            "the agy hook didn't answer; is the plugin installed?"
        },
    );
    let mut ok = shared;
    if let (true, Some(s)) = (ok, find()) {
        let out = run(&[
            "--conversation",
            &s.native_id,
            "--model",
            model,
            "-p",
            "Reply with exactly the word DOCTOR-OK and nothing else.",
        ])?;
        let events = match c.call(&Request::Events {
            session: s.id.clone(),
            since_seq: 0,
            limit: 500,
        }) {
            Ok(Response::Events { events }) => events,
            _ => vec![],
        };
        let mirrored = events
            .iter()
            .any(|e| matches!(&e.kind, styr_core::EventKind::AssistantMessage { text, .. } if text.contains("DOCTOR-OK")));
        let answered = out.contains("DOCTOR-OK");
        line(
            if answered { Mark::Ok } else { Mark::Fail },
            "live: agy",
            if answered {
                "answered"
            } else {
                "didn't answer (signed in?)"
            },
        );
        line(
            if mirrored { Mark::Ok } else { Mark::Fail },
            "live: agy mirroring",
            if mirrored {
                "prompt and reply reached styrd"
            } else {
                "reply missing: agy's hooks or transcript may have changed"
            },
        );
        ok = answered && mirrored;
        let _ = c.call(&Request::ForgetSessions { ids: vec![s.id] });
    }
    let _ = std::fs::remove_dir_all(&dir);
    if ok && let Some(v) = agy_version() {
        let _ = c.call(&Request::MarkVerified {
            agent: AgentKind::Antigravity,
            version: v.clone(),
        });
        line(
            Mark::Ok,
            "live: agy result",
            format!("agy {v} works with Styrman"),
        );
    }
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_plugin_is_complete_and_runnable() {
        let d = tempfile::tempdir().unwrap();
        write_embedded_plugin(d.path()).unwrap();
        let root = d.path().join("claude/styrman");
        let hooks = std::fs::read_to_string(root.join("hooks/hooks.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&hooks).unwrap();
        for ev in [
            "SessionStart",
            "UserPromptSubmit",
            "PermissionRequest",
            "PostToolUse",
            "Stop",
            "SessionEnd",
        ] {
            assert!(v["hooks"][ev].is_array(), "{ev} hook missing");
        }
        assert!(hooks.contains("StopWait"), "background Stop hook present");
        let shim = root.join("bin/styr-hook");
        assert_eq!(
            std::fs::metadata(&shim).unwrap().permissions().mode() & 0o111,
            0o111,
            "shim executable"
        );
        let market: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(d.path().join(".claude-plugin/marketplace.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(market["plugins"][0]["source"], "./claude/styrman");
        assert!(root.join(".claude-plugin/plugin.json").exists());
        assert!(root.join("commands/styr.md").exists());
    }

    #[test]
    fn versions_from_install_paths() {
        let v = |p: &str| crate::hook::version_from_path(Path::new(p));
        assert_eq!(
            v("/Users/a/.local/share/claude/versions/2.1.332"),
            Some("2.1.332".into())
        );
        assert_eq!(v("/opt/homebrew/bin/claude"), None);
        assert_eq!(v("/x/versions/2.1.beta"), None);
    }
}
