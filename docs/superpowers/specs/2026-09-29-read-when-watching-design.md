# A terminal you are looking at has nothing unread

*2026-09-29. Status: designed, not implemented. Issue
[#129](https://github.com/PeterKnego/roost/issues/129). The issue left three
decisions open; they were settled when it was picked up, along with a fourth —
that the behaviour is a setting — and this spec records all four and what they
cost.*

## What is wrong

Notices for a session are marked read in one place, `markSessionNoticesRead`,
reached only through `focusSession`. That covers every *routing* gesture — a
tab-strip click, an in-page notice click, the service worker's `focus` message
after a desktop-notification click, a cold `#session=` load — and nothing else.
So:

1. **Returning to the page.** Switch to roost's browser tab (or window) while
   the notifying terminal is already the active tab and focused, and the bell
   count, title prefix, favicon dot and tab-strip dot all stay lit for output
   that is on the screen in front of you. The `visibilitychange` handler only
   calls `probeControl()`, and no `ActivateTab` happens because the tab is
   already active.
2. **Arriving while watching.** A notice raised by the terminal you are typing
   in arrives unread, and raises a desktop banner for it as well.
3. **Clicking into an already-active terminal** from elsewhere on the page
   (the editor, the tree). The same code comment calls this out for
   `lastFocusedSession`: it fires neither `ActivateTab` nor `focusSession`.

The rule `markSessionNoticesRead`'s own comment states — "cleared when that tab
becomes active" — holds for every route except these.

## The rule

> While `read_when_watching` is on, a notice for the session you are
> *watching* is read: those already unread are marked read the moment you
> start watching, and a new one is marked read on arrival and raises no
> desktop banner.

### Watching

A session is watched when all three hold:

- `document.visibilityState === "visible"`
- `document.hasFocus()` — a roost window visible behind another app is not
  being read
- `document.activeElement` is inside that session's `.termhost`, and that
  host is rendered (`getClientRects().length > 0`)

The third is **keyboard focus**, not `lastFocusedSession`, and that is the
decision "keyboard-focused only" asked for. `lastFocusedSession` means "the
terminal a mention is aimed at" and deliberately outlives focus moving to the
editor; with Claude working in the right pane while you type in the middle
one, its notices are exactly the ones you have *not* read. With two panes
each showing a terminal, only the focused one is watched; the other keeps its
dot.

"Rendered" covers both ways a termhost can be off-screen: the pooled host of a
non-active tab, and a collapsed pane on a phone (`revealPane`). A pane state
roost does not model directly is thereby read off the DOM, which is the thing
the user actually sees.

### When it is checked

One function, `watchedSession()`, returning a session name or `null`, called
from:

| Event | Covers |
|---|---|
| `visibilitychange` (to visible) | case 1, switching browser tabs |
| window `focus` | case 1, switching OS windows while the page stayed visible |
| `focusin` on a termhost (the existing listener) | case 3 |
| `onNotice` | case 2 |

The first three call `markSessionNoticesRead(watchedSession())`; it is already
idempotent and sends nothing when there is nothing unread, so redundant firings
(a tab switch raises both `visibilitychange` and `focus`) cost nothing.

`onNotice` pushes the notice as today, then, if `n.session === watchedSession()`,
sends `MarkNoticeRead` and **skips the OS notification**. The notice still
exists and still appears in the panel as read — history is not the thing being
suppressed, attention is.

Explicit routing gestures (`focusSession`) keep clearing regardless of the
setting: clicking a dotted tab or a notice is asking for it to be read.

## The setting

`read_when_watching`, bool, **default on**, writable in **project and global**
scope (`PROJECT_KEYS`).

- **Why a project may set it.** The `GLOBAL_ONLY_KEYS` test is what a hostile
  checkout could do with it. This one decides whether a notice from a terminal
  you are typing in is shown as unread and bannered. It grants nothing, reveals
  nothing and raises no ceiling — the same argument `autosave` and
  `follow_tree` are project-scoped under. A checkout turning it off gets you
  today's behaviour; turning it on gets you this spec's.
- **Why on.** Off is the behaviour the issue is a bug report about.
- **Live, not embedded.** `followSettings()` already re-reads `autosave` from
  every snapshot; this key follows the same line, so its dialog row has
  `reload: false` and there is no `data-` attribute on the page. A notice can
  only be "watched" once a snapshot has laid out the panes, so there is no
  window in which the page needs the value before the first `State`.
- Documented in `docs/deploy.md` beside `autosave`, including its scope.

Server side this is the ordinary new-key checklist: `RawConfig`, `Settings`,
`load`, `PROJECT_KEYS`, the bool arm in `validate`, a `push` row in
`settings_view` (and the key-order assertion that pins the row list).

### Focus nobody gave (found in review)

`mountTab` focuses every terminal it mounts, including one mounted because
*another client* activated it. An idle second device with roost in front
would then hold keyboard focus in that terminal and read its notices for you —
your laptop's dot clears, and nobody touched the desktop. So focus roost gave
by itself (`autoFocused`) is not watching until a hand lands in that terminal
(`pointerdown` or `keydown` inside it). A mount that follows this page's own
`focusSession` — a tab click, a notice, a desktop-notification click — is a
hand (`handFocus`). The alternative, `mountTab` focusing only on local
activation, would also stop a remote layout change stealing the editor's
focus; it changes more than this feature, and is left for its own issue.

## What it does not do

- **Other windows still banner.** Suppression is per client. The window you
  are watching sends `MarkNoticeRead`; a second window — another device, or a
  second tab in the same browser — received the same `Notice` at the same
  moment, is not watching, and raises its own banner before the read state
  reaches it. Fixing that needs the server to know who is watching, which is
  also what Web Push (`2026-09-16-web-push-design.md`, not built) will need;
  this spec does not pre-empt that design.
- **A brief dot elsewhere.** Those other clients show the notice unread until
  the `Notices` rebroadcast lands. Server-authoritative read state is the
  point; a transient dot is its cost.
- No change to `notify.rs`, the store, or any intent. `MarkNoticeRead` exists.

## Testing

No Rust test reaches `app.js`, so the behaviour is a browser test,
`tests/browser/readwatch.mjs`, raising notices the production way (an OSC 777
printed by a real shell in a real dtach session), as `notices.mjs` does. Every
assertion reads the **server's** read state — a freshly connected page's
`Notices`, or the store via a second page — not the dot, which is a client
cache (`notices.mjs` section E is the precedent for why).

1. **Return.** Page A with terminal `t` focused. Open page B (A goes hidden).
   Raise a notice in `t`; assert unread. `bringToFront(A)`; assert read.
2. **Not focused, not read.** Same, but focus A's editor before hiding; bring
   A back; assert still unread. This is the test that distinguishes keyboard
   focus from `lastFocusedSession` — without it, a `lastFocusedSession`
   implementation passes 1.
3. **Arrival.** A visible, `t` focused, OS permission granted via CDP
   (`Browser.grantPermissions`) and `swReg.active.postMessage` spied; raise a
   notice; assert read and no `notify` post. Then focus the editor, raise
   another; assert unread and one post — the pair, because "no post" alone
   also passes with permission silently denied.
4. **Two panes.** Terminals in two panes, focus one, raise a notice in each;
   only the focused one's is read.
5. **Setting off.** Project `read_when_watching = false`; repeat 1 and 3's
   first half; both unread, one banner.

Risks to settle first, before trusting any of the above: whether headless
Chromium reports `document.hasFocus()` true for a fronted page (if not,
`Emulation.setFocusEmulationEnabled`), and whether `bringToFront` fires window
`focus` or only `visibilitychange`. If the harness can produce only one of the
two, the test says which path it covers rather than implying both. Each test
gets the revert treatment: remove the new call site it covers, watch it fail,
record the failure in the file's header, restore.

Rust side: `config.rs` gets the default-on / either-layer test `autosave` has,
the `validate` refusal of a non-bool, and the row-order assertion updated.
