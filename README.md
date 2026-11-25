# Styrman

**Steer your coding agents from anywhere.**

Styrman is a personal "universal remote control" for AI coding agents (Claude Code, Antigravity CLI, Codex CLI). Type `/styr` in a running session and it shows up in the Styrman iOS app, where you can follow along, approve or deny tool permissions, send new prompts, and get notified when the agent needs you or is done.

- `styrd`: a Rust daemon on your Mac (launchd). It tracks sessions, persists history in SQLite, and serves a token-authed WebSocket API over localhost and Tailscale only.
- `styr`: a Rust CLI. It launches agents (`styr claude …`) and is the hook helper (`styr hook <event>`).
- Adapters: Claude Code first, then Antigravity (`agy`), then Codex, plus a generic PTY fallback.
- iOS app: SwiftUI, paired by QR code, with APNs push for "needs approval" and "done".

> Status: design phase. Logo concepts are in `assets/logo/concepts/`.
