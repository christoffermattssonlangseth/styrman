//! The terminal wrapper behind `styr claude`: runs the agent in a pseudo-terminal
//! that we control, passes every keystroke and byte of output through untouched,
//! and types phone prompts into the agent's prompt box when the daemon hands us one.
//!
//! The agent sees a normal terminal (same size, kept in sync); the user sees the
//! agent exactly as without Styrman. If styrd is not running, prompts simply never
//! arrive. A phone prompt is held while the user is halfway through typing a line.

use anyhow::{Context, Result, bail};
use std::ffi::{CString, OsString};
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use styr_core::ipc::{Client, IpcError, Request, Response, TerminalPrompt};
use styr_core::paths::Paths;

/// Tracks whether the user has a partly typed line (so we don't type over it).
#[derive(Debug, Default, Clone)]
pub struct LineState {
    dirty: bool,
    in_escape: bool,
}

impl LineState {
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.in_escape {
                // CSI/SS3 sequences end with a byte in 0x40..=0x7e (after the opener).
                if (0x40..=0x7e).contains(&b) && b != b'[' && b != b'O' {
                    self.in_escape = false;
                }
                continue;
            }
            match b {
                0x1b => self.in_escape = true,
                // Enter submits; Ctrl-C / Ctrl-U clear the line.
                b'\r' | b'\n' | 0x03 | 0x15 => self.dirty = false,
                0x20..=0x7e | 0x80..=0xff => self.dirty = true,
                _ => {}
            }
        }
    }
}

/// How a phone prompt is typed: bracketed paste for multi-line text (so newlines
/// don't submit early), then Enter.
pub fn keystrokes(text: &str) -> (Vec<u8>, Vec<u8>) {
    let body = if text.contains('\n') {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.as_bytes().to_vec()
    };
    (body, b"\r".to_vec())
}

fn is_tty(fd: RawFd) -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

fn winsize(fd: RawFd) -> Option<libc::winsize> {
    // SAFETY: TIOCGWINSZ fills a winsize we own.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    (unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0).then_some(ws)
}

struct RawMode(libc::termios);

impl RawMode {
    fn enter() -> Result<Self> {
        // SAFETY: plain termios calls on stdin with structs we own.
        unsafe {
            let mut orig: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut orig) != 0 {
                bail!("tcgetattr failed");
            }
            let mut raw = orig;
            libc::cfmakeraw(&mut raw);
            libc::tcsetattr(0, libc::TCSANOW, &raw);
            Ok(Self(orig))
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: restoring the attributes saved in `enter`.
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.0);
        }
    }
}

/// Whether `styr <agent>` should wrap the agent (interactive terminal on both ends).
pub fn can_wrap() -> bool {
    is_tty(0) && is_tty(1) && std::env::var_os("STYR_NO_PTY").is_none()
}

/// Run `bin args…` in a pseudo-terminal and return its exit code.
/// Reads an agent's own permission menu off the screen (agent-specific).
pub type PromptDetector = fn(&str) -> Option<TerminalPrompt>;

/// agy's permission menu:
///
/// ```text
/// Requesting permission for:
///    touch agy-probe2.txt
/// Run this command?
/// > 1. Yes, run command
///   2. Yes, and always allow in this conversation for …
///   4. No, cancel
/// ```
pub fn detect_agy_prompt(screen: &str) -> Option<TerminalPrompt> {
    let lines: Vec<&str> = screen.lines().collect();
    let start = lines
        .iter()
        .rposition(|l| l.contains("Requesting permission for:"))?;
    let mut detail = Vec::new();
    let mut title = None;
    let mut i = start + 1;
    while i < lines.len() {
        let t = lines[i].trim();
        i += 1;
        if t.ends_with('?') {
            title = Some(t.to_owned());
            break;
        }
        if !t.is_empty() {
            detail.push(t.to_owned());
        }
    }
    let title = title?;
    let mut options = Vec::new();
    let mut selected = 0;
    for l in &lines[i..] {
        let t = l.trim_start();
        let (marked, rest) = match t.strip_prefix('>') {
            Some(r) => (true, r.trim_start()),
            None => (false, t),
        };
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let label = (!digits.is_empty())
            .then(|| rest[digits.len()..].strip_prefix(". "))
            .flatten();
        match label {
            Some(label) => {
                if marked {
                    selected = options.len();
                }
                options.push(label.trim().to_owned());
            }
            None if options.is_empty() && t.is_empty() => continue,
            None => break,
        }
    }
    (options.len() >= 2).then(|| TerminalPrompt {
        title,
        detail: detail.join("\n"),
        options,
        selected,
    })
}

pub fn run(
    bin: &Path,
    args: &[OsString],
    env: &[(String, String)],
    launch_id: &str,
    detector: Option<PromptDetector>,
) -> Result<i32> {
    // Environment for the child (and, through it, the agent's hooks).
    for (k, v) in env {
        // SAFETY: still single-threaded here.
        unsafe { std::env::set_var(k, v) };
    }
    unsafe { std::env::set_var("STYR_LAUNCH_ID", launch_id) };

    let mut argv: Vec<CString> = vec![CString::new(bin.as_os_str().as_bytes())?];
    for a in args {
        argv.push(CString::new(a.as_bytes()).context("argument contains a NUL byte")?);
    }
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());

    // SAFETY: termios/winsize of our own terminal are copied to the new pty.
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    unsafe { libc::tcgetattr(0, &mut term) };
    let mut ws = winsize(0).unwrap_or(unsafe { std::mem::zeroed() });
    let mut master: libc::c_int = -1;
    // SAFETY: forkpty; the child only calls execv / _exit.
    let pid = unsafe { libc::forkpty(&mut master, std::ptr::null_mut(), &mut term, &mut ws) };
    if pid < 0 {
        bail!("forkpty failed: {}", std::io::Error::last_os_error());
    }
    if pid == 0 {
        unsafe {
            libc::execv(argv_ptrs[0], argv_ptrs.as_ptr());
            libc::_exit(127);
        }
    }

    let raw = RawMode::enter()?;
    // SAFETY: `master` is a fresh descriptor we own; dup it for the writer side.
    let master_read = unsafe { std::fs::File::from_raw_fd(master) };
    let master_write = Arc::new(Mutex::new(master_read.try_clone()?));
    let line = Arc::new(Mutex::new(LineState::default()));
    let done = Arc::new(AtomicBool::new(false));
    // A terminal emulator over the agent's output, to read its screen.
    let screen = Arc::new(Mutex::new(vt100::Parser::new(
        nz(ws.ws_row),
        nz(ws.ws_col),
        0,
    )));
    let on_screen: Arc<Mutex<Option<TerminalPrompt>>> = Arc::new(Mutex::new(None));

    // Keyboard → agent.
    {
        let (w, line) = (master_write.clone(), line.clone());
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 {
                    break;
                }
                line.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .feed(&buf[..n]);
                if w.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .write_all(&buf[..n])
                    .is_err()
                {
                    break;
                }
            }
        });
    }
    // Agent → screen.
    let output = {
        let mut r = master_read;
        let screen = screen.clone();
        let emulate = detector.is_some();
        std::thread::spawn(move || {
            let mut out = std::io::stdout();
            let mut buf = [0u8; 16384];
            while let Ok(n) = r.read(&mut buf) {
                if n == 0 || out.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = out.flush();
                if emulate {
                    screen
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .process(&buf[..n]);
                }
            }
        })
    };
    // Permission menus on screen → daemon (agents without a permission hook).
    if let Some(detect) = detector {
        let (screen, on_screen, done) = (screen.clone(), on_screen.clone(), done.clone());
        let launch_id = launch_id.to_owned();
        std::thread::spawn(move || {
            let client =
                Client::new(Paths::resolve().socket).with_timeout(Some(Duration::from_secs(2)));
            let mut last: Option<TerminalPrompt> = None;
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(150));
                let contents = screen
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .screen()
                    .contents();
                let now = detect(&contents);
                *on_screen.lock().unwrap_or_else(|p| p.into_inner()) = now.clone();
                if now != last {
                    let _ = client.call(&Request::TerminalPrompt {
                        launch_id: launch_id.clone(),
                        prompt: now.clone(),
                    });
                    last = now;
                }
            }
        });
    }
    // Window size follows the real terminal.
    {
        let done = done.clone();
        let screen_for_resize = screen.clone();
        std::thread::spawn(move || {
            let mut last = winsize(0).map(|w| (w.ws_row, w.ws_col));
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
                if let Some(w) = winsize(0)
                    && Some((w.ws_row, w.ws_col)) != last
                {
                    last = Some((w.ws_row, w.ws_col));
                    // SAFETY: TIOCSWINSZ on our pty master; the kernel signals the agent.
                    unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &w) };
                    screen_for_resize
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .screen_mut()
                        .set_size(nz(w.ws_row), nz(w.ws_col));
                }
            }
        });
    }
    // Phone prompts → agent.
    {
        let (w, line, done, on_screen) = (
            master_write.clone(),
            line.clone(),
            done.clone(),
            on_screen.clone(),
        );
        let launch_id = launch_id.to_owned();
        std::thread::spawn(move || inject_loop(&launch_id, &w, &line, &done, &on_screen));
    }

    // Wait for the agent to exit.
    let mut status: libc::c_int = 0;
    // SAFETY: waiting for our own child.
    unsafe { libc::waitpid(pid, &mut status, 0) };
    done.store(true, Ordering::Relaxed);
    let _ = output.join();
    drop(raw);
    let code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        128 + libc::WTERMSIG(status)
    };
    Ok(code)
}

fn inject_loop(
    launch_id: &str,
    w: &Mutex<std::fs::File>,
    line: &Mutex<LineState>,
    done: &AtomicBool,
    on_screen: &Mutex<Option<TerminalPrompt>>,
) {
    let client = Client::new(Paths::resolve().socket).with_timeout(None);
    while !done.load(Ordering::Relaxed) {
        match client.call(&Request::AttachTerminal {
            launch_id: launch_id.to_owned(),
        }) {
            Ok(Response::TypePrompt { text }) => {
                // Don't type over a half-written line; wait until it's sent or cleared.
                while line.lock().unwrap_or_else(|p| p.into_inner()).dirty()
                    && !done.load(Ordering::Relaxed)
                {
                    std::thread::sleep(Duration::from_millis(150));
                }
                let (body, enter) = keystrokes(&text);
                let mut f = w.lock().unwrap_or_else(|p| p.into_inner());
                let _ = f.write_all(&body);
                let _ = f.flush();
                drop(f);
                std::thread::sleep(Duration::from_millis(150));
                let _ = w
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .write_all(&enter);
            }
            Ok(Response::SelectOption { index }) => {
                // Only answer the menu that is actually on screen right now.
                let current = on_screen.lock().unwrap_or_else(|p| p.into_inner()).clone();
                if let Some(p) = current.filter(|p| index < p.options.len()) {
                    let mut f = w.lock().unwrap_or_else(|p| p.into_inner());
                    for key in menu_keys(p.selected, index) {
                        let _ = f.write_all(&key);
                        let _ = f.flush();
                        std::thread::sleep(Duration::from_millis(60));
                    }
                }
            }
            Ok(_) => std::thread::sleep(Duration::from_secs(1)),
            Err(IpcError::NotRunning(_)) => std::thread::sleep(Duration::from_secs(3)),
            Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    }
}

/// A terminal dimension for the emulator (never zero).
pub fn nz(v: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(v.max(1)).expect("non-zero")
}

/// Keys that move a menu's highlight from `from` to `to` and confirm.
pub fn menu_keys(from: usize, to: usize) -> Vec<Vec<u8>> {
    let step: &[u8] = if to >= from { b"\x1b[B" } else { b"\x1b[A" };
    let mut keys = vec![step.to_vec(); from.abs_diff(to)];
    keys.push(b"\r".to_vec());
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_state_tracks_partial_input() {
        let mut l = LineState::default();
        l.feed(b"git sta");
        assert!(l.dirty());
        l.feed(b"\r");
        assert!(!l.dirty(), "Enter submits");
        l.feed(b"\x1b[A");
        assert!(!l.dirty(), "arrow keys alone don't count");
        l.feed(b"abc\x15");
        assert!(!l.dirty(), "Ctrl-U clears");
        l.feed(b"x\x03");
        assert!(!l.dirty(), "Ctrl-C clears");
        l.feed("é".as_bytes());
        assert!(l.dirty());
    }

    #[test]
    fn multi_line_prompts_are_pasted() {
        assert_eq!(keystrokes("hi"), (b"hi".to_vec(), b"\r".to_vec()));
        let (body, _) = keystrokes("a\nb");
        assert_eq!(body, b"\x1b[200~a\nb\x1b[201~".to_vec());
    }

    /// Rendered by agy 1.5.17 (captured through a pseudo-terminal).
    const AGY_PROMPT: &str = "  Ran touch agy-probe.txt\n\nReason: probe asks\nRequesting permission for:\n   touch agy-probe2.txt\n\nRun this command?\n> 1. Yes, run command\n  2. Yes, and always allow in this conversation for commands that start with 'touch'\n  3. Yes, and always allow for commands that start with 'touch' (Persist to settings.json)\n  4. No, cancel\n\n  ↑/↓ Navigate · tab Amend · ctrl+g edit/expand command\n";

    #[test]
    fn reads_agys_permission_menu() {
        let p = detect_agy_prompt(AGY_PROMPT).unwrap();
        assert_eq!(p.title, "Run this command?");
        assert_eq!(p.detail, "touch agy-probe2.txt");
        assert_eq!(p.options.len(), 4);
        assert_eq!(p.options[0], "Yes, run command");
        assert_eq!(p.options[3], "No, cancel");
        assert_eq!(p.selected, 0);
        let moved = AGY_PROMPT
            .replace("> 1. Yes, run", "  1. Yes, run")
            .replace("  4. No, cancel", "> 4. No, cancel");
        assert_eq!(detect_agy_prompt(&moved).unwrap().selected, 3);
        assert_eq!(
            detect_agy_prompt("> 1. Yes\n  2. No\n"),
            None,
            "needs the agy header"
        );
        assert_eq!(detect_agy_prompt("just output\n"), None);
    }

    #[test]
    fn reads_the_menu_through_a_terminal_emulator() {
        // Cursor-positioned output, as a TUI draws it.
        let mut parser = vt100::Parser::new(nz(20), nz(100), 0);
        parser.process(b"\x1b[2J\x1b[3;1HRequesting permission for:\x1b[4;4Hrm -rf build\x1b[6;1HRun this command?\x1b[7;1H> 1. Yes, run command\x1b[8;3H2. No, cancel");
        let p = detect_agy_prompt(&parser.screen().contents()).unwrap();
        assert_eq!((p.detail.as_str(), p.options.len()), ("rm -rf build", 2));
    }

    #[test]
    fn menu_navigation_keys() {
        assert_eq!(menu_keys(0, 0), vec![b"\r".to_vec()]);
        assert_eq!(menu_keys(0, 3).len(), 4);
        assert_eq!(menu_keys(0, 3)[0], b"\x1b[B".to_vec());
        assert_eq!(menu_keys(2, 0)[0], b"\x1b[A".to_vec());
    }
}
