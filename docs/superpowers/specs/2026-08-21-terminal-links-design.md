# resh — clickable links in the terminal

Makes references printed in a terminal reachable: a URL opens in a browser tab,
a path to a file in this project opens as a tab in the middle pane. Adds two
xterm link providers, one websocket intent that resolves a path server-side, and
a handler for the hyperlinks an application declares for itself. Everything
resh guesses at is behind a modifier key; only what an application declared is
clickable outright.

## The problem this solves

A terminal is where this workspace prints paths, and it is the one pane where a
path is inert. Claude cites `src/hub.rs:426`, rustc points at the line it
refused to compile, `grep -rn` prints nothing but paths — and reaching any of
them means reading the string, moving to the tree, and walking down to it by
hand. Every other pane already does better: a markdown preview's link to another
project file opens it as a tab
(`2026-08-19-preview-links-and-images-design.md`), and the tree and changes list
have opened files on click since v3.

Two symptoms, one absence.

**A URL printed in a terminal cannot be opened.** Selecting it is the whole
route, and selecting is itself unreliable here: the moment a full-screen app
turns on mouse reporting the drag never reaches xterm at all — the reason this
codebase had to add an OSC 52 handler (`app.js`, `term.parser.registerOscHandler(52, …)`).
So the fallback for a link is a fallback that a running Claude session takes
away.

**An application's own hyperlinks are discarded.** The vendored xterm already
registers an `OscLinkProvider`, so OSC 8 sequences are parsed and their ranges
tracked. What is missing is the `linkHandler` option to do anything with one; it
defaults to `null` (`static/vendor/xterm.js`). Applications that go to the
trouble of marking their output are marking it into a void.

## What changes, in one sentence

Two link providers are registered on every xterm instance and gated on a
modifier key, the `linkHandler` option is filled in so the `OscLinkProvider`
xterm already registers stops being inert, and a new `Intent::OpenPath` resolves
whatever any of them matched against the project root under `safe_resolve`
before any tab opens.

## The gate, and why it is not the same for both kinds of link

The question this design exists to answer is whether a link should be marked and
clickable always, or only while a modifier is held. It has two answers, because
there are two kinds of link and they differ in who did the marking.

**An OSC 8 hyperlink is marked by the application.** There is no heuristic and
no false positive: the program said "these cells are a link" in a control
sequence. Underline it on hover and open it on a plain click.

**Everything else is resh guessing at plain text**, and the guess is behind
Cmd (macOS) or Ctrl (elsewhere). Four reasons, in order of weight:

1. **A plain click already belongs to the running application.** Claude Code
   turns on mouse reporting; xterm hands it the click. Intercepting a click
   because resh *thought* the cells under it looked like a path takes input away
   from the program the user is talking to. `app.js` already documents this
   boundary in the copy-on-select comment — "when a full-screen app owns the
   mouse, the drag never reaches xterm at all" — and a link that steals clicks
   is on the wrong side of it.
2. **The heuristic is wrong constantly on this output.** A `cargo test` run, a
   `grep -rn`, an `ls -R`, and most of what Claude writes are largely paths.
   Marked always, the terminal is underlined more often than not, and the
   underline stops carrying information.
3. **It is the convention every terminal the user already uses has settled on** —
   iTerm2, VS Code's terminal, GNOME Terminal, Windows Terminal all open links
   on modifier+click. Discoverability comes free from muscle memory that already
   exists, which is why no on-screen hint is specified here.
4. **Terminal text is chosen by whatever printed it.** A hostile repo's build
   output, a `cat`ed file, a string Claude was asked to echo — all of it lands
   in the same cells. Requiring a deliberate modifier+click makes opening
   something a user action rather than something the output can provoke. This
   does not replace confinement; it is the layer in front of it.

Cmd on macOS specifically, never Ctrl: Ctrl+click there is right-click
emulation. This is the same platform split xterm itself makes in
`shouldForceSelection` — `altKey` on Mac, `shiftKey` elsewhere.

### Arming and disarming

`provideLinks` returns nothing while the modifier is up, so no link exists to
hover. State is tracked on window `keydown`/`keyup`, and a change has to nudge
xterm to ask again: the `Linkifier` caches the last cell it resolved
(`_lastBufferCell`) and will not re-ask for the same position. `activate`
re-checks the modifier at click time, so an underline left stale by a missed
`keyup` — alt-tabbing away while holding it — still cannot open anything.

A `blur` on the window disarms. Otherwise a user who switches apps with the
modifier down comes back to a terminal that is silently armed.

## The risk this design cannot resolve on paper

**Whether xterm delivers link events at all while an application owns the
mouse.** The `Linkifier` registers its own `mousemove`/`mousedown`/`mouseup` on
the screen element and checks nothing about mouse mode. The core's own
`mousedown` on that same element calls `cancel(e)` when
`coreMouseService.areMouseEventsActive`. Both are listeners on one element, so
`stopPropagation` does not settle it, and reading further into minified source
would produce a conclusion rather than an answer.

This is decided in a browser, against a real Claude session, or not at all — no
`cargo test` reaches `static/app.js`. If links do not fire under mouse
reporting, the fallback is a capture-phase `mousedown` on the `.termhost` node
doing its own hit-testing via `term.buffer` and skipping the `Linkifier`
entirely. That fallback is more code, not different behaviour, so it does not
change anything else in this design.

## What matches

Two providers, URL registered first. Where they overlap — the path-looking tail
of a URL — xterm's `_removeIntersectingLinks` resolves it by provider index, so
registration order is the whole mechanism. xterm registers its own
`OscLinkProvider` at construction, ahead of both, which is the ordering this
design wants for free: where an application has declared a hyperlink, that
declaration beats anything resh would have guessed over the same cells.

**URLs.** `https?://` up to whitespace, with trailing `.,;:!?'"` trimmed and a
trailing `)` trimmed only when unbalanced, so a URL inside parentheses in prose
does not swallow the closing bracket. **http and https only.** `javascript:`,
`data:`, `file:`, `vbscript:` and everything else never produce a link — not a
refused one, not one at all. The same allowlist is applied again at `activate`,
and applied to OSC 8 destinations, which are chosen by the application and are
no more trustworthy than plain text.

**Paths**, in two forms:

- Relative containing a slash: `src/main.rs`, `./docs/backlog.md`,
  `docs/superpowers/specs/2026-08-21-terminal-links-design.md`.
- Absolute or `~/`-prefixed: `/home/claude/projects/resh/src/hub.rs`.

A trailing `:42` or `:42:7` is **consumed by the match and then discarded**. It
is consumed so that the whole reference underlines — a link that covers
`src/main.rs` and stops short of the `:42` the user is pointing at looks broken.
It is discarded because the viewer has nowhere to put it: the preview is a plain
highlight.js `<pre><code>` with no line numbers and edit mode is a `<textarea>`,
so there is no line to scroll to. Jump-to-line is a separate feature about line
addressing in the viewer, and it would swallow this one.

Bare filenames with no directory part — `backlog.md`, `main.rs` — deliberately
do not match. They are the biggest coverage gain available and the worst trade:
a repo has many `main.rs`, so resolution would have to guess, and the pattern
also matches version strings and the `foo.bar` in an error message.

A path wrapped across a row boundary does not match. Providers are asked per
row.

## Resolution happens on the server, and it happens before the tab opens

The client sends the matched span **verbatim** — `~/projects/resh/src/hub.rs:42`
and all — and does no parsing. One parser, in Rust, next to the confinement it
feeds.

```
Intent::OpenPath { text: String }
```

That is the whole client-side contract. The handler:

1. Strips a trailing `:line` or `:line:col`.
2. Expands a leading `~/` against the home directory.
3. If the result is absolute, strips the project root to get a rel; **if it is
   not under the project root, refuses.** An absolute path elsewhere on this
   host is a real path to a real file, and it is not this project's to open.
4. `projects::safe_resolve(project_dir, rel)` — the existing confinement, which
   also errors on a file that does not exist (`projects.rs`, and its own test
   asserts `safe_resolve(&alpha, "missing.txt").is_err()`). One call answers
   both "is this inside the project" and "is this really there".
5. Refuses a directory. Opening one would mean a second behaviour — reveal it in
   the tree — and that is a different feature.
6. On success, applies exactly the layout change
   `OpenTab { pane: MIDDLE, tab: File { rel, mode: Preview } }` makes, through
   `workspace::coerce_tab` and `open_buffer_for`, so a `.png` matched in
   terminal output coerces to an image tab identically to one clicked in the
   tree. Reusing that path rather than duplicating it is what keeps the two from
   drifting.

### Why the check cannot be skipped, given "optimistic" marking

Marking is optimistic: a path is underlined without first proving it resolves.
Opening is not, and the asymmetry is deliberate. A wrong underline costs the
person who pressed the modifier one click. A wrong tab costs **everyone** —
`Intent::OpenTab` does not confine (`hub.rs`, which hands the rel straight to
`open_buffer_for`; confinement happens later, when the fragment is fetched), and
the layout is shared across every connected browser. Firing `OpenTab`
optimistically on a false positive would open a dead tab in every window in the
project, which each of those people then has to close.

So: optimistic about the mark, never about the tab.

### A refusal is not a banner

`Event::Error` is wrong for this. It funnels to `showError` → `showBanner`, the
save-conflict styling the backlog already wants redesigned, and — as `app.js`
records at its `Error` case — it "carries no session context", so the client
could not route it back to the terminal that was clicked even if the styling
were right.

```
Event::PathRefused { text: String, msg: String }
```

sent with `send_to`, never `broadcast`. The client matches `text` against the
click it has in flight and calls `termFlash(entry, msg)` — the mechanism the
OSC 52 handler already uses for "copy blocked" and "copy too large" — falling
back to the focused terminal if the click is gone. The message lands in the pane
the user was looking at, and nobody else's window moves.

`broadcast` here would flash every window in the project because one person
mis-clicked.

### Failing closed is safe here, and that is worth saying out loud

This codebase's standing rule is that "I could not determine X" must never
collapse into "X is false", because eleven defects did exactly that and each one
destroyed something. This feature is the benign side of that line: when
`safe_resolve` cannot tell whether a path is real, resh flashes a message and
opens nothing. Nothing is deleted, nothing is overwritten, no shell is killed.
The rule bites where the outcome is destructive, and refusing to open a tab is
recoverable by pressing the key again.

## What a click does

- **URL** — `window.open(url, "_blank", "noopener,noreferrer")`, scheme
  re-checked at this point rather than trusted from match time.
- **Path** — `send({ t: "OpenPath", text })`.
- **OSC 8** — the destination goes through the URL branch, including the scheme
  allowlist. A `file://` OSC 8 pointing into the project is not specially
  handled in v1.

## Security

No new HTTP surface. `OpenPath` is a websocket intent like every other state
change, so the GET-only-plus-two-POSTs constraint is untouched, and it inherits
the socket's `Origin` check rather than needing its own.

The three things carrying weight:

- **`safe_resolve` is the boundary**, not the matcher. The regex is a
  convenience; a crafted path that gets past it still cannot escape the project.
- **Scheme allowlist on every URL**, from a heuristic match and from an
  application's own OSC 8 alike. An OSC 8 destination is attacker-chosen in
  exactly the way plain text is.
- **`noopener,noreferrer`** on every `window.open`, so an opened page gets no
  handle back to the workspace.

## Testing

The trap here is a test that opens a tab and asserts a tab opened. Every
assertion below names the rel, the message, or the recipient.

### Rust (`cargo test`)

- `OpenPath` on `<root>/src/a.rs` opens a `File` tab whose rel is `src/a.rs` —
  asserting the rel, not the tab count.
- `~/`-prefixed input resolves the same way.
- An absolute path outside the project root refuses, **asserting on the
  message**. `is_err()` alone would pass for a path that failed to canonicalize
  for an unrelated reason.
- `../` escape refuses, asserting on the message.
- A path that resolves to a directory refuses, asserting on the message.
- `src/a.rs:42` and `src/a.rs:42:7` both open `src/a.rs`.
- A `.png` opens as an image tab, proving it went through `coerce_tab` rather
  than around it.
- **`PathRefused` reaches only the client that asked** — with **two**
  subscribers. With one, `send_to` and `broadcast` are indistinguishable and the
  test proves nothing, which is on this project's own list of tests that passed
  for the wrong reason.

Each guard gets reverted and the test watched to fail before it counts. Not as a
thought experiment.

### Browser (`tests/browser/termlinks.mjs`, new)

No Rust test reaches `static/app.js`, and this feature is mostly in
`static/app.js`.

- No underline with the modifier up; underline with it down. Asserting the
  class, not that a mouse event was accepted.
- **A plain click while an app has mouse reporting on reaches the app and not
  resh.** This is the one that answers the open risk above, and it needs a real
  mouse-reporting program, not a shell prompt.
- Modifier+click on a real path opens the tab — asserting which rel.
- Modifier+click on a false positive flashes and opens no tab. Asserting the
  tab count did not change, so "opened the wrong thing" cannot pass as
  "correctly refused".
- Releasing the modifier while hovering removes the underline.

### Verification

The suite runs on the Linux host as well, and the modifier behaviour is checked
by hand in a real browser against a real Claude session before this is believed.
Both have caught defects here that a green suite did not.

**One build constraint applies to implementing this.** The feature was specced
in a git worktree, which is safe for markdown and unsafe for cargo: this host
points every workspace at one shared `target-dir` and `build.rs` bakes absolute
asset paths into its generated table, so building from a second checkout leaves
the shared binary built from the other tree while reporting `Fresh resh`.
Implementation happens in the primary checkout, or with `cargo clean -p resh`
between.

## Non-goals

- **Jump to line.** `:42` is matched and discarded. Honouring it means line
  numbers in the code preview, a scroll target, and a separate path for the
  textarea in edit mode — a viewer feature that this one would disappear into.
- **Bare filenames.** Ambiguous and noisy; see *What matches*.
- **Paths wrapped across a row boundary.**
- **Opening a directory.** Refused, rather than revealing it in the tree.
- **An on-screen hint that the modifier exists.** The convention is doing this
  work already; a hint can be added if it turns out it is not.
- **Links in the diff pane.** Markdown preview already has its own, and diff
  output is a different matcher.

## Open questions for review

- **Does the modifier want to be configurable?** Nothing in resh is configurable
  per-user today except themes and `show_hidden`, and adding a setting for this
  would land before the settings system the backlog already wants. Assumed no.
- **Should a `file://` OSC 8 that points inside the project open as a tab?**
  Treated as a URL and refused by the scheme allowlist in v1. Real, but nothing
  observed emits one.
- **Does `:42` want to be preserved anywhere** — a flash saying "opened
  src/main.rs (line 42)" — so the discarded information is at least visible?
