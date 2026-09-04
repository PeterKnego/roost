# roost — Claude notification hooks, toggled from the bell

Makes "Claude finished" and "Claude needs you" notifications a thing a user
switches on per project with one click, instead of a JSON snippet they paste
by hand. roost writes the two hook entries into the project's personal Claude
settings file, reads that file back to show whether they are there, and ships
the command those hooks run.

## The problem this solves

The README says a notification is raised when Claude finishes a task or a
hook needs a decision. The mechanism exists end to end: `roost notify` emits
an OSC 777 sequence to the controlling terminal, the PTY pump parses it, the
notice reaches every client's bell, and the service worker turns it into an
OS notification that focuses the right terminal. But Claude Code emits
nothing itself. The docs say what the README leaves out: add a `Stop` and a
`Notification` hook running `roost notify` to the project's settings.
Measured on 2026-09-03: no settings file on this host, global or in any
project under either root, has such a hook, and the notice store holds an
empty list. The feature has never fired here because nobody pasted the
snippet.

Two things make the snippet as documented the wrong thing to paste anyway:

- `roost notify` exits 1 when it has no terminal to write to, deliberately,
  so a misconfigured hook fails loudly. A `Stop` hook that exits 1 shows a
  visible error in every Claude session that runs *outside* a roost terminal,
  such as one started over plain ssh in the same checkout. A project-level
  hook has to be silent there.
- Fixed strings ("Claude", "needs your input") cannot say what Claude wants.
  The hook receives JSON on stdin naming the notification type and, on
  `Stop`, the last assistant message.

Changing a user's Claude settings is the user's decision. roost therefore
never installs anything on its own; it shows the state and offers the
switch.

## What changes, in one sentence

A `roost claude-hook` subcommand turns Claude Code's hook JSON into a
notification and is silent elsewhere; a new `claudehooks` module reads and
rewrites the project's `.claude/settings.local.json` around exactly the
entries roost owns; one intent flips them, one snapshot field reports them,
and the bell shows the state and carries the switch.

## Decisions taken with the user (2026-09-03)

- **Which file:** `.claude/settings.local.json`, the per-project file Claude
  Code itself keeps personal and gitignored. Nobody who clones the repo
  inherits a hook that runs roost, and the toggle never dirties `git
  status`. The committed `.claude/settings.json` and the global
  `~/.claude/settings.json` are never written.
- **Where the control lives:** on the bell. The bell wears a state mark and
  the notice panel gains a first row with the switch. No new header icon.
- **Which events:** `Notification` (needs input) and `Stop` (finished).
  Subagent stops, auth and quota notices are noise and are not hooked.

## Design

### 1. The hook command: `roost claude-hook`

Reads one JSON object from stdin, the shape Claude Code pipes to every
command hook (`hook_event_name`, plus per-event fields), and either emits
one notification through the same path `roost notify` uses, or does nothing.
It always exits 0. Its stdout is never used (Claude Code reads hook stdout
as decisions; this hook decides nothing), so the sequence goes to `/dev/tty`
exactly as `roost notify` does, via the same `choose_sink`.

*Amended 2026-09-04:* a hook has no `/dev/tty`. Claude Code spawns hooks in
their own session, so the open fails and the sequence went nowhere on every
real event; the manual check in the plan used `setsid` and never a real
Claude. `tty()` now falls back to the controlling terminal of the nearest
ancestor process (`cli::ancestor_tty`, via `/proc`), which is the `claude`
process on the dtach pty roost reads. See *docs/notifications.md*.

| Input | Title | Body |
|---|---|---|
| `Notification`, `notification_type` = `permission_prompt` | Claude needs you | wants permission to run a tool |
| `Notification`, `idle_prompt` | Claude needs you | is waiting for your input |
| `Notification`, `agent_needs_input` | Claude needs you | an agent needs your input |
| `Notification`, `elicitation_dialog` or `elicitation_url_dialog` | Claude needs you | is asking a question |
| `Stop` | Claude finished | first line of `last_assistant_message`, or empty |
| any other event or type | nothing | |

Title and body pass through `osc::sanitise` and the existing caps, as
`roost notify` does. The `Stop` body is additionally cut to the first line
and 120 characters: it is a glance, not a transcript.

Silence rules, all exit 0 with no output:

- `ROOST_NOTIFY` is not set: this Claude is not running in a roost terminal
  (the variable is exported into every terminal roost spawns and nowhere
  else). Not an error; the hook is project-level and the project is also
  used outside roost.
- stdin is not valid JSON, or names an event this command does not handle.
- `ROOST_NOTIFY` is set but no terminal can be written (the `Nowhere` sink).
  This is the one case `roost notify` treats as a loud failure. Here it is
  still exit 0, because exit 1 from a `Stop` hook is a visible error in the
  transcript and the situation (a subagent's hook, say) is not the user's
  misconfiguration. It is logged to stderr in one line so a user who looks
  can see it.

`roost notify` keeps its loud-failure behaviour unchanged; it is the
interactive, hand-written case.

### 2. The settings writer: `src/claudehooks.rs`

**What roost owns.** A hook entry is roost's if and only if its `command`
is exactly `roost claude-hook`. Nothing else in the file is roost's:
other hooks on the same events, other events, every other key, and the
formatting of all of it as far as `serde_json` preserves it (object key
order is preserved with `serde_json`'s `preserve_order` feature, which this
change turns on; it is a feature flag, and the `indexmap` crate it uses is
already in the lockfile through `toml`; whitespace and comments are not
preserved, and Claude Code's own writer does not preserve them either).

**The entries written**, one per event, each its own matcher group:

```json
{
  "hooks": {
    "Notification": [
      { "hooks": [ { "type": "command", "command": "roost claude-hook", "timeout": 5 } ] }
    ],
    "Stop": [
      { "hooks": [ { "type": "command", "command": "roost claude-hook", "timeout": 5 } ] }
    ]
  }
}
```

A group of roost's own rather than an entry appended to a foreign group, so
that disabling can remove it without deciding what to do with a group it
shares. `timeout` is 5 seconds: the command writes one escape sequence.

**Reading** returns one of three states, never two:

- `Present`: both events carry a roost entry.
- `Absent`: the file does not exist (`NotFound`), or parses and carries a
  roost entry on fewer than both events. A single event's entry is "not
  enabled", and enabling adds the missing one; it never duplicates one
  that is there.
- `Unknown`: the file exists but cannot be read (any error other than
  `NotFound`) or is not a JSON object with an object-or-absent `hooks`.
  This is the "could not determine" outcome CLAUDE.md requires to stay
  separate from "absent".

**Writing**, for `on = true` and `on = false`:

- `Unknown` refuses with a message naming the file and the reason. roost
  does not rewrite a file it could not parse; that is how a hand-edited
  settings file gets destroyed.
- The path is `<project>/.claude/settings.local.json`, resolved with
  `projects::safe_resolve_parent` like every other creation path, with
  `.claude/` created if missing. A worktree project writes into its own
  `.claude/`, which is what Claude Code reads there.
- Enable: parse (or start from `{}`), ensure `hooks` is an object, ensure
  each event's array holds one roost group, adding it at the end if absent.
  Disable: remove every roost entry from every group of both events, drop
  groups left empty, drop events left empty, drop `hooks` if left empty.
- Serialize with two-space indentation and a trailing newline, the shape
  Claude Code writes.
- Write to `settings.local.json.tmp.<pid>` in the same directory, then
  `rename` over the target: atomic, as the rule on persistent evidence
  requires. The temp file takes the existing file's mode when there is one
  (Claude Code leaves these 0644; a user may have tightened it), else the
  process default.
- Before the first write that replaces an existing file, and only if no
  `settings.local.json.bak` exists yet, copy the current file to that
  name. The backup is the pre-roost state and is never overwritten, so a
  user can always get back to what they had before roost touched it.

### 3. Protocol

- Intent `SetClaudeHooks { on: bool }`. Handled in the hub like
  `SetShowHidden`: it applies to the project, not the connection.
- `WorkspaceView`, the snapshot carried by `Event::State`, gains
  `claude_hooks: Option<bool>`, `Some(true)`
  for `Present`, `Some(false)` for `Absent`, `None` for `Unknown`, computed
  by reading the file each time a snapshot is built. Nothing about it is
  stored in roost's own state.
- After a write, the hub rebuilds and broadcasts the snapshot to the
  project's clients, so every browser on the project sees the flip. A
  refused write sends the existing `Event::Error { msg }` with the reason,
  which the client shows as a banner.
- The watcher does not report `.claude/` (it is in `SKIP_DIRS`, the
  directories the tree never enters), so an edit made by hand or by Claude
  Code appears at the next snapshot: reconnect, refresh, or any other
  intent that rebuilds it. Live following is deliberately not added; the
  file changes rarely and the cost would be un-skipping a directory the
  tree and search both refuse for good reasons.

### 4. UI

- **The mark.** The bell gets a `data-claude-hooks` attribute of `on`,
  `off` or `unknown`, and CSS draws a small mark on the bell for each: an
  accent ✻ for `on`, a struck grey ✻ for `off`, a ? for `unknown`. (`on`
  originally drew nothing as "the quiet state"; amended 2026-09-04, because
  nothing cannot be told apart from a mark that failed to render, and the
  user asked to see enabled as well as disabled.) The existing unread badge
  is unaffected.
- **The row.** The notice panel's first row reads "Claude notifications for
  this project: on" or "… off" with one button, Disable or Enable. Clicking
  Enable swaps the row for a one-line confirmation, "Write two hooks to
  .claude/settings.local.json?" with Enable and Cancel, because this is a
  write into a file roost does not own. Disable asks the same way, naming
  the file. Both send the intent on confirm.
- **Unknown** shows "Claude notifications: cannot tell" and the reason from
  the server, with no button. The user fixes the file; roost does not.
- The bell's tooltip gains the state so it is readable without opening the
  panel.

### 5. Tests

Unit, `claudehooks.rs`:

- Enable on a missing file writes exactly the two-event document above.
- Enable is idempotent: a second enable leaves the bytes unchanged.
- Enable on a file with foreign hooks on `Stop` and an unrelated event,
  plus unrelated top-level keys, adds roost's groups and leaves every
  foreign byte of content in place, key order included.
- Disable removes only roost's entries: a foreign entry in the same event
  survives, an empty group and an empty event are dropped, and `hooks` is
  dropped when nothing is left.
- One event present and the other missing reads as `Absent`; enable adds
  only the missing one.
- Invalid JSON reads `Unknown`, enable and disable refuse, and the file is
  byte-for-byte untouched.
- An unreadable file (mode 000, skipped when running as root) reads
  `Unknown`.
- The backup is written once and not overwritten by a later write.
- The temp-and-rename leaves no `.tmp.*` file behind and preserves a
  0600 mode on the original.

Unit, `cli.rs`: each row of the table above maps to its title and body, an
unknown type or event emits nothing, missing `ROOST_NOTIFY` emits nothing,
and the exit code is 0 in every case including `Nowhere`.

Integration: a `SetClaudeHooks { on: true }` intent over the workspace
socket creates the file and the next snapshot carries `Some(true)`; `on:
false` empties it and carries `Some(false)`.

Browser, `tests/browser/claudehooks.mjs`: the bell shows `off` on a fresh
project; clicking Enable then confirm makes the file exist with both
events, the mark disappear, and a second browser tab on the same project
show the change; Disable reverses it. A fixture with invalid JSON in the
file shows the unknown mark and no button.

Every new test revert-checked, with the failure recorded in its comment.

### 6. Docs

- `docs/notifications.md`: a section on the switch, what it writes and
  where, and the `roost claude-hook` table. The hand-paste snippet stays
  as the escape hatch, updated to run `roost claude-hook`.
- `README.md`: the notifications paragraph says a Claude Code hook raises
  the notification and that the bell switches it on per project.
- `CLAUDE.md`, hard constraints: roost writes exactly one Claude settings
  file, the project's `settings.local.json`, touches only entries whose
  command is `roost claude-hook`, and refuses to write a file it could not
  parse.

## Security

- The intent is a websocket message, so it is behind the Origin check like
  every other state change; there is no new HTTP surface.
- The file path is fixed relative to the project and confined by
  `safe_resolve_parent`; nothing in the intent names a path.
- The command written is a fixed string; nothing from the client or the
  file is interpolated into it.
- The hook runs whatever `roost` resolves to on the shell's `PATH` at the
  time Claude fires it, which is the same binary the terminal's user would
  run by hand. If `roost` is not on that `PATH`, Claude Code reports the
  failed hook in its transcript; roost does not try to detect this at
  enable time, since it cannot see the shell's `PATH` from the hub.

## Out of scope

- Global hooks, committed project hooks, and the `AskUserQuestion`,
  `PermissionRequest`, `ExitPlanMode` and `Elicitation` events. Each is one
  more row in the table if wanted.
- Following `.claude/` live.
- Any settings pane beyond this one row; the backlog's settings item is
  separate.
- Answering prompts from the notification, which is what notch-911 does.
  roost's notification clicks back to the terminal; the answer is typed
  there.
