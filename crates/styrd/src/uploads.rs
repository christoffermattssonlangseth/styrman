//! Images and files sent from the phone with a message.
//!
//! `POST /v1/upload/{session}?name=<file name>` (device token, raw bytes) saves the
//! file under Styrman's data folder, `uploads/<session>/`, never in your repository,
//! and returns its path; the phone then mentions the path in the message so the
//! agent can open it. Shared sessions only; uploads older than a week are removed.

use crate::daemon::Daemon;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use styr_core::ids::SessionId;

/// Largest file accepted (photos and documents; agents can't use much more anyway).
pub const MAX_BYTES: usize = 25 * 1024 * 1024;
const KEEP: Duration = Duration::from_secs(7 * 24 * 3600);

/// A file name that's safe to use: no directories, no hidden files, plain characters.
pub fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_owned();
    let cleaned: String = cleaned
        .chars()
        .rev()
        .take(80)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if cleaned.is_empty() {
        "file".into()
    } else {
        cleaned
    }
}

impl Daemon {
    /// Save an upload for shared session `session`; returns its path.
    pub fn save_upload(&self, session: &SessionId, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        if !self.summary(session)?.is_some_and(|s| s.shared) {
            bail!("unknown session {session}");
        }
        if bytes.is_empty() {
            bail!("the file is empty");
        }
        let dir = self
            .data_dir_path()
            .context("no data folder")?
            .join("uploads")
            .join(safe_name(session.as_str()));
        std::fs::create_dir_all(&dir)?;
        let stamp = chrono_stamp();
        let path = dir.join(format!("{stamp}-{}", safe_name(name)));
        std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }
}

/// "20261125-223812" in local-ish time (UTC is fine: it's only for ordering).
fn chrono_stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86400, secs % 86400);
    // Civil date from days since 1970 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Remove uploads older than a week (run at startup).
pub fn prune(data_dir: &Path) {
    let root = data_dir.join("uploads");
    let Ok(sessions) = std::fs::read_dir(&root) else {
        return;
    };
    for s in sessions.flatten() {
        let Ok(files) = std::fs::read_dir(s.path()) else {
            continue;
        };
        for f in files.flatten() {
            let old = f
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > KEEP);
            if old {
                let _ = std::fs::remove_file(f.path());
            }
        }
        let _ = std::fs::remove_dir(s.path()); // only if now empty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_made_safe() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name(".ssh"), "ssh");
        assert_eq!(safe_name("Skärmbild 2026.png"), "Skärmbild 2026.png");
        assert_eq!(safe_name("a;rm -rf $HOME.txt"), "a_rm -rf _HOME.txt");
        assert_eq!(safe_name(""), "file");
        assert_eq!(chrono_stamp().len(), 15);
    }
}
