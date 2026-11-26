//! A session folder's uncommitted changes, for the phone's Changes view: `git diff
//! HEAD` (staged and unstaged) plus new, untracked files as additions. Reading is
//! read-only (no `git add -N`, no index writes); only `commit`, which the phone asks
//! for explicitly, changes the repository.

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
    /// Commits not yet pushed to the branch's upstream (`None`: no upstream).
    pub ahead: Option<u32>,
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    git_for(dir, args, Duration::from_secs(10))
}

fn git_for(dir: &Path, args: &[&str], limit: Duration) -> Result<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.quotepath=off", "--no-pager"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0") // reading: don't even refresh the index
        .env("GIT_TERMINAL_PROMPT", "0") // no one is there to type a password
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
        if started.elapsed() > limit {
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
        ahead: ahead(dir),
    })
}

fn ahead(dir: &Path) -> Option<u32> {
    git(dir, &["rev-list", "--count", "@{upstream}..HEAD"])
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// What `commit` did.
#[derive(Debug, PartialEq)]
pub struct Committed {
    /// "abc1234 Subject" of the new commit, if one was made.
    pub commit: Option<String>,
    pub pushed: bool,
}

/// Commit everything in `dir` (as `git add -A`, so .gitignore holds) with `message`,
/// if it's not empty; then push if asked, setting the upstream on a first push.
/// The repository's own identity, hooks and credentials apply.
pub fn commit(dir: &Path, message: &str, push: bool) -> Result<Committed> {
    git(dir, &["rev-parse", "--is-inside-work-tree"])
        .map_err(|_| anyhow::anyhow!("This folder isn't a git repository."))?;
    let message = message.trim();
    let mut commit = None;
    if !message.is_empty() {
        git(dir, &["add", "-A"])?;
        // Hooks (pre-commit) may take a while.
        git_for(
            dir,
            &["commit", "-q", "-m", message],
            Duration::from_secs(120),
        )
        .map_err(|e| anyhow::anyhow!("Commit failed: {e}"))?;
        commit = Some(
            git(dir, &["log", "-1", "--format=%h %s"])?
                .trim()
                .to_owned(),
        );
    }
    let pushed = if push {
        let args: &[&str] = if git(dir, &["rev-parse", "--abbrev-ref", "@{upstream}"]).is_ok() {
            &["push", "-q"]
        } else {
            &["push", "-q", "-u", "origin", "HEAD"]
        };
        git_for(dir, args, Duration::from_secs(120)).map_err(|e| match &commit {
            Some(c) => anyhow::anyhow!("Committed {c}, but the push failed: {e}"),
            None => anyhow::anyhow!("Push failed: {e}"),
        })?;
        true
    } else {
        false
    };
    anyhow::ensure!(
        commit.is_some() || pushed,
        "Nothing to do: give a message to commit."
    );
    Ok(Committed { commit, pushed })
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
    fn commit_then_push_to_a_new_upstream() {
        let remote = tempfile::tempdir().unwrap();
        run(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        run(d, &["init", "-q", "-b", "main"]);
        for (k, v) in [("user.name", "t"), ("user.email", "t@example.com")] {
            run(d, &["config", k, v]);
        }
        run(
            d,
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        std::fs::write(d.join("a.txt"), "one\n").unwrap();
        std::fs::write(d.join(".gitignore"), "secret\n").unwrap();
        std::fs::write(d.join("secret"), "x\n").unwrap();

        assert!(commit(d, "  ", false).is_err(), "nothing to do");
        let c = commit(d, "First things", false).unwrap();
        assert!(c.commit.unwrap().ends_with(" First things"));
        assert!(!c.pushed);
        let tracked = git(d, &["ls-files"]).unwrap();
        assert!(
            tracked.contains("a.txt") && !tracked.contains("secret"),
            "{tracked}"
        );
        assert_eq!(read(d).unwrap().ahead, None, "no upstream yet");

        let p = commit(d, "", true).unwrap();
        assert_eq!(
            p,
            Committed {
                commit: None,
                pushed: true
            }
        );
        assert_eq!(read(d).unwrap().ahead, Some(0));
        std::fs::write(d.join("a.txt"), "two\n").unwrap();
        commit(d, "Second", false).unwrap();
        assert_eq!(read(d).unwrap().ahead, Some(1));
        assert!(commit(d, "", true).unwrap().pushed);
        assert_eq!(read(d).unwrap().ahead, Some(0));
    }

    #[test]
    fn a_folder_without_git_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let e = read(dir.path()).unwrap_err().to_string();
        assert!(e.contains("isn't a git repository"), "{e}");
    }
}
