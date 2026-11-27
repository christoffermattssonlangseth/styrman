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
const MAX_FILES: usize = 200;

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

/// A folder name typed on the phone: one plain path component.
pub fn valid_folder_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() || name.len() > 100 {
        bail!("give the folder a name (up to 100 characters)");
    }
    if name.contains('/') || name.contains('\0') || name.starts_with('.') {
        bail!("a folder name can't contain '/' or start with '.'");
    }
    Ok(name)
}

/// The checked-out branch of a git repository at `dir` (from .git/HEAD).
fn git_branch(dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(dir.join(".git/HEAD")).ok()?;
    let head = head.trim();
    Some(match head.strip_prefix("ref: refs/heads/") {
        Some(b) => b.to_owned(),
        None => format!("detached at {}", head.chars().take(7).collect::<String>()),
    })
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
        let mut files: Vec<String> = std::fs::read_dir(&dir)?
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| !t.is_dir()))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .collect();
        files.sort_by_key(|n| n.to_lowercase());
        let more_files = files.len() > MAX_FILES;
        files.truncate(MAX_FILES);
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
            git_branch: git_branch(&dir),
            entries,
            recent,
            agents: launchable_agents(),
            files,
            more_files,
        })
    }

    /// Make a new folder inside `parent` and list it.
    pub fn create_folder(&self, parent: &str, name: &str) -> Result<FolderListing> {
        let parent = inside_home(Path::new(parent))?;
        let name = valid_folder_name(name)?;
        let path = parent.join(name);
        if path.exists() {
            bail!("“{name}” already exists there");
        }
        std::fs::create_dir(&path)
            .with_context(|| format!("couldn't create {}", path.display()))?;
        tracing::info!(path = %path.display(), "folder created from the phone");
        self.list_folders(Some(&path.to_string_lossy()))
    }

    /// Open a new terminal in `cwd` running `agent` (see `open_terminal`), shared with
    /// the phone. Returns the
    /// session (a placeholder until the agent's first hook adopts it). `prompt` is
    /// typed into the agent once it's ready for input.
    pub fn start_session(
        self: &Arc<Self>,
        agent: AgentKind,
        cwd: &str,
        prompt: Option<String>,
        device: DeviceId,
    ) -> Result<SessionId> {
        self.start_session_in(agent, cwd, prompt, device, None)
    }

    /// `start_session`, as a new tab in cmux workspace `workspace` when given (an
    /// agent's `styr new`: the new agent opens beside it, like ⌘T, not ⌘N).
    pub fn start_session_in(
        self: &Arc<Self>,
        agent: AgentKind,
        cwd: &str,
        prompt: Option<String>,
        device: DeviceId,
        workspace: Option<&str>,
    ) -> Result<SessionId> {
        let name = command_name(agent).context("that agent can't be started from the phone")?;
        if !launchable_agents().contains(&agent) {
            bail!("{} isn't set up on the Mac (run `styr setup`)", agent);
        }
        let cwd = inside_home(Path::new(cwd))?;
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
        let title = format!("{} · {folder}", agent_label(agent));
        // Only fixed text and a hex id go into the command; the folder is passed
        // separately (or quoted).
        let command = format!("STYR_PRESET_LAUNCH_ID={launch} {name}");
        let opened = match open_terminal(&title, &cwd, &command, &launch, workspace) {
            Ok(w) => w,
            Err(e) => {
                self.end_unstarted(&session, &format!("{e:#}"));
                return Err(e);
            }
        };
        tracing::info!(%session, %agent, cwd = %cwd.display(), %opened, "started from the phone");
        // If the agent never comes up (cmux closed, agent missing), say so.
        let d = self.clone();
        let s = session.clone();
        tokio::spawn(async move {
            tokio::time::sleep(START_GRACE).await;
            if !d.terminals.is_live(&s) {
                d.end_unstarted(&s, &format!("The agent didn't start in the new {opened}."));
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

/// Where a session from the phone runs: a new cmux tab if cmux is there (its own
/// shell, so its and Styrman's routing apply as when you type the command), else a
/// tmux session (`tmux attach` to see it), else a Terminal window. Returns what was
/// opened, for messages.
fn open_terminal(
    title: &str,
    cwd: &Path,
    command: &str,
    launch: &str,
    workspace: Option<&str>,
) -> Result<&'static str> {
    let mut why = vec![];
    if let Some(cmux) = cmux_bin() {
        let cmux_cmd = || {
            let mut cmd = std::process::Command::new(&cmux);
            cmd.env("CMUX_QUIET", "1");
            // styrd isn't a cmux process: cmux lets it in with the password `styr
            // setup` set up (password mode).
            if let Some(pw) = home()
                .ok()
                .and_then(|h| std::fs::read_to_string(h.join(".styrman/cmux-password")).ok())
            {
                cmd.env("CMUX_SOCKET_PASSWORD", pw.trim());
            }
            cmd
        };
        if let Some(ws) = workspace.filter(|w| valid_cmux_id(w)) {
            // A tab in that workspace runs a shell in no particular folder: cd first.
            let out = cmux_cmd()
                .args(["new-surface", "--workspace", ws])
                .args([
                    "--command",
                    &format!("cd {} && {command}", shell_quote(&cwd.to_string_lossy())),
                ])
                .args(["--focus", "false"])
                .output()
                .context("couldn't run cmux")?;
            if out.status.success() {
                if let Some(surface) = surface_ref(&String::from_utf8_lossy(&out.stdout)) {
                    let _ = cmux_cmd()
                        .args(["rename-tab", "--workspace", ws, "--surface", surface, title])
                        .output();
                }
                return Ok("cmux tab");
            }
            // The workspace closed, most likely: open one of its own instead.
            tracing::info!(
                "cmux couldn't open a tab in workspace {ws} ({}); opening a workspace",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let out = cmux_cmd()
            .args(["new-workspace", "--name", title])
            .arg("--cwd")
            .arg(cwd)
            .args(["--command", command])
            .args(["--focus", "false"])
            .output()
            .context("couldn't run cmux")?;
        if out.status.success() {
            return Ok("cmux tab");
        }
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        if err.contains("Access denied") {
            bail!(
                "cmux couldn't open a tab: cmux only lets its own programs open tabs. Run `styr setup` on the Mac to allow Styrman."
            );
        }
        // Not running, most likely: try the others.
        tracing::info!("cmux couldn't open a tab ({err}); trying tmux or Terminal");
        why.push(format!("cmux: {err}"));
    }
    if let Some(tmux) = tmux_bin() {
        let out = std::process::Command::new(tmux)
            .args(tmux_args(cwd, command, launch, &login_shell()))
            .output()
            .context("couldn't run tmux")?;
        if out.status.success() {
            return Ok("tmux session");
        }
        why.push(format!(
            "tmux: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let out = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &terminal_script(cwd, command)])
        .output()
        .context("couldn't run osascript")?;
    if out.status.success() {
        return Ok("Terminal window");
    }
    why.push(format!(
        "Terminal: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    ));
    bail!("Couldn't open a terminal on the Mac ({})", why.join("; "))
}

/// A cmux workspace id or ref as cmux gives them (UUID, `workspace:3`): nothing else
/// goes on its command line.
fn valid_cmux_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ':')
}

/// The `surface:N` in cmux's `OK surface:N pane:M workspace:K`.
fn surface_ref(out: &str) -> Option<&str> {
    out.split_whitespace().find(|w| w.starts_with("surface:"))
}

fn tmux_bin() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin".into(), "/usr/local/bin".into()]);
    dirs.into_iter()
        .map(|d| d.join("tmux"))
        .find(|p| p.is_file())
}

fn login_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| s.starts_with('/'))
        .unwrap_or_else(|| "/bin/zsh".into())
}

/// A detached tmux session in `cwd` running `command` in the user's login shell (for
/// their PATH, as in a terminal); it closes when the agent quits.
fn tmux_args(cwd: &Path, command: &str, launch: &str, shell: &str) -> Vec<std::ffi::OsString> {
    vec![
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        format!("styr-{}", &launch[..launch.len().min(8)]).into(),
        "-c".into(),
        cwd.into(),
        shell.into(),
        "-lic".into(),
        command.into(),
    ]
}

/// `'it''s'` for the shell: single quotes, with any inside closed and escaped.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// AppleScript that opens a Terminal window running `command` in `cwd`.
fn terminal_script(cwd: &Path, command: &str) -> String {
    let line = format!("cd {} && {command}", shell_quote(&cwd.to_string_lossy()));
    let escaped = line.replace('\\', "\\\\").replace('"', "\\\"");
    format!("tell application \"Terminal\" to do script \"{escaped}\"")
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
    fn only_a_cmux_id_reaches_its_command_line() {
        assert!(valid_cmux_id("DA99B0BC-CB8F-4895-89A7-962CC9FADC31"));
        assert!(valid_cmux_id("workspace:3"));
        for bad in ["", "--focus", "a b", "x;rm", "$(id)", &"a".repeat(65)] {
            assert!(!valid_cmux_id(bad), "{bad}");
        }
    }

    #[test]
    fn the_new_tab_is_found_in_cmuxs_answer() {
        assert_eq!(
            surface_ref("OK surface:1000040119 pane:1000040052 workspace:1000040020\n"),
            Some("surface:1000040119")
        );
        assert_eq!(surface_ref("OK"), None);
    }

    #[test]
    fn tmux_and_terminal_commands_quote_the_folder() {
        let cwd = Path::new("/Users/me/it's \"odd\" \\ dir");
        let cmd = "STYR_PRESET_LAUNCH_ID=0123456789abcdef claude";
        let args: Vec<String> = tmux_args(cwd, cmd, "0123456789abcdef", "/bin/zsh")
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "new-session",
                "-d",
                "-s",
                "styr-01234567",
                "-c",
                "/Users/me/it's \"odd\" \\ dir",
                "/bin/zsh",
                "-lic",
                cmd
            ]
        );
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(
            terminal_script(cwd, cmd),
            r#"tell application "Terminal" to do script "cd '/Users/me/it'\\''s \"odd\" \\ dir' && STYR_PRESET_LAUNCH_ID=0123456789abcdef claude""#
        );
    }

    #[test]
    fn folder_names_are_one_plain_component() {
        for ok in ["app", "my project", "v2.0"] {
            assert!(valid_folder_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "a/b", "../x", ".hidden", "..", &"x".repeat(101)] {
            assert!(valid_folder_name(bad).is_err(), "{bad}");
        }
    }

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
