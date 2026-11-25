# Styrman design (phase 1 proposal)

Status: **phases 1–3 complete** (Claude Code, Antigravity, Codex). Verified findings are recorded in §8.

## 1. What the three agents actually offer

Checked against the official docs on 2026-11-25: Claude Code 2.1.332; agy 1.5.17; Codex at `rust-v0.172.0`, which isn't installed locally.

| | Claude Code | Antigravity `agy` | Codex |
|---|---|---|---|
| Event source | Command hooks (34 events), JSON on stdin | Command hooks (6 events), camelCase JSON on stdin | `codex app-server` JSON-RPC over a Unix socket or WS; a second client can subscribe to a live thread. Hooks too, with Claude-like shapes. |
| Approval hook | `PermissionRequest`: fires only when Claude would show a prompt. Allow, deny, or allow plus `updatedPermissions` ("always"). | `PreToolUse` only: `{"decision": "auto_approve" \| "deny" \| "ask" \| …}`. No "would prompt" signal. | Server request `item/commandExecution/requestApproval` goes to **every** subscribed client; first answer wins; `serverRequest/resolved` is broadcast. |
| Answer in terminal *or* remote | Undocumented for a blocking hook; to verify in checkpoint 4. | Not with hooks (only Google's Remote Control). | Yes, built in (first answer wins). |
| Inject a prompt | 1. **Channels** (MCP server pushes messages into a running session; research preview, needs a dev flag). 2. Async `Stop` hook with `asyncRewake`: exit 2 wakes Claude even when idle. 3. Blocking `Stop` with `decision: block`. | `Stop` returns `{"decision": "continue", "reason": …}`; `PreInvocation` returns `injectSteps: [{userMessage}]`. | `turn/start`, `turn/steer`, `turn/interrupt`. |
| "Waiting/idle" signal | `Notification` (`permission_prompt`, `idle_prompt`), `Stop` | `Stop.fullyIdle`; statusline command (`agent_state`, `tool_confirmation_pending`) | `thread/status/changed` (`waitingOnApproval`), `turn/completed` |
| `/styr` command | Plugin command; intercepted by a hook so no model call is made (verify in checkpoint 2) | Skill becomes `/styr`, plus a `PreInvocation` intercept | No custom top-level slash command; use a `$styr` skill or a `UserPromptSubmit` intercept |
| Transcript | `~/.claude/projects/<proj>/<id>.jsonl`; format documented as internal and unstable | `~/.gemini/antigravity-cli/brain/<id>/…/transcript.jsonl`; undocumented | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl[.zst]` |
| Headless | `claude -p --input/output-format stream-json` | `agy acp` (ACP v1), `agy -p --… stream-json` | app-server is the native interface |

Conclusions that shape the model:

1. **Two integration styles exist and must share one model.** *Hook-driven* (Claude, agy, Codex-hooks): short-lived processes push events in and sometimes wait for an answer. *Protocol-driven* (Codex app-server, ACP): the daemon holds a connection, receives a stream, and sends commands.
2. **Tool calls need a lifecycle, not one event.** ACP and Codex emit pending, then in progress, then completed or failed, with stable IDs. Claude has `tool_use_id`. agy has only `stepIdx`, so its adapter synthesizes an ID.
3. **Permission options are a list, not a boolean.** ACP: `allow_once`, `allow_always`, `reject_once`, `reject_always`. Codex: `accept`, `acceptForSession`, `decline`, `cancel`. Claude: allow, allow with rules, deny. The phone renders whatever options the adapter offers.
4. **Assistant text is sometimes streamed** (Codex, ACP deltas) **and sometimes whole** (Claude `last_assistant_message`, agy). The model supports both.
5. **"Who resolved it" matters.** The phone must dismiss a card when the terminal (or another client) answered first, as `serverRequest/resolved` does.

## 2. Repo layout

```
styrman/
├─ Cargo.toml                      # workspace
├─ crates/
│  ├─ styr-core/                   # event model, commands, adapter trait, IPC + phone wire types, status reducer
│  ├─ styrd/                       # daemon: unix-socket IPC, session registry, permission broker,
│  │                               #   SQLite store, axum WS API, APNs, pairing
│  ├─ styr/                        # CLI: launcher (`styr claude …`), `styr hook`, `styr pair`, `styr share`, `styr status`
│  ├─ styr-testkit/                # fake-agent harness: scripted hook sequences + assertions
│  └─ adapters/
│     ├─ styr-adapter-claude/      # phase 1
│     ├─ styr-adapter-agy/         # stub, then phase 2
│     ├─ styr-adapter-codex/       # stub, then phase 3
│     └─ styr-adapter-pty/         # stub (generic fallback)
├─ plugins/
│  ├─ .claude-plugin/marketplace.json   # local marketplace: `claude plugin marketplace add ./plugins`
│  ├─ claude/styrman/              # .claude-plugin/plugin.json, hooks/hooks.json, commands/styr.md
│  └─ agy/styrman/                 # later: plugin.json, hooks.json, skills/styr/SKILL.md
├─ ios/Styrman/                    # SwiftUI app (Xcode project), iOS 26+
├─ packaging/launchd/com.styrman.styrd.plist
├─ assets/logo/
└─ docs/
```

Rule: **agent-specific knowledge lives in adapter crates, which run inside the daemon.** `styr hook` is a dumb, agent-agnostic forwarder. It reads stdin, attaches `--agent` and the event name, sends the envelope over the Unix socket, waits (bounded) for a reply, prints the reply verbatim, and exits 0. Benefits:

- The Claude and agy plugins share the same helper.
- Adapter logic can change without reinstalling hooks.
- Failing open is trivial. If the socket is missing, refused, times out, or hits EOF, it prints nothing and exits 0, so the agent behaves exactly as it would without Styrman.

## 3. Event model (`styr-core`)

```rust
pub enum AgentKind { Claude, Antigravity, Codex, Pty }

/// Daemon-assigned stable id (ULID). Native ids (Claude session_id, agy
/// conversationId, Codex threadId) are kept alongside and are unique per agent.
pub struct SessionId(Ulid);
pub struct NativeSessionRef { pub agent: AgentKind, pub native_id: String }

/// What is persisted and streamed to the phone.
pub struct Event {
    pub session: SessionId,
    pub seq: u64,              // per-session, monotonic, daemon-assigned; the phone resumes with `since_seq`
    pub at: Timestamp,
    pub source: EventSource,   // Hook | Protocol | Transcript | Pty | Daemon
    pub kind: EventKind,
}

pub enum EventKind {
    SessionStarted  { agent: AgentKind, native: NativeSessionRef, cwd: PathBuf,
                      workspace_roots: Vec<PathBuf>, model: Option<String>,
                      title: Option<String>, start: StartKind /* New|Resume|Fork|Clear|Compact */ },
    SessionEnded    { reason: Option<String> },
    SharingChanged  { shared: bool },

    TurnStarted     { turn: Option<TurnId> },
    UserMessage     { text: String, origin: Origin /* Terminal | Remote(DeviceId) */ },
    AssistantMessage{ message: MessageId, text: String, mode: TextMode /* Delta | Final */ },
    Reasoning       { message: MessageId, text: String, mode: TextMode },   // optional, collapsed in UI
    Plan            { entries: Vec<PlanEntry> },                             // ACP plan / Codex plan / Claude TodoWrite

    ToolCall        { call: ToolCallId, tool: ToolInfo, status: ToolStatus,  // emitted on each status change
                      output: Option<ToolOutput> },

    PermissionRequested { request: RequestId, call: Option<ToolCallId>, tool: ToolInfo,
                          reason: Option<String>, options: Vec<PermissionOption>,
                          expires_at: Option<Timestamp> },
    PermissionResolved  { request: RequestId, outcome: PermissionOutcome, by: Resolver },

    TurnEnded       { turn: Option<TurnId>, reason: TurnEndReason, last_message: Option<String> },
    Notice          { level: Level, text: String },   // agent notifications, hook warnings
    Error           { message: String, fatal: bool },
}

pub struct ToolInfo {
    pub name: String,                // native name: "Bash", "run_command", "commandExecution"
    pub kind: ToolKind,              // ACP kinds: Read|Edit|Delete|Move|Search|Execute|Think|Fetch|Mcp|Other
    pub title: String,               // human summary: "Run `cargo test`", "Edit src/main.rs"
    pub input: serde_json::Value,    // raw args (redactable)
    pub locations: Vec<PathBuf>,
}
pub enum ToolStatus { Pending, Running, Completed, Failed }

pub struct PermissionOption { pub id: String, pub kind: OptionKind, pub label: String }
pub enum OptionKind { AllowOnce, AllowAlways, RejectOnce, RejectAlways }
pub enum PermissionOutcome { Selected { option: String, kind: OptionKind }, Cancelled, TimedOut }
pub enum Resolver { Phone(DeviceId), Terminal, OtherClient, Daemon /* timeout, shutdown */ }

pub enum TurnEndReason { EndTurn, Interrupted, Failed, MaxSteps, Refusal }
```

**Derived status** (not stored; computed by a pure reducer over events, for the session list):
`Working`, `NeedsApproval` (open permission), `NeedsInput` (turn ended; agent is idle and waiting), `Done` (session ended), `Error`. The reducer is the core unit-test target.

**Commands** (phone → daemon → adapter):

```rust
pub enum Command {
    SendPrompt        { text: String },
    ResolvePermission { request: RequestId, option: String },
    Interrupt,
    SetSharing        { shared: bool },
}
```

### Mapping to your list

| Your event | Model |
|---|---|
| SessionStarted | `SessionStarted` |
| UserMessage | `UserMessage` |
| AssistantMessage | `AssistantMessage` |
| ToolCall | `ToolCall` (lifecycle) |
| PermissionRequest | `PermissionRequested` |
| PermissionResolved | `PermissionResolved` |
| Waiting/Idle | `TurnEnded`, plus derived `NeedsInput` |
| Finished | `SessionEnded` |
| Error | `Error` |

The additions (`TurnStarted`, `Plan`, `Reasoning`, `Notice`, `SharingChanged`) are needed for Codex and ACP parity, or for the UI.

## 4. Adapter trait

```rust
/// Static description used by the daemon and the phone UI.
pub struct Capabilities {
    pub prompt_delivery: PromptDelivery, // None | WhenIdle(StopHook) | Anytime(Channel|Protocol|Pty)
    pub permissions: PermissionMode,     // None | HookBlocking | ConcurrentWithTerminal
    pub streaming_text: bool,
    pub interrupt: bool,
}

#[async_trait]
pub trait AgentAdapter: Send + Sync + 'static {
    fn kind(&self) -> AgentKind;
    fn capabilities(&self) -> Capabilities;

    // ---- hook path (Claude, agy, Codex-hooks) ----
    /// Normalize one raw hook invocation. Pure, so it is unit-tested with fixtures.
    fn on_hook(&self, hook: &RawHook, session: &SessionView) -> Result<HookPlan>;
    /// Render the agent-native stdout for whatever the broker decided.
    fn render_reply(&self, hook: &RawHook, decision: HookDecision) -> Option<serde_json::Value>;

    // ---- protocol path (Codex app-server, agy ACP) ----
    /// Attach to a live session; events flow into `sink`, and the driver accepts commands.
    async fn attach(&self, _target: AttachTarget, _sink: EventSink)
        -> Result<Box<dyn SessionDriver>> { Err(Error::Unsupported) }

    // ---- optional enrichment ----
    fn transcript_tailer(&self, _session: &SessionView) -> Option<Box<dyn TranscriptTailer>> { None }
    fn launcher(&self) -> LaunchSpec;  // how `styr <agent>` execs it (env, extra flags, next-on-PATH)
}

pub struct HookPlan {
    pub session: SessionKeyUpdate,    // register/refresh the session (cwd, transcript path, …)
    pub events: Vec<EventKind>,
    pub wait: Option<WaitFor>,        // what the hook process should block on
}
pub enum WaitFor {
    Permission { request: PermissionRequested, timeout: Duration },
    NextPrompt { timeout: Duration },        // Stop-hook / asyncRewake style delivery
}
pub enum HookDecision { PassThrough, Permission(PermissionOutcome), Prompt(String), Block(String) }

#[async_trait]
pub trait SessionDriver: Send {
    async fn send(&mut self, cmd: Command) -> Result<()>;
    async fn detach(self: Box<Self>);
}
```

`PassThrough` is the universal fail-open answer. Every adapter must render it as "no output", so the agent continues with its normal behaviour.

How each agent fits:

- **Claude, phase 1.** Hook path. `PermissionRequest` produces `WaitFor::Permission`. `Notification` and `Stop` produce status events. Prompts go through Channels if they work in an interactive session, otherwise an async `Stop` hook with `asyncRewake`. The transcript tailer is optional enrichment for full assistant text and tool output.
- **agy, phase 2.** Hook path through the same `styr hook --agent agy`. `PreToolUse` waits for permission only when the session is shared and the tool isn't already allowed by policy (still a known gap). `Stop` returns `continue` for prompts. The statusline command feeds idle and approval status. ACP `attach` is for sessions started from the phone.
- **Codex, phase 3.** Protocol path. `attach` connects to `$CODEX_HOME/app-server-control/app-server-control.sock` and calls `thread/resume`. Approvals use real first-wins semantics. `turn/start`/`turn/steer` carry phone prompts. `styr codex` makes sure the app-server daemon is running so the TUI attaches to it, not to an embedded server.
- **PTY, later.** `styr pty -- <cmd>` wraps any CLI in a pseudo-terminal: output is mirrored as `Notice`/`AssistantMessage` text, and prompts are typed as keystrokes.

## 5. Runtime pieces

- **Sockets and files**
  - Socket: `~/Library/Application Support/Styrman/styrd.sock` on macOS, or `$XDG_RUNTIME_DIR/styrman/styrd.sock` on Linux. Mode 0600.
  - IPC protocol: newline-delimited JSON request/response.
  - Database: SQLite next to the socket (`rusqlite`, WAL mode). Tables: `sessions`, `events (session, seq, json)`, `devices`, `pending_permissions`.
- **Sharing**
  - The daemon stores `shared` per native session id.
  - A hook for an unshared session returns `PassThrough` at once, with one fast local round trip and no waiting.
  - **Decided:** unshared sessions are not listed on the phone at all. Nothing about a session leaves the Mac until `/styr`.
- **Daemon restart**
  - Hooks are short-lived. One that is waiting when the daemon dies sees EOF, and the agent falls back to its own prompt.
  - On restart, sessions reload from SQLite and re-attach on their next hook. Pending permissions in the database are marked `Cancelled{by: Daemon}`.
- **Phone API**
  - Endpoint: `GET /v1/ws`, with `Authorization: Bearer <device token>` in the upgrade headers.
  - Server sends `hello`, `sessions` (snapshot), `event`, `resolved`.
  - Client sends `subscribe {session, since_seq}` and `command {session, Command}`.
  - Bound to `127.0.0.1` and the Tailscale `100.x` address only (found by checking interfaces at startup and again on change).
- **Pairing**
  - `styr pair` shows a QR code containing the URL and a **one-time code**. The code expires in 5 minutes.
  - The app exchanges it at `POST /v1/pair` for a random 256-bit device token, which it keeps in the iOS Keychain. The daemon stores only a hash, compared in constant time (`subtle`).
  - No secrets appear in URLs after pairing.
- **Push.** APNs token auth (.p8) straight to `api.push.apple.com`. The default payload is minimal: agent, repo name and "needs approval" or "done". It never includes code or command text unless you turn that on.
- **Launcher**
  - `styr claude …` resolves the next `claude` on `PATH` other than itself, so the cmux wrapper stays in the chain.
  - It sets `STYR_LAUNCHED=1` and adds any flags the adapter wants (for example the Channels flag), then `exec`s.
  - Sessions not started by `styr` still work through the plugin's hooks.

## 6. Build order and checkpoints

Each checkpoint ends with something runnable and its tests green.

1. Workspace, `styr-core` (model, reducer, wire types), `styrd` with the Unix socket and SQLite, `styr status`.
2. `styr hook` plus the Claude plugin, logging only. The fake-agent harness replays recorded hook payloads. Settle how `/styr` works without a model call.
3. iOS app, read-only: pairing, session list, live session view.
4. Permission round-trip: `PermissionRequest`, the phone card, and the reply. Measure the terminal/phone race.
5. Phone prompts: Channels vs `asyncRewake`; pick whichever works.
6. APNs push.
7. Auth hardening, launchd plist, README (install, aliases, pairing, how to add an adapter).

## 7. Known risks and verification items

- ~~**Claude terminal prompt while the hook waits.**~~ This turned out to be a non-issue: Claude Code shows the terminal prompt alongside the hook (§8).
- **Channels** is a research preview behind `--dangerously-load-development-channels`. It may change, so `asyncRewake` is the fallback.
- **iOS App Transport Security and plain `ws://` over Tailscale.** Needs an ATS exception, or HTTPS via `tailscale cert`. To be decided in checkpoint 7.
- **agy** has no "would-prompt" signal. Its tool-approval UX on the phone is weaker until a better interface exists.

## 8. Verified during the build

- **Claude Code 2.1.332, `/styr`.** `UserPromptSubmit` receives the raw `/styr` before the slash command expands, and `{"decision":"block","reason":…}` stops it with no model call. The terminal shows "UserPromptSubmit operation blocked by hook:" followed by our message. If `styrd` doesn't answer, the plugin's `/styr` command expands instead, and the model tells you to start the daemon.
- **Claude Code 2.1.332, payloads.** `PermissionRequest` has no `tool_use_id`, but it carries `permission_suggestions`, which the phone's "Always" button sends back as `updatedPermissions`. `PostToolUse.tool_response` is an object for Bash. Recorded fixtures are in `crates/adapters/styr-adapter-claude/tests/fixtures/`.
- **Closing permission cards.** Until the phone can answer (checkpoint 4), an open request is resolved as `Cancelled{by: Terminal}` once Claude moves on: `PostToolUse`, `PostToolUseFailure`, `PermissionDenied`, `Stop`, or the next prompt.
- **Exit codes.** `styr hook` is dispatched before argument parsing. Exit code 2 means "block" to Claude Code, so the helper must never use it.
- **iOS App Transport Security and Tailscale.** `NSAllowsLocalNetworking` does not cover Tailscale's `100.64/10` addresses: pairing to `http://100.x` failed with `-1022` in the simulator. The fix is to connect via the MagicDNS name. `styrd` reads it from `tailscale status --json` and advertises `ws://<host>.<tailnet>.ts.net:4719` first, and the app has an ATS exception only for `ts.net`. This was verified in the simulator and installed on a physical iPhone 17e.
- **Permission round-trip, interactive Claude Code 2.1.332.** This was measured by driving a real TUI session through a pseudo-terminal.
  - Claude shows its terminal permission prompt about 2 s after the request, *while* our `PermissionRequest` hook is still waiting.
  - If the user answers in the terminal, the tool runs immediately and Claude cancels the hook process. `styrd` sees the IPC socket close and records `Cancelled{by: Terminal}`.
  - If the phone answers, the tool runs immediately and the terminal prompt disappears.
  - If nobody answers, the terminal prompt stays up.
  - So the Claude adapter is `PermissionMode::Concurrent`. The phone's answer window defaults to 540 s, below the plugin's 600 s hook timeout, and never delays the terminal.
- **Permission mode `auto`.** In `auto`, Claude's classifier approves routine actions itself, and `PermissionRequest` only fires for the ones it would ask about.
- **Phone prompts on Claude Code 2.1.332, via the `asyncRewake` Stop hook.** The plugin's second `Stop` hook (`StopWait`, `async: true`, `asyncRewake: true`, `timeout: 86400`) waits in `styrd` for a phone prompt and exits 2 with it on stderr.
  - An idle interactive session wakes within about 2 s and answers it.
  - Claude Code then submits an internal `<task-notification>…Stop hook feedback…` prompt. The adapter ignores it as a user message.
  - A prompt sent while Claude works is queued and taken by the next `StopWait`.
  - A newer wait, a prompt typed in the terminal, `/styr off` or `SessionEnd` releases the waiting hook, so at most one waits per session.
  - If the daemon restarts mid-wait, the hook reconnects for up to an hour.
- **Channels, considered and deferred.** The official push mechanism (`notifications/claude/channel`, plus permission relay) needs `--dangerously-load-development-channels` and shows a full-screen confirmation in every interactive session during the research preview. It's possible later as an opt-in through `styr claude` for delivery mid-turn.

## 9. Threat model (phase 1)

**Assets.** These are, in order of sensitivity:
1. The power to approve tool calls and send prompts: effectively, code execution on the Mac.
2. Session content: prompts, code and command output.
3. The APNs key.

**Who can reach what:**
- **The Unix socket.** Only the user (0600 socket, 0700 directory). Anyone who can open it is already that user.
- **The phone API.** Reachable over loopback and the tailnet. Every route except `/v1/health` and `/v1/pair` needs a device token.
  - Pairing needs a fresh one-time code that only `styr pair` on the Mac prints.
  - Browsers are refused (`Origin` header), which also defeats DNS rebinding.
  - Token guessing is throttled.
  - The token is 256 bits, so brute force is moot anyway.
- **A lost phone.** Revoke it with `styr devices revoke`; the connection drops at once and the token stops working. The token sits in the Keychain (`AfterFirstUnlockThisDeviceOnly`), so it isn't in backups.
- **Apple (APNs).** Sees device tokens and "agent · repo" text, plus a command title or the first line of a reply if `details` is on.

**Residual risks, accepted for a personal tool:**
- **Plain `ws://` inside the tailnet.** It relies on WireGuard for confidentiality.
- **Anyone else on the tailnet can try to pair.** They would still need a live code from the Mac's terminal.
- **Markdown links in Claude's replies are tappable on the phone.**
- **Permission prompts describe commands, not their effects.** As in the terminal, approve only what you would approve at the keyboard.
- **Phone prompts shown natively (`styr claude` wrapper).** Claude Code renders an `asyncRewake` wake-up only as a collapsed "Stop hook feedback" line; neither stderr nor a `systemMessage` text is visible (verified). So `styr claude` now runs Claude in a pseudo-terminal it owns.
  - It holds an `AttachTerminal` long-poll to `styrd` and types phone prompts into the prompt box: a bracketed paste for multi-line text, then Enter.
  - `styrd` matches the following `UserPromptSubmit` to the typed text and records it as `Origin::Remote(device)`.
  - **Safety:**
    - Nothing is typed while a permission request is open (keys could answer it) or after the session has ended.
    - The wrapper waits while the user has a half-typed line.
    - If the wrapper disappears, its queued prompts fall back to the `StopWait` hook route.
  - Verified on interactive Claude Code 2.1.332: the prompt appears as a normal `❯` line about 1 s after sending.
- **MagicDNS name under launchd.** The Tailscale Mac app's CLI refuses to run in launchd's minimal environment ("The Tailscale GUI failed to start"), which made `styrd` advertise a raw `100.x` URL that iOS rejects. `styrd` now does a reverse DNS lookup (`getnameinfo`, `NI_NAMEREQD`) of its Tailscale IPv4 through the system resolver, with the CLI as a fallback. This was caught by `styr doctor` on its first run.
- **Agent version tracking.** The hook resolves the agent's real executable from its pid (`CLAUDE_PID` is set for hooks); Claude Code installs to `…/claude/versions/<version>`. `styrd` keeps the last seen version and a verified list: built-in plus `styr doctor --live` passes. It warns about unverified versions in `styr status`, `styr doctor` and the phone's `hello`.

## 10. Antigravity (agy 1.5.17), as measured

- **Hooks.** Workspace (`.agents/hooks.json`), global (`~/.gemini/config/hooks.json`) and plugin (`<plugin>/hooks.json`) hooks all load.
  - Events that fire: `SessionStart`, `PreInvocation`/`PostInvocation` around each model call (`invocationNum` 0 = a new user turn), `PreToolUse`, `PostToolUse` (only when a tool actually runs) and `Stop` (`fullyIdle`, `terminationReason`, `continuationActive`).
  - `agy plugin validate` rejects `SessionEnd`, so cmux's `SessionEnd`/`Notification`/`turn-completion` hooks are ignored.
- **Fail-open is inverted compared with Claude.** A `PreToolUse` reply of `{}` (no `decision`) *denies* the tool, while empty output allows it. `PassThrough` must therefore always render as nothing, and it's tested.
- **`/styr`.** A typed command must exist as a skill, or the TUI rejects it ("Unknown command"). Plugin skills are namespaced (`/styrman:styr`), so `styr setup` also writes a user-level skill at `~/.gemini/config/skills/styr` to make plain `/styr` work.
  - The raw `/styr` reaches `PreInvocation.lastUserInput`. `{"terminationBehavior":"terminate","injectSteps":[{"errorMessage":{"userMessage":…}}]}` ends the turn with no model call and shows "⚠ Styrman: …".
  - The skill's text has to be neutral, because agy keeps it in the conversation history.
- **Content.** Hooks carry no reply text or tool output. Both come from the transcript JSONL: `PLANNER_RESPONSE.content`, and the record at the tool's `stepIdx` (with the "Created At/Completed At" lines stripped).
- **Permissions.** There's no hook, so `styr agy` runs agy in the PTY wrapper with a `vt100` emulator and reads agy's menu off the screen ("Requesting permission for: … / Run this command? / > 1. Yes, run command / … / 4. No, cancel").
  - `styrd` raises a `PermissionRequested` for it, described by the last `PreToolUse`.
  - When the phone answers, the wrapper moves the highlight and presses Enter, but only if the same menu is still on screen.
  - If the menu disappears without a phone answer, the request is resolved as `Cancelled{by: Terminal}`.
- **Version.** agy's binary isn't versioned by path, so the hook uses `agy --version` on `SessionStart`. `CLAUDE_PID` is inherited by anything started from a Claude session, so it's trusted only for Claude hooks.
- **Verified live:**
  - `/styr` in a real interactive session
  - a phone-answered "Run this command?" menu running the command
  - a phone prompt typed into agy and answered
  - `styr doctor --live` passing for both agents
- **cmux removes aliases.** cmux's zsh integration (`_cmux_fix_path`, run on every `precmd`) runs `unalias claude agy` and defines shell functions that call its bundled wrappers, so `alias agy='styr agy'` never took effect in cmux tabs. Observed as phone prompts for agy stuck in a queue.
  - **Fix:** `styr setup` installs PATH shims in `~/.styrman/bin` (after cmux's directories, before `~/.local/bin`).
    - cmux's wrapper looks up the next `agy` and finds the shim. The shim execs `styr agy`, which runs cmux's wrapper again with `STYR_WRAPPED=1`, and then the shim hands off to the next real binary after itself.
    - `styr`'s own lookup skips the shim directory, and a hop counter guards against loops.
  - Verified in a zsh started with cmux's `ZDOTDIR` integration: `agy` is a cmux function, yet `styr agy` → the real agy.
- **Undeliverable prompts are refused.** Sessions report `in_terminal_wrapper` / `accepts_prompts`. Phone prompts for an unwrapped agy session are refused with a reason instead of being queued forever, and the app shows a read-only notice in place of the message box.

### Codex CLI 0.172.0 (measured 2026-11-25)

- **The app-server path doesn't work under cmux.** Codex's TUI normally runs through a shared app-server daemon, and a second client can follow a thread, start turns and answer approvals; that part was verified. But any `-c`/`--enable` option forces embedded mode, and cmux's codex wrapper always passes `-c hooks.…`. In embedded mode the shared server lists no threads. So Styrman uses hooks instead.
- **Hooks are Claude-shaped:** SessionStart (fires with the first prompt, not at launch), UserPromptSubmit, PreToolUse, PermissionRequest, PostToolUse, Stop (`last_assistant_message`) and SessionEnd. Payloads are snake_case, with `tool_name: "Bash"`, `tool_use_id` and `turn_id`. Timeouts are in seconds. Hooks in `hooks.json` run alongside cmux's `-c` hooks.
- **Trust:** new or changed hooks must be trusted once ("Hooks need review"). Codex stores per-hook hashes under `[hooks.state]` in config.toml. `--dangerously-bypass-hook-trust`, which cmux passes, skips the question.
- **PermissionRequest is sequential:** Codex shows its own menu only after the hook returns, so a hook waiting for the phone would freeze the terminal. The hook returns at once, and `styr codex` reads the menu off the screen (`detect_codex_prompt`) as it does for agy.
- **`/styr`:** the TUI rejects unknown slash commands before any hook sees them. `styr codex` catches a plainly typed `/styr` line, erases it and sends `ShareLaunch`. If the session has no hook yet, sharing applies when its first hook arrives.
- **Codex creates its session with the first prompt**, so a `/styr` before that makes a placeholder session (native id `launch:<terminal>`). The phone can send the first prompt to it, which is typed into the terminal. The first hook from that terminal renames the placeholder to Codex's real session id, so the history carries over.
- **Hooks can carry another terminal's environment.** Outside cmux, Codex may run hooks in its shared background server, which inherited the environment of the terminal that started it, so `STYR_LAUNCH_ID` can name the wrong terminal (or a closed one). `styr codex` reports its folder when it attaches; a Codex hook whose terminal id isn't a live `styr codex` in the hook's folder is linked to the session's existing terminal, or else to the newest live `styr codex` in that folder that has no real session yet. Passing `--enable hooks` or `-c features.hooks=true` didn't make Codex run hooks in-process.
- **cmux and PATH order.** cmux's codex wrapper execs the first other `codex` on PATH. Depending on the order, that's Styrman's shim (and Styrman must not start cmux's wrapper again: it skips cmux's shim when `CMUX_CODEX_WRAPPER_SHIM` says it already ran), or cmux runs inside Styrman (and the shim continues after the binary styr chose, `STYR_AGENT_BIN`).
- **Terminal input isn't only typing.** Codex enables focus reporting, mouse tracking and the kitty keyboard protocol, and queries the terminal at startup; the replies arrive on stdin. The `/styr` catcher ignores focus/mouse reports and query replies and understands kitty-encoded keys. `STYR_KEYLOG=<path>` logs raw terminal input for diagnosis.
