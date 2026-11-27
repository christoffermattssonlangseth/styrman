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

## Usage monitoring

Open **Usage** from the mobile session list to see the available quota windows, reset times, and activity estimates. Run a KarvMac build with local quota export on the same Mac as `styrd`, then sync Karv once. Styrman reads the export and sends the quota figures over its paired connection. Provider credentials stay with Karv.

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

- **`styrd`** is a Rust daemon (tokio) on your Mac. It tracks sessions, keeps their history in SQLite, and serves a token-authenticated WebSocket API on localhost and your Tailscale address, with an encrypted fallback on your local network for when Tailscale is down.
- **`styr`** is a small Rust CLI. It launches agents (`alias claude='styr claude'`) and is the helper their hooks call (`styr hook <event>`). If the daemon isn't running, it does nothing, and the agent behaves exactly as it would without Styrman.
- **Adapters** turn each agent's own signals into one common event model:
  - **Claude Code** (first): hooks and a `styrman` plugin
  - **Antigravity `agy`** (second): hooks, a plugin and ACP
  - **Codex** (third): hooks in `~/.codex/hooks.json` and the `styr codex` terminal wrapper
  - **PTY**: a generic fallback for agents with no hooks or protocol
- **The iOS app** is SwiftUI. It pairs by scanning a QR code from `styr pair`, and gets "needs approval" and "done" notifications via APNs.

Design details and the reasoning behind them are in [`docs/design.md`](docs/design.md).

## Status

Phase 1 (Claude Code + iPhone app), phase 2 (Antigravity `agy`) and phase 3 (Codex CLI) are complete.

| Checkpoint | |
|---|---|
| 1. Daemon, Unix socket, SQLite, `styr status` | ✅ |
| 2. `styr hook`, Claude plugin, fake-agent harness | ✅ |
| 3. iOS app: pairing and live sessions | ✅ |
| 4. Approve / always allow / deny from the phone | ✅ |
| 5. Prompts from the phone | ✅ |
| 6. Push notifications | ✅ |
| 7. launchd, auth hardening, docs | ✅ |
| Phase 2: Antigravity (`agy`) adapter | ✅ |
| Phase 3: Codex CLI adapter | ✅ |
| Voice, new sessions from the phone, Android app, Homebrew, push relay | ✅ |

Next: deploying the push relay, the first TestFlight build, and notifications on Android.

## Install

You need an Apple Silicon Mac and Tailscale on the Mac and the phone.

With Homebrew:

```sh
brew tap christoffermattssonlangseth/styrman
brew trust christoffermattssonlangseth/styrman   # Homebrew asks you to trust third-party taps
brew install styrman
styr setup
```

From source (needs a Rust toolchain; Xcode with XcodeGen for the iPhone app, and a paid Apple Developer account for your own push notifications):

```sh
git clone https://github.com/christoffermattssonlangseth/styrman && cd styrman
cargo install --path crates/styrd && cargo install --path crates/styr   # into ~/.cargo/bin

styr setup          # guided and safe to re-run: background service, Claude/agy/Codex hooks, routing, pairing
styr doctor         # checks every part; `styr doctor --live` also runs an end-to-end check with Claude Code
```

`styr setup` installs the Claude Code plugin from a copy built into `styr`, so you don't need to keep the checkout around. It asks before touching your shell config.

The iPhone app: `cd ios && xcodegen && open Styrman.xcodeproj`, then Run on your iPhone. To build under your own Apple developer account, put your team and bundle id in `ios/Signing.local.xcconfig` (see `ios/Signing.xcconfig`). Releasing to TestFlight: [`docs/testflight.md`](docs/testflight.md).

**Claude Code updates.** Styrman records the Claude Code version each session runs. If it's a version Styrman hasn't been checked with, `styr status`, `styr doctor` and the phone tell you. `styr doctor --live` then checks it end to end with one small Haiku call, and marks the version verified if everything works.

### Routing `claude`, `agy` and `codex` through Styrman

`styr setup` installs tiny launchers in `~/.styrman/bin` (`claude`, plus `agy` and `codex` when they're installed) and puts that folder on your PATH (it asks first). From then on, typing `claude`, `agy` or `codex` runs them through Styrman's terminal wrapper. That wrapper is what lets the phone type messages into the session and answer agy's and Codex's permission menus.

This uses PATH rather than shell aliases on purpose: cmux's shell integration removes aliases before every prompt and puts its own wrappers first on PATH. Its wrappers then look up the next `claude`/`agy`/`codex` on the PATH, find Styrman's launcher, and both stay in the chain. Styrman makes sure cmux's wrapper runs only once, whichever comes first on PATH.

Sessions started some other way still show up on the phone, but read-only (for agy) or with messages going through the hook route (Claude). The phone tells you which.

### Pair your phone

```sh
styr pair          # QR code + one-time code, valid 5 minutes
```

Scan it with the iPhone Camera and tap **Pair**. `styr devices` lists paired phones, and `styr devices revoke <ID>` unpairs one. A revoked phone is disconnected immediately.

### Push notifications

1. At developer.apple.com, go to Certificates, IDs & Profiles › Keys and click **+**. Tick **Apple Push Notifications service**, set Environment to **Sandbox & Production**, and download `AuthKey_XXXXXXXXXX.p8`. Apple lets you download it only once, so keep a backup.
2. Run `styr push setup ~/Downloads/AuthKey_XXXXXXXXXX.p8`. The key is copied into Styrman's data folder (0600), and the Key ID is read from the filename.
3. Open the app and allow notifications. Then run `styr push test`.

While agents work, a **Live Activity** on the Lock Screen and in the Dynamic Island shows how many are working or waiting for you (agent · folder · status, nothing more), and an **Agents** widget shows the same on the home screen. With the app closed, the Mac keeps the Live Activity current through Apple's push service (Settings › Live Activity turns it off).

Approval notifications get **Allow once** and **Deny** buttons (long-press the notification). The push only says "needs your approval": the app's notification extension asks your Mac for the actual command, directly over Tailscale, and shows it before you choose. If the Mac can't be reached, the notification just opens the session. Allowing needs the phone unlocked.

Every notification is made specific the same way: the subtitle is your latest message to that session (so two Codex sessions in one folder read differently), and the body is the command it wants to run or the first line of its reply. All of that comes from the Mac over Tailscale, never through Apple or the relay.

## Antigravity (agy)

`styr setup` also installs a `styrman` plugin for agy and a user-level `/styr` skill. With `alias agy='styr agy'`, agy works just like Claude Code:

- **Sharing:** type `/styr` in agy to share the session, and `/styr off` to stop.
- **Mirroring:** the phone shows your prompts, agy's replies and its tool calls, with output.
- **Approvals:** agy has no permission hook, so the wrapper reads agy's own "Run this command?" menu off the screen. The phone gets the same options (Yes / always in this conversation / always and save / No). Tapping one selects it in agy's menu; answering at the keyboard closes the card on the phone.
- **Prompts from the phone** are typed into agy's prompt box.

Without the alias, agy sessions are mirrored read-only. In agy's `auto` mode, agy approves most actions itself, so cards only appear for the ones it would ask about.

## Android

`android/` is a Kotlin + Jetpack Compose app with the same features as the iPhone app (sessions, approvals, messages, starting sessions, voice both ways with a voice per agent), except notifications: styrd sends those through Apple's push service only, for now.

```sh
cd android && ./gradlew assembleDebug      # needs a JDK 21 and the Android SDK (compileSdk 37.2)
adb install -r app/build/outputs/apk/debug/app-debug.apk
styr pair                                  # scan the QR code with the camera app, or paste the link
```

## Codex

When Codex is installed, `styr setup` adds Styrman's hooks to `~/.codex/hooks.json`. Your own hooks and other tools' hooks in that file are kept. The next time Codex starts, it asks you to review the new hooks: choose **Trust all and continue**. Styrman never trusts hooks on your behalf. (Inside cmux, cmux's wrapper skips that question.)

- **Sharing:** type `/styr` in Codex. Codex rejects slash commands it doesn't know, so `styr codex` catches the line as you type it. Codex never sees it, and you get a terminal notification instead. `/styr off` stops sharing.
- **Right away:** Codex creates its session only with your first message, so before that the phone shows a placeholder for the terminal. You can send that first message from the phone; the placeholder then becomes the real session, history and all.
- **Mirroring:** your prompts, Codex's replies and its tool calls, with output.
- **Approvals:** Codex shows its own menu only after its permission hook returns, so the hook never waits. Instead, the wrapper reads Codex's "Would you like to run the following command?" menu off the screen. The phone gets the same options, and the first answer wins: a tap on the phone, or a key in the terminal. Other Codex menus (such as "Hooks need review") show up the same way, and phone messages wait until they're closed.
- **Prompts from the phone** are typed into Codex's prompt box.

Codex sessions started without the wrapper are mirrored read-only. Outside cmux, Codex may run hooks in its shared background server, whose environment belongs to whichever terminal started it; Styrman then matches the hook to the `styr codex` running in the same folder. Styrman doesn't use `codex app-server`: cmux starts Codex with per-launch hook options, and in that mode Codex runs in-process, where no second client can attach.

## Using it

In any Claude Code, agy or Codex session:

```
/styr        share this session with your phone (no model call)
/styr off    stop sharing
```

On the phone:

- **Sessions** are grouped into *Needs you*, *Working* and *Done*. Each row shows the repo and your first prompt. Swipe left to stop sharing a session or remove a finished one.
- **The timeline** shows your prompts, Claude's replies, the plan, and every tool call. Tap a tool call to see its full output.
- **Permission cards** offer *Allow once*, *Always allow* (Claude's own "don't ask again" rule) and *Deny*. Claude shows its own prompt in the terminal at the same time, and the first answer wins. Nothing is ever approved automatically. The phone can answer for 9 minutes (`STYR_PERMISSION_TIMEOUT` on `styrd`).
- **The message box:** with the alias, your message is typed into the terminal and shows there as a normal prompt; Claude queues it if it's busy.
  - It's never typed while a permission prompt is open, or while you're halfway through a line in the terminal.
  - Without the alias, an idle Claude still wakes up and answers, and busy sessions get it as soon as they finish.
- **Notifications:**
  - *Needs your approval* arrives if a permission is still unanswered after 8 s.
  - *Finished* arrives for turns of 30 s or more.
  - Nothing arrives while the app is open.
  - Tapping a notification opens the session.
  - Notifications say only "Claude · repo" (or agy, Codex) unless you run `styr push details on`.
- **Agent colours:** Claude is orange, agy blue and Codex green, on the badge, the send button and the composer.

**Starting a session from the phone:** tap **+** in the session list, pick an agent (Claude, Codex or agy) and any folder in your home directory (recent ones are listed first), and optionally a first message. Styrman opens a new cmux tab on the Mac running that agent there, already shared with the phone (without cmux, a detached tmux session, `tmux attach` to see it, or else a Terminal window); the first message is typed in once the agent is ready (never into a question like "trust this folder?"). `styr new codex -C ~/work/app "fix the tests"` does the same from the Mac.

cmux normally only lets programs started inside cmux open tabs. `styr setup` asks to switch cmux's socket access to password mode with a random password that only cmux and Styrman know (`~/.styrman/cmux-password`, readable only by you). It backs up `~/.config/cmux/cmux.json` first; remove the `"automation"` block there to undo it. If you'd rather set it in cmux yourself, run `styr cmux-password <password>`.

**Voice:** tap the mic in the message box to dictate (on the iPhone; on-device where supported), **Listen** under a reply to hear it, or the speaker at the top of a session to hear new replies as they arrive. Each agent has its own voice; pick them in Settings › Voice.

From the Mac:

```sh
styr status [--all]           # sessions: working / needs approval / needs you / done
styr events <ID>              # a session's full history
styr send <ID> <text>         # send a prompt, like the phone does
styr new <agent> [-C dir] [text]  # start an agent in a new terminal tab, shared
styr forget <ID>… | --ended   # delete sessions and their history
styr service restart          # after `cargo install --path crates/styrd`
styr net [restart]            # how the phone reaches this Mac; restart Tailscale
```

Sessions that are never shared stay invisible: only their start and end are recorded (for `styr status --all`). If an agent's process dies without saying goodbye (closed window, crash), its session is marked done within 30 s.

## Security

| | |
|---|---|
| **Network** | The phone API binds to loopback and the Mac's Tailscale addresses (port 4719), never `0.0.0.0`. The phone connects through the Mac's MagicDNS name over the WireGuard tunnel. iOS allows plain `ws://` only to `ts.net` hosts. |
| **Tailscale down** | Tailscale on a Mac can lose its tunnel while still saying Running. When the Mac has had no Tailscale address for 30 s, `styrd` restarts Tailscale (at most every 10 minutes; never when you stopped it yourself). `styr net` shows the state and `styr net restart` does it by hand. Meanwhile, on the same Wi-Fi, the phone falls back to the Mac's private local-network address on port 4720. That listener speaks TLS only, with a certificate made on the Mac; the phone learns its SHA-256 over Tailscale and accepts no other certificate there. Pairing is not offered on it. |
| **Pairing** | The QR code holds a one-time code (about 49 bits) that expires after 5 minutes. Ten wrong codes cancel all outstanding codes. The link contains no other secret. |
| **Devices** | Each phone has a 256-bit bearer token, stored in the iOS Keychain and sent only in the `Authorization` header. The daemon stores only SHA-256 of the token and compares in constant time. After 20 failed logins in a minute, logins are refused for a minute. Revoking a phone takes effect immediately. |
| **Browsers** | Any request carrying an `Origin` header is refused, so a web page on the Mac (including a DNS-rebinding page) can't use the API. |
| **Local** | The Unix socket is 0600 in a 0700 directory. The database, APNs key and config are 0600. |
| **Fail-open** | `styr hook` exits 0 and prints nothing unless the daemon answers. If `styrd` is down, every agent behaves exactly as it would without Styrman. |
| **Third parties** | The only one is Apple's push service, which sees "Claude · repo" text unless you turn details on. There's no relay and no account. |

## Adding an adapter

Each agent is a crate under `crates/adapters/` implementing `styr_core::adapter::AgentAdapter`. PTY still exists only as a stub.

1. **Pick the integration style** and declare it in `capabilities()`:
   - **Hook path** (Claude, agy, Codex hooks). The agent's hooks run `styr hook <agent> <event>` with the payload on stdin. Implement:
     - `native_session` to find the session id;
     - `on_hook` to turn one hook into a `HookPlan`: session metadata, normalized `EventKind`s, and optionally `wait: Permission | NextPrompt`, `set_sharing` or an immediate `reply`;
     - `render_reply` / `render_exit` to turn the daemon's `HookDecision` back into what the agent's hook protocol expects. `PassThrough` must always render as "print nothing, exit 0".
   - **Protocol path** (Codex `app-server`, agy ACP). Implement `attach` to connect to a live session, push events into the `EventSink`, and return a `SessionDriver` that carries out `Command`s (prompts, permission answers, interrupt).
2. **Map the agent's concepts** onto the event model in `crates/styr-core/src/event.rs`:
   - tool calls with a stable id and a lifecycle;
   - permission requests with their options (`allow_once`, `allow_always`, `reject_once`, `reject_always`);
   - text sent either `Delta` or `Final`.
3. **Register** the adapter in `styrd::default_adapters()` and in `styr`'s launcher (`crates/styr/src/launch.rs`).
4. **Ship the agent-side glue** next to `plugins/claude/`, for example an agy plugin whose `hooks.json` calls `styr hook agy <event>`.
5. **Test it.** Record real payloads with `STYR_HOOK_RECORD=/tmp/hooks.jsonl`, keep them as fixtures under `tests/fixtures/`, and test `on_hook`/`render_reply` against them, as `styr-adapter-claude` does. `styr-testkit` provides an in-process daemon, a hook runner, and `styr-fake-agent` / `styr-fake-phone` for end-to-end tests.

The integration plan for each agent, from their official docs, is in [`docs/design.md`](docs/design.md) §1 and §4.

## Development

```sh
cargo test --workspace                                   # Rust: unit + end-to-end (real binaries, in-process daemon)
cd ios && xcodebuild -scheme Styrman -destination 'platform=iOS Simulator,name=iPhone 18 Pro' test
cargo run -p styr-testkit --bin styr-fake-agent          # demo session: pauses for a phone approval and a phone prompt
cargo run -p styr-testkit --bin styr-fake-phone          # a scripted phone that answers the next permission request
STYR_HOOK_RECORD=/tmp/hooks.jsonl claude …               # record raw hook payloads
```

Environment variables:

- `STYR_HOME`: data directory.
- `STYR_SOCKET`: socket path. macOS limits socket paths to 103 bytes.
- `STYR_PORT`: phone API port, default 4719.
- `STYR_TAILSCALE=0`: listen on loopback only.
- `STYR_LAN=0`: no local-network fallback. `STYR_LAN_PORT`: its port, default 4720.
- `STYR_TAILSCALE_HEAL=0`: never restart Tailscale.
- `STYR_PERMISSION_TIMEOUT`: how long the phone can answer a permission.
- `STYR_LOG`: daemon log filter.
- `STYR_BIN`: which `styr` the plugin calls.
- `STYR_DEBUG`: make `styr hook` report errors on stderr.

Logs go to `~/Library/Logs/styrd.log`.

## Repository layout

```
crates/styr-core          event model, status reducer, adapter trait, IPC + phone protocol types
crates/styrd              the daemon: sessions, SQLite, permission broker, prompt queue, phone API, APNs
crates/styr               the CLI: launcher, hook helper, status, pairing, service, push
crates/styr-testkit       in-process daemon, hook runner, fake agent, fake phone
crates/adapters/*         claude, agy, codex (complete); pty (stub)
plugins/                  Claude Code plugin + local marketplace
ios/                      SwiftUI app (XcodeGen project.yml)
packaging/launchd/        example launchd plist (`styr service install` writes the real one)
assets/logo/              icon and logo concepts
docs/design.md            architecture, protocol notes, verified agent behaviour
```
