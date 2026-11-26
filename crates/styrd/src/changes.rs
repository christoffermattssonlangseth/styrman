//! A session folder's uncommitted changes, for the phone's Changes view: `git diff
//! HEAD` (staged and unstaged) plus new, untracked files as additions. Read-only:
//! nothing here changes the repository (no `git add -N`, no index writes).

use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Most diff text sent to the phone; past this it's cut (and says so).
const MAX_DIFF: usize = 400 * 1024;
/// New files shown in full only up to this size and count.
const MAX_NEW_FILE: u64 = 64 * 1024;
const MAX_NEW_FILES: usize = 40;

#[derive(Debug, Serialize, PartialEq)]
pub struct Changes {
    /// The checked-out branch, if any.
    pub branch: Option<String>,
    /// A unified diff (`diff --git` per file), new files included as additions.
    pub diff: String,
    /// The diff was too long and was cut.
    pub truncated: bool,
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.quotepath=off", "--no-pager"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0") // read-only: don't even refresh the index
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("couldn't run git")?;
    // A huge repository must not hold up the phone forever.
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if started.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            bail!("git took too long");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The uncommitted changes in `dir` (a git working tree).
pub fn read(dir: &Path) -> Result<Changes> {
    git(dir, &["rev-parse", "--is-inside-work-tree"])
        .map_err(|_| anyhow::anyhow!("This folder isn't a git repository."))?;
    let branch = git(dir, &["branch", "--show-current"])
        .ok()
        .map(|b| b.trim().to_owned())
        .filter(|b| !b.is_empty());
    // Against HEAD: staged and unstaged together. A repo without commits yet diffs
    // against the empty tree.
    let base = if git(dir, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok() {
        "HEAD"
    } else {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    };
    let mut diff = git(dir, &["diff", "--no-color", "--no-ext-diff", "-U3", base])?;
    let untracked = git(dir, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    for path in untracked
        .split('\0')
        .filter(|p| !p.is_empty())
        .take(MAX_NEW_FILES)
    {
        diff.push_str(&new_file_diff(dir, path));
    }
    let truncated = diff.len() > MAX_DIFF;
    if truncated {
        let mut cut = MAX_DIFF;
        while !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        diff.truncate(cut);
        // End on a whole line.
        if let Some(nl) = diff.rfind('\n') {
            diff.truncate(nl + 1);
        }
    }
    Ok(Changes {
        branch,
        diff,
        truncated,
    })
}

/// An untracked file as a diff that adds it (text files up to `MAX_NEW_FILE`).
fn new_file_diff(dir: &Path, path: &str) -> String {
    let mut out = format!(
        "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n"
    );
    let full = dir.join(path);
    let meta = std::fs::symlink_metadata(&full).ok();
    let Some(meta) = meta.filter(|m| m.is_file()) else {
        return out;
    };
    if meta.len() > MAX_NEW_FILE {
        out.push_str(&format!(
            "@@ new file, {} KB: not shown @@\n",
            meta.len() / 1024
        ));
        return out;
    }
    let Ok(bytes) = std::fs::read(&full) else {
        return out;
    };
    let Ok(text) = String::from_utf8(bytes) else {
        out.push_str("@@ binary file: not shown @@\n");
        return out;
    };
    let lines: Vec<&str> = text.lines().collect();
    out.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
    for l in lines {
        out.push('+');
        out.push_str(l);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn edits_and_new_files_without_touching_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        run(d, &["init", "-q", "-b", "main"]);
        std::fs::write(d.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        run(d, &["add", "a.txt"]);
        run(d, &["commit", "-q", "-m", "first"]);
        std::fs::write(d.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(d.join("new.rs"), "fn main() {}\n").unwrap();
        let index_before = std::fs::read(d.join(".git/index")).unwrap();

        let c = read(d).unwrap();
        assert_eq!(c.branch.as_deref(), Some("main"));
        assert!(c.diff.contains("diff --git a/a.txt b/a.txt"));
        assert!(c.diff.contains("-two\n+TWO\n"));
        assert!(
            c.diff
                .contains("+++ b/new.rs\n@@ -0,0 +1,1 @@\n+fn main() {}\n")
        );
        assert!(!c.truncated);
        assert_eq!(
            std::fs::read(d.join(".git/index")).unwrap(),
            index_before,
            "read-only"
        );
    }

    #[test]
    fn a_folder_without_git_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let e = read(dir.path()).unwrap_err().to_string();
        assert!(e.contains("isn't a git repository"), "{e}");
    }
}
