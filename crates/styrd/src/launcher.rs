//! Starting a new session from the phone: pick a folder in your home directory and an
//! agent, and Styrman opens a new cmux tab there running that agent (through the same
//! `~/.styrman/bin` routing as when you type it), already shared with the phone.
//!
//! Only `claude`, `codex` or `agy` can be started, only in a folder inside your home
//! directory, and the agent asks for approvals exactly as it would at the keyboard.

use crate::daemon::Daemon;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use styr_core::event::{EventKind, EventSource};
use styr_core::ids::{AgentKind, DeviceId, SessionId};
use styr_core::phone::{FolderEntry, FolderListing};

/// The phone's first message waits at most this long for the agent to start.
const START_GRACE: Duration = Duration::from_secs(90);
const MAX_ENTRIES: usize = 400;
const MAX_RECENT: usize = 12;

fn home() -> Result<PathBuf> {
    let h = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(std::fs::canonicalize(h)?)
}

/// `path` as a real directory inside the home directory (symlinks resolved).
pub fn inside_home(path: &Path) -> Result<PathBuf> {
    let home = home()?;
    let real =
        std::fs::canonicalize(path).with_context(|| format!("{} doesn't exist", path.display()))?;
    if !real.starts_with(&home) {
        bail!("only folders inside your home directory can be used");
    }
    if !real.is_dir() {
        bail!("{} isn't a folder", real.display());
    }
    Ok(real)
}

/// The agents `styr setup` routed through Styrman (it only does so when installed).
pub fn launchable_agents() -> Vec<AgentKind> {
    let Ok(home) = home() else { return vec![] };
    [
        (AgentKind::Claude, "claude"),
        (AgentKind::Codex, "codex"),
        (AgentKind::Antigravity, "agy"),
    ]
    .into_iter()
    .filter(|(_, bin)| home.join(".styrman/bin").join(bin).exists())
    .map(|(a, _)| a)
    .collect()
}

fn command_name(agent: AgentKind) -> Option<&'static str> {
    match agent {
        AgentKind::Claude => Some("claude"),
        AgentKind::Codex => Some("codex"),
        AgentKind::Antigravity => Some("agy"),
        AgentKind::Pty => None,
    }
}

fn cmux_bin() -> Option<PathBuf> {
    let app = PathBuf::from("/Applications/cmux.app/Contents/Resources/bin/cmux");
    if app.exists() {
        return Some(app);
    }
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join("cmux"))
            .find(|c| c.exists())
    })
}

fn random_hex() -> String {
    let mut b = [0u8; 8];
    rand::fill(&mut b);
    hex::encode(b)
}

impl Daemon {
    /// Folders for the phone's picker: subfolders of `path` (default: home), plus the
    /// folders recent sessions ran in.
    pub fn list_folders(&self, path: Option<&str>) -> Result<FolderListing> {
        let home = home()?;
        let dir = match path {
            Some(p) if !p.is_empty() => inside_home(Path::new(p))?,
            _ => home.clone(),
        };
        let mut entries: Vec<FolderEntry> = std::fs::read_dir(&dir)?
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                // Hidden folders and macOS's own (Library) are noise in a picker.
                if name.starts_with('.') || (dir == home && name == "Library") {
                    return None;
                }
                let path = e.path();
                Some(FolderEntry {
                    is_repo: path.join(".git").exists(),
                    path: path.to_string_lossy().into_owned(),
                    name,
                })
            })
            .collect();
        entries.sort_by_key(|e| e.name.to_lowercase());
        entries.truncate(MAX_ENTRIES);
        let mut recent: Vec<String> = Vec::new();
        for s in self.summaries(true)? {
            let Some(cwd) = s.cwd else { continue };
            let Ok(real) = inside_home(&cwd) else {
                continue;
            };
            let real = real.to_string_lossy().into_owned();
            if !recent.contains(&real) {
                recent.push(real);
            }
            if recent.len() == MAX_RECENT {
                break;
            }
        }
        Ok(FolderListing {
            path: dir.to_string_lossy().into_owned(),
            parent: (dir != home)
                .then(|| dir.parent().map(|p| p.to_string_lossy().into_owned()))
                .flatten(),
            home: home.to_string_lossy().into_owned(),
            entries,
            recent,
            agents: launchable_agents(),
        })
    }

    /// Open a new cmux tab in `cwd` running `agent`, shared with the phone. Returns the
    /// session (a placeholder until the agent's first hook adopts it). `prompt` is
    /// typed into the agent once it's ready for input.
    pub fn start_session(
        self: &Arc<Self>,
        agent: AgentKind,
        cwd: &str,
        prompt: Option<String>,
        device: DeviceId,
    ) -> Result<SessionId> {
        let name = command_name(agent).context("that agent can't be started from the phone")?;
        if !launchable_agents().contains(&agent) {
            bail!("{} isn't set up on the Mac (run `styr setup`)", agent);
        }
        let cwd = inside_home(Path::new(cwd))?;
        let cmux = cmux_bin().context("cmux isn't installed on the Mac")?;
        let launch = random_hex();
        self.share_launch(&launch, true, Some(agent), Some(cwd.clone()), None)?;
        let session = self
            .session_by_native(&crate::daemon::placeholder_native(agent, &launch))?
            .context("placeholder vanished")?
            .id;
        if let Some(text) = prompt
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
        {
            self.terminals
                .enqueue(&session, crate::prompts::QueuedPrompt { text, device });
        }
        let folder = cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "~".into());
        // The command is typed into the tab's own shell, so it goes through cmux's and
        // Styrman's routing exactly like typing `codex` there. Only fixed text and a
        // hex id go into it; the folder is passed separately.
        let mut cmd = std::process::Command::new(&cmux);
        cmd.env("CMUX_QUIET", "1");
        // styrd isn't a cmux process: cmux lets it in with the password `styr setup`
        // set up (password mode).
        if let Some(pw) = home()
            .ok()
            .and_then(|h| std::fs::read_to_string(h.join(".styrman/cmux-password")).ok())
        {
            cmd.env("CMUX_SOCKET_PASSWORD", pw.trim());
        }
        let out = cmd
            .args([
                "new-workspace",
                "--name",
                &format!("{} · {folder}", agent_label(agent)),
            ])
            .arg("--cwd")
            .arg(&cwd)
            .args([
                "--command",
                &format!("STYR_PRESET_LAUNCH_ID={launch} {name}"),
            ])
            .args(["--focus", "false"])
            .output()
            .context("couldn't run cmux")?;
        if !out.status.success() {
            let mut why = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            if why.contains("Access denied") {
                why = "cmux only lets its own programs open tabs. Run `styr setup` on the Mac to allow Styrman.".into();
            }
            self.end_unstarted(&session, &format!("cmux couldn't open a tab: {why}"));
            bail!("cmux couldn't open a tab: {why}");
        }
        tracing::info!(%session, %agent, cwd = %cwd.display(), "started from the phone");
        // If the agent never comes up (cmux closed, agent missing), say so.
        let d = self.clone();
        let s = session.clone();
        tokio::spawn(async move {
            tokio::time::sleep(START_GRACE).await;
            if !d.terminals.is_live(&s) {
                d.end_unstarted(&s, "The agent didn't start in the new cmux tab.");
            }
        });
        Ok(session)
    }

    fn end_unstarted(&self, session: &SessionId, why: &str) {
        let Ok(Some(s)) = self.summary(session) else {
            return;
        };
        if s.status == styr_core::SessionStatus::Done {
            return;
        }
        let native = styr_core::ids::NativeSessionRef::new(s.agent, s.native_id);
        let _ = self.ingest(
            &styr_core::ipc::SessionUpdate::for_native(native),
            EventSource::Daemon,
            vec![
                EventKind::Error {
                    message: why.to_owned(),
                    fatal: true,
                },
                EventKind::SessionEnded {
                    reason: Some(why.to_owned()),
                },
            ],
        );
        self.terminals.forget(session);
    }
}

fn agent_label(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "Claude",
        AgentKind::Codex => "Codex",
        AgentKind::Antigravity => "agy",
        AgentKind::Pty => "Shell",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_outside_home_are_refused() {
        assert!(inside_home(Path::new("/etc")).is_err());
        assert!(inside_home(Path::new("/")).is_err());
        let home = home().unwrap();
        assert_eq!(inside_home(&home).unwrap(), home);
        // `..` can't climb out either.
        assert!(inside_home(&home.join("..")).is_err());
    }
}
