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

## Attachments

A 📎 in the composer (on a phone: photo library, camera or files), a pasted
screenshot, or files dropped on the chat. Two routes, by what Claude can do
with the file, and no new endpoint:

- **A picture Claude can see** (PNG, JPEG, GIF, WebP) goes to `POST /paste`,
  exactly as a screenshot pasted onto the terminal: stored outside the
  project, its path bracket-pasted into Claude's input, where it becomes
  `[Image #n]` on the next message. A message of only pictures is sent with
  Enter alone; text after a picture gets a leading space.
- **Anything else** (a PDF, a log, an iPhone HEIC `/paste` cannot name) goes
  to `POST /upload` at the project root, and `@name` is written into the
  message.

Each file is its own request with its own chip (thumbnail or 📄, progress,
✓ or the server's refusal); Send waits for every upload to finish.
Thumbnails are `data:` URLs because the page's CSP allows `img-src 'self'
data:` only.

## Subagents

An `Agent` (or older `Task`) card opens that subagent's own conversation.
Claude Code 2.1 writes each subagent to
`<session>/subagents/agent-<id>.jsonl`, beside an `agent-<id>.meta.json`
holding `toolUseId` (the call that started it), `description`, `agentType`
and, once done, `finished: true`. The record is written at spawn, so a
running subagent is found too, and a nested subagent's record sits in the
same directory keyed by its call in its parent's file — one lookup serves
every depth.

- **Who picks the file.** The client sends only `{"t":"agent","tool":id}`
  (or `null`). The id must look like one the API mints
  (`[A-Za-z0-9_]{1,64}`) and is only compared against records; the
  directory comes from the main transcript `locate` already confined, and
  the file found is confined to `~/.claude/projects` again before it is
  read. A browser never names a path.
- **One at a time.** The server streams one subagent (`agent` for its
  record when it changes, `agent_entries` like `entries`), re-reading the
  record every 3 s so *running* becomes *finished*. The client holds the
  stack, one history entry per level, so the back button and the phone's
  back gesture climb out a level at a time and a reconnect re-asks for the
  innermost.
- **Read-only.** What the chat types goes to the PTY, which is the main
  conversation, so the composer is hidden while a subagent is shown. The
  live menu stays: a subagent's permission prompt is drawn on the same
  screen and is answered from there.
- **Not found** (an older Claude Code that kept sidechains inline, or a
  record not written yet) says so and points at the terminal.

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
and the paste filter (both revert-checked). Subagents: `locate_agent`'s
lookup, confinement and id-shape refusal, sidechain reading and Agent-card
marking (each revert-checked), and `tests/browser/chatagents.mjs`, which
drives the view against a fake `claude` the real `locate` finds by `/proc`.
