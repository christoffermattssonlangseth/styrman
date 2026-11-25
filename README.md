<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo/styrman-glyph-light.svg">
    <img src="assets/logo/styrman-glyph.svg" alt="Styrman" width="140">
  </picture>
</p>

<h1 align="center">Styrman</h1>

<p align="center"><b>Steer your coding agents from anywhere.</b></p>

<p align="center">
  A personal remote control for Claude Code, Antigravity CLI and Codex CLI:<br>
  type <code>/styr</code> in a running session and follow, approve and prompt it from your iPhone.
</p>

---

## Why Styrman

Coding agents now run for minutes, sometimes hours, but they still need you at odd moments: to approve a command, to answer a question, or to hear "done, what next?". If you step away from the terminal, the agent sits idle at a permission prompt, or it finishes and waits for nobody.

The agents' own answers to this don't fit how I work:

- **Each one has its own remote.** Claude Code has Remote Control, and Antigravity has its own web remote. Each covers only its own agent, needs its own app or tab, and goes through the vendor's cloud. When three different agents run side by side in different repos, there is no single place to see which one needs me.
- **Prompts block the whole session.** A Claude session waiting on "Allow `cargo test`?" makes no progress until someone presses a key in that terminal.
- **I want it to be mine.** No relay service, no account, nothing leaving my machines except the push notification itself. It stays on my Mac and my tailnet.

Styrman (Swedish/Norwegian *styrmann*, "helmsman") is one helm for all of them. Your terminal workflow doesn't change. Sessions stay invisible until you type `/styr` in one; from then on, that session appears on your phone with its live conversation, tool calls, permission cards (Allow once / Always / Deny) and a prompt box, and you get a notification when it needs you or is done.

## How it works

```
 terminal                         your Mac                               iPhone
┌───────────────┐  hooks   ┌──────────────┐   WebSocket over   ┌──────────────────┐
│ claude / agy  │ ───────► │              │   Tailscale only   │  Styrman (iOS)   │
│ / codex       │ ◄─────── │    styrd     │ ◄────────────────► │  sessions, chat, │
└───────────────┘  answers │  (launchd)   │                    │  approvals,      │
        ▲                  │   SQLite     │ ──── APNs push ──► │  prompt box      │
        │ styr claude …    └──────────────┘                    └──────────────────┘
        └── styr: launcher + `styr hook` helper (fails open if styrd is down)
```

- **`styrd`** is a Rust daemon (tokio) on your Mac. It tracks sessions, keeps their history in SQLite, and serves a token-authenticated WebSocket API on localhost and your Tailscale address only.
- **`styr`** is a small Rust CLI. It launches agents (`alias claude='styr claude'`) and is the helper their hooks call (`styr hook <event>`). If the daemon isn't running, it does nothing, and the agent behaves exactly as it would without Styrman.
- **Adapters** turn each agent's own signals into one common event model:
  - **Claude Code** (first): hooks and a `styrman` plugin
  - **Antigravity `agy`** (second): hooks, a plugin and ACP
  - **Codex** (third): attaches to `codex app-server` as a second client
  - **PTY**: a generic fallback for agents with no hooks or protocol
- **The iOS app** is SwiftUI. It pairs by scanning a QR code from `styr pair`, and gets "needs approval" and "done" notifications via APNs.

Design details and the reasoning behind them are in [`docs/design.md`](docs/design.md).

## Status

| Checkpoint | State |
|---|---|
| 1. Daemon, Unix socket, SQLite, `styr status` | ✅ done |
| 2. `styr hook` plus the Claude plugin (logging only) and the fake-agent test harness | next |
| 3. iOS app: pairing and live sessions (read-only) | |
| 4. Permission round-trip | |
| 5. Prompts from the phone | |
| 6. Push notifications | |
| 7. Pairing and auth hardening, launchd, install docs | |

## Try it (checkpoint 1)

```sh
cargo build --release
./target/release/styrd &            # socket + db in ~/Library/Application Support/Styrman/
./target/release/styr ping
./target/release/styr status --all  # sessions appear once agents report in (checkpoint 2)
```

Environment overrides: `STYR_HOME` sets the data directory, `STYR_SOCKET` sets the socket path, and `STYR_LOG` sets the log filter (for example `debug`).

Install, launchd setup, shell aliases, pairing and "adding a new adapter" will be written up as those pieces land.

## Repository layout

```
crates/styr-core   event model, status reducer, adapter trait, IPC types
crates/styrd       the daemon
crates/styr        the CLI
assets/logo        icon and logo concepts
docs/design.md     architecture and protocol notes
```
