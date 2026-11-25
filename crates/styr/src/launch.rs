//! `styr claude …` / `styr agy …` / `styr codex …`: exec the real agent with
//! Styrman's environment. Meant to be aliased (`alias claude='styr claude'`).
//!
//! The agent binary is the next one on PATH that is not `styr` itself, so wrappers
//! earlier on PATH (e.g. cmux shims) stay in the chain.

use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use styr_core::adapter::{AgentAdapter, LaunchSpec};
use styr_core::ids::AgentKind;

pub fn launch_spec(agent: AgentKind) -> Option<LaunchSpec> {
    let spec = match agent {
        AgentKind::Claude => styr_adapter_claude::ClaudeAdapter::default().launcher(),
        AgentKind::Antigravity => styr_adapter_agy::AntigravityAdapter.launcher(),
        AgentKind::Codex => styr_adapter_codex::CodexAdapter.launcher(),
        AgentKind::Pty => return None,
    };
    Some(spec)
}

fn is_executable(p: &Path) -> bool {
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// First `program` on `path_var` that does not resolve to `self_exe`.
pub fn find_on_path(
    program: &str,
    path_var: &OsString,
    self_exe: Option<&Path>,
) -> Option<PathBuf> {
    let me = self_exe.and_then(|p| p.canonicalize().ok());
    let shims = shim_dir().and_then(|d| d.canonicalize().ok());
    std::env::split_paths(path_var)
        // Our own shims (`~/.styrman/bin/<agent>` → `styr <agent>`) would loop.
        .filter(|dir| shims.is_none() || dir.canonicalize().ok() != shims)
        .map(|dir| dir.join(program))
        .filter(|c| is_executable(c))
        .find(|c| match (&me, c.canonicalize()) {
            (Some(me), Ok(real)) => &real != me,
            _ => true,
        })
}

/// Where `styr setup` puts the `claude`/`agy` shims that route through Styrman.
pub fn shim_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".styrman/bin"))
}

/// The shim: `claude`/`agy` on PATH → `styr <agent>`. Inside `styr <agent>`
/// (STYR_WRAPPED set, e.g. when cmux's wrapper looks up the next `agy` on PATH and
/// finds us) it hands straight to the next real binary after its own directory.
pub const SHIM: &str = r#"#!/bin/sh
# Styrman shim (installed by `styr setup`): runs this agent through `styr <agent>`
# so approvals and prompts from the phone work, whatever the shell does to aliases.
name=$(basename "$0")
self_dir=$(cd "$(dirname "$0")" && pwd -P)
next_after_self() {
  seen=0
  old_ifs=$IFS; IFS=:
  for d in $PATH; do
    if [ "$(cd "$d" 2>/dev/null && pwd -P)" = "$self_dir" ]; then seen=1; continue; fi
    if [ "$seen" = 1 ] && [ -x "$d/$name" ]; then IFS=$old_ifs; printf '%s' "$d/$name"; return 0; fi
  done
  IFS=$old_ifs
  return 1
}
# The next `$name` on PATH after the directory of $1 (the binary styr already chose).
next_after_dir_of() {
  target_dir=$(cd "$(dirname "$1")" 2>/dev/null && pwd -P) || return 1
  seen=0
  old_ifs=$IFS; IFS=:
  for d in $PATH; do
    dd=$(cd "$d" 2>/dev/null && pwd -P)
    if [ "$dd" = "$target_dir" ]; then seen=1; continue; fi
    if [ "$seen" = 1 ] && [ "$dd" != "$self_dir" ] && [ -x "$d/$name" ]; then IFS=$old_ifs; printf '%s' "$d/$name"; return 0; fi
  done
  IFS=$old_ifs
  return 1
}
# Guard against wrapper loops (another tool's shim pointing back at us).
STYR_SHIM_HOPS=$(( ${STYR_SHIM_HOPS:-0} + 1 )); export STYR_SHIM_HOPS
if [ "$STYR_SHIM_HOPS" -gt 4 ]; then echo "styr shim: $name keeps resolving to itself; check PATH" >&2; exit 1; fi
styr_bin=$(command -v styr 2>/dev/null)
[ -n "$styr_bin" ] || { [ -x "$HOME/.cargo/bin/styr" ] && styr_bin="$HOME/.cargo/bin/styr"; }
if [ -z "$STYR_WRAPPED" ] && [ -n "$styr_bin" ]; then
  exec "$styr_bin" "$name" "$@"
fi
# Already inside styr. If a wrapper styr ran (e.g. cmux's) called back into us,
# continue after it rather than from here, or that wrapper would run twice.
real=""
[ -n "$STYR_AGENT_BIN" ] && real=$(next_after_dir_of "$STYR_AGENT_BIN")
[ -n "$real" ] || real=$(next_after_self) || { echo "$name: not found on PATH" >&2; exit 127; }
exec "$real" "$@"
"#;

pub fn exec(agent: AgentKind, args: Vec<OsString>) -> Result<std::convert::Infallible> {
    let spec = launch_spec(agent).with_context(|| format!("no launcher for {agent}"))?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let exe = std::env::current_exe().ok();
    let Some(bin) = find_on_path(&spec.program, &path, exe.as_deref()) else {
        bail!("`{}` not found on PATH", spec.program);
    };
    // Interactive Claude: run it in our terminal wrapper so phone prompts can be
    // typed into it. Everything else (pipes, -p, other agents for now): plain exec.
    // The shim uses this to skip past `bin` if a wrapper we run calls back into it.
    let mut env = spec.env.clone();
    env.push(("STYR_AGENT_BIN".into(), bin.to_string_lossy().into_owned()));
    let interactive = !args
        .iter()
        .any(|a| a == "-p" || a == "--print" || a == "--prompt")
        && !(agent == AgentKind::Codex && codex_is_batch(&args));
    let detector: Option<crate::pty::PromptDetector> = match agent {
        AgentKind::Antigravity => Some(crate::pty::detect_agy_prompt),
        AgentKind::Codex => Some(crate::pty::detect_codex_prompt),
        _ => None,
    };
    let wrappable = matches!(
        agent,
        AgentKind::Claude | AgentKind::Antigravity | AgentKind::Codex
    );
    if wrappable && interactive && crate::pty::can_wrap() {
        let launch_id = format!("{:016x}", rand_u64());
        let mut all: Vec<OsString> = spec.extra_args.iter().map(OsString::from).collect();
        all.extend(args);
        let catch_styr = agent == AgentKind::Codex;
        let code = crate::pty::run(&bin, &all, &env, &launch_id, detector, catch_styr)?;
        std::process::exit(code);
    }
    let mut cmd = std::process::Command::new(&bin);
    cmd.args(&spec.extra_args).args(args);
    for (k, v) in &env {
        cmd.env(k, v);
    }
    // Only returns on failure.
    let err = cmd.exec();
    Err(err).with_context(|| format!("exec {}", bin.display()))
}

/// `codex exec …`, `codex login`, `codex --version`, …: not the interactive TUI.
fn codex_is_batch(args: &[OsString]) -> bool {
    const BATCH: &[&str] = &[
        "exec", "e", "review", "login", "logout", "mcp", "mcp-server", "app-server",
        "completion", "sandbox", "debug", "apply", "a", "cloud", "features", "queue",
        "help", "-h", "--help", "-V", "--version",
    ];
    args.first()
        .and_then(|a| a.to_str())
        .is_some_and(|a| BATCH.contains(&a))
}

fn rand_u64() -> u64 {
    let mut b = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut b);
    }
    u64::from_ne_bytes(b) ^ std::process::id() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn skips_itself_and_finds_the_next_binary() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let me = d.path().join("styr");
        std::fs::write(&me, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&me, std::fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&me, a.join("claude")).unwrap(); // e.g. a symlink claude -> styr
        let real = b.join("claude");
        std::fs::write(&real, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths([&a, &b]).unwrap();
        assert_eq!(find_on_path("claude", &path, Some(&me)), Some(real));
        assert_eq!(find_on_path("nope", &path, Some(&me)), None);
    }

    /// The chain inside cmux: cmux's `agy` function → cmux wrapper (execs the next
    /// `agy` on PATH after its own dir) → our shim → `styr agy` → cmux wrapper again →
    /// our shim (now STYR_WRAPPED) → the real agy. No loops, styr exactly once.
    #[test]
    fn shim_routes_through_styr_once_even_behind_cmux() {
        let d = tempfile::tempdir().unwrap();
        let dir = |n: &str| {
            let p = d.path().join(n);
            std::fs::create_dir_all(&p).unwrap();
            p
        };
        let (cmux, shims, real, tools) = (dir("cmux"), dir("shims"), dir("real"), dir("tools"));
        let write = |p: std::path::PathBuf, body: &str| {
            std::fs::write(&p, body).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        // cmux-like wrapper: exec the next `agy` on PATH after its own directory.
        write(
            cmux.join("agy"),
            "#!/bin/sh\nself=$(cd \"$(dirname \"$0\")\" && pwd -P); seen=0; IFS=:\nfor d in $PATH; do if [ \"$(cd \"$d\" && pwd -P)\" = \"$self\" ]; then seen=1; continue; fi; if [ $seen = 1 ] && [ -x \"$d/agy\" ]; then exec \"$d/agy\" \"$@\"; fi; done; exit 99\n",
        );
        write(shims.join("agy"), SHIM);
        write(
            real.join("agy"),
            "#!/bin/sh\necho \"REAL $* wrapped=$STYR_WRAPPED via=$VIA\"\n",
        );
        // styr picks the first agy on PATH that isn't a shim: here cmux's wrapper.
        let cmux_agy = cmux.join("agy");
        write(
            tools.join("styr"),
            &format!(
                "#!/bin/sh\nshift\nVIA=\"$VIA+styr\" STYR_WRAPPED=1 exec {} \"$@\"\n",
                cmux_agy.display()
            ),
        );
        let sys = std::path::PathBuf::from("/usr/bin");
        let bin = std::path::PathBuf::from("/bin");
        let path = std::env::join_paths([&cmux, &shims, &tools, &real, &sys, &bin]).unwrap();
        let out = std::process::Command::new(&cmux_agy)
            .arg("hello")
            .env("PATH", &path)
            .env_remove("STYR_WRAPPED")
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "REAL hello wrapped=1 via=+styr",
            "stderr: {} code {:?}",
            String::from_utf8_lossy(&out.stderr),
            out.status.code()
        );
        // Outside cmux the shim itself is first on PATH.
        let path = std::env::join_paths([&shims, &tools, &real, &sys, &bin]).unwrap();
        write(
            tools.join("styr"),
            &format!(
                "#!/bin/sh\nshift\nVIA=\"$VIA+styr\" STYR_WRAPPED=1 exec {} \"$@\"\n",
                real.join("agy").display()
            ),
        );
        let out = std::process::Command::new(shims.join("agy"))
            .arg("x")
            .env("PATH", &path)
            .env_remove("STYR_WRAPPED")
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "REAL x wrapped=1 via=+styr"
        );
    }

    #[test]
    fn cmux_codex_wrapper_runs_once_when_it_calls_back_into_the_shim() {
        let d = tempfile::tempdir().unwrap();
        let dir = |n: &str| {
            let p = d.path().join(n);
            std::fs::create_dir_all(&p).unwrap();
            p
        };
        let (shims, cmux, real, tools) = (dir("shims"), dir("cmux"), dir("real"), dir("tools"));
        let write = |p: std::path::PathBuf, body: &str| {
            std::fs::write(&p, body).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        // Like cmux's codex wrapper: add a flag, then exec the FIRST other codex on PATH
        // (which is Styrman's shim, since ~/.styrman/bin comes first).
        write(
            cmux.join("codex"),
            "#!/bin/sh\nself=$(cd \"$(dirname \"$0\")\" && pwd -P); IFS=:\nfor d in $PATH; do if [ \"$(cd \"$d\" && pwd -P)\" = \"$self\" ]; then continue; fi; if [ -x \"$d/codex\" ]; then exec \"$d/codex\" --cmux \"$@\"; fi; done; exit 99\n",
        );
        write(shims.join("codex"), SHIM);
        write(real.join("codex"), "#!/bin/sh\necho \"REAL $*\"\n");
        // styr: picks cmux's wrapper (first codex that isn't a shim) and says so.
        write(
            tools.join("styr"),
            &format!(
                "#!/bin/sh\nshift\nSTYR_WRAPPED=1 STYR_AGENT_BIN={0} exec {0} \"$@\"\n",
                cmux.join("codex").display()
            ),
        );
        let path = std::env::join_paths([&shims, &cmux, &tools, &real, &"/usr/bin".into(), &"/bin".into()]).unwrap();
        let out = std::process::Command::new(shims.join("codex"))
            .arg("hi")
            .env("PATH", &path)
            .env_remove("STYR_WRAPPED")
            .env_remove("STYR_AGENT_BIN")
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "REAL --cmux hi",
            "cmux's flag once; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
