# resh — the modes an app declared once must survive an attachment too

The alternate screen was the visible half of a general defect: resh rebuilds a
client's terminal from a bounded byte log, and anything an app declares *once*
is gone from that log within minutes. `screen.rs` now fixes it for the screen
buffer. This describes the rest of the set — mouse reporting, bracketed paste,
focus reporting, cursor visibility, keypad and cursor-key encoding — which have
the same shape, break input rather than display, and are measurably broken
today.

## The problem, measured

Claude Code declares its whole terminal contract in the first 100 bytes it
writes, one sequence each, and never repeats most of them. From a capture of a
real run on this host, in emission order:

    ESC[?2004h   bracketed paste
    ESC[?1004h   focus reporting
    ESC[?2031h   tell me when the OS colour scheme changes
    ESC[?1049h   alternate screen          ← the half already fixed
    ESC[?1000h   report mouse press/release
    ESC[?1002h   …and drags
    ESC[?1003h   …and motion with no button held
    ESC[?1006h   report them in SGR encoding
    ESC[?25l     hide the cursor

xterm.js exposes its own view of these as `term.modes`, so what the browser
believes can be read rather than argued. Driving exactly that sequence into a
resh terminal and then reattaching:

    app has declared:   bracketedPasteMode: true,  mouseTrackingMode: "any",  sendFocusMode: true
    after reattaching:  bracketedPasteMode: false, mouseTrackingMode: "none", sendFocusMode: false

And the consequence, on the wire, for the same paste of `one\ntwo`:

    while in step:   "\x1b[200~one\rtwo\x1b[201~"
    desynchronised:  "one\rtwo"

That second line is the whole bug in one place. Bracketed paste is what lets an
app tell a paste from typing; without it the newline inside a pasted block is
an Enter. **Paste a three-line prompt into Claude after a reload and the first
line submits on its own**, with the remaining two landing in whatever comes
next. Mouse reporting fails the same way and more quietly: clicks and drags
never reach the app, and the wheel scrolls the browser's own scrollback instead.

**Corrected while building this.** The paragraph above is what a *restart*
produces, and a restart is what the measurement used. It is not what a reload
produces, because one ring per screen — landed with the alternate-screen fix —
changed the picture: an app declares bracketed paste and focus reporting
*before* it enters the alternate screen (`?2004h`, `?1004h`, then `?1049h`), so
those land in the normal ring, which stops growing the moment the app switches
and therefore never evicts them. They are already durable across a reload.

What a reload still loses is everything declared *after* the switch — mouse
reporting and cursor visibility — because that lives in the alternate ring,
which the app's own repaints turn over within minutes. Reverting this change
and reattaching reads `bracketedPaste: true, mouse: none, focus: true`: two
carried by the normal ring, one gone.

So the honest scope is **mouse reporting and cursor visibility on a reload**,
plus everything on a restart, which this does not fix. Paste survives a reload
today and breaks only across a restart. The browser test asserts the mouse mode
for exactly that reason: a paste assertion there passes with the whole change
reverted, which is how this correction was found.


Nothing about this is Claude-specific. `vim`, `less`, `htop` and anything else
that wants a mouse declares it the same way, once.

## What is different from the screen case

Two things, and both matter for deciding whether this is worth building.

**There is no reconciliation for a restart.** The screen fix has two halves:
synthesize the switch for a client attaching mid-app (fixes reload, reconnect,
second tab, ring turnover), and reconcile an exit whose entry this process
never saw (makes a restart *degrade* cleanly instead of garbling). Modes have no
second half. After a resh restart the mode state is simply unknown, and unlike
the screen there is no later event to reconcile against — the app will not
mention its modes again. The measurement above used a restart precisely because
it is the sharpest way to produce the state; the fix would *not* have repaired
that particular run. It repairs every attachment where resh watched the app
start: page reloads, laptop wake, a second browser, ring turnover.

**It fails softer.** A desynced screen paints the exit banner over a leftover
frame — visible, alarming, and it looked like corruption. A desynced mode
misroutes input, which reads as "the mouse doesn't work here" or "paste is
weird" and gets lived with. Worth building, but not the same severity, and the
restart gap means it cannot be sold as a complete fix.

## What changes

No new module: `screen.rs` already scans every `ESC[?…h/l` and already owns the
question "what does an attaching client have to be told". Three changes inside
it.

**The scanner reports more than screens.** `Switch` becomes an enum — a screen
switch as today, or a plain mode set/reset carrying its number. Nothing about
the parsing changes; the allowlist below decides what `Screens` does with each.

**`Screens` keeps a mode table.** Last value wins per mode number, because that
is exactly what a terminal does. It is not per screen buffer: xterm.js holds
these globally, so one table is the faithful model.

**`replay()` emits the table last.** Order is: the normal ring, then the screen
switch and the alternate ring, then the mode state. Last-write-wins puts the
tracked state after any stale copies still sitting in either ring, which is the
property that makes this robust rather than another thing that can be evicted.

### The allowlist, and why it is an allowlist

Only modes whose set and reset are pure sticky flags — no side effect on screen
content, cursor position or geometry — may be replayed:

| | |
|---|---|
| 1, 66 | application cursor keys / keypad — arrows and the numeric pad send different codes |
| 7 | wraparound |
| 25 | cursor visibility |
| 1000, 1002, 1003 | mouse reporting level |
| 1005, 1006, 1015, 1016 | mouse coordinate encoding |
| 1004 | focus reporting |
| 2004 | bracketed paste |

Deliberately excluded, each because replaying it *does something*:

- **3** (DECCOLM) clears the screen in this emulator. Replaying a mode to
  restore state must never destroy the state it is restoring.
- **47, 1047, 1049** belong to the screen logic, which owns their ordering and
  their cursor semantics.
- **1048** saves and restores the cursor. Replaying it moves the cursor.
- **6** (origin mode) is meaningful only together with the scroll region
  (`ESC[…r`), which nothing tracks. Half of a pair is worse than neither.
- **2031** is accepted by the allowlist grep and ignored by this emulator —
  `grep -c 2031 static/vendor/xterm.js` is 0 — so tracking it buys nothing
  today. Harmless to include later if xterm.js gains it.

### The keyboard protocols are out of scope, and that is safe here

The same stream carries `ESC[>4m` (xterm's modifyOtherKeys) and `ESC[<u` (the
kitty keyboard protocol). Both are *stacks*, not flags — push and pop — and a
bounded log cannot tell resh how deep the stack is, the same argument that
forced the screen switch to be tracked rather than replayed. They are excluded,
and for this client that costs nothing: the vendored xterm.js implements
neither (`grep -c modifyOtherKeys static/vendor/xterm.js` is 0), so the browser
never held the state there is nothing to restore. This stops being true if
xterm.js is ever upgraded into supporting them, which is worth a comment in the
code rather than a mechanism now.

## Testing

Unit, in `screen.rs`: last-value-wins per mode; a mode set then reset is not
replayed; the allowlist actually excludes 3, 1048 and the screen modes; a mode
sequence split across two reads still lands; multi-parameter sequences
(`ESC[?1000;1006h`) set both.

Browser, extending `tests/browser/altscreen.mjs` or beside it — and it must use
the **ring-turnover** path, not a restart, since a restart is the case this
does not fix. The assertion pair is the measurement above, which is already
known to discriminate: after reattaching, `term.modes` must match what the app
declared, and `term.paste("one\ntwo")` must go on the wire wrapped in
`ESC[200~`…`ESC[201~` rather than raw. Both halves were measured before writing
this, one against each state, so neither can pass vacuously.

And the standing rule: revert the fix, run it, read the failure, restore.

## Risks

- **More synthesized bytes on every attachment.** The mode table is at most a
  dozen short sequences and is emitted once per attach, after content, so a
  wrong entry cannot corrupt the screen — only the input contract, which is the
  thing being repaired.
- **An app that changes modes while resh is down** leaves the table stale in
  the same way the screen's does. Bounded by the same window, with the same
  answer: what resh has never observed, it does not claim to know.
- **The allowlist will drift** as terminals gain modes. It is a list of numbers
  with reasons attached, which is the cheapest thing to keep honest, and the
  exclusions are the half that matters — a new mode is only dangerous if
  someone adds it without asking what its reset *does*.
