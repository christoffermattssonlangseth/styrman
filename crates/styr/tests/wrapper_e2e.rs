//! End to end: the real `styr codex` terminal wrapper in a real pseudo-terminal,
//! around a fake Codex that behaves like the real one where it matters (it turns on
//! focus/mouse/kitty reporting, queries the terminal, calls the hooks and shows its
//! input box). The test plays the terminal and the user: it answers the queries,
//! sends focus and mouse reports, types `/styr` key by key, and sends a message from
//! the "phone". Today's three typing bugs all lived exactly here.

use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use styr_core::ids::AgentKind;
use styr_core::ipc::{Request, Response};
use styr_testkit::TestDaemon;

const STYR: &str = env!("CARGO_BIN_EXE_styr");

/// The fake Codex: hooks run through the real `styr hook`, lines it receives go to $OUT.
const FAKE_CODEX: &str = r#"#!/usr/bin/env -S python3 -I
# A fake Codex that reads keys like a real TUI: raw mode, escape sequences (replies,
# focus, mouse) understood and ignored, backspace edits, Enter submits.
import json, os, subprocess, sys, termios, tty
def hook(event, **extra):
    p = {"session_id": "fx1", "cwd": os.getcwd(), "hook_event_name": event, "model": "m",
         "permission_mode": "default", "transcript_path": None, **extra}
    subprocess.run([os.environ["STYR_BIN"], "hook", "codex", event], input=json.dumps(p).encode(),
                   stdout=subprocess.DEVNULL)
def out(s):
    sys.stdout.write(s); sys.stdout.flush()
fd = sys.stdin.fileno()
tty.setraw(fd)
# Like Codex: focus, all-motion mouse and kitty keyboard reporting, then terminal queries.
out("\x1b[?1004h\x1b[?1003h\x1b[?1006h\x1b[>5u\x1b[c\x1b[6n\x1b]10;?\x1b\\\x1b]11;?\x1b\\")
hook("SessionStart", source="startup")
box = "\r\n\u203a Ask Codex to do anything\r\n\r\n  \u2190 for agents \u00b7 ? for shortcuts\r\n"
out(box)
line, esc = "", None
while True:
    b = os.read(fd, 1)
    if not b:
        break
    c = b[0]
    if esc is not None:
        esc += b
        if len(esc) == 2 and esc[1:] not in (b"[", b"]", b"O"):
            esc = None
        elif esc[1:2] == b"]" and (c == 7 or esc.endswith(b"\x1b\\")):
            esc = None
        elif esc[1:2] == b"[" and len(esc) > 2 and 0x40 <= c <= 0x7e:
            esc = None
        elif esc[1:2] == b"O" and len(esc) == 3:
            esc = None
        continue
    if c == 0x1b:
        esc = b
    elif c in (0x7f, 0x08):
        line = line[:-1]
    elif c == 0x03:
        break
    elif c in (13, 10):
        if line.strip():
            with open(os.environ["OUT"], "a") as f:
                f.write(line + "\n")
            hook("UserPromptSubmit", turn_id="t", prompt=line)
            hook("Stop", turn_id="t", stop_hook_active=False, last_assistant_message="ok")
            out("\r\n\u2022 ok\r\n" + box)
        line = ""
    elif c >= 0x20:
        line += chr(c)
"#;

struct Pty {
    master: std::fs::File,
    output: Vec<u8>,
    answered: Vec<&'static [u8]>,
}

impl Pty {
    /// Read what the wrapper shows, answering terminal queries like a real terminal.
    fn pump(&mut self, secs: f64) {
        let end = Instant::now() + Duration::from_secs_f64(secs);
        let mut buf = [0u8; 65536];
        while Instant::now() < end {
            match self.master.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => self.output.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(_) => break,
            }
            for (query, reply) in [
                (&b"\x1b[c"[..], &b"\x1b[?62;22c"[..]),
                (b"\x1b[6n", b"\x1b[12;1R"),
                (b"\x1b]10;?", b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
                (b"\x1b]11;?", b"\x1b]11;rgb:0000/0000/0000\x07"),
            ] {
                if !self.answered.contains(&query)
                    && self.output.windows(query.len()).any(|w| w == query)
                {
                    self.answered.push(query);
                    self.master.write_all(reply).unwrap();
                }
            }
        }
    }

    fn key(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
        self.pump(0.05);
    }
}

fn open_pty() -> (std::fs::File, OwnedFd) {
    let (mut master, mut slave) = (0, 0);
    let mut ws = libc::winsize {
        ws_row: 40,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty fills two fresh descriptors we take ownership of.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut ws,
        )
    };
    assert_eq!(rc, 0, "openpty");
    unsafe {
        let flags = libc::fcntl(master, libc::F_GETFL);
        libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
        (
            std::fs::File::from_raw_fd(master),
            OwnedFd::from_raw_fd(slave),
        )
    }
}

fn wait_for(what: &str, mut ok: impl FnMut(&Pty) -> bool, pty: &mut Pty) {
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        if ok(pty) {
            return;
        }
        pty.pump(0.2);
    }
    panic!(
        "timed out waiting for {what}; terminal showed:\n{}",
        String::from_utf8_lossy(&pty.output)
    );
}

#[test]
fn styr_codex_catches_styr_and_types_phone_messages_in_a_real_terminal() {
    let d = TestDaemon::start();
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake = bin.join("codex");
    std::fs::write(&fake, FAKE_CODEX).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = tmp.path().join("received.txt");
    let work = tmp.path().join("repo");
    std::fs::create_dir(&work).unwrap();

    let (master, slave) = open_pty();
    let stdio = || Stdio::from(slave.try_clone().unwrap());
    let mut child = Command::new(STYR)
        .arg("codex")
        .current_dir(&work)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", tmp.path())
        .env("STYR_HOME", &d.paths.data_dir)
        .env("STYR_SOCKET", &d.paths.socket)
        .env("STYR_BIN", STYR)
        .env("OUT", &out)
        .env_remove("STYR_WRAPPED")
        .env_remove("CMUX_CODEX_WRAPPER_SHIM")
        .env_remove("CMUX_CODEX_WRAPPER_SHIM_ROOT")
        .stdin(stdio())
        .stdout(stdio())
        .stderr(stdio())
        // Its own process group, so cleanup also stops the fake Codex underneath.
        .process_group(0)
        .spawn()
        .expect("styr codex");
    let _cleanup = KillGroup(child.id() as i32);
    drop(slave);
    let mut pty = Pty {
        master,
        output: Vec::new(),
        answered: Vec::new(),
    };

    // Codex is up and its session registered (through the wrapper's terminal id).
    wait_for(
        "the input box",
        |p| String::from_utf8_lossy(&p.output).contains("for shortcuts"),
        &mut pty,
    );
    let session = || {
        d.sessions(true)
            .into_iter()
            .find(|s| s.agent == AgentKind::Codex)
    };
    wait_for(
        "the session",
        |_| session().is_some_and(|s| s.in_terminal_wrapper),
        &mut pty,
    );
    assert!(
        pty.answered.len() >= 3,
        "the fake asked the terminal and got answers"
    );

    // The user clicks into the terminal, moves the mouse, and types /styr key by key.
    pty.key(b"\x1b[I");
    pty.key(b"\x1b[<35;20;10M");
    for k in b"/st" {
        pty.key(&[*k]);
    }
    pty.key(b"\x1b[<35;21;10M");
    for k in b"yr" {
        pty.key(&[*k]);
    }
    pty.key(b"\r");
    wait_for(
        "/styr to share the session",
        |_| session().is_some_and(|s| s.shared),
        &mut pty,
    );
    // The confirmation reaches the terminal just after the daemon records the share.
    wait_for(
        "the terminal to say the session is on the phone",
        |p| {
            String::from_utf8_lossy(&p.output)
                .contains("Styrman: this session is now on your phone")
        },
        &mut pty,
    );

    // A message from the phone is typed into Codex, despite all the terminal chatter.
    let id = session().unwrap().id;
    let r = d
        .client()
        .call(&Request::SendPrompt {
            session: id.clone(),
            text: "hello from the phone".into(),
        })
        .unwrap();
    assert!(matches!(r, Response::PromptAccepted { .. }), "{r:?}");
    wait_for(
        "the phone message to reach Codex",
        |_| {
            std::fs::read_to_string(&out)
                .unwrap_or_default()
                .contains("hello from the phone")
        },
        &mut pty,
    );
    let received = std::fs::read_to_string(&out).unwrap();
    assert!(
        !received.contains("/styr"),
        "Codex never saw /styr:\n{received:?}"
    );

    // And it's on the phone as the phone's message.
    let Response::Events { events } = d
        .client()
        .call(&Request::Events {
            session: id,
            since_seq: 0,
            limit: 100,
        })
        .unwrap()
    else {
        panic!()
    };
    let from_phone = events.iter().any(|e| matches!(&e.kind,
        styr_core::event::EventKind::UserMessage { text, origin: styr_core::event::Origin::Remote(_) } if text == "hello from the phone"));
    assert!(
        from_phone,
        "{:#?}",
        events.iter().map(|e| e.kind.name()).collect::<Vec<_>>()
    );

    let _ = child.kill();
    let _ = child.wait();
}

/// Kills the whole process group on drop (also when an assertion fails).
struct KillGroup(i32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        // SAFETY: signalling our own child's process group.
        unsafe { libc::kill(-self.0, libc::SIGKILL) };
    }
}
