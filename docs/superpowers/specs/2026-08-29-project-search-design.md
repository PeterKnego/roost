# Project-wide search behind ⇧⇧

`docs/backlog.md:27` asks for "all project search, ala Idea shift-shift. Opens
new search dialog with results." The header has carried a reserved,
non-functional slot for it since the chrome redesign
(`2026-08-23-main-view-chrome-redesign-design.md:76-81`: "It exists so the
layout is final now and the search feature (its own spec) lands into a reserved
slot instead of reopening the header"). This is that spec.

## What changes, in one sentence

A double-tap of Shift opens a client-local overlay whose query runs as a
websocket intent against a new `src/search.rs`, which walks the project off the
hub lock and answers the asking connection alone with files, contents and
sessions — and `Tab::File` gains a line, so a content hit opens where it was
found.

## Scope

**In:** file paths, file contents, and live sessions, within the current
project. Line addressing for `Tab::File`, because a content hit is worth little
without it.

**Out:** symbols (needs parsing or ctags), search *and replace*, regular
expressions, cross-project search, and any persistent index. Cross-project was
considered and declined for v1: it multiplies both the walk cost and the
ranking question, and the overview page (`/`) already exists for "where is that
other project".

### The scope tension this resolves

`docs/backlog.md:163-167` lists "project-wide search/replace" as explicitly out
of scope, with a stated reason: the browser editor is where a human *reads*
what the agent wrote and makes one small correction, and "anything that helps
you author at volume or navigate a codebase" is the agent's job.

That entry scopes the **editor**, and this spec does not touch it. What is
built here is a *navigation* surface — find the file, land on the line — not an
authoring one. The distinction is load-bearing and is why **replace is out**:
the moment search can write, it becomes the authoring tool that entry declines.
A later reader who wants to add replace should treat that as reversing a
recorded decision, not as filling a gap.

## Why not shell out to ripgrep

Shelling out to `rg` would be faster and `.gitignore`-aware for free. It is
declined for two reasons.

First, it is not there. `command -v rg` on a clean PATH on this host returns
nothing; the `rg` available in an interactive Claude Code shell is a shim
function that re-execs Claude Code's own bundled copy. Depending on it would
add an undeclared runtime dependency to a project whose first line of
description is "a single Rust binary".

Second, and worse, it lands on this codebase's most-repeated defect. A
subprocess has three outcomes — success, failure, and *ran but I cannot trust
the output* — and a missing binary or a non-zero exit rendered as an empty
result list is "absence of evidence read as evidence of absence" exactly as
`CLAUDE.md` describes it. Nothing here can kill a shell, so the consequence is
milder than the eleven defects in that table; the mistake is identical, and a
silently empty result list is undetectable by the user.

A persistent index maintained by `watch.rs` was also considered. It is the only
design that makes *contents* search-as-you-type, and it is rejected for v1: it
adds a consistency surface to a module that is already load-bearing, and its
failure mode is stale results, which are harder to notice than slow ones.

## The trigger

`⇧⇧` is two Shift keydowns with **no other keydown between them**, within
400 ms. Any other key resets the pending state.

Two properties make this safe to arm on `document` globally rather than only
when some pane has focus:

- **It steals no keystroke.** Shift alone emits nothing to a shell, so
  intercepting it while a terminal has focus costs the terminal nothing. This
  is why ⇧⇧ is the right chord and a letter chord is not — xterm.js has the
  keyboard most of the time, and any `Ctrl-` binding would have to be taken
  away from the program running in the terminal.
- **The intervening-key rule prevents false fires.** Typing `HI` presses Shift
  twice in quick succession, but `H` lands between them and resets.

`Escape` closes and **restores focus to the element that had it**, or the
overlay costs you your terminal focus every time you dismiss it. `↑`/`↓` move
the selection, `Enter` activates it, `Tab` cycles category.

`#searchbox` (`render.rs:1150`) becomes the click target for the same overlay
and drops its "not implemented yet" tooltip.

## The categories

Three groups, each labeled, each capped separately, in this order:

1. **Files** — fuzzy subsequence match over the project-relative path. Ranked:
   exact basename, then basename prefix, then basename subsequence, then path
   subsequence; ties broken by shorter path. Cheapest and most-used, so first.
2. **Sessions** — substring match on session name. Activating one opens its
   terminal tab. The names come from the snapshot's `ws.live_sessions`
   (`workspace.rs:90`), which the hub already maintains. Search must **not**
   call `session::list_sessions` to freshen them: `refresh_live_sessions`'
   doc (`hub.rs:1105-1116`) records that it "forks a `ps` per session *while
   holding the global session-registry mutex*". A search that reached for
   fresher session data would reintroduce exactly the stall this design is
   otherwise built to avoid.
3. **Contents** — literal, case-insensitive substring, grouped by file with a
   per-file line cap.

The header hint changes from **"Search files, symbols, sessions"** to **"Search
files, contents, sessions"**. Symbols are out of scope, and a reserved slot
that keeps promising a category nobody is building is how a placeholder becomes
a lie.

## The engine — `src/search.rs`

A new module. Its `//!` explains why search is a walk rather than an index and
why it is not a subprocess.

**What it walks.** `projects::TreeFilter` (`projects.rs:31-49`), the same filter
the tree and the watcher share. Search therefore sees exactly the rows the tree
shows, `show_hidden` already means the right thing for it, and `SKIP_DIRS`
(`.git`, `.claude`, `target`, `node_modules`, `__pycache__`, `.venv`) is why v1
needs no `.gitignore` parser.

**What it reads.** Content candidates reuse the existing rule from
`projects::read_text_file` (`projects.rs:454-471`): a file over `MAX_FILE_BYTES`
(2 MB) is skipped, and a NUL byte in the first 8000 bytes means binary unless
the extension is in `TEXT_EXTENSIONS`. One rule for "is this text", not two.

**Where it will not go.** Directory descent stats with `symlink_metadata` and
does not follow symlinks, for cycle safety and because a symlink is how a walk
leaves the project root.

**The caps.** Three at once, because any one alone has a case that defeats it:

| Cap | Value | Defeats |
|---|---|---|
| Results per category | 50 | A query like `e` that matches everything |
| Lines per file (contents) | 5 | One generated file burying every other hit |
| Files examined | 20 000 | A wide tree of small files |
| Wall-clock deadline | 1500 ms | A slow or cold filesystem, where neither count is reached |

These are starting values, not measured ones, and the plan should say so where
it writes them down. They are constants in `search.rs`, deliberately not
config keys: a settings pane with a project/global split is already planned,
and every new key should get a deliberate scope rather than an accidental one.
A per-project override in particular would let a cloned repo widen its own
ceiling — the same reasoning that keeps `max_upload_bytes` global.

**The outcome is three-state, not a list.** This is the part of the design that
matters most:

```rust
enum Outcome { Complete, Truncated { reason: String }, Failed { msg: String } }
```

A directory that cannot be read is **counted and reported**, never skipped into
silence, and the UI renders the difference: "no matches" and "12 matches; 3
directories unreadable" and "search stopped at the 1.5s deadline" are three
different sentences. `CLAUDE.md`'s rule is that "I could not determine X" is a
third outcome and is never folded into "X is false". Search cannot destroy
anything, which is exactly why this is worth writing down: there is no crash,
no lost shell, and no way for the user to tell — an empty list simply looks
like an answer.

## Concurrency

`wsconn.rs:230-243` decodes and dispatches every text frame **while holding
`Hub::lock`**, the project-wide mutex. An `Intent::Search` handled the ordinary
way would hold that lock across the entire walk, stalling every other browser
on the project and everything else that needs the hub — the failure `CLAUDE.md`
records as already shipped once ("the global session registry held across a PTY
write, which wedged every session in every project").

So `Search` is routed **before** that lock is taken, to a per-connection worker
thread:

1. Take the hub lock *briefly* and snapshot `{dir, show_hidden, hide, session
   names, open tabs}`. Drop it.
2. Walk with **no lock held at all**.
3. Re-take the lock only to `send_to` (`hub.rs:309`) — the asking connection
   alone, never `broadcast`. A query is one browser's business, and the
   overlay is client-local by design.

Snapshot-then-unlock is already this codebase's idiom; `wsconn`'s buffer replay
does the same two-phase dance, which is why it carries tests named "a buffer
edited while unlocked…".

**Cancellation.** `Intent::Search { q, seq }`, with an `Arc<AtomicU64>` per
connection holding the latest seq. The worker re-checks it at every directory
boundary and abandons a superseded query; the reply carries its `seq` so a late
answer for an old query is also dropped client-side. **One reused worker thread
per connection** — a fast typist must not spawn a thread per keystroke.

**No panic escapes.** The walk is total: every per-file and per-directory error
is counted into the outcome, never propagated.

The client debounces ~120 ms, and contents only participate at ≥3 characters —
paths and sessions answer from the first keystroke.

## Line addressing

The line travels as its own intent and its own event — `Intent::OpenAtLine
{ pane, rel, line }` and `Event::RevealLine { rel, line }` — and **not** as a
field on `Tab::File`.

*(Revised while planning. This section originally specified `line:
Option<u32>` on `Tab::File`. Two facts killed it: `Tab::File { .. }` is
constructed at 74 sites in `src/`, which is a great deal of churn for a value
none of them care about; and `Tab` is persisted by `wsstate.rs`, so a line
would survive a restart and scroll you to yesterday's search hit. A line
belongs to one act of navigation, not to the layout.)*

`RevealLine` is broadcast rather than sent to the asker, so a second browser
mirroring the tab follows it to the same line — but it stays an event, so
nothing about it persists.

**Same `rel` remains one tab**, and this needs no new code:
`workspace::tab_identity_eq` (`workspace.rs:193-205`) already compares File
tabs on `rel` alone, with a doc saying why ("A File tab differing only in Mode
is still the same file"). A second hit in an open file re-scrolls it rather
than cloning the tab.

**A content hit opens in Edit.** Search matched the file's source text, and a
rendered markdown preview has no line 412 in it to land on. `coerce_tab` still
demotes anything that cannot be edited as text.

The client scrolls both Preview and Edit to the line and highlights that row.
This also retires the gap at `app.js:1249-1253`, where a terminal link
`hub.rs:412` currently strips its line number and flashes "line 412 — opening
file" because, in that comment's words, "the viewer has no line addressing to
spend it on". Same mechanism, so fixing it here is not scope creep — and it
closes `docs/backlog.md`'s "Line numbers, and go-to-line" for the viewer.

## Components and files

| File | Change |
|---|---|
| `src/search.rs` | New. The walk, the matchers, the caps, the three-state outcome. |
| `src/proto.rs` | `Intent::Search { q, seq }` and `Intent::OpenAtLine`; `Event::SearchResults { seq, .. }` and `Event::RevealLine`. `Tab` is unchanged. |
| `src/wsconn.rs` | Route `Search` before the hub lock; own the per-connection worker and its seq. |
| `src/hub.rs` | Snapshot accessor for the worker; honour `line` on `OpenTab`. |
| `src/render.rs` | Hint text; `#searchbox` becomes a control; the overlay's static shell. |
| `static/app.js` | ⇧⇧ handler, overlay, result list, scroll-to-line for Preview and Edit. |

### Where the result rows are built

`CLAUDE.md` says all HTML is built in Rust in `render.rs`, and results arrive
over the websocket as a serde event rather than as a fragment — so the rows are
necessarily built client-side. That does not contradict the rule; `app.js`
already has the narrower one it needs (`app.js:76-78`): innerHTML carries
**constant markup only**, "nothing here is ever interpolated, which is what
makes innerHTML safe; anything dynamic stays in text nodes and dataset
attributes as everywhere else."

Result rows are the most attacker-influenced strings the client has ever
rendered — a matched line is arbitrary file content, and a path is arbitrary
filesystem content. So every dynamic part of a row (path, session name, matched
line, and the match-highlight span) is a **text node or a dataset attribute**,
never interpolated markup. The overlay's fixed chrome is rendered in
`render.rs` and escaped like everything there.
| `static/style.css` | Overlay styling. |

## Testing

The named traps below are the ones this codebase has actually shipped; each
test exists to not repeat one.

- **An unreadable directory reports unreadable, not empty.** Verified by
  reverting the fix and watching it fail, not by inspection.
- **Two subscribers** on the results test. With a single subscriber `send_to`
  and `broadcast` are indistinguishable and result privacy regresses silently —
  a trap `CLAUDE.md` lists by name.
- **A timed lock test.** With a search in flight over a slow tree, a second
  connection's intent must still complete promptly. A deadlock hangs rather
  than fails, so this asserts on elapsed time; a pass/fail count would prove
  nothing.
- **Symlink escape**, written so it reaches the confinement check rather than
  failing earlier on `ENOENT` — the exact way a symlink escape once survived
  review here.
- **Cap tests assert on the reported reason**, not only on result count, or
  swapping which cap fired leaves them green.
- **A result row containing markup renders as text.** A file named
  `<img src=x onerror=…>.txt`, and a file whose matched *line* contains the
  same, must both appear as visible characters. The fixture has to carry a real
  metacharacter — `CLAUDE.md` records an escaping test whose fixture had none
  and so asserted nothing.
- **`tests/browser/search.mjs`.** No Rust test can reach `static/app.js`, and
  the trigger, the overlay and scroll-to-line live entirely there. Against a
  scratch resh with real dtach, never the live instance.
- `cargo test -- --test-threads=1`, and on the Linux deploy host as well as
  here.

## Risks

- **Scroll-to-line in Edit is the riskiest piece.** The editor is a `textarea`
  with a `code-input` highlight overlay painted beneath; scrolling to a line
  means computing line geometry against a wrapped textarea, and the highlight
  layer must not desynchronise. Only the browser test can settle it. It must
  also not touch the bytes: save is conflict-guarded against a hash of what was
  read, so anything that normalises whitespace or newlines is disqualified.
- **A cold walk on a large project may reach the deadline routinely.** That is
  a truthful "truncated", not a bug, but if it is the common case the answer is
  the index design, not a larger deadline.
- **The overlay is resh's first modal.** `docs/backlog.md:24` wants popup UX
  fixed generally; this spec deliberately does not attempt that, and the
  overlay should be built so a later popup pass can absorb it.
