# Chat v2: a phone-first chat view over the same Claude session

*2026-11-22. Issue [#209](https://github.com/PeterKnego/roost/issues/209)
carries the decisions: a second **view** of the same session, never a second
session. This records the shape as built and what the screen parser trusts.*

## Sources

| What | From | Code |
|---|---|---|
| Conversation | Claude Code's own `~/.claude/projects/<cwd>/<session>.jsonl` | `chat::Transcript` — incremental tail (3 MB initial, 4 MB per poll, 400 entries kept), tool results folded into their tool row by `tool_use_id` |
| Which transcript | `~/.claude/sessions/<pid>.json`, accepted only when its `procStart` equals `/proc/<pid>/stat` field 22 (a reused pid cannot hand over another conversation); `claudesess::recorded` as fallback | `chat::locate`, `confined()` keeps the path inside `~/.claude/projects` and `.jsonl` |
| What is live | a `vt100::Parser` in the session, fed by the pump beside `screens.ingest`, resized with the PTY | `session::screen_snapshot`, `chat::live_from_rows` |

## The screen parser trusts little

`parse_prompt` returns a menu only when:

- the last rows carry a footer hint (`Esc to cancel`, `Enter to confirm`,
  `Enter to select`) — or, the one hintless menu, AskUserQuestion's closing
  **Review your answers** over a list ending in `2. Cancel`;
- a `❯` sits on an option row that is not the input box between two rules;
- numbered options count 1..n in order (a rule is stepped over only when an
  option follows it, for AskUserQuestion's "Chat about this").

Anything else offers no buttons; the terminal is always one tap away.

## Input

Into the same PTY as keystrokes, so the terminal stays the source of truth:

- **send:** control characters stripped (but `\n` and `\t`), bracketed paste if
  the app asked for it, 60 ms, then `\r`. Capped at 64 KB.
- **choose:** re-parse the screen when the request arrives; the label must
  still match, then arrows from the selected option to the chosen one and
  `\r`, in application-cursor form when the app set it.
- **key:** esc, enter, tab, shift+tab (`\x1b[Z`), ctrl+c, arrows.

## Socket

`/ws/{project}/chat/{session}`: Origin-checked like every browser socket;
attaches as a subscriber and **never sends a size**, so it cannot narrow the
PTY (`session::min_geometry` counts only clients that resized); spawns
nothing; `catch_unwind` around the loop so no panic escapes the thread.

## Client

`static/chat.js`, loaded after `app.js`. Opened from a 💬 pane icon on a
terminal running Claude. Full screen under 900 px (native momentum scroll,
pinned to the bottom unless scrolled up, ↓ pill), right-hand panel above.
Web links open in a new tab; paths open in roost with a "Chat" pill back.

## Tests

`src/chat.rs`: 18 unit tests over screens captured from Claude Code 2.1.327
and 2.1.330 (permission prompt, trust dialog, AskUserQuestion at phone width,
the multi-question review), transcript tailing, locate's pid-reuse refusal
and the paste filter (both revert-checked). No browser test yet: it needs a
fake Claude, listed as a follow-up in #209.
