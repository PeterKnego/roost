# resh — a second Claude gets its own worktree

Retires `resh peers` and replaces the premise behind it. Instead of telling
several Claudes in one directory about each other, the ✻ button steers the
second one into a git worktree of its own — offered, not forced — and resh
takes on the worktree lifecycle that this creates: what state each worktree
is in, and removing one when there is positive evidence it is finished.

This is the first of three specs. It is followed by *session accounting*
(a resh-owned record per AI session, fed by resh-owned signals) and *the
overview page* (the first page re-based on that record). Both are outlined
under *What this does not do*.

## The problem this solves

Reproduced 2026-08-25 in a real browser against a scratch resh with the real
`claude` (script kept out of the tree; the sequence is the browser test in
*Testing*):

1. ✻ twice in one project → sessions A (`2ff85f10…`) and B (`357621be…`),
   one conversation each.
2. `/exit` in A. Claude prints `Resume this session with: claude --resume
   2ff85f10…` and scrolls it away.
3. `claude --resume` in A's shell. The picker sorts every session *with this
   cwd* by recency, so B's still-running session is on top. Enter — and
   `claude -c` — resume **B's** session in A's terminal. Two processes then
   append to one transcript (`357621be….jsonl` gained a second
   `hook_success`); the person at A is now looking at B's context.
4. On that resume the `SessionStart` hook reported *"1 other Claude session
   … (pid 3605227, unknown state, started just now)"* — pid 3605227 being the
   process that had just started. It warned the session about itself and hid
   B. `peers::roster` identifies "self" by exact `sessionId`, and after a
   resume two live processes carry one id.

A's own session was intact throughout (`claude -p --resume 2ff85f10…`
answered from its history). Nothing was lost; "continue" simply stops meaning
"mine" once a second Claude shares the directory.

Two conclusions, both of which this spec acts on:

- **Several Claudes in one directory is the problem, not something to
  coordinate around.** Claude Code keys sessions by cwd; every ✻ terminal has
  the same cwd. A worktree is a different cwd *and* a different branch, so
  the picker, `-c`, and git all agree on what "mine" means.
- **Every part of `resh peers` that broke was resh reading Claude's state.**
  `~/.claude/sessions/<pid>.json` is another program's file format, and the
  identity rule in it (one sessionId ⇔ one process) turned out not to hold.
  Dependencies on Claude's *documented interfaces* — a CLI flag, the IDE
  protocol, hook stdout — are one thing; dependencies on the shape of its
  private files are another, and this spec removes the only one resh has.

## What changes, in one sentence

`NewTerminal{launch:claude}` in a project where resh has positive evidence of
a running Claude answers with a prompt instead of a terminal; "new worktree"
creates `.claude/worktrees/claude-N`, opens it in a new browser tab with
Claude already typed in, and the worktree switcher learns each worktree's
state and offers removal only when every check is positively clean.

## Evidence resh already owns

"Is a Claude already running in this project?" is answered without touching
`~/.claude`:

| Signal | Where it lives today | What it proves |
|---|---|---|
| Parked launch | `session::PENDING_LAUNCH`, consumed at spawn (`session.rs:142`, `term.rs:163`) | This terminal was started by ✻ |
| IDE connection | `ide.rs` `CONNS`, one `Conn` per connected Claude, with the terminal it sits in (`idesess.rs`) | A Claude is alive and connected, and in which terminal |

The parked launch is consumed and forgotten today. This spec keeps it: the
`Session` record gains `launched: Option<Launch>` set on the spawn that typed
it, cleared when the session is reaped. That does not prove Claude is still
running there — it proves the shell was handed `claude`. Combined with the
IDE connection it is enough for a prompt; the exact process state is B's job.

```rust
pub enum ClaudeEvidence {
    /// At least one IDE connection, or one attached session spawned with
    /// `Launch::Claude`. Carries the terminal names it could attribute.
    Present(Vec<String>),
    /// IDE integration is on and nothing is connected, and no attached
    /// session was launched with Claude.
    Absent,
    /// IDE integration is off (`ide = false`) and no session was launched
    /// with Claude: a `claude` typed by hand into a `+` terminal is
    /// invisible, so "nothing found" is not "nothing there".
    Unknown,
}
pub fn claude_evidence(project: &str) -> ClaudeEvidence
```

Only `Present` changes behaviour. `Absent` and `Unknown` proceed exactly as
today. Steering on positive evidence only is the destruction rule applied in
the mild direction: a missed prompt costs a mess that is recoverable by id; a
wrong prompt is noise on every click.

## The ✻ path — `Hub::do_new_terminal`

`Intent::NewTerminal` gains `force: bool` (default `false`, so today's
clients and tests decode unchanged). With `launch == Some(Claude)` and
`!force`, the hub calls `claude_evidence` first:

- `Present(terminals)` → `send_to(from, Event::ClaudeHere { terminals })` and
  return. No name allocated, no layout change, nothing broadcast: the other
  browsers on this project saw nothing happen, which is true.
- otherwise → the existing path, unchanged.

`force: true` skips the check. It is what "start here anyway" sends. It is not
a security boundary — the user could always type `claude` — so it needs no
more validation than any other intent field.

### The launch line

`launch::keystrokes(Launch::Claude)` changes from `claude\r` to
`claude --session-id <uuid>\r`, with a v4 uuid minted by the hub at
allocation and stored on the parked launch. `--session-id <uuid>` is a
documented flag of the current CLI (`claude --help`, 2.1.245: *"Use a
specific session ID for the conversation"*); it was verified present but not
exercised end-to-end in the repro, so the browser test below is where the
flag is proven to start a session rather than error out. The uuid is recorded
on the `Session` next to `launched`. This spec does not yet *use* it beyond
recording it; B does (the "resume the Claude that was here" affordance). It
goes in now because it is one line in `launch.rs` and every session started
before B ships would otherwise be unaccounted for.

`keystrokes` therefore takes the uuid: `keystrokes(launch, uuid: &str)`. A
uuid is `[0-9a-f-]{36}` — it lands on a command line, so it is validated
against exactly that before being typed, the way session names are.

## Creating the worktree — `Intent::NewWorktree`

```rust
NewWorktree { launch: Option<Launch> }
```

No name from the browser; the server mints it. The steps, in order, each of
which stops the whole thing with `Event::Error{msg}` on failure:

1. **The project must be a main worktree.** `worktree::list(dir)` must
   report `dir` as `is_main`. A worktree of a worktree is refused
   (*"start worktrees from the main checkout"*) — the switcher shows the
   family, so the user has one click to get there. A non-repo is refused
   (*"not a git repository"*). `list` returning empty is `Unknown`, refused
   with *"git did not answer"* — not treated as "not a repo".
2. **Mint `claude-N`.** N is the smallest positive integer for which *both*
   `git show-ref --verify --quiet refs/heads/claude-N` says absent (exit 1,
   not any non-zero) *and* `symlink_metadata(.claude/worktrees/claude-N)` is
   `Err(NotFound)`. Any other outcome from either check — `Err(_)` other
   than NotFound, git exiting 128, git not answering — is "cannot tell" and
   refuses rather than skipping to N+1. Capped at N ≤ 64; over that is
   *"too many worktrees"*, which is a signal about the lifecycle, not a
   limit to raise.
3. **Confine the path.** `projects::safe_resolve_parent(dir,
   ".claude/worktrees/claude-N")` — the target does not exist yet, so the
   parent is what is canonicalised. `.claude/worktrees/` is created with
   `create_dir_all` if absent.
4. **Record the base before creating.** HEAD's branch name (`git symbolic-ref
   --short HEAD`), or the commit hash when detached, written atomically to
   `{state}/worktrees/{wt_key}.base` — temp file with a pid-unique name,
   then `rename`, the `.origin` pattern. Written *before* `worktree add` so a
   crash between the two leaves a base file for a worktree that does not
   exist (harmless, reaped below) rather than a worktree with no base
   (permanently "ahead unknown").
5. **`git worktree add -b claude-N <path> HEAD`** through `gitio::run_git`,
   15 s deadline, stdout and stderr drained. Non-zero exit → `Error` with
   git's stderr, and the `.base` file removed.
6. **Register and announce.** `registry::known_projects` is refreshed and
   `Event::ProjectsChanged` broadcast, so every open page's picker and
   switcher show the new entry. The inotify watcher is not relied on for this
   (`CLAUDE.md`: directories created after startup were once never watched).
7. **Reply** `Event::WorktreeReady { url, launch }` to the sender only.

`wt_key` is the ordinary project storage key of the worktree path
(`resh%2F.claude%2Fworktrees%2Fclaude-1`), which `worktree::is_vouched_worktree`
already admits through the dot-segment rule.

### Opening it with Claude already typed

The browser opened a tab **synchronously in the click handler** —
`const tab = window.open("about:blank")` — before sending `NewWorktree`. A
`window.open` after a websocket round trip is not reliably inside the user
gesture and popup blockers eat it; a blank tab opened on the click and
navigated later is the standard way past that. On `WorktreeReady` the page
sets `tab.location = "/" + url + "?launch=claude"`. If `tab` is `null` (the
blocker won anyway) the page falls back to a link in the prompt box: *"opened
claude-1 — click to go there"*.

On load, `app.js` in the new tab reads `launch` from the query string, strips
it with `history.replaceState` so a reload does not start a second Claude,
and — once the control socket is open and the first `State` has arrived —
sends the ordinary `NewTerminal { pane: 3, launch: "claude" }`. That goes
through `do_new_terminal` including the evidence check, which finds nothing
in a fresh worktree, and the parked-launch path types the command. Nothing
server-side reaches into another project's hub; the worktree's hub is
created the way every hub is, by its first browser.

A `?launch=claude` link is a same-origin page action equivalent to clicking
✻; a cross-origin page cannot reach the control socket at all
(`origin.rs`). It adds no surface.

## State — what the switcher shows

`registry::ProjectStatus` gains, populated for worktree entries only:

```rust
pub claude: ClaudeEvidence,      // from claude_evidence(key)
pub dirty: Option<bool>,         // None: git did not answer
pub ahead: Option<u32>,          // None: git did not answer, or no base
pub base: Option<String>,        // what `ahead` is measured against
```

- `dirty` — `git status --porcelain` in the worktree: any output → `true`,
  empty → `false`, failure → `None`.
- `ahead` — `git rev-list --count {base}..HEAD`. `base` is the `.base` file's
  branch name; measuring against the *branch* means a merged worktree reads
  `0` even after the base branch has moved on. For a worktree with no
  `.base` (made by Claude's own `EnterWorktree`, or by hand) the main
  worktree's current branch is used and `base` says so in the tooltip.
- **Squash merges are invisible to this.** A squash-merged branch keeps its
  commits and stays "N ahead" forever. The tooltip says exactly that, and
  such a worktree never qualifies for the remove control — the person
  removes it by hand, which is the correct default for a check that cannot
  tell.

These run only for the fragment the switcher panel requests when it opens
(`/frag/_worktrees?current=…&state=1`); the `refresh from:body` reload of the
strip stays as cheap as it is today. Two `run_git` calls per worktree, each
with the 15 s deadline.

The row: `● claude-2 ⎇ claude-2 · ✻ · dirty · 3 ahead`, where each field is
its glyph, `—` when negative, and `?` with a tooltip when unknown. `✻` is
present only on `Present`; `Unknown` renders `?` there too.

## Removal — `Intent::RemoveWorktree`

```rust
RemoveWorktree { key: String }
```

The `✕ remove` control renders only when the row is positively clean on every
axis: `live == 0` (no dtach session in that project), `claude == Absent`,
`dirty == Some(false)`, `ahead == Some(0)`. A single `?` anywhere and there is
no control. Clicking it goes through `confirm()` naming the path.

The server does not trust the row. It **re-derives all four at the moment of
the intent** and refuses with a banner naming the first one that is not
positively clean (*"claude-2 has a live terminal"*, *"…git did not answer"*).
Then, in order:

1. `git worktree remove <path>` — **no `--force`**. git itself refuses a
   dirty or locked worktree; that refusal is a second gate that does not
   share code with the first.
2. `git branch -d claude-N` — **no `-D`**. git refuses an unmerged branch —
   the third gate, and the one that catches a squash-merge misread if a
   future change ever lets one through step 1.
3. Only after both succeed: remove `{state}/worktrees/{wt_key}.base` and the
   worktree's own layout/state under the state dir, then refresh and
   broadcast `ProjectsChanged`.

A failure at 1 leaves everything. A failure at 2 leaves the branch and says
so (*"worktree removed; branch claude-N kept: git reports it unmerged"*) — a
branch is cheap, a lost commit is not. Nothing here is ever scheduled or
swept; `registry::reconcile` does not learn about worktrees.

A `.base` file whose worktree does not exist (the step-4/5 crash window, or a
worktree removed by hand) is reaped by `reconcile`, since it is a file about
nothing — the one destructive act here, and it destroys a line of text.

## The UI

**The prompt** (`ClaudeHere`): an inline box at the top of the pane the click
came from, the `.conflict` styling `app.js:1942` already uses for transient
notices: *"A Claude is already working in this project (term, term2)."* with
two buttons — **Start in a new worktree** and *Start here anyway* — and
dismiss. It is per-browser and not persisted: a second browser on the same
project did not click.

**The switcher** (`render::worktrees_strip`): the state fields above, and the
remove control. All built in `render.rs`, everything escaped, as today.

**Config**: one key, `worktree_prompt = true` (global config only — it
changes what a button does in every project, and per-project would let a
checkout decide). `false` makes ✻ behave as before the spec, including in a
project with a Claude. The switcher's state and removal are not gated: they
show git's answers and never act without a click.

## Retirement

Removed: `src/peers.rs`, the `peers` arm in `src/cli.rs` and its `lib.rs`
export, `docs/peers.md`, and the `resh peers` sentence in `main.rs`'s
roots-conflict message and in `docs/deploy.md`. `errlog.rs` stays; the
server still records roots conflicts there. The `roots` key in the global
config stays too, for the same reason.

Not automated, listed in `docs/deploy.md` as a step: delete the
`SessionStart` entry from `~/.claude/settings.json` on each host. Left in
place it fails with *command not found* on every session start — loud, not
dangerous, and the note says so.

`docs/backlog.md`'s *Peer sessions* section is kept as the record, with one
closing line pointing here and at the repro.

## What this does not do

- **Does not know what the Claude in a terminal is doing.** `Present` means
  "connected or launched", not busy/idle/waiting. That is spec B (session
  accounting): a resh-owned record per terminal fed by the terminal title
  (OSC 0/2, which `osc.rs` currently discards), the existing notification
  path, the process tree and PTY output recency — each three-valued.
- **Does not offer "resume the Claude that was here."** The uuid is recorded
  here; B adds the affordance when Claude exits in a launched terminal.
- **Does not change the first page.** Spec C re-bases the front page, the
  header strip and the ◆ panel on B's record.
- **Does not rename worktrees.** `claude-N` is a directory name; C's rows
  will show what the Claude there is working on, from B's title capture.
- **Does not prevent two Claudes in one directory.** "Start here anyway" is
  one click. This steers.

## Testing

Rust, `cargo test`, each written to fail with the code it covers deleted:

- `claude_evidence`: four fixtures — IDE connection only; launched-and-attached
  session only; neither with `ide = true` → `Absent`; neither with
  `ide = false` → `Unknown`. The `Unknown` test asserts on the variant, not on
  "not Present", or it passes with `Absent`.
- `do_new_terminal`: with `Present`, the sender gets `ClaudeHere`, **no other
  subscriber gets anything**, and the layout version is unchanged (two
  subscribers, so `send_to` and `broadcast` are distinguishable —
  `CLAUDE.md`). With `force: true` and `Present`, a terminal opens.
- Minting: a repo with `claude-1` as a branch but no directory yields
  `claude-2`; a directory but no branch yields `claude-2`; a `show-ref` that
  returns 128 refuses with *"cannot tell"* — asserted on the message.
- `.base` is written before `worktree add` runs (a fake `run_git` that fails
  observes the file present, and the caller removes it after).
- `RemoveWorktree`: **four tests, one condition dirty each** — a live
  session, a `Present` Claude, a dirty tree, one commit ahead — each
  asserting on the message naming *that* condition. A fifth with all clean
  removes, and asserts the directory and branch are both gone. A sixth where
  `git branch -d` fails leaves the branch and reports it.
- `keystrokes(Claude, uuid)` contains `--session-id <uuid>`; a uuid with a
  shell metacharacter is refused before typing.

Browser, `deno run -A tests/browser/worktree-launch.mjs`, on the
`claudeterm.mjs` fixture (fake `claude` on `PATH` that prints its argv and
`CLAUDE_CODE_SSE_PORT`):

- ✻ once → a terminal showing `FAKE-CLAUDE-STARTED` with `--session-id` and
  a 36-char uuid on its line.
- ✻ again → the prompt is in the pane and the session list is unchanged
  (assert on the `State` snapshot's tabs, not on event order — client-visible
  ordering is pipelined per connection and proved non-discriminating once).
- **Start in a new worktree** → a second CDP target whose URL is the
  `.claude/worktrees/claude-1` project, whose query string is empty after
  load, and whose first terminal shows `FAKE-CLAUDE-STARTED`. `git worktree
  list` in the fixture repo names the path.
- *Start here anyway* → a second terminal in the original project.
- Switcher with `state=1` shows `claude-1` with `—` dirty and `0 ahead`;
  `touch` a file in it and reopen → `dirty`; the remove control is absent
  while the fake claude's terminal is attached and present after
  `EndSession`; clicking it removes the directory and the branch.

Each browser assertion revert-checked per `tests/browser/README.md`; the
result recorded in the file's header comment.

**Not covered, stated:** removal with a real Claude connected on the IDE
socket, and `--session-id` with the real CLI. Both verified by hand on the
Linux host before deploy, and the deploy note records the run.

## Risks

- **`--session-id` semantics could change.** It is a documented flag, and the
  fallback is typing `claude` alone — the launch works, the uuid is simply
  not known. `launch::probe` could check `claude --help` for the flag at
  startup the way it checks for `claude` itself; deferred until B needs the
  uuid to be reliable.
- **`.claude/worktrees` is shared with Claude Code's own `EnterWorktree`.**
  Both create there; Claude auto-removes its own untouched ones. resh never
  removes without a click, so the two cannot race on a deletion. Claude's
  worktrees have no `.base` and show `ahead` against the main branch, marked
  as such.
- **Proliferation.** Prompt-per-✻ and a visible state row are the whole
  answer in this spec; C's overview is where "what did I leave running for
  days" gets answered at a glance. If sixty-four worktrees are reached the
  cap says so rather than minting `claude-65`.
- **The popup blocker.** Handled by opening on the click and navigating
  later, with a link fallback. The browser test asserts on the second CDP
  target existing, which is the same mechanism a user's browser uses.
