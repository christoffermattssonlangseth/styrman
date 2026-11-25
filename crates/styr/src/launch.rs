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
    std::env::split_paths(path_var)
        .map(|dir| dir.join(program))
        .filter(|c| is_executable(c))
        .find(|c| match (&me, c.canonicalize()) {
            (Some(me), Ok(real)) => &real != me,
            _ => true,
        })
}

pub fn exec(agent: AgentKind, args: Vec<OsString>) -> Result<std::convert::Infallible> {
    let spec = launch_spec(agent).with_context(|| format!("no launcher for {agent}"))?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let exe = std::env::current_exe().ok();
    let Some(bin) = find_on_path(&spec.program, &path, exe.as_deref()) else {
        bail!("`{}` not found on PATH", spec.program);
    };
    // Interactive Claude: run it in our terminal wrapper so phone prompts can be
    // typed into it. Everything else (pipes, -p, other agents for now): plain exec.
    let interactive = !args
        .iter()
        .any(|a| a == "-p" || a == "--print" || a == "--prompt");
    let detector: Option<crate::pty::PromptDetector> = match agent {
        AgentKind::Antigravity => Some(crate::pty::detect_agy_prompt),
        _ => None,
    };
    let wrappable = matches!(agent, AgentKind::Claude | AgentKind::Antigravity);
    if wrappable && interactive && crate::pty::can_wrap() {
        let launch_id = format!("{:016x}", rand_u64());
        let mut all: Vec<OsString> = spec.extra_args.iter().map(OsString::from).collect();
        all.extend(args);
        let code = crate::pty::run(&bin, &all, &spec.env, &launch_id, detector)?;
        std::process::exit(code);
    }
    let mut cmd = std::process::Command::new(&bin);
    cmd.args(&spec.extra_args).args(args);
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    // Only returns on failure.
    let err = cmd.exec();
    Err(err).with_context(|| format!("exec {}", bin.display()))
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
}
