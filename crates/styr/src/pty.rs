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
use styr_core::ipc::{Client, IpcError, Request, Response};
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
pub fn run(
    bin: &Path,
    args: &[OsString],
    env: &[(String, String)],
    launch_id: &str,
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
        std::thread::spawn(move || {
            let mut out = std::io::stdout();
            let mut buf = [0u8; 16384];
            while let Ok(n) = r.read(&mut buf) {
                if n == 0 || out.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = out.flush();
            }
        })
    };
    // Window size follows the real terminal.
    {
        let done = done.clone();
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
                }
            }
        });
    }
    // Phone prompts → agent.
    {
        let (w, line, done) = (master_write.clone(), line.clone(), done.clone());
        let launch_id = launch_id.to_owned();
        std::thread::spawn(move || inject_loop(&launch_id, &w, &line, &done));
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
            Ok(_) => std::thread::sleep(Duration::from_secs(1)),
            Err(IpcError::NotRunning(_)) => std::thread::sleep(Duration::from_secs(3)),
            Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    }
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
}
