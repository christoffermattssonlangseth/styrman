//! The models (and reasoning efforts) the phone can switch a session to, per agent:
//! - Claude: its aliases (the latest of each family).
//! - Codex: the models it lists itself, from its model cache.
//! - agy: `agy models` (the effort is part of each model's name).

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use styr_core::ids::AgentKind;

pub use styr_core::phone::ModelChoice;

const CLAUDE_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

fn claude() -> Vec<ModelChoice> {
    [
        ("opus", "Opus"),
        ("sonnet", "Sonnet"),
        ("haiku", "Haiku"),
        ("fable", "Fable"),
    ]
    .into_iter()
    .map(|(id, label)| ModelChoice {
        id: id.into(),
        label: label.into(),
        efforts: CLAUDE_EFFORTS.iter().map(|e| e.to_string()).collect(),
    })
    .collect()
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Codex's own cache of the models it offers (`visibility: "list"`).
fn codex() -> Vec<ModelChoice> {
    let dir = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".codex")));
    let Some(text) = dir.and_then(|d| std::fs::read_to_string(d.join("models_cache.json")).ok())
    else {
        return vec![];
    };
    codex_from(&text)
}

fn codex_from(text: &str) -> Vec<ModelChoice> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return vec![];
    };
    v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["visibility"] == "list")
        .filter_map(|m| {
            let id = m["slug"].as_str()?.to_owned();
            Some(ModelChoice {
                label: m["display_name"].as_str().unwrap_or(&id).to_owned(),
                efforts: m["supported_reasoning_levels"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|l| l["effort"].as_str().map(Into::into))
                    .collect(),
                id,
            })
        })
        .collect()
}

/// `agy models` prints `id<TAB>label` per line (after a "Fetching…" line).
fn agy_from(out: &str) -> Vec<ModelChoice> {
    out.lines()
        .filter_map(|l| {
            let (id, label) = l.split_once('\t')?;
            let id = id.trim();
            (!id.is_empty() && !id.contains(' ')).then(|| ModelChoice {
                id: id.into(),
                label: label.trim().into(),
                efforts: vec![],
            })
        })
        .collect()
}

fn agy_bin() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(h) = home() {
        dirs.push(h.join(".local/bin"));
    }
    dirs.extend(["/opt/homebrew/bin".into(), "/usr/local/bin".into()]);
    dirs.into_iter()
        .map(|d| d.join("agy"))
        .find(|p| p.is_file())
}

/// `agy models` asks the network: keep the answer a while.
static AGY: Mutex<Option<(Instant, Vec<ModelChoice>)>> = Mutex::new(None);

fn agy() -> Vec<ModelChoice> {
    if let Some((at, list)) = AGY.lock().unwrap_or_else(|p| p.into_inner()).as_ref()
        && at.elapsed() < Duration::from_secs(600)
        && !list.is_empty()
    {
        return list.clone();
    }
    let Some(bin) = agy_bin() else { return vec![] };
    let Ok(mut child) = std::process::Command::new(bin)
        .arg("models")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return vec![];
    };
    let started = Instant::now();
    while child.try_wait().ok().flatten().is_none() {
        if started.elapsed() > Duration::from_secs(15) {
            let _ = child.kill();
            return vec![];
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let list = child
        .wait_with_output()
        .map(|o| agy_from(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default();
    *AGY.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), list.clone()));
    list
}

/// What `agent` can be switched to (blocking: agy may ask the network).
pub fn choices(agent: AgentKind) -> Vec<ModelChoice> {
    match agent {
        AgentKind::Claude => claude(),
        AgentKind::Codex => codex(),
        AgentKind::Antigravity => agy(),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_lists_only_listed_models_with_their_efforts() {
        let cache = r#"{"models":[
            {"slug":"gpt-6.2-sol","display_name":"GPT-6.2-Sol","visibility":"list",
             "supported_reasoning_levels":[{"effort":"low"},{"effort":"high"}]},
            {"slug":"gpt-reserve","display_name":"GPT-Reserve","visibility":"hide"}]}"#;
        assert_eq!(
            codex_from(cache),
            [ModelChoice {
                id: "gpt-6.2-sol".into(),
                label: "GPT-6.2-Sol".into(),
                efforts: vec!["low".into(), "high".into()],
            }]
        );
        assert!(codex_from("not json").is_empty());
    }

    #[test]
    fn agy_models_output() {
        let out = "Fetching available models...\ngemini-4-neon-high\tGemini 4 Neon (High)\ngemini-4-neon-low\tGemini 4 Neon (Low)\n";
        let m = agy_from(out);
        assert_eq!(
            m.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["gemini-4-neon-high", "gemini-4-neon-low"]
        );
        assert_eq!(m[0].label, "Gemini 4 Neon (High)");
    }
}
