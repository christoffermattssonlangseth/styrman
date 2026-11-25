---
description: Share this session with the Styrman phone app (`/styr off` stops sharing)
argument-hint: "[off]"
---
<!--
Normally you never get here: Styrman's UserPromptSubmit hook intercepts `/styr`
before it expands, so no model call happens. Reaching this text means the hook got
no answer from the styrd daemon.
-->
The user typed `/styr $ARGUMENTS` to share this session with the Styrman iPhone app, but the Styrman daemon (`styrd`) did not answer, so nothing was shared. Reply with one short sentence: Styrman isn't running; start it with `styrd` (or `launchctl kickstart gui/$(id -u)/com.styrman.styrd` if installed as a launch agent), then run `/styr` again. Do not use any tools.
