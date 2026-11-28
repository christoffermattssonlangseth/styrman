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
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use styr_core::ipc::{Client, IpcError, Request, Response, TerminalPrompt};
use styr_core::paths::Paths;
use styr_core::phone::WorkStatus;

/// Catches `/styr` typed into an agent that has no hook for it (Codex rejects unknown
/// slash commands): the Enter is swallowed, the line erased, and the command returned.
///
/// Only lines typed plainly qualify, so what we erase is exactly what was typed. The
/// terminal may also send focus and mouse reports (Codex turns both on) and encode
/// keys with the kitty keyboard protocol; those are understood. Anything else that
/// could move the cursor or change the box (arrows, Tab, pastes, …) disqualifies the line.
#[derive(Debug)]
pub struct StyrCatcher {
    typed: Vec<u8>,
    clean: bool,
    /// An escape sequence in progress (it can span reads).
    esc: Option<Vec<u8>>,
    /// Raw bytes still owed to a legacy `ESC [ M b x y` mouse report.
    mouse_bytes: u8,
}

impl Default for StyrCatcher {
    fn default() -> Self {
        Self {
            typed: Vec::new(),
            clean: true,
            esc: None,
            mouse_bytes: 0,
        }
    }
}

/// What a complete escape sequence means for the line being typed.
#[derive(Debug, PartialEq)]
enum Key {
    Enter,
    Backspace,
    Char(char),
    /// Focus or mouse report: not typing.
    Neutral,
    /// Anything else: the line may no longer be what we saw typed.
    Other,
}

fn classify(seq: &[u8]) -> Key {
    let s = String::from_utf8_lossy(seq);
    if s == "\x1bOM" {
        return Key::Enter; // keypad Enter
    }
    let Some(body) = s.strip_prefix("\x1b[") else {
        return Key::Other;
    };
    let fin = body.chars().last().unwrap_or(' ');
    let params = &body[..body.len() - fin.len_utf8()];
    match fin {
        'I' | 'O' if params.is_empty() => Key::Neutral, // focus in/out
        // Replies to the agent's own terminal queries (device attributes, kitty
        // keyboard flags, mode reports, cursor position) arrive as input too.
        _ if params.starts_with('?') || params.starts_with('>') => Key::Neutral,
        'R' if params.contains(';') => Key::Neutral,
        'M' | 'm' if params.starts_with('<') => Key::Neutral, // SGR mouse
        'u' => {
            // kitty: code[:alternates];mods[:event];text
            let mut fields = params.split(';');
            let code: u32 = fields
                .next()
                .and_then(|c| c.split(':').next())
                .and_then(|c| c.parse().ok())
                .unwrap_or(0);
            let mods_field = fields.next().unwrap_or("1");
            let mut m = mods_field.split(':');
            let mods: u32 = m.next().and_then(|v| v.parse().ok()).unwrap_or(1);
            if m.next().is_some_and(|e| e == "3") {
                return Key::Neutral; // key release
            }
            let text = fields.next().and_then(|t| {
                t.split(':')
                    .filter_map(|c| c.parse::<u32>().ok())
                    .map(char::from_u32)
                    .collect::<Option<String>>()
            });
            match (code, mods) {
                (13, 1) => Key::Enter,
                (127 | 8, 1) => Key::Backspace,
                (c, 1 | 2) if c >= 32 && c != 127 => text
                    .and_then(|t| t.chars().next())
                    .or_else(|| char::from_u32(c))
                    .map_or(Key::Other, Key::Char),
                _ => Key::Other,
            }
        }
        '~' if params == "27;1;13" => Key::Enter, // xterm modifyOtherKeys
        _ => Key::Other,
    }
}

impl StyrCatcher {
    /// Returns the bytes to pass on and the command, if one was caught.
    pub fn feed(&mut self, input: &[u8]) -> (Vec<u8>, Option<styr_core::adapter::StyrCommand>) {
        let mut out = Vec::with_capacity(input.len());
        let mut caught = None;
        for &b in input {
            if self.mouse_bytes > 0 {
                self.mouse_bytes -= 1;
                out.push(b);
                continue;
            }
            if let Some(seq) = self.esc.as_mut() {
                seq.push(b);
                // OSC / DCS / APC / PM strings (e.g. colour query replies) run to
                // BEL or ESC \\ and are never typing.
                if seq.len() > 2 && matches!(seq[1], b']' | b'P' | b'_' | b'^') {
                    let end =
                        b == 0x07 || (b == b'\\' && seq[seq.len() - 2] == 0x1b) || seq.len() > 4096;
                    if end {
                        out.extend(self.esc.take().expect("seq"));
                    }
                    continue;
                }
                let done = match seq.len() {
                    2 => !matches!(b, b'[' | b'O' | b']' | b'P' | b'_' | b'^'),
                    3 if seq[1] == b'O' => true,
                    3 if seq[1] == b'[' && b == b'M' => {
                        // Legacy mouse report: three raw bytes follow.
                        self.mouse_bytes = 3;
                        let seq = self.esc.take().expect("seq");
                        out.extend(seq);
                        continue;
                    }
                    n => n > 2 && seq[1] == b'[' && (0x40..=0x7e).contains(&b) || n > 64,
                };
                if done {
                    let seq = self.esc.take().expect("seq");
                    match classify(&seq) {
                        Key::Enter => {
                            if self.on_enter(&mut out, &mut caught) {
                                continue; // swallowed
                            }
                        }
                        Key::Backspace => self.backspace(),
                        Key::Char(c) => {
                            let mut buf = [0u8; 4];
                            self.typed
                                .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                        }
                        Key::Neutral => {}
                        Key::Other => self.clean = false,
                    }
                    out.extend(seq);
                }
                continue;
            }
            match b {
                b'\r' | b'\n' => {
                    if self.on_enter(&mut out, &mut caught) {
                        continue;
                    }
                    out.push(b);
                    continue;
                }
                0x1b => {
                    self.esc = Some(vec![b]);
                    continue;
                }
                0x03 | 0x15 => self.reset(),
                0x7f | 0x08 => self.backspace(),
                0x20..=0x7e | 0x80..=0xff => self.typed.push(b),
                _ => self.clean = false,
            }
            out.push(b);
        }
        (out, caught)
    }

    /// On Enter: if the line is a plainly typed `/styr`, erase it and return true
    /// (the Enter must not be passed on). Either way a new line starts.
    fn on_enter(
        &mut self,
        out: &mut Vec<u8>,
        caught: &mut Option<styr_core::adapter::StyrCommand>,
    ) -> bool {
        let line = String::from_utf8_lossy(&self.typed).into_owned();
        let cmd = self
            .clean
            .then(|| styr_core::adapter::parse_styr_command(&line))
            .flatten();
        self.reset();
        match cmd.filter(|_| caught.is_none()) {
            Some(cmd) => {
                out.extend(std::iter::repeat_n(0x7f, line.chars().count()));
                *caught = Some(cmd);
                true
            }
            None => false,
        }
    }

    fn backspace(&mut self) {
        // Drop one whole UTF-8 character.
        while let Some(last) = self.typed.pop() {
            if last & 0xC0 != 0x80 {
                break;
            }
        }
    }

    fn reset(&mut self) {
        self.typed.clear();
        self.clean = true;
    }
}

/// Tracks whether the user has a partly typed line (so we don't type over it).
///
/// Only keys count. The terminal also sends replies to the agent's own queries
/// (colours, cursor position, keyboard mode), focus and mouse reports; those are
/// skipped whole, or a colour reply like `]11;rgb:…` would look like typing.
///
/// Counting keys alone can't see every way a line empties (Esc Esc, Ctrl-W, a menu
/// answered with `y`), so [`LineState::typing`] also asks the screen: an input box
/// that visibly holds no text is not a half-typed line, and when the box can't be
/// read the count expires a while after the last key.
#[derive(Debug, Default, Clone)]
pub struct LineState {
    /// Characters typed on the current line (backspace takes one away). Counting
    /// rather than a flag: after `/styr` is caught and erased with backspaces the line
    /// is empty again, and phone messages mustn't wait for an Enter that never comes.
    chars: usize,
    esc: Option<Vec<u8>>,
    mouse_bytes: u8,
    /// When the last key (not a terminal reply or report) came in.
    last_key: Option<std::time::Instant>,
    /// The count when a menu opened: keys typed into a menu (`y`, `1`) answer it,
    /// and the line is as it was once the menu is gone.
    before_menu: Option<usize>,
}

/// Keys just typed may not be drawn yet: until then the screen's word doesn't count.
const SCREEN_LAG: Duration = Duration::from_millis(1500);
/// When the input box can't be read, a count this long untouched is stale.
const STALE_LINE: Duration = Duration::from_secs(30);

impl LineState {
    pub fn dirty(&self) -> bool {
        self.chars > 0
    }

    /// Whether the user is halfway through a line, given what the input box shows
    /// (`Some(true)`: text in it, `Some(false)`: visibly empty, `None`: unknown).
    pub fn typing(&self, box_text: Option<bool>, now: std::time::Instant) -> bool {
        let within = |d: Duration| {
            self.last_key
                .is_some_and(|t| now.saturating_duration_since(t) < d)
        };
        match box_text {
            Some(true) => self.dirty(),
            Some(false) => self.dirty() && within(SCREEN_LAG),
            None => self.dirty() && within(STALE_LINE),
        }
    }

    /// A menu opened (`true`) or closed on the agent's screen.
    pub fn menu(&mut self, open: bool) {
        match (open, self.before_menu) {
            (true, None) => self.before_menu = Some(self.chars),
            (false, Some(c)) => {
                self.before_menu = None;
                self.chars = c;
            }
            _ => {}
        }
    }

    fn key(&mut self) {
        self.last_key = Some(std::time::Instant::now());
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.mouse_bytes > 0 {
                self.mouse_bytes -= 1;
                continue;
            }
            if let Some(seq) = self.esc.as_mut() {
                seq.push(b);
                if seq.len() > 2 && matches!(seq[1], b']' | b'P' | b'_' | b'^') {
                    // A string (OSC, DCS, …) ends at BEL or ESC \.
                    if b == 0x07 || (b == b'\\' && seq[seq.len() - 2] == 0x1b) || seq.len() > 4096 {
                        self.esc = None;
                    }
                    continue;
                }
                let done = match seq.len() {
                    2 => !matches!(b, b'[' | b'O' | b']' | b'P' | b'_' | b'^'),
                    3 if seq[1] == b'O' => true,
                    3 if seq[1] == b'[' && b == b'M' => {
                        self.mouse_bytes = 3;
                        self.esc = None;
                        continue;
                    }
                    n => n > 2 && seq[1] == b'[' && (0x40..=0x7e).contains(&b) || n > 64,
                };
                if done {
                    let seq = self.esc.take().expect("seq");
                    match classify(&seq) {
                        Key::Enter => self.chars = 0,
                        Key::Char(_) => self.chars += 1,
                        Key::Backspace => self.chars = self.chars.saturating_sub(1),
                        // Focus, mouse, replies: not typing at all.
                        Key::Neutral => continue,
                        // Arrows and the like: a key, but no character.
                        Key::Other => {}
                    }
                    self.key();
                }
                continue;
            }
            match b {
                0x1b => {
                    self.esc = Some(vec![b]);
                    continue;
                }
                // Enter submits; Ctrl-C / Ctrl-U clear the line.
                b'\r' | b'\n' | 0x03 | 0x15 => self.chars = 0,
                0x7f | 0x08 => self.chars = self.chars.saturating_sub(1),
                // UTF-8 continuation bytes belong to the character before.
                0x80..=0xbf => {}
                0x20..=0x7e | 0xc0..=0xff => self.chars += 1,
                _ => {}
            }
            self.key();
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

/// The terminal's own attributes while it's in raw mode, for the signal handler.
struct SavedTermios(std::cell::UnsafeCell<std::mem::MaybeUninit<libc::termios>>);
// SAFETY: written only by `RawMode::enter` before `RAW_ACTIVE` is set, read only
// while it is set.
unsafe impl Sync for SavedTermios {}
static SAVED_TERMIOS: SavedTermios =
    SavedTermios(std::cell::UnsafeCell::new(std::mem::MaybeUninit::uninit()));
static RAW_ACTIVE: AtomicBool = AtomicBool::new(false);
/// A terminating signal (SIGTERM, SIGHUP, SIGQUIT) that styr got, 0 for none.
static SIGNALLED: AtomicI32 = AtomicI32::new(0);
/// The signals that would kill styr with the terminal left in raw mode.
const FATAL_SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];

/// SIGTERM/SIGHUP/SIGQUIT: give the terminal back its attributes and pass the signal
/// on to the agent; `run` then ends as the agent exits (or `watch_signals` ends
/// styr if it doesn't). Only async-signal-safe calls in here.
extern "C" fn on_fatal_signal(sig: libc::c_int) {
    SIGNALLED.store(sig, Ordering::SeqCst);
    // SAFETY: tcsetattr and kill are async-signal-safe; SAVED_TERMIOS is initialised
    // whenever RAW_ACTIVE is set.
    unsafe {
        if RAW_ACTIVE.load(Ordering::SeqCst) {
            libc::tcsetattr(0, libc::TCSANOW, (*SAVED_TERMIOS.0.get()).as_ptr());
        }
        let pid = CHILD.load(Ordering::SeqCst);
        if pid > 0 {
            libc::kill(pid, sig);
        }
    }
}

/// Install the handler (once per process).
fn install_signal_handlers() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        for sig in FATAL_SIGNALS {
            // SAFETY: installing a handler that only makes async-signal-safe calls.
            unsafe {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = on_fatal_signal as extern "C" fn(libc::c_int) as usize;
                libc::sigemptyset(&mut sa.sa_mask);
                sa.sa_flags = libc::SA_RESTART;
                libc::sigaction(sig, &sa, std::ptr::null_mut());
            }
        }
        // If the agent ignores the signal, don't hang on: give it a moment, then go.
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let sig = SIGNALLED.load(Ordering::SeqCst);
                if sig == 0 {
                    continue;
                }
                std::thread::sleep(Duration::from_secs(3));
                // SAFETY: restoring the saved attributes, then exiting.
                unsafe {
                    if RAW_ACTIVE.load(Ordering::SeqCst) {
                        libc::tcsetattr(0, libc::TCSANOW, (*SAVED_TERMIOS.0.get()).as_ptr());
                    }
                    libc::_exit(128 + sig);
                }
            }
        });
    });
}

/// The signals the agent's process starts with: default handling, nothing blocked.
/// Called in the forked child before exec.
fn reset_child_signals() {
    // SAFETY: async-signal-safe calls in the freshly forked child.
    unsafe {
        for sig in FATAL_SIGNALS {
            libc::signal(sig, libc::SIG_DFL);
        }
        let mut none: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut none);
        libc::sigprocmask(libc::SIG_SETMASK, &none, std::ptr::null_mut());
    }
}

impl RawMode {
    fn enter() -> Result<Self> {
        // SAFETY: plain termios calls on stdin with structs we own.
        unsafe {
            let mut orig: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut orig) != 0 {
                bail!("tcgetattr failed");
            }
            if !RAW_ACTIVE.load(Ordering::SeqCst) {
                (*SAVED_TERMIOS.0.get()).write(orig);
                RAW_ACTIVE.store(true, Ordering::SeqCst);
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
        RAW_ACTIVE.store(false, Ordering::SeqCst);
        // SAFETY: restoring the attributes saved in `enter`.
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.0);
        }
    }
}

/// How often an open menu is told to the daemon again (a daemon that restarted
/// knows nothing of it, and the phone couldn't answer it).
const MENU_REANNOUNCE: Duration = Duration::from_secs(5);
/// How often the agent's mode is told to the daemon again.
const MODE_REANNOUNCE: Duration = Duration::from_secs(15);

/// The daemon got the report (it may still have refused it: sending it again
/// wouldn't help). Not when it couldn't be reached: then it's sent again.
fn delivered(r: &Result<Response, IpcError>) -> bool {
    matches!(r, Ok(_) | Err(IpcError::Remote(_)))
}

/// When to tell the daemon about the menu on screen: whenever it changes, again
/// until a report gets through, and now and then while one is open.
#[derive(Debug, Default)]
struct Announcer {
    /// What the daemon last heard, and when.
    last: Option<(Option<TerminalPrompt>, std::time::Instant)>,
}

impl Announcer {
    fn due(&self, now: &Option<TerminalPrompt>, at: std::time::Instant, every: Duration) -> bool {
        match &self.last {
            None => now.is_some(),
            Some((last, _)) if last != now => true,
            Some((last, sent)) => last.is_some() && at.saturating_duration_since(*sent) >= every,
        }
    }

    fn sent(&mut self, now: Option<TerminalPrompt>, at: std::time::Instant) {
        self.last = Some((now, at));
    }
}

/// Whether `styr <agent>` should wrap the agent (interactive terminal on both ends).
pub fn can_wrap() -> bool {
    is_tty(0) && is_tty(1) && std::env::var_os("STYR_NO_PTY").is_none()
}

/// Run `bin args…` in a pseudo-terminal and return its exit code.
/// Reads an agent's own permission menu off the screen (agent-specific).
pub type PromptDetector = fn(&str) -> Option<TerminalPrompt>;

/// The phone asked to end the session (see `end_agent`).
pub static END_REQUESTED: AtomicBool = AtomicBool::new(false);
/// The phone asked to restart the agent with other options (see `crate::restart`).
pub static RESTART: Mutex<Option<crate::restart::Restart>> = Mutex::new(None);
/// The agent's pid, once forked.
static CHILD: AtomicI32 = AtomicI32::new(0);
/// Send the screen until this time (ms since the epoch): a phone is watching.
static SCREEN_UNTIL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Reads the agent's permission mode off its screen.
pub type ModeReader = fn(&str) -> Option<&'static str>;

/// Reads the reply the agent is writing off its screen.
pub type PreviewReader = fn(&str) -> Option<String>;

/// Reads the agent's status line ("✻ Crunching… (3s · thinking)") off its screen.
pub type StatusReader = fn(&str) -> Option<WorkStatus>;

/// What the wrapper reads off an agent's screen besides permission menus.
#[derive(Default, Clone, Copy)]
pub struct ScreenReaders {
    pub mode: Option<ModeReader>,
    pub preview: Option<PreviewReader>,
    pub status: Option<StatusReader>,
}

/// Claude's status line while it works, above its input box:
/// `✻ Crunching… (1m 3s · ↓ 2.1k tokens · thinking)`. A long one wraps onto the
/// next line. Nothing once the turn is over (`✻ Crunched for 1m 3s`).
pub fn claude_status(contents: &str) -> Option<WorkStatus> {
    let lines: Vec<&str> = contents.lines().collect();
    let rule = |l: &str| l.trim().len() >= 20 && l.trim().chars().all(|c| c == '─');
    let bottom = lines
        .iter()
        .rposition(|l| l.starts_with('❯'))
        .and_then(|i| (0..i).rev().find(|&j| rule(lines[j])))
        .unwrap_or(lines.len());
    let (i, rest) = (0..bottom).rev().find_map(|i| {
        let l = lines[i];
        let t = l.trim_start();
        let rest = ["✻", "✶", "✳", "✢", "✽", "·", "*"]
            .iter()
            .find_map(|g| t.strip_prefix(g)?.strip_prefix(' '))?;
        (l.len() - t.len() <= 2).then_some((i, rest))
    })?;
    let (label, detail) = match rest.split_once(" (") {
        Some((label, d)) => {
            let mut d = d.trim_end().to_owned();
            for next in lines.iter().take(bottom).skip(i + 1).take(2) {
                if d.ends_with(')') {
                    break;
                }
                d = format!("{} {}", d.trim_end(), next.trim());
            }
            (label, Some(d.trim_end_matches(')').to_owned()))
        }
        None => (rest.trim_end(), None),
    };
    let word = label.strip_suffix('…')?;
    if word.is_empty() || word.contains(' ') || !word.starts_with(char::is_alphabetic) {
        return None;
    }
    Some(status_parts(label, detail.as_deref(), " · "))
}

/// Codex's status line while it works: `• Working (3s • esc to interrupt)`, or
/// what it's doing in its place (`• Running tests (12s • esc to interrupt)`).
pub fn codex_status(contents: &str) -> Option<WorkStatus> {
    let lines: Vec<&str> = contents.lines().collect();
    let input = lines.iter().rposition(|l| l.starts_with("› "))?;
    lines[..input].iter().rev().take(6).find_map(|l| {
        let t = l.trim().strip_prefix("• ")?;
        let (label, d) = t.split_once(" (")?;
        let d = d.strip_suffix(')')?;
        d.contains("esc to interrupt")
            .then(|| status_parts(label, Some(d), " • "))
    })
}

/// A status line's label and the parts in its brackets.
fn status_parts(label: &str, detail: Option<&str>, sep: &str) -> WorkStatus {
    let mut st = WorkStatus {
        label: label.trim().to_owned(),
        elapsed_secs: None,
        tokens: None,
        thinking: false,
        note: None,
    };
    let mut notes = vec![];
    for part in detail.unwrap_or("").split(sep).map(str::trim) {
        if part.is_empty() || part.starts_with("esc to") {
            continue;
        } else if let Some(secs) = duration_secs(part).filter(|_| st.elapsed_secs.is_none()) {
            st.elapsed_secs = Some(secs);
        } else if part.ends_with("tokens") {
            st.tokens = Some(part.to_owned());
        } else if part == "thinking" {
            st.thinking = true;
        } else {
            notes.push(part);
        }
    }
    st.note = (!notes.is_empty()).then(|| notes.join(" · "));
    st
}

/// "1h 2m 3s", "18m 9s", "4s" in seconds.
fn duration_secs(s: &str) -> Option<u64> {
    s.split(' ').try_fold(0u64, |acc, w| {
        let n: u64 = w.get(..w.len().checked_sub(1)?)?.parse().ok()?;
        let unit = match w.chars().last()? {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
        Some(acc + n * unit)
    })
}

/// The reply Claude is writing, from its screen: the last `⏺` block above the input
/// box, unless it's a tool line (`⏺ Bash(ls)`, `⏺ Ran 3 stop hooks`). Lines the
/// terminal wrapped are joined back into paragraphs; lists and indented code keep
/// their line breaks. If the start has scrolled off, it begins with "…".
pub fn claude_preview(contents: &str) -> Option<String> {
    let lines: Vec<&str> = contents.lines().collect();
    // The input box: a rule, the `❯` line, a rule. The reply is above it.
    let rule = |l: &str| l.trim().len() >= 20 && l.trim().chars().all(|c| c == '─');
    let bottom = lines
        .iter()
        .rposition(|l| l.starts_with('❯'))
        .and_then(|i| (0..i).rev().find(|&j| rule(lines[j])))?;
    // The block's `⏺` line, or, when a long reply has scrolled it off, the top of
    // the screen if that's indented reply text.
    let top = lines.iter().position(|l| !l.trim().is_empty())?;
    let (start, scrolled) = match (0..bottom).rev().find(|&i| lines[i].starts_with("⏺ ")) {
        Some(i) => (i, false),
        // Reply text sits two spaces in (four in a list); deeper is a tool's
        // output whose `⏺` line scrolled off (a file being written, numbered).
        None if top < bottom
            && lines[top].starts_with("  ")
            && lines[top].len() - lines[top].trim_start().len() <= 4
            && !lines[top].trim_start().starts_with('⎿') =>
        {
            (top, true)
        }
        None => return None,
    };
    // A block above your latest prompt is the previous reply, not one being written.
    if lines[start + 1..bottom].iter().any(|l| l.starts_with('❯')) {
        return None;
    }
    let first = lines[start].trim_start_matches("⏺ ").trim();
    let tool_like = first
        .find('(')
        .is_some_and(|i| i < 40 && !first[..i].contains(' '))
        || tool_summary(first)
        || timed_step(first);
    // A line with a `⎿` right under it is a tool step (`⏺ Sleeping 5 seconds then
    // printing hi` / `⎿ $ sleep 5`), even while its `⏺` blinks off; reply text never
    // has one.
    let detail = |i: usize| {
        lines
            .get(i + 1)
            .is_some_and(|n| n.trim_start().starts_with('⎿'))
    };
    if tool_like || detail(start) {
        return None;
    }
    let mut body = vec![first.to_owned()];
    for (i, l) in lines.iter().enumerate().take(bottom).skip(start + 1) {
        let t = l.trim_start();
        // The spinner ("✻ Baked for 4s"), a tool result, the next block. (The spinner
        // can have a `⎿ Tip: …` under it: that's not a tool step.)
        if ["⏺", "⎿", "✻", "✶", "✳", "✢", "✽", "·", "*", "❯"]
            .iter()
            .any(|p| t.starts_with(p) && l.len() - t.len() <= 2)
        {
            break;
        }
        // The latest block is a tool whose `⏺` is off right now: a line with its
        // details under it, or a summary ("Reading 1 file…"). The reply above it is
        // finished (it arrives as a message); reading the tool line as more of it
        // made the preview flip between two texts with every blink.
        if detail(i) || tool_summary(t) || timed_step(t) || folded_output(t) {
            return None;
        }
        body.push(l.strip_prefix("  ").unwrap_or(l).trim_end().to_owned());
    }
    if let Some(last) = body.iter_mut().rev().find(|l| !l.trim().is_empty()) {
        *last = without_tag(last).to_owned();
    }
    reflow(body, scrolled)
}

/// Claude tags some replies after their last word (" · summary", joined with a
/// no-break space): screen furniture, not part of the reply.
fn without_tag(line: &str) -> &str {
    match line.rsplit_once(" ·\u{a0}") {
        Some((text, tag))
            if !tag.is_empty() && tag.chars().all(|c| c.is_ascii_lowercase() || c == ' ') =>
        {
            text
        }
        _ => line,
    }
}

/// How Claude starts a tool step's line: done ("Read 3 files") or in progress
/// ("Reading 2 files…").
const TOOL_VERBS: [&str; 19] = [
    "Ran ",
    "Read ",
    "Searched ",
    "Listed ",
    "Wrote ",
    "Updated ",
    "Edited ",
    "Fetched ",
    "Called ",
    "Interrupted",
    "Running ",
    "Reading ",
    "Searching ",
    "Listing ",
    "Writing ",
    "Editing ",
    "Updating ",
    "Fetching ",
    "Calling ",
];

/// Claude's one-line summary of tool steps ("Reading 1 file…", "Read 3 files (ctrl+o
/// to expand)", "Listed 1 directory", "Searched for 2 patterns"), not reply text
/// that happens to start with "Reading".
fn tool_summary(t: &str) -> bool {
    let t = t.trim_end();
    let Some(rest) = TOOL_VERBS
        .iter()
        .find_map(|v| t.strip_prefix(v))
        .filter(|_| t.chars().count() < 90)
    else {
        return false;
    };
    if t.starts_with("Interrupted") {
        return rest.is_empty() || rest.starts_with(" ·");
    }
    let rest = rest.strip_prefix("for ").unwrap_or(rest);
    t.ends_with('…') || t.contains("ctrl+o") || rest.starts_with(|c: char| c.is_ascii_digit())
}

/// Where Claude folds a tool's long output: "… +102 lines (ctrl+o to expand)",
/// "… +102 lines (~7 KB)". Reply text never has it.
fn folded_output(t: &str) -> bool {
    t.strip_prefix("… +")
        .and_then(|r| r.split_once(' '))
        .is_some_and(|(n, rest)| n.chars().all(|c| c.is_ascii_digit()) && rest.starts_with("line"))
}

/// A running step with its timer: Claude shows a command by its description while
/// it runs ("Waiting for the smoke test output · 1m 28s", "Running 1 shell command
/// · 2s…"), often with no `⎿` details under it.
fn timed_step(t: &str) -> bool {
    let t = t.trim_end().trim_end_matches('…');
    let Some((what, time)) = t.rsplit_once(" · ") else {
        return false;
    };
    let unit = |w: &str| {
        w.strip_suffix(['h', 'm', 's'])
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    };
    !what.trim().is_empty() && t.chars().count() < 120 && time.split(' ').all(unit)
}

/// A reply's lines (indent removed) as text: lines the terminal wrapped are joined
/// back into paragraphs; lists and indented code keep their line breaks. "…" first
/// if its start has scrolled off.
fn reflow(body: Vec<String>, scrolled: bool) -> Option<String> {
    let mut body: Vec<String> = body.into_iter().filter_map(table_line).collect();
    while body.last().is_some_and(|l| l.is_empty()) {
        body.pop();
    }
    // Join wrapped lines: a line continues the one before unless it's blank, a list
    // item, a table row, indented code, or follows a blank line. Inside a list item
    // the terminal indents wrapped lines to the item's text: those continue it.
    let mut out = String::new();
    let mut prev_blank = true;
    let mut in_item = false;
    let mut prev_row = false;
    for l in &body {
        let t = l.trim_start();
        let item = list_item(t);
        let row = t.starts_with("| ");
        if l.is_empty() {
            out.push_str("\n\n");
            prev_blank = true;
            in_item = false;
            prev_row = false;
            continue;
        }
        if !prev_blank {
            let indented = l.starts_with("  ");
            let joins = !item && !row && !prev_row && (!indented || in_item);
            out.push(if joins { ' ' } else { '\n' });
        }
        out.push_str(if prev_blank || !in_item || item || row {
            l.trim_end()
        } else {
            t.trim_end()
        });
        if item {
            in_item = true;
        } else if !l.starts_with("  ") {
            in_item = false;
        }
        prev_row = row;
        prev_blank = false;
    }
    let text = out.trim().to_owned();
    if text.chars().count() < 2 {
        return None;
    }
    Some(if scrolled { format!("…{text}") } else { text })
}

/// "- x", "* x", "12. x" (or "3." still being written).
fn list_item(t: &str) -> bool {
    let numbered = |n: &str| !n.is_empty() && n.len() <= 3 && n.chars().all(|c| c.is_ascii_digit());
    t.starts_with("- ")
        || t.starts_with("* ")
        || t.split_once(". ").is_some_and(|(n, _)| numbered(n))
        || t.strip_suffix('.').is_some_and(numbered)
}

/// Tables as agents draw them in the terminal: borders (`┌──┬──┐`, `├──┼──┤`) go,
/// rows (`│ a │ b │`) become `| a | b |`.
fn table_line(l: String) -> Option<String> {
    let t = l.trim();
    if t.is_empty() {
        return Some(l);
    }
    const BOX: &str = "─━┌┐└┘├┤┬┴┼│┃╭╮╰╯═║╔╗╚╝╠╣╦╩╬╞╡╪ ";
    if t.chars().all(|c| BOX.contains(c)) {
        return None;
    }
    if t.starts_with(['│', '┃', '║']) {
        let cells: Vec<&str> = t
            .split(['│', '┃', '║'])
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .collect();
        return Some(format!("| {} |", cells.join(" | ")));
    }
    Some(l)
}

/// The screen's rows, one per line (trailing blanks trimmed). With `styled`, its
/// formatting goes back to Markdown: bold as `**…**`, italic as `*…*`, and inline
/// code (text in a colour of its own on a line of plain text) as `` `…` ``, so the
/// phone can show a reply formatted while it's written.
pub fn screen_lines(screen: &vt100::Screen, styled: bool) -> String {
    let (rows, cols) = screen.size();
    let (rows, cols) = (rows.get(), cols.get());
    let gray = |c: vt100::Color| match c {
        vt100::Color::Rgb(r, g, b) => r.max(g).max(b) - r.min(g).min(b) < 24,
        vt100::Color::Idx(i) => matches!(i, 0 | 7 | 8 | 15) || i >= 232,
        vt100::Color::Default => true,
    };
    let mut out = String::new();
    for r in 0..rows {
        let cells: Vec<&vt100::Cell> = (0..cols)
            .filter_map(|c| screen.cell(r, c))
            .filter(|c| !c.is_wide_continuation())
            .collect();
        // The line's own text colour: what most of its letters are written in.
        let mut counts: Vec<(vt100::Color, usize)> = vec![];
        for c in &cells {
            if c.contents().chars().any(char::is_alphanumeric) {
                match counts.iter_mut().find(|(k, _)| *k == c.fgcolor()) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((c.fgcolor(), 1)),
                }
            }
        }
        let base = counts.iter().max_by_key(|(_, n)| *n).map(|(k, _)| *k);
        let style = |c: &vt100::Cell| -> (bool, bool, bool) {
            if !styled {
                return (false, false, false);
            }
            let code = c.fgcolor() != vt100::Color::Default
                && Some(c.fgcolor()) != base
                && !gray(c.fgcolor());
            (c.bold() && !code, c.italic() && !code, code)
        };
        // Runs of one style; markers go tight around the words, not the spaces.
        let mut line = String::new();
        let mut i = 0;
        while i < cells.len() {
            let st = style(cells[i]);
            let mut text = String::new();
            while i < cells.len() && style(cells[i]) == st {
                let t = cells[i].contents();
                text.push_str(if t.is_empty() { " " } else { t });
                i += 1;
            }
            let (b, it, code) = st;
            if !(b || it || code) || !text.chars().any(char::is_alphanumeric) {
                line.push_str(&text);
                continue;
            }
            let lead = text.len() - text.trim_start().len();
            let core = text.trim();
            let mark = if code {
                "`"
            } else if b {
                "**"
            } else {
                "*"
            };
            let (open, close) = if b && it && !code {
                ("***", "***")
            } else {
                (mark, mark)
            };
            line.push_str(&text[..lead]);
            line.push_str(open);
            line.push_str(core);
            line.push_str(close);
            line.push_str(&text[lead + core.len()..]);
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// `read` on the formatted screen when it says the same as on the plain one
/// (markers aside); the plain reading otherwise.
pub fn styled_preview(read: PreviewReader, screen: &vt100::Screen) -> Option<String> {
    let plain = read(&screen_lines(screen, false))?;
    let strip = |s: &str| s.replace(['*', '`'], "");
    match read(&screen_lines(screen, true)) {
        Some(styled) if strip(&styled) == strip(&plain) => Some(styled),
        _ => Some(plain),
    }
}

/// While a long reply is written, its start scrolls off the agent's screen: keep
/// what was already read and add what's new, so the preview only grows. `seen` is
/// the preview so far, `now` what the screen shows ("…" first when scrolled).
pub fn grow_preview(seen: &str, now: &str) -> String {
    let Some(tail) = now.strip_prefix('…') else {
        return now.to_owned();
    };
    for n in [80, 40, 20] {
        let probe: String = tail.chars().take(n).collect();
        if probe.chars().count() < n.min(tail.chars().count()) || probe.is_empty() {
            continue;
        }
        if let Some(i) = seen.rfind(&probe) {
            return format!("{}{tail}", &seen[..i]);
        }
    }
    now.to_owned()
}

/// How long a reply's preview may be missing (a frame drawn halfway) and still
/// go on from a screen whose start has scrolled off.
const PREVIEW_GAP: Duration = Duration::from_secs(5);

/// The previews read off the screen, one after the other.
#[derive(Debug, Default)]
pub struct Previews {
    last: Option<String>,
    /// When there last was one.
    seen_at: Option<std::time::Instant>,
}

impl Previews {
    /// The preview to show for what the screen reads now. Past a screenful, what's
    /// new is added to what was read rather than showing the tail. A screen whose
    /// start scrolled off ("…" first) only goes on a reply that was being shown:
    /// otherwise it's as likely the wrapped echo of your own long prompt.
    pub fn next(&mut self, read: Option<String>, at: std::time::Instant) -> Option<String> {
        let recent = self
            .seen_at
            .is_some_and(|t| at.saturating_duration_since(t) < PREVIEW_GAP);
        let now = match read {
            Some(n) if n.starts_with('…') && !recent => None,
            Some(n) => Some(match &self.last {
                Some(seen) => grow_preview(seen, &n),
                None => n,
            }),
            None => None,
        };
        if now.is_some() {
            self.seen_at = Some(at);
            self.last = now.clone();
        } else if !recent {
            self.last = None;
        }
        now
    }
}

/// The reply Codex is writing, from its screen: the last `•` block above its input
/// line (`› …`), while the turn runs. Not a tool step (`• Ran cargo test`,
/// `• Explored`) or the status line (`• Working (3s • esc to interrupt)`); nothing
/// once the turn is over (`Worked for 13s`). "…" first if its start scrolled off.
pub fn codex_preview(contents: &str) -> Option<String> {
    let lines: Vec<&str> = contents.lines().collect();
    let input = lines.iter().rposition(|l| l.starts_with("› "))?;
    let status = |l: &str| {
        let t = l.trim();
        t.starts_with("• Working") || t.contains("esc to interrupt")
    };
    // Upwards from the input line: past blanks and the status line, to the reply's
    // last line.
    let mut end = input;
    while end > 0 && (lines[end - 1].trim().is_empty() || status(lines[end - 1])) {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    let last = lines[end - 1].trim_start_matches(['─', ' ']);
    if last.starts_with("Worked for") || last.starts_with("■") {
        return None; // the turn is over (or was interrupted)
    }
    let mut start = end - 1;
    let scrolled = loop {
        let l = lines[start];
        if l.starts_with("• ") {
            break false;
        }
        // On a short screen Codex pins your prompt to the top, cut to one line
        // ("…"): right under it, the reply's start has scrolled off. (In the history
        // a prompt wraps instead, and a reply under it starts with `•`.)
        if l.starts_with("› ") && l.trim_end().ends_with('…') && start + 1 < end {
            start += 1;
            while start + 1 < end && lines[start].trim().is_empty() {
                start += 1;
            }
            break true;
        }
        // Not reply text: the prompt, a tool's output, a rule.
        if !(l.trim().is_empty() || l.starts_with("  "))
            || l.trim_start().starts_with(['└', '│', '›'])
        {
            return None;
        }
        if start == 0 {
            break true;
        }
        start -= 1;
    };
    let first = if scrolled {
        lines[start].trim()
    } else {
        lines[start].trim_start_matches("• ").trim()
    };
    let tool_like = [
        "Ran ",
        "Running ",
        "Explored",
        "Exploring",
        "Edited ",
        "Editing ",
        "Added ",
        "Deleted ",
        "Read ",
        "Reading ",
        "Searched ",
        "Searching ",
        "Listed ",
        "Updated Plan",
        "Called ",
        "Calling ",
        "Waited",
        "Waiting",
        "Viewed ",
        "Applied ",
        "Interrupted",
    ]
    .iter()
    .any(|p| first.starts_with(p));
    if !scrolled && tool_like {
        return None;
    }
    if scrolled {
        // With its `•` line gone, a tool's output (indented deeper than reply text,
        // or a diff: `    12 +  let a = 1;`) can look like a reply. Lines deeper
        // than reply text at the top are the end of something we can't see.
        let block = &lines[start..end];
        if block.iter().any(|l| diff_line(l)) {
            return None;
        }
        let deep = |l: &str| !l.trim().is_empty() && l.len() - l.trim_start().len() > 2;
        let skip = block
            .iter()
            .take_while(|l| deep(l) || l.trim().is_empty())
            .count();
        if skip > 0 {
            let from = block.get(skip..).filter(|r| !r.is_empty())?;
            return codex_body(from[0].trim(), &from[1..], true);
        }
    }
    codex_body(first, &lines[start + 1..end], scrolled)
}

/// A Codex reply's first line and the lines after it, as text.
fn codex_body(first: &str, rest: &[&str], scrolled: bool) -> Option<String> {
    let mut body = vec![first.to_owned()];
    for l in rest {
        body.push(l.strip_prefix("  ").unwrap_or(l).trim_end().to_owned());
    }
    reflow(body, scrolled)
}

/// A line of a diff as Codex shows an edit: its line number, then `+`, `-` or
/// blanks, then the code (`    12 +  let a = 1;`, `    11    fn main() {`).
fn diff_line(l: &str) -> bool {
    let t = l.trim_start();
    if l.len() - t.len() < 2 {
        return false;
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    digits > 0
        && (t[digits..].starts_with(" +")
            || t[digits..].starts_with(" -")
            || t[digits..].starts_with("   "))
}

/// Claude's mode, from its footer: "⏸ manual mode on", "⏵⏵ accept edits on",
/// "⏸ plan mode on", "⏵⏵ auto mode on", "⏵⏵ bypass permissions on". Versions that
/// show nothing in the default mode still show "? for shortcuts" there. `None` when
/// the footer isn't on screen (a dialog is open, or it's still starting).
pub fn claude_mode(contents: &str) -> Option<&'static str> {
    const MODES: [(&str, &str); 6] = [
        ("manual mode on", "default"),
        ("default mode on", "default"),
        ("accept edits on", "acceptEdits"),
        ("plan mode on", "plan"),
        ("auto mode on", "auto"),
        ("bypass permissions on", "bypassPermissions"),
    ];
    let bottom: Vec<&str> = contents
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(6)
        .collect();
    for l in &bottom {
        if (l.contains('⏸') || l.contains('⏵'))
            && let Some((_, m)) = MODES.iter().find(|(needle, _)| l.contains(needle))
        {
            return Some(m);
        }
    }
    bottom
        .iter()
        .any(|l| l.contains("? for shortcuts"))
        .then_some("default")
}

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

/// Codex's approval menu (0.172.0), as `styr codex` sees it on screen:
/// ```text
///   Would you like to run the following command?
///   Environment: local
///   Reason: Do you want to allow creating menu.txt in the current workspace?
///   $ touch menu.txt
/// › 1. Yes, proceed (y)
///   2. Yes, and don't ask again for commands that start with `touch menu.txt` (p)
///   3. No, and tell Codex what to do differently (esc)
///   Press enter to confirm or esc to cancel
/// ```
/// Only an open menu counts: the footer must follow the options.
pub fn detect_codex_prompt(screen: &str) -> Option<TerminalPrompt> {
    let lines: Vec<&str> = screen.lines().collect();
    // "Press enter to confirm or esc to cancel" (approvals), "enter confirm · esc
    // skip" (e.g. "Hooks need review" at startup), "enter continue · esc back"
    // (trusting a new folder), "Press enter to continue".
    let footer = lines.iter().rposition(|l| {
        let l = l.trim_start();
        l.starts_with("Press enter to") || (l.starts_with("enter ") && l.contains(" · esc"))
    })?;
    // The options sit directly above the footer (blank lines aside).
    let mut end = footer;
    while end > 0 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    // Options may wrap onto indented continuation lines in a narrow terminal.
    let continuation = |l: &str| {
        let t = l.trim_start();
        !t.is_empty() && l.len() - t.len() >= 4 && numbered(l).is_none()
    };
    let mut first_option = end;
    while first_option > 0
        && (numbered(lines[first_option - 1]).is_some() || continuation(lines[first_option - 1]))
    {
        first_option -= 1;
    }
    // The block must start at an option (not at wrapped detail text above it).
    while first_option < end && numbered(lines[first_option]).is_none() {
        first_option += 1;
    }
    let mut raw: Vec<(bool, String)> = Vec::new();
    for l in &lines[first_option..end] {
        match numbered(l) {
            Some((marked, label)) => raw.push((marked, label.to_owned())),
            None => {
                let last = raw.last_mut()?;
                // Codex breaks long words at '-' or '/'; don't add a space there.
                if !last.1.ends_with('-') && !last.1.ends_with('/') {
                    last.1.push(' ');
                }
                last.1.push_str(l.trim());
            }
        }
    }
    let mut options = Vec::new();
    let mut selected = 0;
    for (marked, label) in &raw {
        if *marked {
            selected = options.len();
        }
        // "Yes, proceed (y)" → "Yes, proceed": the shortcut means nothing on the phone.
        let label = label.trim();
        let label = match label.rfind(" (") {
            Some(i) if label.ends_with(')') && label.len() - i <= 8 => &label[..i],
            _ => label,
        };
        options.push(label.trim().to_owned());
    }
    // The question: the topmost line ending in '?' in the menu's block, which starts
    // after the transcript's last `•`/`›` line ("Reason: …?" lines are detail).
    let block_start = (0..first_option)
        .rev()
        .take(30)
        .find(|&i| {
            let l = lines[i].trim_start();
            l.starts_with('•') || l.starts_with('›')
        })
        .map_or(first_option.saturating_sub(30), |i| i + 1);
    let title_at = (block_start..first_option).find(|&i| lines[i].trim().ends_with('?'));
    // "Trust this folder? Codex can read, …": a question that the line goes on past.
    if title_at.is_none()
        && let Some(i) = (block_start..first_option).find(|&i| lines[i].contains("? "))
    {
        let line = lines[i].trim();
        let q = line.find("? ").expect("found") + 1;
        let detail: Vec<String> = std::iter::once(line[q..].trim().to_owned())
            .chain(
                lines[block_start..first_option]
                    .iter()
                    .enumerate()
                    .filter(|&(j, l)| block_start + j != i && !l.trim().is_empty())
                    .map(|(_, l)| l.trim().to_owned()),
            )
            .filter(|l| !l.is_empty())
            .collect();
        return (options.len() >= 2).then(|| TerminalPrompt {
            title: line[..q].to_owned(),
            detail: detail.join("\n"),
            options,
            selected,
        });
    }
    let title_at =
        title_at.or_else(|| (block_start..first_option).find(|&i| !lines[i].trim().is_empty()))?;
    // In a narrow pane the question wraps ("Would you like to run the following" /
    // "command?"): take the whole sentence, back to the start of its paragraph.
    let mut title_start = title_at;
    while title_start > block_start && !lines[title_start - 1].trim().is_empty() {
        title_start -= 1;
    }
    let title = lines[title_start..=title_at]
        .iter()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join(" ");
    let detail: Vec<String> = lines[title_at + 1..first_option]
        .iter()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect();
    (options.len() >= 2).then(|| TerminalPrompt {
        title,
        detail: detail.join("\n"),
        options,
        selected,
    })
}

/// `› 2. Label` / `  1. Label` → (selected, label).
fn numbered(line: &str) -> Option<(bool, &str)> {
    let t = line.trim_start();
    let (marked, rest) = match t.strip_prefix('›').or_else(|| t.strip_prefix('>')) {
        Some(r) => (true, r.trim_start()),
        None => (false, t),
    };
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    rest[digits..]
        .strip_prefix(". ")
        .map(|l| (marked, l.trim_end()))
}

pub fn run(
    bin: &Path,
    args: &[OsString],
    env: &[(String, String)],
    launch_id: &str,
    detector: Option<PromptDetector>,
    readers: ScreenReaders,
    catch_styr: bool,
) -> Result<i32> {
    let mode_reader = readers.mode;
    // Keep a screen copy (vt100) for whatever reads it: menus, mode, preview, phones.
    let emulate_screen = detector.is_some() || readers.mode.is_some() || readers.preview.is_some();
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
        reset_child_signals();
        unsafe {
            libc::execv(argv_ptrs[0], argv_ptrs.as_ptr());
            libc::_exit(127);
        }
    }

    CHILD.store(pid, Ordering::Relaxed);
    install_signal_handlers();
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
    // The menu the daemon last heard about (what a phone's answer is for).
    let reported: Arc<Mutex<Option<TerminalPrompt>>> = Arc::new(Mutex::new(None));
    // Codex says when it waits for an approval (OSC 7501 state=blocked); used to
    // notice a menu we failed to read.
    let blocked = Arc::new(AtomicBool::new(false));

    // Keyboard → agent.
    {
        let (w, line, done) = (master_write.clone(), line.clone(), done.clone());
        let launch_id = launch_id.to_owned();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut catcher = catch_styr.then(StyrCatcher::default);
            // Diagnostics: STYR_KEYLOG=/path logs what the terminal sends (escaped).
            let mut keylog = std::env::var_os("STYR_KEYLOG").and_then(|p| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                    .ok()
            });
            loop {
                // Wait for keys, but notice when the agent has exited: after a restart
                // the next run's thread reads the keyboard, and none may be lost here.
                let mut pfd = libc::pollfd {
                    fd: 0,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: polling our own stdin.
                let ready = unsafe { libc::poll(&mut pfd, 1, 200) };
                if done.load(Ordering::Relaxed) {
                    break;
                }
                if ready <= 0 {
                    continue;
                }
                // Straight from the descriptor: std's buffered stdin could keep keys
                // that poll then doesn't see.
                // SAFETY: reading into our own buffer.
                let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
                if n < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                    continue;
                }
                if n <= 0 {
                    break;
                }
                let n = n as usize;
                if let Some(f) = keylog.as_mut() {
                    let _ = writeln!(f, "{}", buf[..n].escape_ascii());
                }
                let (bytes, caught) = match catcher.as_mut() {
                    Some(c) => c.feed(&buf[..n]),
                    None => (buf[..n].to_vec(), None),
                };
                line.lock().unwrap_or_else(|p| p.into_inner()).feed(&bytes);
                if w.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .write_all(&bytes)
                    .is_err()
                {
                    break;
                }
                if let Some(cmd) = caught {
                    let launch_id = launch_id.clone();
                    std::thread::spawn(move || share_from_terminal(cmd, &launch_id, pid as u32));
                }
            }
        });
    }
    // Agent → screen.
    let output = {
        let mut r = master_read;
        let screen = screen.clone();
        let blocked = blocked.clone();
        let emulate = emulate_screen;
        std::thread::spawn(move || {
            let mut out = std::io::stdout();
            let mut buf = [0u8; 16384];
            while let Ok(n) = r.read(&mut buf) {
                if n == 0 || out.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = out.flush();
                let chunk = &buf[..n];
                if let Some(i) = find(chunk, b"\x1b]7501;state=") {
                    blocked.store(
                        chunk[i..].starts_with(b"\x1b]7501;state=blocked"),
                        Ordering::Relaxed,
                    );
                }
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
        let (screen, on_screen, reported, done, blocked, line) = (
            screen.clone(),
            on_screen.clone(),
            reported.clone(),
            done.clone(),
            blocked.clone(),
            line.clone(),
        );
        let launch_id = launch_id.to_owned();
        std::thread::spawn(move || {
            let client =
                Client::new(Paths::resolve().socket).with_timeout(Some(Duration::from_secs(2)));
            let mut announcer = Announcer::default();
            let mut blocked_since: Option<std::time::Instant> = None;
            let mut dumped = false;
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(150));
                let contents = screen
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .screen()
                    .contents();
                let now = detect(&contents);
                // Waiting for an approval, but no menu we can read: keep a copy of the
                // screen (private, local) so the detector can be fixed.
                if blocked.load(Ordering::Relaxed) && now.is_none() {
                    let since = *blocked_since.get_or_insert_with(std::time::Instant::now);
                    if !dumped && since.elapsed() > Duration::from_secs(2) {
                        dumped = true;
                        dump_unread_menu(&contents);
                    }
                } else {
                    blocked_since = None;
                    dumped = false;
                }
                *on_screen.lock().unwrap_or_else(|p| p.into_inner()) = now.clone();
                line.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .menu(now.is_some());
                let at = std::time::Instant::now();
                if announcer.due(&now, at, MENU_REANNOUNCE) {
                    let sent = client.call(&Request::TerminalPrompt {
                        launch_id: launch_id.clone(),
                        prompt: now.clone(),
                    });
                    if delivered(&sent) {
                        *reported.lock().unwrap_or_else(|p| p.into_inner()) = now.clone();
                        announcer.sent(now, at);
                    }
                }
            }
        });
    }
    // The screen → daemon while a phone watches it (twice a second, when it changed).
    if emulate_screen {
        let (screen, done) = (screen.clone(), done.clone());
        let launch_id = launch_id.to_owned();
        std::thread::spawn(move || {
            let client =
                Client::new(Paths::resolve().socket).with_timeout(Some(Duration::from_secs(2)));
            let mut last = String::new();
            let mut sent_at = std::time::Instant::now();
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                if now_ms() > SCREEN_UNTIL.load(Ordering::Relaxed) {
                    last.clear();
                    continue;
                }
                let (text, cols) = {
                    let s = screen.lock().unwrap_or_else(|p| p.into_inner());
                    (s.screen().contents(), s.screen().size().1.get())
                };
                // Unchanged: still send now and then, so a phone that just opened it gets it.
                if text == last && sent_at.elapsed() < Duration::from_secs(3) {
                    continue;
                }
                let _ = client.call(&Request::TerminalScreen {
                    launch_id: launch_id.clone(),
                    text: text.clone(),
                    cols,
                });
                last = text;
                sent_at = std::time::Instant::now();
            }
        });
    }
    // The reply being written → daemon, whenever it grows (only forwarded while the
    // session is working).
    // With its status line ("✻ Crunching… (3s · thinking)").
    if readers.preview.is_some() || readers.status.is_some() {
        let (read, read_status) = (readers.preview, readers.status);
        let (screen, done) = (screen.clone(), done.clone());
        let launch_id = launch_id.to_owned();
        std::thread::spawn(move || {
            let client =
                Client::new(Paths::resolve().socket).with_timeout(Some(Duration::from_secs(2)));
            let mut previews = Previews::default();
            let mut last: Option<String> = None;
            let mut last_status: Option<WorkStatus> = None;
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(300));
                let (read_now, status) = {
                    let s = screen.lock().unwrap_or_else(|p| p.into_inner());
                    (
                        read.and_then(|r| styled_preview(r, s.screen())),
                        read_status.and_then(|r| r(&s.screen().contents())),
                    )
                };
                let now = previews.next(read_now, std::time::Instant::now());
                if now != last || status != last_status {
                    let _ = client.call(&Request::TerminalPreview {
                        launch_id: launch_id.clone(),
                        text: now.clone(),
                        status: status.clone(),
                    });
                    last = now;
                    last_status = status;
                }
            }
        });
    }
    // The agent's permission mode → daemon, whenever it changes.
    if let Some(read) = mode_reader {
        let (screen, done) = (screen.clone(), done.clone());
        let launch_id = launch_id.to_owned();
        std::thread::spawn(move || {
            let client =
                Client::new(Paths::resolve().socket).with_timeout(Some(Duration::from_secs(2)));
            let mut last: Option<&'static str> = None;
            let mut sent_at = std::time::Instant::now();
            let mut seen_at = std::time::Instant::now();
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(300));
                let contents = screen
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .screen()
                    .contents();
                // A dialog hides the footer for a moment; keep the last mode then.
                let Some(now) = read(&contents) else {
                    if last.is_some() && seen_at.elapsed() > Duration::from_secs(30) {
                        last = None;
                        let _ = client.call(&Request::TerminalMode {
                            launch_id: launch_id.clone(),
                            mode: None,
                        });
                    }
                    continue;
                };
                seen_at = std::time::Instant::now();
                // Now and then even if unchanged: a daemon that restarted has lost it.
                if Some(now) != last || sent_at.elapsed() > MODE_REANNOUNCE {
                    let sent = client.call(&Request::TerminalMode {
                        launch_id: launch_id.clone(),
                        mode: Some(now.to_owned()),
                    });
                    if delivered(&sent) {
                        last = Some(now);
                        sent_at = std::time::Instant::now();
                    }
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
        let agent = catch_styr.then_some(styr_core::ids::AgentKind::Codex);
        let ctx = Ctx {
            w: master_write.clone(),
            line: line.clone(),
            done: done.clone(),
            on_screen: on_screen.clone(),
            reported: reported.clone(),
            screen: screen.clone(),
            detector,
            codex: agent == Some(styr_core::ids::AgentKind::Codex),
        };
        let launch_id = launch_id.to_owned();
        std::thread::spawn(move || inject_loop(agent, &launch_id, &ctx, mode_reader));
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

/// Codex is at its input box: no menu open, and the box or its footer is on screen.
/// The footer changes with the width (a narrow pane drops "? for shortcuts" and shows
/// "← for agents  ⚠ 3 warnings"), so any of these marks counts.
pub fn codex_ready(screen: &str) -> bool {
    detect_codex_prompt(screen).is_none()
        && screen.lines().rev().take(12).any(|l| {
            let l = l.trim_start();
            l.contains("for shortcuts")
                || l.contains("for agents")
                || l.starts_with("› Ask Codex")
                || l.contains("Ask Codex to do anything")
        })
}

/// Whether the agent's input box shows text: `Some(true)` if it does, `Some(false)`
/// if it's visibly empty (a dim placeholder such as `Try "…"` or `Ask Codex to do
/// anything` is no text), `None` if no input box is recognised. Claude's box is the
/// `❯` line under a rule; Codex's (`codex`) the last `›` line that isn't a menu option.
pub fn input_box_text(screen: &vt100::Screen, codex: bool) -> Option<bool> {
    let cols = screen.size().1.get();
    let rows: Vec<String> = screen.rows(0, cols).collect();
    let rule = |l: &str| l.trim().chars().count() >= 20 && l.trim().chars().all(|c| c == '─');
    let row = (1..rows.len())
        .rev()
        .find(|&r| rows[r].starts_with('❯') && rule(&rows[r - 1]))
        .or_else(|| {
            codex
                .then(|| {
                    rows.iter()
                        .rposition(|l| l.starts_with('›') && numbered(l).is_none())
                })
                .flatten()
        })?;
    // Codex's usual placeholder, in case it isn't drawn dim.
    let typed = rows[row].chars().skip(1).collect::<String>();
    if codex && typed.trim() == "Ask Codex to do anything" {
        return Some(false);
    }
    let row = u16::try_from(row).ok()?;
    let text = (1..cols).filter_map(|c| screen.cell(row, c)).any(|c| {
        !c.dim()
            && !c
                .contents()
                .trim_matches(|ch: char| ch.is_whitespace() || ch == '\u{a0}')
                .is_empty()
    });
    Some(text)
}

#[cfg(test)]
mod codex_ready_tests {
    use super::codex_ready;

    #[test]
    fn codex_is_ready_in_a_narrow_pane_too() {
        // A real 42-column cmux pane: the footer has no "? for shortcuts".
        let s = "  >_ OpenAI Codex (v0.172.0)\n\n\n\n\n› Ask Codex to do anything\n \n  GPT-6.2-Sol default · ~/work/styrman\n  ← for agents  ⚠ 3 warnings · f2 to view\n";
        assert!(codex_ready(s));
        // Even if only the footer is left on screen.
        assert!(codex_ready("\n  ← for agents  ⚠ 3 warnings · f2 to view\n"));
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `~/.styrman/unread-menu.txt`: the last screen where the agent waited for an
/// approval that Styrman couldn't read. Readable only by you.
fn dump_unread_menu(contents: &str) {
    use std::os::unix::fs::OpenOptionsExt;
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let path = std::path::Path::new(&home).join(".styrman/unread-menu.txt");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
    {
        let _ = f.write_all(contents.as_bytes());
    }
}

/// Tell the daemon about a caught `/styr` and confirm with a terminal notification
/// (OSC 9), which leaves the agent's screen alone.
fn share_from_terminal(cmd: styr_core::adapter::StyrCommand, launch_id: &str, agent_pid: u32) {
    let shared = cmd == styr_core::adapter::StyrCommand::On;
    let client = Client::new(Paths::resolve().socket).with_timeout(Some(Duration::from_secs(5)));
    let msg = match client.call(&Request::ShareLaunch {
        launch_id: launch_id.to_owned(),
        shared,
        agent: Some(styr_core::ids::AgentKind::Codex),
        cwd: std::env::current_dir().ok(),
        agent_pid: Some(agent_pid),
    }) {
        Ok(Response::LaunchShared { .. }) if shared => "Styrman: this session is now on your phone",
        Ok(Response::LaunchShared { .. }) => "Styrman: stopped sharing this session",
        _ => "Styrman: styrd isn't running (styr service start)",
    };
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]9;{msg}\x07");
    let _ = out.flush();
}

/// What the threads acting on the agent's terminal share.
struct Ctx<W> {
    /// The agent's terminal (keys go in here). Held while a phone prompt is typed,
    /// so no key of yours lands in the middle of it.
    w: Arc<Mutex<W>>,
    line: Arc<Mutex<LineState>>,
    done: Arc<AtomicBool>,
    /// The menu on screen, as last read.
    on_screen: Arc<Mutex<Option<TerminalPrompt>>>,
    /// The menu the daemon last heard about.
    reported: Arc<Mutex<Option<TerminalPrompt>>>,
    screen: Arc<Mutex<vt100::Parser>>,
    detector: Option<PromptDetector>,
    codex: bool,
}

impl<W> Clone for Ctx<W> {
    fn clone(&self) -> Self {
        Self {
            w: self.w.clone(),
            line: self.line.clone(),
            done: self.done.clone(),
            on_screen: self.on_screen.clone(),
            reported: self.reported.clone(),
            screen: self.screen.clone(),
            detector: self.detector,
            codex: self.codex,
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// After this long without recognising Codex's input box (and with no menu open),
/// a phone prompt is typed anyway: holding it forever is worse.
const CODEX_READY_WAIT: Duration = Duration::from_secs(20);

impl<W: Write> Ctx<W> {
    fn done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    fn contents(&self) -> String {
        lock(&self.screen).screen().contents()
    }

    fn menu(&self, contents: &str) -> Option<TerminalPrompt> {
        self.detector.and_then(|d| d(contents))
    }

    fn write(w: &mut W, bytes: &[u8]) {
        let _ = w.write_all(bytes);
        let _ = w.flush();
    }

    /// Type a phone prompt once it's safe:
    /// - never over a half-typed line (the user is typing);
    /// - never into a menu or question ("trust this folder?"), and for Codex
    ///   preferably only once its input box is recognised (see `CODEX_READY_WAIT`).
    ///
    /// The check and the typing happen with the keyboard held off, through to Enter.
    fn type_prompt(&self, text: &str) {
        let since = std::time::Instant::now();
        let mut dumped = false;
        loop {
            if self.done() {
                return;
            }
            {
                let mut w = lock(&self.w);
                let (contents, box_text) = {
                    let s = lock(&self.screen);
                    (
                        s.screen().contents(),
                        input_box_text(s.screen(), self.codex),
                    )
                };
                let typing = lock(&self.line).typing(box_text, std::time::Instant::now());
                let menu_open = self.menu(&contents).is_some();
                let ready = !self.codex || codex_ready(&contents);
                let waited = since.elapsed();
                if !typing && !menu_open && (ready || waited > CODEX_READY_WAIT) {
                    let (body, enter) = keystrokes(text);
                    Self::write(&mut w, &body);
                    std::thread::sleep(Duration::from_millis(150));
                    // A menu that opened just now would take the Enter as its answer.
                    if self.menu(&self.contents()).is_none() {
                        Self::write(&mut w, &enter);
                        return;
                    }
                    drop(w);
                    self.enter_after_menu(&enter);
                    return;
                }
                if self.codex && !dumped && waited > Duration::from_secs(5) && !typing {
                    dumped = true;
                    let s = lock(&self.screen);
                    let (rows, cols) = s.screen().size();
                    dump_unread_menu(&format!(
                        "[waiting for Codex's input box; screen {rows}x{cols}]\n{}",
                        s.screen().contents()
                    ));
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// A menu opened between a prompt's text and its Enter: press Enter once it has
    /// gone, unless you've typed since (then the line is yours to send).
    fn enter_after_menu(&self, enter: &[u8]) {
        while !self.done() {
            std::thread::sleep(Duration::from_millis(150));
            let mut w = lock(&self.w);
            if self.menu(&self.contents()).is_none() {
                if !lock(&self.line).dirty() {
                    Self::write(&mut w, enter);
                }
                return;
            }
        }
    }

    /// Answer the menu on screen with option `index`, if it's still the menu the
    /// daemon was told about (the phone answered that one); the highlight must be on
    /// the option before Enter confirms it.
    fn select_option(&self, index: usize) -> bool {
        let Some(reported) = lock(&self.reported).clone() else {
            return false;
        };
        let same = |m: &TerminalPrompt| {
            m.title == reported.title
                && m.detail == reported.detail
                && m.options == reported.options
        };
        let mut w = lock(&self.w);
        let Some(now) = self.menu(&self.contents()).filter(|m| same(m)) else {
            return false;
        };
        if index >= now.options.len() {
            return false;
        }
        let mut keys = menu_keys(now.selected, index);
        let enter = keys.pop().expect("menu_keys ends with Enter");
        for key in keys {
            Self::write(&mut w, &key);
            std::thread::sleep(Duration::from_millis(60));
        }
        let until = std::time::Instant::now() + Duration::from_millis(1500);
        loop {
            match self.menu(&self.contents()) {
                Some(m) if same(&m) && m.selected == index => {
                    Self::write(&mut w, &enter);
                    return true;
                }
                Some(m) if same(&m) && std::time::Instant::now() < until => {}
                _ => return false,
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Types phone prompts in the order they came, each once it's safe; meanwhile the
/// daemon keeps being listened to (a menu that opens can still be answered).
fn typer_loop<W: Write>(ctx: &Ctx<W>, prompts: std::sync::mpsc::Receiver<String>) {
    use std::sync::mpsc::RecvTimeoutError;
    while !ctx.done() {
        match prompts.recv_timeout(Duration::from_millis(200)) {
            Ok(text) => ctx.type_prompt(&text),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn inject_loop(
    agent: Option<styr_core::ids::AgentKind>,
    launch_id: &str,
    ctx: &Ctx<std::fs::File>,
    mode_reader: Option<ModeReader>,
) {
    let (w, done, on_screen, screen) = (&*ctx.w, &*ctx.done, &*ctx.on_screen, &*ctx.screen);
    let client = Client::new(Paths::resolve().socket).with_timeout(None);
    let cwd = std::env::current_dir().ok();
    let (prompts, queued) = std::sync::mpsc::channel::<String>();
    {
        let ctx = ctx.clone();
        std::thread::spawn(move || typer_loop(&ctx, queued));
    }
    while !done.load(Ordering::Relaxed) {
        match client.call(&Request::AttachTerminal {
            launch_id: launch_id.to_owned(),
            agent,
            cwd: cwd.clone(),
            level: styr_core::ipc::WRAPPER_LEVEL,
        }) {
            Ok(Response::TypePrompt { text }) => {
                // Typed when it's safe (see `Ctx::type_prompt`), in order; listening
                // to the daemon goes on meanwhile.
                let _ = prompts.send(text);
            }
            Ok(Response::SelectOption { index }) => {
                ctx.select_option(index);
            }
            Ok(Response::WatchScreen) => {
                SCREEN_UNTIL.store(now_ms() + 20_000, Ordering::Relaxed);
            }
            Ok(Response::Interrupt) => {
                // Esc stops the turn in Claude Code and Codex.
                let mut f = w.lock().unwrap_or_else(|p| p.into_inner());
                let _ = f.write_all(b"\x1b");
                let _ = f.flush();
            }
            Ok(Response::EndSession) => {
                END_REQUESTED.store(true, Ordering::Relaxed);
                end_agent(w, done);
            }
            Ok(Response::SetTitle { title }) => crate::launch::title_tab(&title),
            Ok(Response::Restart {
                resume,
                model,
                effort,
                mode,
            }) => {
                let what = [model.as_deref(), effort.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(", ");
                *RESTART.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(crate::restart::Restart {
                        resume,
                        model,
                        effort,
                        mode,
                    });
                end_agent(w, done);
                // After the agent's own goodbye.
                eprint!("\r\nStyrman: restarting with {what} (from the phone)…\r\n");
            }
            Ok(Response::SetMode { mode }) => {
                if let Some(read) = mode_reader {
                    switch_mode(read, &mode, w, screen, on_screen);
                }
            }
            Ok(_) => std::thread::sleep(Duration::from_secs(1)),
            Err(IpcError::NotRunning(_)) => std::thread::sleep(Duration::from_secs(3)),
            Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    }
}

/// Quit the agent as you would: Ctrl+C (the first may only interrupt a turn, so up
/// to three), then SIGTERM, then SIGKILL if it still hasn't exited.
fn end_agent(w: &Mutex<std::fs::File>, done: &AtomicBool) {
    let exited_within = |ms: u64| {
        let until = std::time::Instant::now() + Duration::from_millis(ms);
        while std::time::Instant::now() < until {
            if done.load(Ordering::Relaxed) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        done.load(Ordering::Relaxed)
    };
    for _ in 0..3 {
        {
            let mut f = w.lock().unwrap_or_else(|p| p.into_inner());
            let _ = f.write_all(b"\x03");
            let _ = f.flush();
        }
        if exited_within(700) {
            return;
        }
    }
    let pid = CHILD.load(Ordering::Relaxed);
    if pid <= 0 {
        return;
    }
    for (signal, wait) in [(libc::SIGTERM, 2000), (libc::SIGKILL, 1000)] {
        // SAFETY: signalling our own child (not yet reaped: `done` is still false).
        unsafe { libc::kill(pid, signal) };
        if exited_within(wait) {
            return;
        }
    }
}

/// Press Shift+Tab until the screen shows `target`: never while a menu or dialog
/// hides the mode, and at most once round the cycle.
fn switch_mode(
    read: ModeReader,
    target: &str,
    w: &Mutex<std::fs::File>,
    screen: &Mutex<vt100::Parser>,
    on_screen: &Mutex<Option<TerminalPrompt>>,
) {
    let current = || {
        read(
            &screen
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .screen()
                .contents(),
        )
    };
    let Some(start) = current() else { return };
    let mut now = start;
    for pressed in 0..6 {
        if now == target
            || (pressed > 0 && now == start)
            || on_screen
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_some()
        {
            return;
        }
        {
            let mut f = w.lock().unwrap_or_else(|p| p.into_inner());
            let _ = f.write_all(b"\x1b[Z");
            let _ = f.flush();
        }
        // Wait for the footer to redraw (and to change: it can lag the key).
        let pressed_at = std::time::Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(80));
            match current() {
                Some(m) if m != now => {
                    now = m;
                    break;
                }
                _ if pressed_at.elapsed() > Duration::from_millis(1200) => return,
                _ => {}
            }
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

    /// Real Claude Code 2.1.332 screens (100 columns), mid-reply and done.
    #[test]
    fn reads_the_reply_claude_is_writing() {
        let box_ = "────────────────────────────────────────────────────────────────────────────────────────────────────\n❯ \n────────────────────────────────────────────────────────────────────────────────────────────────────\n  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt\n";
        let mid = format!(
            " ▐▛███▛█   Claude Code v2.1.332\n\n❯ Without using any tools, write three paragraphs about lighthouses.\n\n\
⏺ Lighthouses trace their origins to antiquity, when builders raised towers to guide ships safely\n\
  into harbors. The most famous early example was the Pharos of Alexandria.\n\n\
  After the Roman era, lighthouse building declined in Europe, and many coastal signals relied on\n\
  open fires or bonfires kept by local communities. Medieval and early modern towers appeared in\n\n{box_}"
        );
        assert_eq!(
            claude_preview(&mid).as_deref(),
            Some(
                "Lighthouses trace their origins to antiquity, when builders raised towers to guide ships safely into harbors. The most famous early example was the Pharos of Alexandria.\n\nAfter the Roman era, lighthouse building declined in Europe, and many coastal signals relied on open fires or bonfires kept by local communities. Medieval and early modern towers appeared in"
            )
        );
        // Done: the spinner line ends the block.
        let done = mid.replace(
            box_,
            &format!("⏺ Ran 3 stop hooks\n  ⎿  Stop hook failed\n\n✻ Baked for 4s\n{box_}"),
        );
        assert_eq!(
            claude_preview(&done),
            None,
            "the last block is the hooks line"
        );
        let spinner = mid.replace(box_, &format!("\n✻ Baked for 4s · done\n{box_}"));
        assert!(
            claude_preview(&spinner)
                .unwrap()
                .ends_with("towers appeared in")
        );
        // Your next prompt (echoed with `❯`) isn't part of the reply before it.
        let prompt = format!(
            "⏺ Shared with your phone.\n\n❯ Without using any tools, write three paragraphs\n  about lighthouses.\n\n{box_}"
        );
        assert_eq!(
            claude_preview(&prompt),
            None,
            "that reply is the previous one"
        );
        // A tool call, not a reply.
        let tool = format!("⏺ Bash(cargo test)\n  ⎿  Running…\n\n{box_}");
        assert_eq!(claude_preview(&tool), None);
        // Claude 2.1.332 titles a running command with its description, and blinks
        // its `⏺`: neither state is a reply (the text before it isn't new).
        let starting = format!("⏺ Running 1 shell command…\n✻ Polishing…\n{box_}");
        assert_eq!(claude_preview(&starting), None);
        for bullet in ["⏺", " "] {
            let running = format!(
                "⏺ I'll run it now.\n\n{bullet} Sleeping 5 seconds then printing hi\n  ⎿  $ sleep 5 && echo hi\n\n✻ Polishing… (3s)\n{box_}"
            );
            assert_eq!(claude_preview(&running), None, "bullet {bullet:?}");
        }
        // Claude 2.1.332 puts a step's summary under the text before it, and blinks
        // its `⏺`: with it on or off, the text is finished and the step is newest.
        for bullet in ["⏺", " "] {
            let reading = format!(
                "⏺ I'll read the notes first, then sum up.\n{bullet} Reading 1 file…\n✢ Razzle-dazzling… (2s · thinking)\n{box_}"
            );
            assert_eq!(claude_preview(&reading), None, "bullet {bullet:?}");
            let listed =
                format!("⏺ That's part 1.\n\n{bullet} Listed 1 directory \n\n✻ Grooving…\n{box_}");
            assert_eq!(claude_preview(&listed), None, "bullet {bullet:?}");
        }
        // A command shown by its description while it runs, with its timer and no
        // details: the text above is finished.
        for bullet in ["⏺", " "] {
            for timer in ["1m 28s", "9s", "2s…"] {
                let waiting = format!(
                    "⏺ Fixes are in. Next I'm running a quick smoke test.\n\n{bullet} Waiting for the smoke test output · {timer}\n\n✳ Crunching… (14m 26s · ↓ 57.1k tokens)\n{box_}"
                );
                assert_eq!(claude_preview(&waiting), None, "{bullet:?} {timer}");
            }
        }
        let tagged = format!(
            "⏺ About 27% is the glial map. I'll explain once\n  they have reviewed them. ·\u{a0}summary\n\n✶ Crunching… (16m 8s)\n{box_}"
        );
        assert_eq!(
            claude_preview(&tagged).as_deref(),
            Some("About 27% is the glial map. I'll explain once they have reviewed them.")
        );
        assert!(timed_step("Running 1 shell command · 2s…"));
        assert!(!timed_step("It costs about · 5 dollars"));
        assert!(!timed_step("Done · 12 files"));
        // Reply text that starts like one is still text.
        let prose =
            format!("⏺ The plan is simple.\n  Reading the code first, I found two bugs.\n\n{box_}");
        assert_eq!(
            claude_preview(&prose).as_deref(),
            Some("The plan is simple. Reading the code first, I found two bugs.")
        );
        // A tip under the spinner isn't a tool step.
        let tip = format!(
            "⏺ The project is small.\n\n✽ Grooving… (10s · thinking)\n  ⎿  Tip: Run /ultrareview for a review\n\n{box_}"
        );
        assert_eq!(
            claude_preview(&tip).as_deref(),
            Some("The project is small.")
        );
        // Lists keep their lines.
        let list = format!("⏺ Two things:\n  - first\n  - second\n\n{box_}");
        assert_eq!(
            claude_preview(&list).as_deref(),
            Some("Two things:\n- first\n- second")
        );
        // Scrolled off the top: it starts with "…".
        let scrolled = format!("  the start is gone and\n  this goes on\n\n{box_}");
        assert_eq!(
            claude_preview(&scrolled).as_deref(),
            Some("…the start is gone and this goes on")
        );
    }

    /// A file being written whose `⏺ Write(…)` line has scrolled off is not a reply.
    #[test]
    fn a_tool_output_scrolled_up_is_no_preview() {
        let rule = "─".repeat(60);
        let box_ = format!("{rule}\n❯ \n{rule}\n  ⏵⏵ auto mode on · esc to interrupt");
        let screen = format!(
            "       1 \"\"\"§741 arm G: matched random-programme null for t\n       2 he Feng 2025 MERSCOPE result\n       3\n       4 Same tiles, covariates, NB fit\n      … +102 lines (~7 KB)\n\n✻ Applying final fixes… (3s)\n{box_}"
        );
        assert_eq!(claude_preview(&screen), None);
        // Folded output under reply-deep text is still a tool's.
        let screen = format!("  some output line\n  … +12 lines (ctrl+o to expand)\n\n{box_}");
        assert_eq!(claude_preview(&screen), None);
        // A long reply scrolled off keeps its preview.
        let screen = format!("  the middle of a long reply that\n  goes on here.\n\n{box_}");
        assert_eq!(
            claude_preview(&screen).as_deref(),
            Some("…the middle of a long reply that goes on here.")
        );
    }

    #[test]
    fn previews_keep_list_items_whole_and_tables_readable() {
        let body = |lines: &[&str]| lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        // A wrapped list item continues; a nested item starts a line.
        let list = body(&[
            "Main findings so far:",
            "1. The tissue carries a disease clock (ρ 0.83 in RR, with one animal",
            "   held out). The clock is driven by the interferon response.",
            "2. Two cell-level effects:",
            "   - Oligodendrocytes near lymphocytes carry an imprint, which",
            "     replicates in Kukanja et al.",
            "   - Endothelial collagen IV is a candidate mechanism.",
            "3.",
        ]);
        assert_eq!(
            reflow(list, false).as_deref(),
            Some(
                "Main findings so far:\n1. The tissue carries a disease clock (ρ 0.83 in RR, with one animal held out). The clock is driven by the interferon response.\n2. Two cell-level effects:\n   - Oligodendrocytes near lymphocytes carry an imprint, which replicates in Kukanja et al.\n   - Endothelial collagen IV is a candidate mechanism.\n3."
            )
        );
        // Box-drawn tables: no borders, a row per line.
        let table = body(&[
            "What's in it:",
            "┌──────────┬──────────────────┐",
            "│ Folder   │ Contents         │",
            "├──────────┼──────────────────┤",
            "│ runs/    │ 638 run folders  │",
            "└──────────┴──────────────────┘",
        ]);
        assert_eq!(
            reflow(table, false).as_deref(),
            Some("What's in it:\n| Folder | Contents |\n| runs/ | 638 run folders |")
        );
    }

    #[test]
    fn reads_the_status_line() {
        let rule = "─".repeat(40);
        let box_ = format!("{rule}\n❯ \n{rule}\n  ⏵⏵ auto mode on · esc to interrupt");
        let screen = |spinner: &str| {
            format!("⏺ Reading the notes.\n\n{spinner}\n  ⎿  Tip: Use /btw\n\n{box_}")
        };
        let st =
            claude_status(&screen("✢ Crunching… (18m 5s · ↓ 73.6k tokens · thinking)")).unwrap();
        assert_eq!(st.label, "Crunching…");
        assert_eq!(st.elapsed_secs, Some(18 * 60 + 5));
        assert_eq!(st.tokens.as_deref(), Some("↓ 73.6k tokens"));
        assert!(st.thinking);
        assert_eq!(st.note, None);
        // Wrapped onto a second line, with a note.
        let st = claude_status(&screen(
            "✳ Crunching… (running PreToolUse hooks… 2/3 · 17m 54s · \n↓ 73.1k tokens)",
        ))
        .unwrap();
        assert_eq!(st.elapsed_secs, Some(17 * 60 + 54));
        assert_eq!(st.tokens.as_deref(), Some("↓ 73.1k tokens"));
        assert_eq!(st.note.as_deref(), Some("running PreToolUse hooks… 2/3"));
        let st = claude_status(&screen("· Razzle-dazzling… (2s · thought for 1s)")).unwrap();
        assert_eq!(st.label, "Razzle-dazzling…");
        assert_eq!(
            (st.thinking, st.note.as_deref()),
            (false, Some("thought for 1s"))
        );
        assert_eq!(
            claude_status(&screen("✻ Pondering…")).unwrap().elapsed_secs,
            None
        );
        // Over: no status.
        assert_eq!(claude_status(&screen("✻ Crunched for 18m 9s")), None);
        assert_eq!(claude_status(&format!("⏺ Done.\n\n{box_}")), None);
        // A list item in a reply isn't a status line.
        assert_eq!(
            claude_status(&format!("⏺ Two:\n  * one thing…\n\n{box_}")),
            None
        );

        let codex =
            |status: &str| format!("• Reading.\n\n{status}\n\n› Ask Codex\n  gpt-5 · 80% left");
        let st = codex_status(&codex("• Working (1m 3s • esc to interrupt)")).unwrap();
        assert_eq!((st.label.as_str(), st.elapsed_secs), ("Working", Some(63)));
        assert_eq!(codex_status(&codex("─ Worked for 13s ─")), None);
    }

    #[test]
    fn a_long_preview_grows_instead_of_jumping() {
        let seen = "Intro paragraph that has scrolled away by now.\n\nThe main data isn't in the repo. The atlas sits on external volumes";
        let now = "…The main data isn't in the repo. The atlas sits on external volumes and the remote Mac.";
        assert_eq!(
            grow_preview(seen, now),
            "Intro paragraph that has scrolled away by now.\n\nThe main data isn't in the repo. The atlas sits on external volumes and the remote Mac."
        );
        // Nothing in common (another block): what the screen shows.
        assert_eq!(
            grow_preview("Something else entirely.", "…Brand new text here"),
            "…Brand new text here"
        );
        // Not scrolled: the screen has it all.
        assert_eq!(grow_preview("Old", "New reply"), "New reply");
    }

    /// Real Codex 0.172.0 screens (100 columns) while it writes an answer.
    #[test]
    fn reads_the_reply_codex_is_writing() {
        let head = [
            "  >_ OpenAI Codex (v0.172.0)",
            "",
            "› Without using any tools, write three paragraphs (about 80 words each) about the history of",
            "  lighthouses.",
            "",
            "",
        ]
        .join("\n");
        let foot = "\n› Ask Codex to do anything\n\n  GPT-6.2-Sol default · ~/cap · Write lighthouse history paragraphs\n  ← for agents · ? for shortcuts\n";
        let reply = [
            "• Some of the earliest lighthouses were fires kept burning on headlands or at harbor entrances. The",
            "  most famous ancient example was the Pharos of Alexandria.",
            "",
            "  As maritime trade expanded, builders took on the challenge of placing lights on reefs and rocks",
            "  far from shore. In 1759, John Smeaton completed a stone",
            "",
        ]
        .join("\n");
        let mid = format!("{head}{reply}{foot}");
        assert_eq!(
            codex_preview(&mid).as_deref(),
            Some(
                "Some of the earliest lighthouses were fires kept burning on headlands or at harbor entrances. The most famous ancient example was the Pharos of Alexandria.\n\nAs maritime trade expanded, builders took on the challenge of placing lights on reefs and rocks far from shore. In 1759, John Smeaton completed a stone"
            )
        );
        // Thinking: only the status line.
        let thinking = format!("{head}• Working (1s • esc to interrupt)\n{foot}");
        assert_eq!(codex_preview(&thinking), None);
        // With the status line under the reply, still the reply.
        let both = format!("{head}{reply}\n• Working (5s • esc to interrupt)\n{foot}");
        assert!(codex_preview(&both).unwrap().ends_with("completed a stone"));
        // The turn is over.
        let done = format!("{head}{reply}\n─ Worked for 13s • 13:50 ─────────\n{foot}");
        assert_eq!(codex_preview(&done), None);
        // A tool step, not a reply.
        let tool = format!("{head}• Ran cargo test\n  └ running 12 tests\n    test ok\n{foot}");
        assert_eq!(codex_preview(&tool), None);
        let explored = format!("{head}• Explored\n  └ Read pty.rs\n{foot}");
        assert_eq!(codex_preview(&explored), None);
        // Scrolled off the top: it starts with "…".
        // Short screens pin the prompt to the top.
        let pinned = format!(
            "› Without using any tools, write three paragraphs about the history of light…\n  far from shore.\n{foot}"
        );
        assert_eq!(codex_preview(&pinned).as_deref(), Some("…far from shore."));
        // Codex's blank lines may be a single space.
        assert_eq!(
            codex_preview(&mid.replace("\n\n  As", "\n \n  As")),
            codex_preview(&mid)
        );
        let scrolled =
            format!("  far from shore. In 1759, John Smeaton\n  completed a stone{foot}");
        assert_eq!(
            codex_preview(&scrolled).as_deref(),
            Some("…far from shore. In 1759, John Smeaton completed a stone")
        );
        // Commentary between tool steps is text too.
        let note = format!("{head}• Ran ls\n  └ a b\n\n• I'll read the parser next.\n{foot}");
        assert_eq!(
            codex_preview(&note).as_deref(),
            Some("I'll read the parser next.")
        );
    }

    #[test]
    fn reads_claudes_mode_from_its_footer() {
        // As Claude Code 2.1.332 draws them, cycling with Shift+Tab.
        let screen =
            |footer: &str| format!("╭───╮\n│ > Try \"fix lint errors\"\n╰───╯\n  {footer}\n\n");
        for (footer, mode) in [
            (
                "⏸ manual mode on · ? for shortcuts · ← for agents",
                "default",
            ),
            (
                "⏵⏵ accept edits on (shift+tab to cycle) · ← for agents",
                "acceptEdits",
            ),
            ("⏸ plan mode on (shift+tab to cycle)", "plan"),
            ("⏵⏵ auto mode on (shift+tab to cycle)", "auto"),
            (
                "⏵⏵ bypass permissions on (shift+tab to cycle)",
                "bypassPermissions",
            ),
            // Older versions: nothing in the default mode but the shortcuts hint.
            ("? for shortcuts", "default"),
        ] {
            assert_eq!(claude_mode(&screen(footer)), Some(mode), "{footer}");
        }
        // A dialog hides the footer: unknown, not "default".
        assert_eq!(
            claude_mode("Do you want to proceed?\n❯ 1. Yes\n  2. No\n"),
            None
        );
        // The conversation mentioning a mode doesn't count.
        let talk = "● I'll switch to plan mode on your say-so.\n".repeat(3) + "\n";
        assert_eq!(claude_mode(&talk), None);
    }

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
        // Terminal replies and reports aren't typing.
        let mut l = LineState::default();
        l.feed(b"\x1b]11;rgb:0000/0000/0000\x1b\\\x1b]10;rgb:ffff/ffff/ffff\x07\x1b[?62;22c\x1b[12;1R\x1b[?5u\x1b[I\x1b[<35;10;5M");
        assert!(
            !l.dirty(),
            "colour/DA/cursor/kitty replies, focus and mouse"
        );
        // Erasing the line with backspaces (what the /styr catcher does) empties it.
        let mut l = LineState::default();
        l.feed(b"/styr");
        assert!(l.dirty());
        l.feed(&[0x7f; 5]);
        assert!(!l.dirty(), "backspaced to an empty line");
        l.feed("é".as_bytes());
        l.feed(&[0x7f]);
        assert!(!l.dirty(), "one backspace removes a multi-byte character");
        // kitty-encoded keys still count.
        l.feed(b"\x1b[104u");
        assert!(l.dirty());
        l.feed(b"\x1b[13u");
        assert!(!l.dirty(), "kitty Enter submits");
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

    #[test]
    fn styr_is_caught_and_erased_in_codex() {
        use styr_core::adapter::StyrCommand;
        let mut c = StyrCatcher::default();
        let (out, cmd) = c.feed(b"/styr\r");
        assert_eq!(cmd, Some(StyrCommand::On));
        assert_eq!(
            out,
            [b"/styr".as_slice(), &[0x7f; 5]].concat(),
            "typed, then erased; no Enter"
        );
        // Typed in pieces, with a correction.
        let mut c = StyrCatcher::default();
        assert_eq!(c.feed(b"/styr ofx").1, None);
        let (out, cmd) = c.feed(&[0x7f, b'f', b'\r']);
        assert_eq!(cmd, Some(StyrCommand::Off));
        assert_eq!(out, [&[0x7f, b'f'][..], &[0x7f; 9]].concat());
        // Everything else passes through untouched.
        let mut c = StyrCatcher::default();
        assert_eq!(
            c.feed(b"fix /styr please\r"),
            (b"fix /styr please\r".to_vec(), None)
        );
        // A line edited with arrow keys isn't caught (we can't know what's in the box).
        let mut c = StyrCatcher::default();
        assert_eq!(c.feed(b"/sty\x1b[Dr\r").1, None);
        assert_eq!(
            c.feed(b"/styr\r").1,
            Some(StyrCommand::On),
            "the next line is clean again"
        );
        // Focus and mouse reports (Codex turns both on) don't disqualify a line.
        let mut c = StyrCatcher::default();
        let (out, cmd) = c.feed(b"\x1b[I\x1b[<35;10;5M/st\x1b[<35;11;5Myr\r");
        assert_eq!(cmd, Some(StyrCommand::On));
        assert!(!out.contains(&b'\r'), "Enter swallowed");
        assert!(out.starts_with(b"\x1b[I"), "reports still reach the agent");
        // kitty keyboard protocol: Enter as CSI 13 u, letters as CSI u with text.
        let mut c = StyrCatcher::default();
        let (out, cmd) = c.feed(b"/styr\x1b[13u");
        assert_eq!(
            (cmd, out),
            (
                Some(StyrCommand::On),
                [b"/styr".as_slice(), &[0x7f; 5]].concat()
            )
        );
        let mut c = StyrCatcher::default();
        assert_eq!(
            c.feed(b"\x1b[47u\x1b[115u\x1b[116u\x1b[121u\x1b[114u").1,
            None
        );
        assert_eq!(c.feed(b"\x1b[13;1u").1, Some(StyrCommand::On));
        // Split across reads.
        let mut c = StyrCatcher::default();
        assert_eq!(c.feed(b"/styr\x1b[1").1, None);
        assert_eq!(c.feed(b"3u").1, Some(StyrCommand::On));
        // Replies to Codex's startup queries come in on stdin before any typing.
        let mut c = StyrCatcher::default();
        let replies = b"\x1b[?62;22c\x1b[12;1R\x1b]10;rgb:ffff/ffff/ffff\x1b\\\x1b]11;rgb:0000/0000/0000\x07\x1b[?1u\x1b[?1007;2$y\x1b[>1;10;0c";
        let (out, cmd) = c.feed(&[replies.as_slice(), b"/styr\r"].concat());
        assert_eq!(cmd, Some(StyrCommand::On));
        assert!(out.starts_with(replies), "replies still reach the agent");
        // Shift+Enter (a newline in the box) and arrows still disqualify.
        let mut c = StyrCatcher::default();
        assert_eq!(c.feed(b"/styr\x1b[13;2u").1, None);
        let mut c = StyrCatcher::default();
        assert_eq!(c.feed(b"/sty\x1b[Dr\x1b[13u").1, None);
    }

    fn codex_screen(file: &str) -> String {
        let b = std::fs::read(format!(
            "{}/tests/fixtures/{file}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let mut p = vt100::Parser::new(nz(40), nz(120), 0);
        p.process(&b);
        p.screen().contents()
    }

    #[test]
    fn codex_approval_menu_is_read_off_the_screen() {
        let p = detect_codex_prompt(&codex_screen("codex_approval.bin")).expect("menu");
        assert_eq!(p.title, "Would you like to run the following command?");
        assert!(p.detail.contains("$ touch menu.txt"), "{}", p.detail);
        assert!(
            p.detail
                .contains("Reason: Do you want to allow creating menu.txt")
        );
        assert_eq!(
            p.options,
            vec![
                "Yes, proceed",
                "Yes, and don't ask again for commands that start with `touch menu.txt`",
                "No, and tell Codex what to do differently",
            ]
        );
        assert_eq!(p.selected, 0);
        let moved = detect_codex_prompt(&codex_screen("codex_approval.down.bin")).unwrap();
        assert_eq!(moved.selected, 1, "the › marker follows the arrow keys");
        // The user's own prompt line (also starting with ›) isn't an option or a title.
        assert!(!p.title.contains("Run the shell command"));
    }

    #[test]
    fn codex_startup_menus_count_too() {
        // Shown at startup when hooks are new; a phone prompt must not be typed into it.
        let s = "› Ask Codex to do anything\n\n  Hooks need review\n  6 hooks are new or changed.\n  Hooks can run outside the sandbox after you trust them.\n\n› 1. Review hooks\n  2. Trust all and continue\n  3. Continue without trusting (hooks won't run)\n\n  enter confirm · esc skip\n";
        let p = detect_codex_prompt(s).expect("menu");
        assert_eq!(p.title, "Hooks need review");
        assert_eq!(
            p.options,
            vec![
                "Review hooks",
                "Trust all and continue",
                "Continue without trusting (hooks won't run)"
            ]
        );
        assert!(p.detail.contains("6 hooks are new or changed."));
    }

    #[test]
    fn codex_options_wrapped_in_a_narrow_pane() {
        let s = "• Running curl -sI https://example.com\n\n  Would you like to run the following command?\n\n  Reason: Do you want to allow network access to\n  example.com?\n\n  $ curl -sI https://example.com\n\n› 1. Yes, proceed (y)\n  2. Yes, and don't ask again for commands that\n     start with `curl -sI https://example.com` (p)\n  3. No, and tell Codex what to do differently\n     (esc)\n\n  Press enter to confirm or esc to cancel\n";
        let p = detect_codex_prompt(s).expect("menu");
        assert_eq!(p.title, "Would you like to run the following command?");
        assert_eq!(
            p.options,
            vec![
                "Yes, proceed",
                "Yes, and don't ask again for commands that start with `curl -sI https://example.com`",
                "No, and tell Codex what to do differently",
            ]
        );
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn no_codex_menu_without_the_footer() {
        let s = "  Would you like to run the following command?\n› 1. Yes, proceed (y)\n  2. No (esc)\n";
        assert_eq!(detect_codex_prompt(s), None);
        assert_eq!(detect_codex_prompt("› Ask Codex to do anything"), None);
    }

    #[test]
    fn codex_trust_folder_screen_is_a_menu() {
        // Codex (Nov 2026) on a folder it hasn't seen, as `styr codex` captured it.
        let s = " \n  Folder access\n  /Users/me/work/projects/lesionSegmenter\n \n  Trust this folder? Codex can read, edit, and run files here, subject to your permission settings. Folder settings can run code\n  automatically, even without a model request. Continue only if you trust these files. Your trust decision will be saved.\n \n› 1. Trust and continue\n  2. Back to Agent Command Center\n \n  enter continue · esc back\n \n \n";
        let p = detect_codex_prompt(s).expect("the trust question is a menu");
        assert_eq!(p.title, "Trust this folder?");
        assert_eq!(
            p.options,
            ["Trust and continue", "Back to Agent Command Center"]
        );
        assert_eq!(p.selected, 0);
        assert!(p.detail.contains("lesionSegmenter"), "{}", p.detail);
        assert!(p.detail.starts_with("Codex can read"), "{}", p.detail);
        // So a message from the phone waits instead of being typed into it.
        assert!(!codex_ready(s));
    }

    #[test]
    fn codex_is_ready_only_at_its_input_box() {
        assert!(
            !codex_ready(&codex_screen("codex_approval.bin")),
            "a menu is open"
        );
        let idle = "› Ask Codex to do anything\n\n  GPT-5.6-Luna default · ~/work\n  ← for agents · ? for shortcuts\n";
        assert!(codex_ready(idle));
        assert!(!codex_ready(
            "  Do you trust the contents of this directory?\n› 1. Yes, continue\n  2. No, quit\n\n  Press enter to continue\n"
        ));
    }

    /// Real Codex 0.172.0 approval menus (tests/fixtures/codex_menus/<kind>_<cols>.bin),
    /// rendered at the width they were captured at.
    #[test]
    fn real_codex_menus_at_every_width() {
        let dir = format!("{}/tests/fixtures/codex_menus", env!("CARGO_MANIFEST_DIR"));
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let cols: u16 = name.rsplit('_').next().unwrap().parse().unwrap();
            let mut p = vt100::Parser::new(nz(40), nz(cols), 0);
            p.process(&std::fs::read(&path).unwrap());
            let screen = p.screen().contents();
            let m = detect_codex_prompt(&screen)
                .unwrap_or_else(|| panic!("{name}: no menu found in\n{screen}"));
            eprintln!(
                "{name}: {:?} / {:?} / options {:?}",
                m.title, m.detail, m.options
            );
            assert!(m.title.ends_with('?'), "{name}: title {:?}", m.title);
            assert!(m.options.len() >= 2, "{name}: options {:?}", m.options);
            assert!(
                m.options.iter().any(|o| o.starts_with("Yes")),
                "{name}: {:?}",
                m.options
            );
            assert!(
                m.options.iter().any(|o| o.starts_with("No")),
                "{name}: {:?}",
                m.options
            );
            assert!(
                m.options
                    .iter()
                    .all(|o| !o.ends_with(')') || o.contains('`')),
                "{name}: shortcut left in {:?}",
                m.options
            );
            assert!(
                m.options.iter().all(|o| !o.contains("- ")),
                "{name}: a word broken at a hyphen was re-joined with a space: {:?}",
                m.options
            );
            assert!(
                !codex_ready(&screen),
                "{name}: a menu is open, not the input box"
            );
            seen += 1;
        }
        assert!(seen >= 4);
    }
}

#[cfg(test)]
mod wrapper_tests {
    use super::*;
    use std::time::Instant;

    fn parser(text: &str) -> vt100::Parser {
        let mut p = vt100::Parser::new(nz(20), nz(80), 0);
        p.process(text.replace('\n', "\r\n").as_bytes());
        p
    }

    fn redraw(p: &Mutex<vt100::Parser>, text: &str) {
        lock(p).process(format!("\x1b[2J\x1b[H{}", text.replace('\n', "\r\n")).as_bytes());
    }

    const RULE: &str = "────────────────────────────────────────";

    #[test]
    fn the_input_box_says_whether_it_holds_text() {
        // As Claude Code draws it: `❯`, a no-break space, the placeholder dimmed.
        let claude = |input: &str| {
            parser(&format!(
                "⏺ Done.\n\n{RULE}\n\x1b[39m❯\u{a0}{input}\x1b[22m\n{RULE}\n  ? for shortcuts"
            ))
        };
        let p = claude("\x1b[2mTry \"edit <filepath> to…\"");
        assert_eq!(input_box_text(p.screen(), false), Some(false));
        let p = claude("fix the parser");
        assert_eq!(input_box_text(p.screen(), false), Some(true));
        let p = claude("");
        assert_eq!(input_box_text(p.screen(), false), Some(false));
        // Codex: the last `›` line, placeholder dimmed.
        let codex = |input: &str| {
            parser(&format!(
                "› an earlier prompt\n\n• Done.\n\n\x1b[1m›\x1b[22m {input}\x1b[22m\n\n  ? for shortcuts"
            ))
        };
        let p = codex("\x1b[2mAsk Codex to do anything");
        assert_eq!(input_box_text(p.screen(), true), Some(false));
        let p = codex("half a thought");
        assert_eq!(input_box_text(p.screen(), true), Some(true));
        // The placeholder, even when it isn't drawn dim.
        let p = codex("Ask Codex to do anything");
        assert_eq!(input_box_text(p.screen(), true), Some(false));
        // The real recording's input line.
        let mut p = vt100::Parser::new(nz(40), nz(120), 0);
        p.process(b"\x1b[36;1H\x1b[1m\xe2\x80\xba\x1b[36;3H\x1b[22m\x1b[2mAsk Codex to do anything\x1b[38;1H\x1b[22m");
        assert_eq!(input_box_text(p.screen(), true), Some(false));
        // No box: unknown.
        let p = parser("just some output\n");
        assert_eq!(input_box_text(p.screen(), true), None);
        // A history line starting with `❯` isn't Claude's box without its rule.
        let p = parser("❯ an old prompt\n\n⏺ Reply.\n");
        assert_eq!(input_box_text(p.screen(), false), None);
    }

    #[test]
    fn a_line_is_typing_only_while_it_can_be() {
        let t0 = Instant::now();
        let later = |s: u64| t0 + Duration::from_secs(s);
        let l = LineState::default();
        assert!(!l.typing(None, t0) && !l.typing(Some(false), t0));
        // A Codex menu answered with `y`, Esc Esc, Ctrl-W: keys counted, line empty.
        for keys in [b"y".as_slice(), b"1", b"abc\x1b\x1b", b"abc\x17"] {
            let mut l = LineState::default();
            l.feed(keys);
            assert!(l.dirty(), "{keys:?}");
            let at = l.last_key.unwrap();
            // The screen shows an empty box: not typing once it has had time to draw.
            assert!(l.typing(Some(false), at), "{keys:?} just typed");
            assert!(
                !l.typing(Some(false), at + Duration::from_secs(2)),
                "{keys:?}"
            );
            // Can't read the box: the count goes stale.
            assert!(l.typing(None, at + Duration::from_secs(10)), "{keys:?}");
            assert!(!l.typing(None, at + Duration::from_secs(31)), "{keys:?}");
        }
        // Text in the box holds, however long ago it was typed.
        let mut l = LineState::default();
        l.feed(b"half a thou");
        assert!(l.typing(Some(true), later(3600)));
        // An arrow is a key, but no character.
        let mut l = LineState::default();
        l.feed(b"\x1b[A");
        assert!(!l.dirty() && l.last_key.is_some());
        assert!(!l.typing(Some(true), Instant::now()));
        // Terminal replies aren't keys.
        let mut l = LineState::default();
        l.feed(b"\x1b[I\x1b]11;rgb:0000/0000/0000\x07\x1b[12;1R");
        assert_eq!(l.last_key, None);
    }

    #[test]
    fn keys_typed_into_a_menu_dont_count() {
        let mut l = LineState::default();
        l.feed(b"draft");
        l.menu(true);
        l.feed(b"y");
        l.menu(true);
        l.menu(false);
        assert_eq!(l.chars, 5, "the half-typed line is back as it was");
        let mut l = LineState::default();
        l.menu(true);
        l.feed(b"1");
        l.menu(false);
        assert!(!l.dirty(), "the menu's answer isn't a line");
        l.menu(false);
        assert!(!l.dirty());
    }

    fn menu(title: &str) -> TerminalPrompt {
        TerminalPrompt {
            title: title.into(),
            detail: String::new(),
            options: vec!["Yes".into(), "No".into()],
            selected: 0,
        }
    }

    #[test]
    fn menus_are_announced_until_the_daemon_has_them() {
        let t0 = Instant::now();
        let every = Duration::from_secs(5);
        let mut a = Announcer::default();
        assert!(!a.due(&None, t0, every), "nothing to say yet");
        let m = Some(menu("Run it?"));
        assert!(a.due(&m, t0, every));
        // The report failed (daemon restarting): still due.
        assert!(a.due(&m, t0 + Duration::from_secs(1), every));
        a.sent(m.clone(), t0);
        assert!(!a.due(&m, t0 + Duration::from_secs(1), every));
        assert!(a.due(&m, t0 + every, every), "again while it's open");
        assert!(
            a.due(&None, t0 + Duration::from_secs(1), every),
            "it closed"
        );
        a.sent(None, t0);
        assert!(!a.due(&None, t0 + Duration::from_secs(60), every));
        assert!(delivered(&Ok(Response::Ok)));
        assert!(delivered(&Err(IpcError::Remote("no".into()))));
        assert!(!delivered(&Err(IpcError::NotRunning("/x".into()))));
    }

    #[test]
    fn a_scrolled_preview_only_continues_a_reply() {
        let t0 = Instant::now();
        let mut p = Previews::default();
        // The wrapped tail of your own long prompt, start scrolled off: no preview.
        assert_eq!(p.next(Some("…about lighthouses.".into()), t0), None);
        // A reply that was shown goes on past a screenful.
        let mut p = Previews::default();
        let head = "Lighthouses trace their origins to antiquity, when builders raised towers";
        assert_eq!(p.next(Some(head.into()), t0).as_deref(), Some(head));
        let tail = "…when builders raised towers to guide ships.";
        let grown = p.next(Some(tail.into()), t0 + Duration::from_millis(300));
        assert_eq!(
            grown.as_deref(),
            Some(
                "Lighthouses trace their origins to antiquity, when builders raised towers to guide ships."
            )
        );
        // A frame read as nothing in between doesn't break it.
        assert_eq!(p.next(None, t0 + Duration::from_millis(600)), None);
        assert!(
            p.next(Some(tail.into()), t0 + Duration::from_secs(1))
                .is_some()
        );
        // Long after the last one: a scrolled screen is no reply.
        assert_eq!(p.next(None, t0 + Duration::from_secs(2)), None);
        assert_eq!(p.next(Some(tail.into()), t0 + Duration::from_secs(9)), None);
    }

    #[test]
    fn replies_that_start_like_a_tool_step_are_replies() {
        let box_ = format!("{RULE}\n❯ \n{RULE}\n  ⏵⏵ auto mode on");
        for first in [
            "Updated the README with the new flags.",
            "Read the error carefully: it's a missing feature flag.",
            "Running the tests locally works; CI differs.",
            "Interrupted runs leave a lock file behind.",
        ] {
            let s = format!("⏺ {first}\n\n{box_}");
            assert_eq!(claude_preview(&s).as_deref(), Some(first));
        }
        for tool in [
            "Read 3 files (ctrl+o to expand)",
            "Updated 2 files",
            "Running 1 shell command…",
            "Interrupted · What should Claude do instead?",
        ] {
            let s = format!("⏺ {tool}\n\n{box_}");
            assert_eq!(claude_preview(&s), None, "{tool}");
        }
    }

    #[test]
    fn codex_tool_output_scrolled_up_is_no_preview() {
        let foot = "\n› Ask Codex to do anything\n\n  ← for agents · ? for shortcuts\n";
        // A diff whose `• Edited` line scrolled off.
        let diff =
            format!("    11    fn main() {{\n    12 +  let a = 1;\n    13 -  let b = 2;\n{foot}");
        assert_eq!(codex_preview(&diff), None);
        // A command's output, deeper than reply text.
        let out = format!("    test a ... ok\n    test b ... ok\n{foot}");
        assert_eq!(codex_preview(&out), None);
        // A reply scrolled off mid-list: its first whole line on.
        let list = format!(
            "    replicates in the second cohort.\n  - The other effect is smaller.\n  And that's it.\n{foot}"
        );
        assert_eq!(
            codex_preview(&list).as_deref(),
            Some("…- The other effect is smaller. And that's it.")
        );
        assert!(!diff_line("  12 apples were counted"));
        assert!(diff_line("    7 -    x"));
    }

    const CODEX_MENU: &str = "  Would you like to run the following command?\n\n  $ touch menu.txt\n\n› 1. Yes, proceed (y)\n  2. No, and tell Codex what to do differently (esc)\n\n  Press enter to confirm or esc to cancel\n";
    const CODEX_IDLE: &str = "• Done.\n\n› \x1b[2mAsk Codex to do anything\x1b[22m\n\n  ← for agents · ? for shortcuts\n";

    fn ctx(screen: &str) -> Ctx<Vec<u8>> {
        Ctx {
            w: Arc::new(Mutex::new(Vec::new())),
            line: Arc::default(),
            done: Arc::default(),
            on_screen: Arc::default(),
            reported: Arc::default(),
            screen: Arc::new(Mutex::new(parser(screen))),
            detector: Some(detect_codex_prompt),
            codex: true,
        }
    }

    fn written(c: &Ctx<Vec<u8>>) -> Vec<u8> {
        lock(&c.w).clone()
    }

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_held_prompt_doesnt_stop_the_menu_being_answered() {
        // A menu is open: the prompt waits in the typer, and the phone's answer to
        // the menu still gets through (it used to wait behind the prompt for good).
        let c = ctx(CODEX_MENU);
        *lock(&c.reported) = detect_codex_prompt(&c.contents());
        let (tx, rx) = std::sync::mpsc::channel();
        let typer = {
            let c = c.clone();
            std::thread::spawn(move || typer_loop(&c, rx))
        };
        tx.send("first".into()).unwrap();
        tx.send("second".into()).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(written(&c), b"", "nothing typed into the menu");
        assert!(c.select_option(0));
        assert_eq!(written(&c), b"\r");
        redraw(&c.screen, CODEX_IDLE);
        wait_for("both prompts", || written(&c).ends_with(b"second\r"));
        assert_eq!(written(&c), b"\rfirst\rsecond\r", "in order");
        c.done.store(true, Ordering::Relaxed);
        typer.join().unwrap();
    }

    #[test]
    fn a_prompt_waits_for_your_line_and_takes_the_keyboard_through_enter() {
        let c = ctx(CODEX_IDLE);
        lock(&c.line).feed(b"draft");
        // The box shows what you typed.
        redraw(
            &c.screen,
            &CODEX_IDLE.replace("\x1b[2mAsk Codex to do anything", "draft"),
        );
        let typer = {
            let c = c.clone();
            std::thread::spawn(move || c.type_prompt("from the phone"))
        };
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(written(&c), b"");
        // Sent: the line is empty again.
        lock(&c.line).feed(b"\r");
        redraw(&c.screen, CODEX_IDLE);
        // Your next key can't land between the prompt and its Enter: whenever the
        // terminal is free with the prompt in it, the Enter is there too.
        wait_for("the prompt", || !written(&c).is_empty());
        lock(&c.w).extend_from_slice(b"x");
        assert_eq!(written(&c), b"from the phone\rx");
        typer.join().unwrap();
    }

    #[test]
    fn a_menu_answer_needs_the_menu_the_phone_saw() {
        // Never reported: refused.
        let c = ctx(CODEX_MENU);
        assert!(!c.select_option(0));
        // Another menu on screen than the one reported: refused.
        *lock(&c.reported) = Some(menu("Something else?"));
        assert!(!c.select_option(0));
        assert_eq!(written(&c), b"");
        // The same menu: arrows to the option, Enter once it's highlighted.
        *lock(&c.reported) = detect_codex_prompt(CODEX_MENU);
        let mover = {
            let screen = c.screen.clone();
            std::thread::spawn(move || {
                // The agent moves its highlight a moment after the arrow.
                std::thread::sleep(Duration::from_millis(300));
                let moved = CODEX_MENU
                    .replace("› 1. Yes", "  1. Yes")
                    .replace("  2. No", "› 2. No");
                redraw(&screen, &moved);
            })
        };
        assert!(c.select_option(1));
        mover.join().unwrap();
        assert_eq!(written(&c), b"\x1b[B\r");
        // Out of range: refused.
        assert!(!c.select_option(5));
        // The highlight never gets there: no Enter.
        let c = ctx(CODEX_MENU);
        *lock(&c.reported) = detect_codex_prompt(CODEX_MENU);
        assert!(!c.select_option(1));
        assert_eq!(written(&c), b"\x1b[B");
    }
}

#[cfg(test)]
mod styled_tests {
    use super::*;

    fn screen(bytes: &str) -> vt100::Parser {
        let n = |v| std::num::NonZeroU16::new(v).unwrap();
        let mut p = vt100::Parser::new(n(12), n(90), 0);
        p.process(bytes.as_bytes());
        p
    }

    // As Claude Code draws it: a white ⏺, code in a colour of its own, bold and
    // italic in the default colour, then its input box.
    const CODE: &str = "\x1b[38;2;177;185;249m";
    const WHITE: &str = "\x1b[38;2;255;255;255m";
    const GRAY: &str = "\x1b[38;2;136;136;136m";

    fn reply() -> String {
        let rule = format!("{GRAY}{}\x1b[39m", "─".repeat(60));
        [
            format!("{WHITE}⏺\x1b[39m First check {CODE}git status\x1b[39m so you know a \x1b[1mclean baseline\x1b[22m."),
            String::new(),
            "  1. \x1b[1mRead\x1b[22m the failure output.".into(),
            "  2. \x1b[1mIsolate\x1b[22m the \x1b[3mlast\x1b[23m change.".into(),
            String::new(),
            rule.clone(),
            "❯ ".into(),
            rule,
        ]
        .join("\r\n")
    }

    #[test]
    fn a_reply_keeps_its_formatting() {
        let p = screen(&reply());
        assert_eq!(
            styled_preview(claude_preview, p.screen()).unwrap(),
            "First check `git status` so you know a **clean baseline**.\n\n1. **Read** the failure output.\n2. **Isolate** the *last* change."
        );
        // Plain, as before.
        assert_eq!(
            claude_preview(&screen_lines(p.screen(), false)).unwrap(),
            "First check git status so you know a clean baseline.\n\n1. Read the failure output.\n2. Isolate the last change."
        );
    }

    #[test]
    fn a_line_all_in_one_colour_is_text_not_code() {
        let p = screen(&format!(
            "{WHITE}⏺ Everything here is white text.\x1b[39m\r\n"
        ));
        assert_eq!(
            screen_lines(p.screen(), true).lines().next().unwrap(),
            "⏺ Everything here is white text."
        );
    }
}
