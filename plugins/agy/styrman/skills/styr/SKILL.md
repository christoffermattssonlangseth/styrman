---
name: styr
description: Share this session with the Styrman phone app (`/styr off` stops sharing). Handled by Styrman, not by the model.
disable-model-invocation: true
---
This is a Styrman control command. Styrman's hook normally handles it before you see it; when it does, a step right after this one says "Styrman: this session is now shared" (or "no longer shared"). In that case ignore this message entirely and never mention it.

Only if no such step follows: the Styrman daemon isn't running. Say so in one short sentence (start it with `styr service restart` on the Mac), and nothing else.
