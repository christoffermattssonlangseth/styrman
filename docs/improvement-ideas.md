# Styrman improvement ideas

Keep the native iOS session list and detail screen. Make it easier to spot what needs attention, act on a request, and get back to reading the conversation.

## iPhone layout

### Session list

- Give **Needs you** rows more space. Lead with the repo and the reason it needs attention, such as “Waiting to run `cargo test`.” Put the agent and elapsed time on a quieter line. Show the full path when it helps distinguish sessions in the same repo.
- Give errors their own place in the list. Include Codex in the empty-state instructions for sharing with `/styr`.

[Session list and row layout](../ios/Styrman/Views/SessionListView.swift)

### Agent colors and session header

- Try a lightly tinted banner below the navigation bar on the session detail screen. Use the existing palette: warm orange for Claude Code, blue for Antigravity, green for Codex, and gray for the generic terminal adapter. Put the repo, agent name, and session status in the banner so the color belongs to a useful part of the layout.
- Echo that color in a small agent badge or icon on the session list. Keep the conversation background neutral and give pending approvals and errors stronger, separate status styling.
- Tune the banner tint for light and dark mode. Keep labels readable without relying on color, and let the banner and session rows reflow at larger Dynamic Type sizes. Keep the wheel mark as the shared app identity.

[Agent palette](../ios/Styrman/Views/Theme.swift) · [Session header](../ios/Styrman/Views/SessionDetailView.swift#L47)

### Approval controls

- Put the active approval just above the composer so it stays reachable while reading older messages. Show the full command, the scope of each option, and the available decisions. Give persistent approvals their own visual treatment so one-time, session-wide, and saved choices are easy to distinguish where the agent offers them.
- Record the outcome in the timeline. If the terminal answers first, show that on the card. Let buttons stack at large text sizes.

[Permission card](../ios/Styrman/Views/SessionDetailView.swift#L195)

### Conversation and tool output

- Render headings, lists, and fenced code as distinct blocks. Group consecutive successful tool calls into a summary such as “Read 4 files · Ran 2 commands.” Keep failures visible and retain the existing sheet for full tool output.
- Follow live activity while the reader is near the bottom. Preserve their position when they scroll back and show a **Jump to latest** button as new activity arrives.

[Timeline and tool output](../ios/Styrman/Views/SessionDetailView.swift)

## Reliability

- **Prompt and approval delivery.** Give each prompt a request ID and distinguish sending, queued, delivered, and failed. Preserve drafts through disconnects and let users recover failed prompts. Tie approval feedback to the actual resolution and surface send failures so a card can recover after an interrupted connection. [Connection and send handling](../ios/Styrman/Services/DaemonConnection.swift)
- **Pairing and reconnecting.** Resume QR scanning after a dismissed confirmation and surface camera startup errors. Show the connection state and a retry action in the session detail screen, and identify the timeline as stale while it reconnects. [Pairing flow](../ios/Styrman/Views/PairingView.swift) · [Session detail](../ios/Styrman/Views/SessionDetailView.swift)
- **Terminal-wrapper compatibility.** Extend the existing Codex and Antigravity permission-menu fixtures to cover narrow terminals, wrapped commands, menu wording variants, and the phone and terminal answering nearly simultaneously. Run those checks when updating supported agent versions. [Adapter design notes](design.md#10-antigravity-agy-1517-as-measured)

## Suggested order

Start with the session rows, the tinted detail header, and the active approval layout. Follow with delivery and reconnect feedback, then refine the conversation layout and tool grouping.
