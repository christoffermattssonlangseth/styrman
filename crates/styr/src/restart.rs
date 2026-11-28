//! Restarting the agent with another model or effort, from the phone: the same
//! conversation, resumed in the same terminal. The agent's own `/model` would also
//! save the choice as the user's default everywhere; flags only touch this session.
//!
//! The new command line is the old one with:
//! - the first prompt dropped (it was sent already: resuming must not send it again),
//! - earlier resume, model and effort choices replaced,
//! - everything else kept (cmux's hook options for Codex, `--add-dir`, …).

use std::ffi::OsString;
use styr_core::ids::AgentKind;

/// What to restart with (from the daemon's `Response::Restart`).
#[derive(Debug, Clone, PartialEq)]
pub struct Restart {
    /// The conversation to resume (the agent's own session id).
    pub resume: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Claude's permission mode to keep ("acceptEdits", "plan", …).
    pub mode: Option<String>,
}

/// How an agent's options take values.
enum Takes {
    /// No value (`--continue`).
    Nothing,
    /// One value (`--model opus`).
    One,
    /// One value if the next argument isn't an option (`--resume [id]`).
    Maybe,
    /// Values until the next option (`--add-dir a b`).
    Many,
}

fn claude_flag(f: &str) -> Takes {
    match f {
        // Claude's `--resume` picks from a list when no id follows.
        "--resume" | "-r" => Takes::Maybe,
        "--model"
        | "--effort"
        | "--session-id"
        | "--permission-mode"
        | "--settings"
        | "--append-system-prompt"
        | "--system-prompt"
        | "--fallback-model"
        | "--agent"
        | "--agents"
        | "--output-format"
        | "--input-format"
        | "--setting-sources"
        | "--max-turns"
        | "--name"
        | "--json-schema"
        | "--max-budget-usd" => Takes::One,
        "--add-dir" | "--allowedTools" | "--allowed-tools" | "--disallowedTools"
        | "--disallowed-tools" | "--mcp-config" | "--plugin-dir" | "--tools" | "--betas"
        | "--file" => Takes::Many,
        _ => Takes::Nothing,
    }
}

fn codex_flag(f: &str) -> Takes {
    match f {
        "-c"
        | "--config"
        | "--enable"
        | "--disable"
        | "-m"
        | "--model"
        | "-p"
        | "--profile"
        | "-s"
        | "--sandbox"
        | "-a"
        | "--ask-for-approval"
        | "-C"
        | "--cd"
        | "--add-dir"
        | "--local-provider"
        | "--remote"
        | "--remote-auth-token-env" => Takes::One,
        "-i" | "--image" => Takes::Many,
        _ => Takes::Nothing,
    }
}

fn agy_flag(f: &str) -> Takes {
    match f.trim_start_matches('-') {
        "add-dir" | "agent" | "conversation" | "r" | "resume" | "effort" | "log-file" | "mode"
        | "model" | "project" | "settings" | "json-schema" | "input-format" | "output-format"
        | "print-timeout" | "i" | "prompt-interactive" => Takes::One,
        _ => Takes::Nothing,
    }
}

/// An option split into its name and, for `--name=value`, the value.
fn split(arg: &str) -> (&str, bool) {
    match arg.split_once('=') {
        Some((name, _)) if arg.starts_with('-') => (name, true),
        _ => (arg, false),
    }
}

/// The options to keep, as (name, values…) groups; positional arguments dropped.
fn options(args: &[OsString], takes: fn(&str) -> Takes) -> Vec<(String, Vec<OsString>)> {
    let mut out: Vec<(String, Vec<OsString>)> = vec![];
    let mut i = 0;
    while i < args.len() {
        let a = args[i].to_string_lossy().into_owned();
        i += 1;
        if a == "--" {
            break; // the rest is the prompt
        }
        if !a.starts_with('-') || a == "-" {
            continue; // a prompt, or Codex's subcommand and its arguments
        }
        let (name, inline) = split(&a);
        let mut group = vec![args[i - 1].clone()];
        if !inline {
            match takes(name) {
                Takes::Nothing => {}
                Takes::One => {
                    if i < args.len() {
                        group.push(args[i].clone());
                        i += 1;
                    }
                }
                Takes::Maybe => {
                    if i < args.len() && !args[i].to_string_lossy().starts_with('-') {
                        group.push(args[i].clone());
                        i += 1;
                    }
                }
                Takes::Many => {
                    while i < args.len() && !args[i].to_string_lossy().starts_with('-') {
                        group.push(args[i].clone());
                        i += 1;
                    }
                }
            }
        }
        out.push((name.to_owned(), group));
    }
    out
}

/// The command line to restart `agent` with, from the one it was started with.
pub fn args(agent: AgentKind, old: &[OsString], r: &Restart) -> Vec<OsString> {
    let takes = match agent {
        AgentKind::Codex => codex_flag,
        AgentKind::Antigravity => agy_flag,
        _ => claude_flag,
    };
    let opts = options(old, takes);
    let mut out: Vec<OsString> = vec![];
    let keep = |drop: &[&str], out: &mut Vec<OsString>| {
        for (name, group) in &opts {
            if !drop.contains(&name.as_str()) {
                out.extend(group.iter().cloned());
            }
        }
    };
    match agent {
        AgentKind::Codex => {
            out.push("resume".into());
            out.push(r.resume.clone().into());
            let mut drop = vec!["--last", "--all"];
            if r.model.is_some() {
                drop.extend(["-m", "--model"]);
            }
            // Effort is a config value: drop only that one.
            let effort_cfg = |g: &[OsString]| {
                g.iter().any(|v| {
                    v.to_string_lossy()
                        .trim_start()
                        .starts_with("model_reasoning_effort")
                })
            };
            for (name, group) in &opts {
                let is_effort = matches!(name.as_str(), "-c" | "--config") && effort_cfg(group);
                if drop.contains(&name.as_str()) || (is_effort && r.effort.is_some()) {
                    continue;
                }
                out.extend(group.iter().cloned());
            }
            if let Some(m) = &r.model {
                out.extend(["-m".into(), m.into()]);
            }
            if let Some(e) = &r.effort {
                out.extend([
                    "-c".into(),
                    format!("model_reasoning_effort=\"{e}\"").into(),
                ]);
            }
        }
        AgentKind::Antigravity => {
            let mut drop = vec![
                "--conversation",
                "-conversation",
                "-r",
                "--resume",
                "-resume",
                "-c",
                "--continue",
                "-continue",
                "-i",
                "--prompt-interactive",
                "-prompt-interactive",
            ];
            if r.model.is_some() {
                drop.extend(["--model", "-model"]);
            }
            if r.effort.is_some() {
                drop.extend(["--effort", "-effort"]);
            }
            keep(&drop, &mut out);
            out.push(format!("--conversation={}", r.resume).into());
            if let Some(m) = &r.model {
                out.push(format!("--model={m}").into());
            }
            if let Some(e) = &r.effort {
                out.push(format!("--effort={e}").into());
            }
        }
        _ => {
            let mut drop = vec![
                "--resume",
                "-r",
                "--continue",
                "-c",
                "--session-id",
                "--fork-session",
            ];
            if r.model.is_some() {
                drop.push("--model");
            }
            if r.effort.is_some() {
                drop.push("--effort");
            }
            if r.mode.is_some() {
                drop.extend(["--permission-mode", "--dangerously-skip-permissions"]);
            }
            keep(&drop, &mut out);
            out.extend(["--resume".into(), r.resume.clone().into()]);
            if let Some(m) = &r.model {
                out.extend(["--model".into(), m.into()]);
            }
            if let Some(e) = &r.effort {
                out.extend(["--effort".into(), e.into()]);
            }
            if let Some(mode) = &r.mode {
                out.extend(["--permission-mode".into(), mode.into()]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<OsString> {
        s.iter().map(OsString::from).collect()
    }
    fn strs(a: &[OsString]) -> Vec<String> {
        a.iter().map(|s| s.to_string_lossy().into_owned()).collect()
    }
    fn to(model: Option<&str>, effort: Option<&str>) -> Restart {
        Restart {
            resume: "ID".into(),
            model: model.map(Into::into),
            effort: effort.map(Into::into),
            mode: None,
        }
    }

    #[test]
    fn claude_drops_the_first_prompt_and_old_choices() {
        let old = v(&[
            "--model",
            "opus",
            "--add-dir",
            "../lib",
            "../docs",
            "--verbose",
            "fix the parser",
        ]);
        let r = Restart {
            mode: Some("acceptEdits".into()),
            ..to(Some("haiku"), Some("high"))
        };
        assert_eq!(
            strs(&args(AgentKind::Claude, &old, &r)),
            [
                "--add-dir",
                "../lib",
                "../docs",
                "--verbose",
                "--resume",
                "ID",
                "--model",
                "haiku",
                "--effort",
                "high",
                "--permission-mode",
                "acceptEdits"
            ]
        );
        // Resumed before, with `=` forms: replaced, not doubled.
        let old = v(&["--resume=OLD", "--model=opus", "-c"]);
        assert_eq!(
            strs(&args(AgentKind::Claude, &old, &to(None, Some("low")))),
            ["--model=opus", "--resume", "ID", "--effort", "low"]
        );
        // `--resume` without an id (Claude then let you pick): the next option
        // is kept, not taken as its value.
        for flag in ["--resume", "-r"] {
            let old = v(&[flag, "--model", "opus", "--verbose"]);
            assert_eq!(
                strs(&args(AgentKind::Claude, &old, &to(None, None))),
                ["--model", "opus", "--verbose", "--resume", "ID"],
                "{flag}"
            );
        }
        let old = v(&["-r", "OLD", "--verbose"]);
        assert_eq!(
            strs(&args(AgentKind::Claude, &old, &to(None, None))),
            ["--verbose", "--resume", "ID"]
        );
    }

    #[test]
    fn codex_keeps_cmux_hook_options_and_resumes() {
        let old = v(&[
            "--disable",
            "computer_use",
            "--enable",
            "hooks",
            "-c",
            "hooks.Stop=[{hooks=[]}]",
            "-c",
            "model_reasoning_effort=\"low\"",
            "-m",
            "gpt-6-sol",
            "write the tests",
        ]);
        assert_eq!(
            strs(&args(
                AgentKind::Codex,
                &old,
                &to(Some("gpt-6.2-sol"), Some("high"))
            )),
            [
                "resume",
                "ID",
                "--disable",
                "computer_use",
                "--enable",
                "hooks",
                "-c",
                "hooks.Stop=[{hooks=[]}]",
                "-m",
                "gpt-6.2-sol",
                "-c",
                "model_reasoning_effort=\"high\""
            ]
        );
        // Already a resumed session: its old subcommand and id go.
        let old = v(&["resume", "OLD", "--enable", "hooks"]);
        assert_eq!(
            strs(&args(AgentKind::Codex, &old, &to(Some("m"), None))),
            ["resume", "ID", "--enable", "hooks", "-m", "m"]
        );
    }

    #[test]
    fn agy_uses_conversation_and_drops_the_interactive_prompt() {
        let old = v(&[
            "-i",
            "plan the release",
            "--model=gemini-4-neon-low",
            "--sandbox",
        ]);
        assert_eq!(
            strs(&args(
                AgentKind::Antigravity,
                &old,
                &to(Some("gemini-4-neon-high"), None)
            )),
            [
                "--sandbox",
                "--conversation=ID",
                "--model=gemini-4-neon-high"
            ]
        );
    }
}
