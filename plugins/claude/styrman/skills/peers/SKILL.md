---
name: styrman-peers
description: Talk to another AI coding agent the user has shared with Styrman (Claude, Codex, agy), only when the user asks you to, e.g. "ask Codex to review this", "check with Claude", "hand this to agy".
---
# Talking to other agents through Styrman

The user can share several coding-agent sessions with Styrman (Claude Code, Codex, agy), often working in different folders. Two commands reach them:

- `styr peers` lists the other shared sessions: id, agent, status, folder, and the first message.
- `styr ask <session> "<message>"` types your message into that session and prints its reply when its turn ends, which may take minutes. `<session>` is an agent name (`codex`, `claude`, `agy`) or an id from `styr peers`.
- An agent name means that agent **in your folder**: the one running there, or, if none is, a new one Styrman starts there for this (in a new terminal tab, shared with the user's phone). This is the normal way to consult another agent about your work. Don't pick a session working in another folder unless the user asks for that one; to reach it, use its id.

Rules:

- **Only when the user asks you to** consult, ask or hand off to another agent. Never on your own initiative.
- Write the message so it stands on its own: the other agent doesn't see your conversation. Include file paths, what you want back, and any constraints, such as "read only, don't change files".
- The other agent keeps its own approvals; you can't make it run anything without the user approving.
- Report the reply to the user in your own words, and say which agent it came from.
- If `styr ask` says the other agent is waiting for your answer, or a limit is reached, tell the user instead of retrying.
- A message that starts with "Message from …, sent through Styrman" is another agent asking you something. Answer it normally; your reply goes back to it automatically.
