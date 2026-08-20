# resh — the alternate screen must survive an attachment

Exiting Claude Code in a resh terminal leaves the screen garbled: the exit
banner and the shell prompt are painted over the middle of the app's leftover
frame, with fragments of the old screen still showing to the right of the
prompt. The same session exited in a local terminal is clean. This describes
why, and what has to change in `session.rs` for a full-screen app's terminal
*state* — not just its bytes — to survive a browser attaching.

## What is actually wrong

Claude Code is a full-screen app. Like `vim`, `less` and `htop`, it runs on the
**alternate screen**: it emits `ESC [ ? 1049 h` once at startup, paints frames
for however many hours it runs, and emits `ESC [ ? 1049 l` once at exit. Those
two sequences are the entire declaration of which buffer every frame in between
belongs to. Captured from a real run on this host:

    b'\x1b[?1049h' count= 1 at [543] of 3478
    b'\x1b[?1049l' count= 1 at [3132] of 3478

resh's replay is a raw byte ring: `Session::scrollback`, a `VecDeque<u8>` that
`push_scrollback` trims to `MAX_SCROLLBACK` (1 MB) by dropping bytes off the
front. Everything a client needs in order to render is assumed to be *in* those
bytes. For a full-screen app it stops being true a minute or two in, because
the one sequence that establishes the buffer is at the very front and every
repaint pushes it closer to the edge.

So a browser attaching to a session where Claude is already running gets a
replay — and then a live stream — that never switches buffers. It paints
Claude's frames into the **normal** buffer. Nothing looks wrong: the frames are
absolutely positioned and they fill the screen either way. The app and the
terminal now disagree about which buffer they are on, and nothing surfaces that
until the app exits.

At exit, `ESC [ ? 1049 l` reaches a terminal that was never switched. xterm.js
takes the normal-buffer branch it is already on, so instead of restoring the
pre-app screen it only restores the normal buffer's saved cursor — never saved,
therefore (0, 0). The exit banner prints from the top of the screen, over the
leftover frame, with no erase:

    row 1   │ (leftover frame line, untouched)
    row 2   │ Resume this session with:
    row 3   │ claude --resume 790980cd-…
    row 4   │ user@host:~/x$ eader        ← "eader" is the old frame showing past
    row 5   │ user@host:~/x$ █              a prompt that never erased to EOL

That is the screenshot, line for line, including why text survives to the right
of the prompt.

A local terminal cannot get into this state. It processed `?1049h` when Claude
started and has held the two buffers ever since. resh is the only participant
that reconstructs a terminal's state from a byte log, and a byte log with a
bounded head cannot carry a one-shot declaration made an arbitrary time ago.

## Two ways in, and both happen on this host

**The ring turns over.** Every Claude repaint is kilobytes; a streaming
response is a repaint several times a second. Any reattachment after roughly
the first megabyte — a page reload, a laptop waking, a second tab, `app.js`'s
reconnect path, which calls `term.reset()` and replays from scratch — lands on
the wrong buffer.

**resh restarts.** The scrollback lives in this process's memory, so a restart
starts every session's ring empty; `dtach` keeps the shell but replays nothing.
Probing the live `resh/term` session on this host — Claude running in it for 21
hours — a fresh attachment receives:

    replay bytes: 0 frames: 1
      b'\x1b[?1049h' -> 0

That session is desynchronised right now and will garble when it exits. Every
deploy does this to every terminal with a full-screen app in it.

The second one cannot be repaired by watching harder. `dtach -r winch` makes
the app repaint on attach, and a repaint does not re-declare the mode —
measured, by resizing a live Claude the way a reattach does:

    b'\x1b[?1049h' whole: 1  after the winch repaint: 0

Once the declaration is lost, no amount of asking the app brings it back. It
has to be remembered, or reconciled at exit.

## Reproduction

`tests/browser/altscreen.mjs` (new) drives a real Chromium against a real resh
with real `dtach`: mark the normal screen, enter the alternate screen and paint
a frame, spend a megabyte of output, reattach, then leave the alternate screen.
Against today's `master`:

      ok    the browser followed the app onto the alternate screen
      ok    the frame is on the alternate screen and the normal screen is untouched behind it
      ok    the ring has turned over
      ok    reattached
      FAIL  the replay put the browser back on the alternate screen the app is still on
      ok    leaving the alternate screen lands on the normal one
      FAIL  the pre-app screen came back — the exit did not print over the app's leftover frame
      FAIL  the app's own output is off the screen, not sitting under the exit banner

It asserts on the **viewport**, not the buffer. A scrollback-wide search finds
the pre-app marker sitting far above the visible screen and passes in exactly
the broken case — the vacuous pass CLAUDE.md keeps warning about, and one this
test hit while being written.

No Rust test can reach any of this: the emulator is in the browser.

## What changes

Three pieces, in order of how much they buy.

### 1. Track the buffer, synthesize it on attach

A small scanner in the pump thread, alongside `osc::Parser` and for the same
reason — it must survive a sequence split across two 8 KiB reads. It watches
for CSI sequences with a `?` prefix and a final `h`/`l` and looks for 1049,
1047 or 47 among the (possibly multiple, `;`-separated) parameters. It records
one bit — on the alternate screen or not — plus the exact sequence that got us
there, so the replay can re-emit that one rather than assume `1049`.

`attach` then prefixes the replay with that sequence when the session is on the
alternate screen. The declaration is now generated at attach time and can never
age out.

This alone fixes the reported bug for every case where resh watched the app
start.

### 2. Split the ring by buffer

Today the two screens share one 1 MB ring, so an app on the alternate screen
evicts the *normal* screen's history within a minute or two. Attach a browser
to a session mid-Claude and your shell history is gone — replaced by frames
that would not have been in a local terminal's scrollback at all, because a
local terminal never puts alternate-screen output into scrollback.

Keep two rings: normal-buffer bytes, and alternate-buffer bytes cleared each
time the alternate screen is entered. Replay is: normal ring, then (if on the
alternate screen) the enter sequence, then the alternate ring. Neither switch
sequence is stored in either ring — they are synthesized.

The alternate ring can still evict its own head, so a replayed frame can start
mid-paint and look torn until the app repaints. Optional follow-on: force one
repaint after the replay by jiggling the PTY size (`rows - 1`, then back —
Linux only signals `SIGWINCH` on an actual change), which is the same trick
`dtach -r winch` uses. Left out of the core change because it makes every
attachment poke the app.

### 3. Reconcile an exit we never saw the entry for

After a restart, resh's tracked state is not "normal" — it is **unknown**, and
the codebase's own rule is that "I could not determine X" is a third outcome
rather than a synonym for false. What is knowable is that every client attached
to that session is on the normal buffer, because resh never sent them anything
else. So when an alternate-screen *exit* arrives while resh has never seen an
entry for that session, the exit cannot mean what it says: rewrite it to a
clear-and-home, so the banner lands on a clean screen instead of over a
leftover frame.

This is a degradation, not a repair — the pre-app screen is genuinely gone with
the process that held it, and no design recovers it. It converts the garbled
screen into an ordinary one for the case that no in-memory tracking can fix.

Cost: an app that emits `?1049l` defensively without ever having entered the
alternate screen would get its screen cleared, during the window between a
restart and the first transition resh observes. That window closes the first
time any transition is seen on that session.

**Rejected alternative: persist the bit to the state directory.** It would
restore the pre-app screen state across restarts, and `registry` already writes
per-session markers atomically, so the mechanism exists. It is rejected because
a marker written before a restart is evidence about a process that is no longer
running: if the app leaves the alternate screen while resh is down, the marker
says "alternate", the next attachment is put on a blank alternate screen, and
the shell's output goes somewhere the user cannot see. Trading a recoverable
ugly screen for an unrecoverable blank one is the wrong direction, and it is
the same "acted on stale evidence" shape this codebase keeps getting bitten by.

## Scope

- `src/session.rs`: two rings instead of one, scanner state on the pump, replay
  construction in `attach`.
- One new module for the scanner (`screen.rs`), implementation first, tests at
  the bottom, `//!` explaining why it exists.
- `tests/browser/altscreen.mjs`: the reproduction above, flipping to green.
- No protocol change, no client change, no new dependency. `app.js`'s
  `term.reset()` on reconnect stays exactly as it is — it is what makes the
  replay authoritative, which is the property this change relies on.

Unit-testable without a browser: `push_scrollback` routing, the scanner across
split reads and multi-parameter sequences, and the bytes `attach` produces for
a session on the alternate screen. The browser test is what proves the whole
chain, and per CLAUDE.md each assertion gets checked by reverting the fix and
watching it fail — not as a thought experiment.

## Risks

- **Rewriting the stream (piece 3) is the only place resh alters what the app
  said.** It is confined to one sequence, in one state, and that state is
  exited permanently by the first transition observed.
- **Synthesizing an enter on attach when the app has since exited** cannot
  happen within a process: the exit is observed on the same thread that
  observes the enter, and both mutate the same state under the registry lock.
- **`?47`/`?1047` do not save the cursor.** Re-emitting the sequence that was
  actually seen, rather than normalising everything to 1049, keeps a `less`
  from being handed 1049's cursor semantics.
