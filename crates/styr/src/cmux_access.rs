//! Letting Styrman open cmux tabs (for sessions started from the phone).
//!
//! cmux's default socket mode, `cmuxOnly`, lets only processes started inside cmux
//! control it; styrd runs under launchd. With the user's consent, `styr setup` switches
//! cmux to password mode with a random password, stored for Styrman in
//! `~/.styrman/cmux-password` (0600). cmux's config is JSON with comments, so the
//! setting is inserted as text, after a timestamped backup.

use anyhow::{Context, Result, bail};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

pub fn cmux_bin() -> Option<PathBuf> {
    let app = PathBuf::from("/Applications/cmux.app/Contents/Resources/bin/cmux");
    app.exists().then_some(app)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn config_path() -> Option<PathBuf> {
    Some(home()?.join(".config/cmux/cmux.json"))
}

pub fn password_path() -> Option<PathBuf> {
    Some(home()?.join(".styrman/cmux-password"))
}

/// The config text with JSONC comments removed (strings kept intact).
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let (mut in_str, mut esc) = (false, false);
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            match (esc, c) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_str = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Trailing commas are allowed in cmux's JSONC.
fn parse_jsonc(text: &str) -> Option<serde_json::Value> {
    let s = strip_comments(text);
    let s = regex_lite_trailing_commas(&s);
    serde_json::from_str(&s).ok()
}

fn regex_lite_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut in_str = false;
    let mut esc = false;
    for (i, &c) in chars.iter().enumerate() {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            out.push(c);
            continue;
        }
        if c == '"' {
            in_str = true;
        }
        if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                continue;
            }
        }
        out.push(c);
    }
    out
}

#[derive(Debug, PartialEq)]
pub enum State {
    /// cmux isn't installed.
    NoCmux,
    /// Password mode and Styrman has the password.
    Ready,
    /// cmux only lets its own processes in (the default).
    Locked,
    /// Some other mode or a config we can't read; leave it to the user.
    Other(String),
}

pub fn state() -> State {
    if cmux_bin().is_none() {
        return State::NoCmux;
    }
    let Some(cfg) = config_path() else {
        return State::Locked;
    };
    let text = std::fs::read_to_string(&cfg).unwrap_or_default();
    let Some(doc) = parse_jsonc(if text.trim().is_empty() { "{}" } else { &text }) else {
        return State::Other(format!("{} couldn't be read", cfg.display()));
    };
    let auto = &doc["automation"];
    match auto["socketControlMode"].as_str().unwrap_or("cmuxOnly") {
        "password" => {
            let theirs = auto["socketPassword"].as_str().unwrap_or("");
            let ours = password_path()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_default();
            if !theirs.is_empty() && theirs == ours.trim() {
                State::Ready
            } else {
                State::Other("cmux uses password mode with a password Styrman doesn't have".into())
            }
        }
        "cmuxOnly" | "automation" => State::Locked,
        "allowAll" | "openAccess" | "fullOpenAccess" | "full" => State::Ready,
        other => State::Other(format!("cmux's socket mode is `{other}`")),
    }
}

/// Switch cmux to password mode with a fresh password Styrman keeps.
pub fn enable() -> Result<()> {
    let cfg = config_path().context("HOME is not set")?;
    let text = std::fs::read_to_string(&cfg).unwrap_or_default();
    let doc = parse_jsonc(if text.trim().is_empty() { "{}" } else { &text })
        .with_context(|| format!("{} isn't valid JSON with comments", cfg.display()))?;
    if doc.get("automation").is_some() {
        bail!(
            "{} already has an \"automation\" section; set \"socketControlMode\": \"password\" and a \"socketPassword\" there, then run `styr cmux-password <password>`",
            cfg.display()
        );
    }
    let mut pw = [0u8; 24];
    rand_bytes(&mut pw)?;
    let password: String = pw.iter().map(|b| format!("{b:02x}")).collect();
    let new_text = with_password_mode(&text, &password)?;
    if cfg.exists() {
        let stamp = chrono_like_stamp();
        let backup = cfg.with_file_name(format!("cmux.json.{stamp}.bak"));
        std::fs::copy(&cfg, &backup).context("backing up cmux.json")?;
        std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o600))?;
    } else {
        std::fs::create_dir_all(cfg.parent().expect("parent"))?;
    }
    let pw_path = password_path().context("HOME is not set")?;
    std::fs::create_dir_all(pw_path.parent().expect("parent"))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&pw_path)?;
    std::io::Write::write_all(&mut f, password.as_bytes())?;
    let tmp = cfg.with_extension("json.styrman-tmp");
    std::fs::write(&tmp, new_text)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, &cfg)?;
    if let Some(bin) = cmux_bin() {
        let _ = std::process::Command::new(bin)
            .env("CMUX_QUIET", "1")
            .env("CMUX_SOCKET_PASSWORD", &password)
            .arg("reload-config")
            .output();
    }
    Ok(())
}

/// `text` (cmux.json) with password-mode socket access added.
pub fn with_password_mode(text: &str, password: &str) -> Result<String> {
    let block = format!(
        "  // Added by Styrman (`styr setup`): lets sessions started from your phone open\n  // cmux tabs. Remove this block to go back to cmux-only socket access.\n  \"automation\": {{\n    \"socketControlMode\": \"password\",\n    \"socketPassword\": \"{password}\"\n  }},\n"
    );
    let new_text = if text.trim().is_empty() {
        format!(
            "{{\n{}}}\n",
            block.trim_end().trim_end_matches(',').to_owned() + "\n"
        )
    } else {
        // Right after the opening brace: valid whatever follows (a key or the end).
        let open = text.find('{').context("no opening brace")?;
        let rest = &text[open + 1..];
        let has_keys = parse_jsonc(text)
            .and_then(|v| v.as_object().map(|o| !o.is_empty()))
            .unwrap_or(false);
        let block = if has_keys {
            block
        } else {
            block.replacen("},\n", "}\n", 1)
        };
        format!(
            "{}\n{block}{}",
            &text[..=open],
            rest.strip_prefix('\n').unwrap_or(rest)
        )
    };
    // Must still parse, and say what we meant, before anything is written.
    let check = parse_jsonc(&new_text).context("the edited cmux.json wouldn't parse")?;
    anyhow::ensure!(
        check["automation"]["socketControlMode"] == "password",
        "the edited cmux.json lost the setting"
    );
    Ok(new_text)
}

/// Store a password the user set in cmux themselves.
pub fn store_password(password: &str) -> Result<()> {
    let p = password_path().context("HOME is not set")?;
    std::fs::create_dir_all(p.parent().expect("parent"))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&p)?;
    std::io::Write::write_all(&mut f, password.trim().as_bytes())?;
    Ok(())
}

fn rand_bytes(buf: &mut [u8]) -> Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(buf)?;
    Ok(())
}

fn chrono_like_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_with_only_comments_parses_as_empty() {
        let t = "{\n  \"$schema\": \"https://x/y.json\",\n  \"schemaVersion\": 1,\n\n  // a comment with \"quotes\" and // more\n  //   \"app\" : {\n  //   },\n}\n";
        let v = parse_jsonc(t).unwrap();
        assert_eq!(v["schemaVersion"], 1);
        assert!(v.get("app").is_none());
        // `//` inside a string is not a comment.
        assert_eq!(v["$schema"], "https://x/y.json");
    }

    #[test]
    fn password_mode_is_added_to_a_real_template() {
        let template = "{\n  \"$schema\": \"https://raw.githubusercontent.com/manaflow-ai/cmux/main/web/data/cmux.schema.json\",\n  \"schemaVersion\": 1,\n\n  // This file uses JSON with comments (JSONC).\n  //   \"app\" : {\n  //     \"accentColor\" : \"cmux\",\n  //   },\n}\n";
        let out = with_password_mode(template, "abc").unwrap();
        let v = parse_jsonc(&out).unwrap();
        assert_eq!(v["automation"]["socketControlMode"], "password");
        assert_eq!(v["automation"]["socketPassword"], "abc");
        assert_eq!(v["schemaVersion"], 1, "everything else kept");
        assert!(out.contains("// This file uses JSON with comments"));
        // Empty file and an empty object work too.
        for t in ["", "{}", "{\n}\n"] {
            let v = parse_jsonc(&with_password_mode(t, "x").unwrap()).unwrap();
            assert_eq!(v["automation"]["socketPassword"], "x", "{t:?}");
        }
    }
}

#[cfg(test)]
mod real_config {
    /// `cargo test -p styr real_cmux_config -- --ignored`: dry run on this Mac's cmux.json.
    #[test]
    #[ignore]
    fn real_cmux_config() {
        let p = super::config_path().unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        let before = super::parse_jsonc(&text).expect("current config parses");
        let out = super::with_password_mode(&text, "dry-run").unwrap();
        let after = super::parse_jsonc(&out).unwrap();
        let mut stripped = after.clone();
        stripped.as_object_mut().unwrap().remove("automation");
        assert_eq!(stripped, before, "only `automation` was added");
        assert_eq!(out.lines().count(), text.lines().count() + 6);
        println!(
            "ok: {} keys kept, 6 lines added",
            before.as_object().unwrap().len()
        );
    }
}
