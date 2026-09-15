# Offer `claude --resume <id>` on a terminal whose shell is gone

*2026-09-10. #18 step 2. Step 1 shipped in `888b66e` (`src/claudesess.rs`).*
*Status: implemented 2026-09-10. Every task's revert-check was performed and*
*its observed failure recorded in the test's own comment.*

## Why now

Prerequisite of the container work (spec
`2026-09-10-container-image-design.md`): a container restart is, from roost's
point of view, the same event as a host reboot, and this is what makes that
survivable. It is equally useful on the host, which is the case it was written
for.

## The state of play, verified in the code

- `claudesess::record_from_hook` runs on every `SessionStart`/`Notification`/
  `Stop`, before the notification gate, and writes
  `$ROOST_STATE_DIR/claude/<key>/<session>.json` atomically at 0600.
- `claudesess::recorded()` has **exactly one caller outside its own module and
  it is a test** (`session.rs`, inside `#[cfg(test)]`). The id is written and
  read by nobody.
- `session::end_session` already calls `claudesess::forget`, so a record only
  outlives a session that was *not* ended — i.e. one lost to a reboot or a
  reaped socket. Resumable is therefore a subset of `lost_sessions`.
- `workspace::WsState.lost_sessions` is computed once at hub load
  (`hub::lost_terminal_sessions`, from positive evidence only) and filtered as
  sessions go live. The placeholder already renders a `.termlost` note for it.
- `term.rs:155` is the only production call of `launch::keystrokes`, typing
  `l.launch` and `l.session_id` into the shell the attach spawned.
- `session::LaunchRequest` is **not** on the wire — no `Serialize`.

## Design

### The id never crosses the wire

The client sends `StartTerminal { session, resume: true }`; the server looks the
id up. Same reasoning as session names, which CLAUDE.md already reserves to the
server: an id that arrives from the browser is a string the browser chose, and
it lands on a command line. The client is told only *that* a resume is on offer,
never the value.

### `Option<String>` becomes an enum, so the invalid state cannot be built

`LaunchRequest.session_id: Option<String>` is replaced by:

```rust
pub enum ClaudeSession {
    /// Minted by roost for a fresh launch — `claude --session-id <id>`.
    Fresh(String),
    /// Recorded from a hook — `claude --resume <id>`.
    Resume(String),
}
```

A bare `Option<String>` plus a `resume: bool` would make "resume a freshly
minted id" — a launch that always fails with *no conversation found* —
representable. This is server-side only, so it costs nothing on the wire.

`launch::keystrokes` gains the arm and keeps the shape it has:

| Launch | Session | Typed |
|---|---|---|
| `Claude` | `Fresh(id)`, valid | `claude --session-id <id>` |
| `Claude` | `Resume(id)`, valid | `claude --resume <id>` |
| `Claude` | none, or an id that fails its check | `claude` |
| `PrLoop` | any | unchanged |

### The two validators disagree, and the resume arm must use the right one

`launch::valid_session_id` is a strict lowercase v4 UUID — correct for `Fresh`,
because roost mints those itself. `claudesess::valid_session_id` is deliberately
looser (≤64 chars, no leading `-`, alphanumerics/`-`/`_`) because the id arrives
from another program's JSON and #18 chose not to pin Claude Code's format.

Gating `Resume` on the strict one would mean roost records ids it then silently
refuses to use — the resume degrades to a bare `claude` and the conversation is
lost with no message. So `claudesess::valid_session_id` becomes `pub` and the
`Resume` arm uses it. **The id is validated twice by the same rule**: once
before it is stored, once before it is typed.

### `relaunch` must stay unable to resume

#17 is explicit and `relaunch.rs` quotes it: relaunch records *the launch kind
only, never a resume*, because resuming continues a conversation whose last turn
may have been mid-edit. That property survives untouched here — `relaunch::record`
stores `req.launch` (`Claude`), and `hub::rearm_launches` rebuilds the request
with no session at all — but it now survives *by construction rather than by
absence*, so it gets a test of its own that would fail if a future change passed
the recorded id through.

### Where the offer is computed

`WsState.resumable_sessions`, alongside `lost_sessions`: computed once at hub
load and filtered by the same `retain` when a session goes live.

**Not derived in `snapshot_event`.** `WorkspaceView::claude_sessions` and
`show_hidden` both carry the warning: a snapshot goes out on every
`EditBuffer` — every debounced keystroke — so a filesystem read there is a read
per terminal per keystroke. `lost_sessions` has exactly the lifetime this needs
and is already computed in the right place.

### The client

`terminalPlaceholder` already distinguishes a lost shell. When the session is in
`state.resumable_sessions` **and** `claude` is in `LAUNCHES` (the same
`offered_for` gate the ✻ button uses — offering a resume where `claude` is not
on `PATH` buys a `command not found`), it adds one button that starts the
terminal with `resume: true`.

Offered, never automatic — #17, again. Enter still starts a plain shell; the
resume is a second, explicit control.

And the placeholder says nothing when there is no record. `claudesess.rs`'s
module doc is explicit that "no recorded session" must never be rendered as
"there was no session": hooks are per-project and opt-in, so absence means
*unknown*. The existing `.termlost` note makes no claim about Claude and stays
as it is.

## Tasks

Each ends with `cargo test -- --test-threads=1` green and a review before the
next.

1. **`ClaudeSession` + `keystrokes`.** The enum, the resume arm, the shared
   validator (`claudesess::valid_session_id` made `pub`), call sites in
   `term.rs`, `hub.rs` and `session.rs` updated. Tests: the table above, one row
   per assertion; a recorded non-UUID id (`abc123`, the one `claudesess`'s own
   test accepts) resumes rather than degrading; an id that fails the check types
   a bare `claude` and never a fragment of the id.
2. **`resumable_sessions`** through `WsState` → `WorkspaceView`. Tests: it is a
   subset of `lost_sessions`; a session with no record is absent; a session that
   goes live drops out; an unreadable state dir yields an empty list and no
   claim.
3. **`Intent::StartTerminal { session, resume }`** and `do_start_terminal`
   parking the launch. `#[serde(default)]` so an old client's message still
   parses. Tests: resume parks a `Resume` request; `resume: true` with no record
   parks nothing and still starts a plain terminal; the relaunch property above.
4. **The client button**, and `tests/browser/lostshell.mjs` extended — it
   already seeds the exact post-reboot state on disk, so the record is one more
   file beside it. A negative control with no record, which is what caught the
   original over-broad `lost_sessions`.
5. **Docs**: `docs/deploy.md` under *Projects and sessions*, and a line in the
   container spec's table moving this row from missing to shipped.

## What this does not do

Non-Claude terminals, scrollback, and projects with hooks off are all unchanged
and unrecoverable; the container spec says so and `docs/deploy.md` will repeat
it. Backup/restore of transcripts (#18 steps 3-4) is untouched, including the
absolute-cwd question in the transcript path, which this deliberately does not
depend on: roost resumes by **id**, and lets Claude Code find its own file.
