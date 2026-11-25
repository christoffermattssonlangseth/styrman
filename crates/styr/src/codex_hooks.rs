//! Styrman's Codex hooks: entries in `$CODEX_HOME/hooks.json` that run a small shim
//! (`~/.styrman/codex/styr-hook`), which forwards to `styr hook codex <event>`.
//!
//! Other hooks in the file (the user's own, other tools') are kept as they are; only
//! entries running our shim are replaced. Codex asks the user to trust new or changed
//! hooks once ("Trust all and continue"); we never trust them on the user's behalf.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// The events Styrman listens to. PermissionRequest is left out on purpose: Codex
/// shows its own menu only after that hook returns, and `styr codex` relays the menu.
pub const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
    "SessionEnd",
];

/// Seconds (measured on 0.172.0: Codex hook timeouts are in seconds).
const TIMEOUT_SECS: u64 = 30;

const SHIM: &str = include_str!("../../../plugins/agy/styrman/bin/styr-hook");
/// How to talk to other shared agents (`styr ask`), as a Codex skill.
const PEERS_SKILL: &str = include_str!("../../../plugins/codex/styrman-peers.SKILL.md");

fn peers_skill_path() -> Option<PathBuf> {
    Some(codex_home()?.join("skills/styrman-peers/SKILL.md"))
}
const MARKER: &str = "/.styrman/codex/styr-hook";

pub fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")))
}

fn shim_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".styrman/codex/styr-hook"))
}

fn ours(entry: &Value) -> bool {
    entry["hooks"].as_array().is_some_and(|hs| {
        hs.iter()
            .any(|h| h["command"].as_str().is_some_and(|c| c.contains(MARKER)))
    })
}

/// `hooks.json` with Styrman's entries (re)placed and everything else kept.
pub fn merged(existing: Option<Value>, shim: &str) -> Value {
    let mut doc = existing
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    if !doc["hooks"].is_object() {
        doc["hooks"] = json!({});
    }
    let hooks = doc["hooks"].as_object_mut().expect("object");
    for groups in hooks.values_mut() {
        if let Some(a) = groups.as_array_mut() {
            a.retain(|g| !ours(g));
        }
    }
    hooks.retain(|_, g| g.as_array().is_none_or(|a| !a.is_empty()));
    for ev in EVENTS {
        let entry = json!({"hooks": [{
            "type": "command",
            "command": format!("{shim} codex {ev}"),
            "timeout": TIMEOUT_SECS,
        }]});
        match hooks.get_mut(*ev).and_then(Value::as_array_mut) {
            Some(a) => a.push(entry),
            None => {
                hooks.insert((*ev).to_owned(), json!([entry]));
            }
        }
    }
    doc
}

/// Whether hooks.json already has exactly our entries.
pub fn installed() -> bool {
    let (Some(home), Some(shim)) = (codex_home(), shim_path()) else {
        return false;
    };
    let shim_ok = std::fs::read_to_string(&shim).is_ok_and(|s| s == SHIM);
    let Ok(text) = std::fs::read_to_string(home.join("hooks.json")) else {
        return false;
    };
    let Ok(doc) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let skill_ok = peers_skill_path()
        .is_some_and(|p| std::fs::read_to_string(p).is_ok_and(|s| s == PEERS_SKILL));
    shim_ok && skill_ok && merged(Some(doc.clone()), &shim.to_string_lossy()) == doc
}

/// Whether Codex has recorded trust for our hooks (any trust state for hooks.json
/// entries of each of our events). A changed hook needs trusting again, which
/// Codex asks about itself.
pub fn trusted() -> bool {
    let Some(home) = codex_home() else {
        return false;
    };
    let Ok(cfg) = std::fs::read_to_string(home.join("config.toml")) else {
        return false;
    };
    let file = home.join("hooks.json");
    let file = file.to_string_lossy();
    EVENTS
        .iter()
        .all(|ev| cfg.contains(&format!("{file}:{}:", snake(ev))))
}

fn snake(ev: &str) -> String {
    let mut out = String::new();
    for (i, c) in ev.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Install the shim and our hooks.json entries. Returns whether anything changed.
pub fn install() -> Result<bool> {
    if installed() {
        return Ok(false);
    }
    let home = codex_home().context("HOME is not set")?;
    let shim = shim_path().context("HOME is not set")?;
    std::fs::create_dir_all(shim.parent().expect("parent"))?;
    std::fs::write(&shim, SHIM)?;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))?;
    std::fs::create_dir_all(&home)?;
    let path = home.join("hooks.json");
    let existing = match std::fs::read_to_string(&path) {
        Ok(t) => Some(serde_json::from_str(&t).with_context(|| {
            format!(
                "{} isn't valid JSON; fix or remove it first",
                path.display()
            )
        })?),
        Err(_) => None,
    };
    let doc = merged(existing, &shim.to_string_lossy());
    let tmp = path.with_extension("json.styrman-tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&doc)? + "\n")?;
    std::fs::rename(&tmp, &path)?;
    if let Some(skill) = peers_skill_path() {
        std::fs::create_dir_all(skill.parent().expect("parent"))?;
        std::fs::write(&skill, PEERS_SKILL)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIM_AT: &str = "/Users/dev/.styrman/codex/styr-hook";

    #[test]
    fn other_hooks_are_kept_and_ours_replaced() {
        let existing = json!({"hooks": {
            "Stop": [{"hooks": [{"type": "command", "command": "notify-me"}]}],
            "PreToolUse": [{"hooks": [{"type": "command", "command": "/old/.styrman/codex/styr-hook codex PreToolUse"}]}],
        }, "other": 1});
        let doc = merged(Some(existing), SHIM_AT);
        assert_eq!(doc["other"], 1);
        let stop = doc["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2, "user's hook kept, ours added");
        assert_eq!(stop[0]["hooks"][0]["command"], "notify-me");
        let pre = doc["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1, "the stale entry of ours was replaced");
        assert_eq!(
            pre[0]["hooks"][0]["command"],
            format!("{SHIM_AT} codex PreToolUse")
        );
        assert_eq!(pre[0]["hooks"][0]["timeout"], 30);
        assert!(doc["hooks"].get("PermissionRequest").is_none());
        // Idempotent.
        assert_eq!(merged(Some(doc.clone()), SHIM_AT), doc);
    }

    #[test]
    fn event_names_match_codex_trust_keys() {
        assert_eq!(snake("UserPromptSubmit"), "user_prompt_submit");
        assert_eq!(snake("SessionStart"), "session_start");
    }
}
