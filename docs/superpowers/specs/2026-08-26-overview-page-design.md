# resh — the front page as a projects/worktrees/sessions overview

The front page is a filesystem directory picker: browse a tree under the
roots, click **Open** to start a project. It answers "what directory do I want
to open" and nothing else. This turns it into the overview the workspace never
had — what projects and worktrees exist, and what is running in each — while
keeping the picker one click away for opening a directory resh has not seen.

This is the third of three specs from the 2026-08-25 worktree work. The first
(`2026-08-25-worktree-launch-design.md`) shipped: ✻ steers a second Claude into
its own worktree, and the header switcher shows each worktree's state. The
second — *session accounting*, a richer per-session state (idle / working /
waiting-for-input) — is still unwritten and is this spec's main deferral.

## The problem this solves

After the worktree work, a user can have several projects, each with several
worktrees, each with a terminal or two — some running Claude, some plain
shells. The only view of all that is the header switcher *inside* one
workspace, which shows one repo's worktree family and no sessions at all. To
answer "what do I have running, where" you open projects one at a time. The
front page has the whole viewport free and shows none of it.

## What changes, in one sentence

`/` with no query renders a two-pane overview — a project/worktree tree on the
left, the live terminal/Claude sessions on the right, the right pane filtered
by what is selected on the left — and the existing picker moves behind a
`?at=` query and an "Open a directory" button.

## Scope — what this is and is not

**In:** the two-pane overview; the left tree with expand/collapse and
selection; the right session list with the Claude/shell mark, age, and
attached-browser count; click-to-open-focused; live refresh by polling; the
picker relocated behind `?at=`.

**Out (deferred, stated so the next reader finds a decision, not a gap):**

- **Per-session activity state** — idle / working / waiting-for-input. resh
  does not know this yet; it needs the OSC-title capture (`osc.rs` currently
  discards window-title sequences) and the existing notification path folded
  into a persisted per-session record. That is the *session accounting* spec.
  This overview ships on what resh already knows and gains the state column
  when that lands.
- **Session actions from the overview** — no new-terminal (`+`/✻), no ✕/end,
  no rename. The overview is a read-only launchpad; every mutation stays in
  the workspace, where it already is and is already tested. Adding action
  buttons here would duplicate that surface and its confirmations.
- **Websocket push.** The page refreshes by polling (below), not a live socket
  like the workspace. A launchpad does not need sub-second liveness, and a new
  socket type is a cost the v1 does not earn.

## Layout

```
┌ resh ─────────────────────────────────────  [＋ Open a directory] ┐
│ PROJECTS / WORKTREES          │ SESSIONS · all active              │
│ ▾ ● ultima        ⎇ main      │  ultima · claude ✻        4h  ·1   │
│     └ ● claude-1  dirty 3▲    │  ultima/claude-1 · claude ✻  20m   │
│     └ ○ claude-2              │  resh · shell             2h  ·1   │
│ ▸ ● resh          ⎇ master    │  resh · claude ✻          4h       │
│   ○ karpie                    │                                    │
└───────────────────────────────┴────────────────────────────────────┘
```

Two panes, a fixed-width left and a flexible right, built in `render.rs` like
every other resh page. Wide content scrolls inside its own pane; the body
never scrolls horizontally.

## The route — one path, keyed by `?at=`

No new reserved path (which would collide with a project of that name, the way
`static`/`frag` already do). `serve_index` branches on whether the `at` query
key is **present**:

- **absent** (`/`) → the overview.
- **present** (`/?at=` or `/?at=<path>`, empty value included) →
  the picker, exactly as today.

`req.query.get("at")` is `None` for `/` and `Some("")` for `/?at=`, so the two
are distinguishable without a sentinel. The picker's own navigation already
emits `/?at=<path>` (see `picker.js`), so browsing is unchanged; only its entry
point moves. The "＋ Open a directory" button is an anchor to `/?at=`. **Open**
still navigates to `/<path>` (the workspace), unchanged.

## Left pane — the project/worktree tree

Source: `registry::known_projects_with_state(roots)` — the same call the header
switcher uses, so the left pane reuses the shipped worktree-state work rather
than recomputing it. It returns every *known* project (a saved layout, a live
session, or discovered as a worktree), pre-ordered `parent:None` then its
`parent:Some` children, each child carrying `wt: Some(WorktreeStatus)`.

**Rows.** A top-level row per project; a `▸`/`▾` caret when it has worktree
children. Expanding reveals the children (the `parent == key` family). The tree
is exactly two deep — a worktree is its own project and a worktree of a
worktree is refused (`worktree::create`), so there is no third level to draw.
Each row shows:

- `●` (a live session in that project) or `○` (known, nothing running).
- the name (project url, or a worktree's leaf segment).
- `⎇ <branch>`.
- for a worktree row, the same chips the switcher renders from
  `WorktreeStatus`: `✻` when a Claude is there, `dirty`, `N▲` ahead — each
  three-valued (`?` when git could not answer), reusing the switcher's render
  helper.
- an unreachable worktree (git reports it, but it is outside the roots) renders
  as inert dimmed text, never a link — the same rule `worktrees_strip` already
  applies.

**Interaction.** Clicking a row does two things: it **selects** the row (which
filters the right pane) and, if the row has children, toggles its expansion.
Selection is carried in the URL as `?sel=<key>` so a poll refresh (below) does
not lose it; expansion is client-side only (a `Set` of expanded keys in
`overview.js`, the picker-sized script this page gets). A row is a link to
`/<url>` for opening the project itself; the select/expand is layered on with
`preventDefault` for the left-click, so a ⌘/ctrl-click still opens the project
in a new tab the browser's own way (matching the switcher's anchor convention).

**Clearing the selection.** A "SESSIONS · all active" header on the right pane
doubles as the deselect: an `All` link that navigates to `/` (no `?sel=`). The
selected row is marked `current`.

## Right pane — the sessions

**What is shown.** The scope is decided by `?sel=`:

- **no `?sel=`** → every live session across every known project.
- **`?sel=<project key>`** → that project **and its worktrees** — the family.
  Selecting a project answers "what is running in this repo, anywhere."
- **`?sel=<worktree key>`** → just that worktree's sessions.

Sessions come from `session::list_sessions(project)` per project in scope.
**Ages for the all-projects view come from a single process snapshot**, not a
`ps` fork per session: `list_sessions` forks `ps` per session for age, and the
unfiltered view could touch every session on the host. The overview computes
ages from one `registry::process_snapshot()`-style listing and joins by pid, so
the page costs one `ps`, not one per session. (A future refactor could push
this into `list_sessions`; this spec keeps the change local to the overview.)

**Rows.** Each session row shows:

- `●` **Claude** vs `○` **shell**. Claude only on *positive evidence*: the
  session was launched as Claude in this process (`session::launched_names`),
  or a Claude is connected on the IDE socket for its project
  (`claudes::claude_evidence`). This is deliberately best-effort and honest:
  the launch record is in-memory, so a Claude terminal that predates the last
  resh restart and is not currently IDE-connected shows as a plain terminal
  rather than being mislabeled — the same three-valued discipline
  `ClaudeEvidence` enforces. It never claims Claude on a guess.
- the session name.
- its project / worktree label (so the unfiltered view is legible).
- age (coarse, like the switcher: `20m`, `4h`, `1d`; `—` when unknown, never
  `0`, matching `SessionInfo::age_secs`'s existing rule).
- `·N` browsers attached, when > 0.

**Click → open focused.** A row is a link to `/<project>?focus=<session>`. The
workspace consumes `?focus=` after its first `State`: it activates the Terminal
tab whose session matches, then strips the param with `history.replaceState` so
a reload does not re-focus. This mirrors the `?launch=` handling Task 11 added
to `app.js` — same shape, same one-shot consumption. A `focus` naming a session
the layout does not contain is ignored (the workspace opens normally); the
session name is validated (`session::valid_name`) before use.

## Liveness — polling

Both panes are htmx fragments with `hx-trigger="load, every <interval>s"`:

- `/frag/_overview_projects?sel=<key>` → the left tree (selection marks the
  current row; expansion is client-side and survives the swap because
  `overview.js` re-applies it after each htmx swap).
- `/frag/_overview_sessions?sel=<key>` → the right list.

Polling, not a socket: the front page has no websocket today, and a launchpad
tolerates a few seconds' lag. The interval is a single constant (a few
seconds), not configurable in v1. `ProjectsChanged` (already broadcast to
workspace clients) is not consumed here — wiring the front page to that stream
is the websocket-push upgrade this spec defers.

The one client-side subtlety: an htmx swap of the left fragment replaces the
rows, so `overview.js` keeps expansion state in a `Set` and re-applies
`▾`/visibility after `htmx:afterSwap`, the same way the workspace re-applies
transient UI after a fragment refresh.

## Components and files

| File | Change |
|---|---|
| `src/routes.rs` | `serve_index` branches on `at` presence; two new frag arms `_overview_projects` / `_overview_sessions` |
| `src/render.rs` | `overview_page()`; `overview_projects(sel, &[ProjectStatus])`; `overview_sessions(sel, …)`; a shared worktree-chip helper factored from `worktrees_strip` |
| `src/session.rs` | a batch age path (or an overview helper) that joins `list_sessions` names to one process snapshot; nothing that holds a lock across the `ps` |
| `static/overview.js` | expand/select, re-apply expansion after htmx swap (picker.js-sized) |
| `static/app.js` | `?focus=<session>` consumed once after first `State`, then stripped |
| `static/style.css` | the two-pane grid, tree rows, session rows |
| `static/picker.js` | entry-point link `/?at=` (navigation already emits `?at=`; verify no other change needed) |

`overview_page` is a new top-level page renderer beside `index_page` and
`workspace_page`; the picker's `index_page` stays exactly as is, now reached
only via `?at=`.

## Testing

Rust (`cargo test`, never `--release`; every new test revert-checked with the
observed failure in its comment):

- `overview_projects`: a project with two worktrees renders a caret and, when
  `sel` names the parent, marks it current; a worktree row carries its
  `WorktreeStatus` chips; an unreachable worktree is inert text, not a link.
  Revert-check: drop the `parent`-grouping and the child rows attach to the
  wrong parent / the test fails.
- `overview_sessions`: with no `sel`, a session from each of two projects
  appears; with `sel` = one project's key, only that family's sessions appear
  (a two-project fixture, so the filter is discriminating — not a single
  project where filtered and unfiltered are identical). A session launched as
  Claude renders `●`; a plain shell renders `○`; a session with neither
  positive signal renders `○` (asserted on the mark, not on "not ●"). Ages
  come from the injected snapshot, so the test needs no real `ps`.
- `serve_index` routing: `/` (no `at`) yields the overview markup; `/?at=`
  yields the picker markup. Before the branch exists this cannot pass, so it
  discriminates the route change.
- `?focus`: a `render`/`app`-level assertion that the workspace page carries no
  behavioral change when `focus` is absent (the Rust side only passes the URL
  through; the activation is JS, covered by the browser test).

Browser (`deno run -A tests/browser/overview.mjs`, on the existing harness;
Chromium is present so it runs, does not skip; every assertion revert-checked
per `tests/browser/README.md`'s four traps, recorded in the file header):

- The overview lists a running session (start one via the workspace as the
  other browser tests do), and clicking its row lands on
  `/<project>?focus=<session>` with that Terminal tab **active** (assert on the
  active-tab state, not on event order — trap 2).
- Expanding a project with a worktree shows the worktree row (assert the child
  row was hidden before the caret click and present after — not on a timeout,
  trap 1).
- Selecting a project filters the right pane to its family and back to all on
  `All` (a two-project fixture so the filter is observable).
- The "Open a directory" button reaches the picker (`?at=` markup present).

**Substitution traps to respect** (`CLAUDE.md`): the session list must be
verified with a *real dtach* session in the browser test, because the
Claude/shell mark and the attached-count are exactly the kind of thing a
`RESH_CMD=cat` unit test renders without ever proving against a real PTY.

## Risks

- **`list_sessions` cost at scale.** One `ps` per session on the unfiltered
  view is why the batch snapshot exists; if a host ever has hundreds of
  sessions the snapshot join is O(sessions) in memory but one subprocess. If
  even that is too much, the fallback is to render the unfiltered view without
  ages (count only) — not needed now, noted.
- **The Claude mark understates after a restart.** Stated above and by design.
  When *session accounting* lands (persisted per-session record), the mark
  becomes reliable across restarts; until then it is positive-evidence-only,
  which is the safe direction (never a false Claude).
- **Poll interval vs. load.** Two server-rendered fragments every few seconds
  per open front page. Each is the same work the header strip already does on
  its own triggers; the interval is the one knob to turn if it shows up in
  load. A websocket push (the deferred upgrade) removes the polling entirely.
- **A project literally named to collide with a reserved segment** is a
  pre-existing class (`static`, `frag`), not widened here — the overview adds
  no new reserved top-level path, since it lives on `/` and keys off `?at=`.
