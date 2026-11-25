//! `styr service install|uninstall|status|restart`: run styrd as a per-user
//! launchd agent (starts at login, restarted if it exits). macOS only.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const LABEL: &str = "com.styrman.styrd";

fn plist_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn domain() -> String {
    // SAFETY: getuid has no preconditions.
    format!("gui/{}", unsafe { libc::getuid() })
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The launchd plist for `styrd` at `program`, logging to `log`.
pub fn plist(program: &Path, log: &Path) -> String {
    let (p, l) = (
        xml_escape(&program.display().to_string()),
        xml_escape(&log.display().to_string()),
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{p}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ThrottleInterval</key>
	<integer>5</integer>
	<key>ProcessType</key>
	<string>Background</string>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key>
		<string>/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin</string>
	</dict>
	<key>StandardOutPath</key>
	<string>{l}</string>
	<key>StandardErrorPath</key>
	<string>{l}</string>
</dict>
</plist>
"#
    )
}

fn launchctl(args: &[&str]) -> Result<std::process::Output> {
    Command::new("launchctl")
        .args(args)
        .output()
        .context("running launchctl")
}

fn loaded() -> bool {
    launchctl(&["print", &format!("{}/{LABEL}", domain())]).is_ok_and(|o| o.status.success())
}

/// `styrd` next to this `styr`, else on PATH.
fn find_styrd() -> Result<PathBuf> {
    if let Some(p) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("styrd")))
        && p.is_file()
    {
        return Ok(p);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|d| d.join("styrd"))
        .find(|p| p.is_file())
        .context("styrd not found (cargo install --path crates/styrd)")
}

pub fn is_loaded() -> bool {
    loaded()
}

/// Install without printing (for `styr setup`).
pub fn install_quiet() -> Result<()> {
    install_inner(false)
}

pub fn install() -> Result<()> {
    install_inner(true)
}

fn install_inner(verbose: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("launchd is macOS only; on Linux run styrd from a systemd user unit");
    }
    let styrd = find_styrd()?.canonicalize()?;
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    let log = home.join("Library/Logs/styrd.log");
    let path = plist_path()?;
    std::fs::create_dir_all(path.parent().expect("has parent"))?;
    if loaded() {
        let _ = launchctl(&["bootout", &format!("{}/{LABEL}", domain())]);
    }
    std::fs::write(&path, plist(&styrd, &log))?;
    let out = launchctl(&["bootstrap", &domain(), &path.display().to_string()])?;
    if !out.status.success() {
        bail!(
            "launchctl bootstrap failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    if verbose {
        println!(
            "installed {LABEL}: {} (starts at login, restarted if it exits)",
            styrd.display()
        );
        println!("plist {}\nlog   {}", path.display(), log.display());
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let path = plist_path()?;
    if loaded() {
        let out = launchctl(&["bootout", &format!("{}/{LABEL}", domain())])?;
        if !out.status.success() {
            bail!(
                "launchctl bootout failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    println!("removed {LABEL}; styrd no longer starts at login");
    Ok(())
}

pub fn restart() -> Result<()> {
    if !loaded() {
        bail!("the launchd agent is not installed (styr service install)");
    }
    let out = launchctl(&["kickstart", "-k", &format!("{}/{LABEL}", domain())])?;
    if !out.status.success() {
        bail!(
            "launchctl kickstart failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    println!("restarted {LABEL}");
    Ok(())
}

pub fn status() -> Result<()> {
    let path = plist_path()?;
    println!(
        "plist:  {}",
        if path.exists() {
            path.display().to_string()
        } else {
            "not installed".into()
        }
    );
    println!("loaded: {}", if loaded() { "yes" } else { "no" });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_is_well_formed() {
        let p = plist(
            Path::new("/Users/a b/.cargo/bin/styrd"),
            Path::new("/Users/a b/Library/Logs/styrd.log"),
        );
        assert!(p.contains("<string>/Users/a b/.cargo/bin/styrd</string>"));
        assert!(p.contains("<key>KeepAlive</key>\n\t<true/>"));
        let out = Command::new("plutil")
            .args(["-lint", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                c.stdin.take().unwrap().write_all(p.as_bytes())?;
                c.wait_with_output()
            });
        if let Ok(o) = out {
            assert!(
                o.status.success(),
                "plutil: {}",
                String::from_utf8_lossy(&o.stdout)
            );
        }
    }
}
