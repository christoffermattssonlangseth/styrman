# Developing Styrman

## Build and test

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

The integration plan for each agent, from their official docs, is in [`docs/design.md`](design.md) §1 and §4.

