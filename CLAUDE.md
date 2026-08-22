# Working in resh

Single Rust binary, no async runtime, thread per connection, hand-rolled
HTTP (GET, plus two upload POSTs) with websockets, server-rendered HTML,
plain JS with no framework. See [README.md](README.md) for what it does and
[docs/deploy.md](docs/deploy.md) for running and deploying it.

## Hard constraints

These are load-bearing. Breaking one is a defect, not a style choice.

- **Bind `127.0.0.1` only.** The websocket spawns a shell; the loopback bind is
  the security boundary.
- **HTTP is GET-only apart from `POST /upload` and `POST /paste`.** Every other
  state change is a websocket intent. Those two endpoints are the entire CSRF
  surface, and the only thing closing it is that they check `Origin` exactly as
  the websocket handshakes do — *including refusing a request that carries
  none*, because a `multipart/form-data` POST is a CORS simple request that any
  page can submit cross-origin with no preflight. Keep the surface at two.
- **Every browser-facing websocket checks `Origin`** in its handshake, and
  refuses a handshake that carries none. Handshakes bypass the same-origin
  policy, so a socket without this check is drive-by RCE; and every browser
  sends an `Origin`, so its absence means a non-browser client, which has no
  business on a socket that spawns a shell. `src/origin.rs` is the check.
- **The IDE socket inverts that rule on purpose.** `src/ide.rs` refuses any
  handshake that *carries* an `Origin` and authenticates by constant-time
  comparison against the lock-file token instead. Its client is a Bun process,
  which sends no `Origin`, so on that socket a browser is the only thing that
  sends one — and a browser has no business there. Both rules are right, and
  they are opposites because their clients are.

  **Reconciling the two is the vulnerability, not the cleanup.** Claude Code's
  own extensions shipped exactly this socket Origin-blind *and* unauthenticated
  through 1.0.23: any page the user visited could scan localhost, connect, and
  read files with no user interaction. That is CVE-2025-52882, fixed in 1.0.24
  by the lock-file token `src/ide.rs` implements. Making `ide.rs` "consistent"
  with the bullet above — or dropping its token check because the Origin check
  above looks like it already covers it — reintroduces it.
- **Every filesystem path is confined** before use: `projects::safe_resolve`
  for existing targets, `projects::safe_resolve_parent` for creation and rename
  destinations (it canonicalises the parent and validates the final component,
  because the target does not exist yet).
- **Session names match `^[A-Za-z0-9_-]{1,32}$`** — they land in a dtach socket
  path and a command line.
- **A text file opens in Edit; only a file with a rendered form opens in
  Preview** (markdown, images — and svg, which has both and keeps the ✎ that
  switches). A file the server cannot read as text is demoted back to Preview
  by `hub::open_buffer_for`, silently when the mode was a default and with a
  banner when a user asked for it: an empty textarea over a file that is not
  empty is how work gets overwritten.
- **Project storage keys are percent-encoded** (`karpie%2Fsrc`) while URLs keep
  readable slashes. Existing top-level keys must stay byte-for-byte identical.
- **Never hold a lock across blocking I/O.** This project has already shipped
  one deadlock that way (the global session registry held across a PTY write,
  which wedged every session in every project).
- **No panics may escape a socket or watcher thread.**
- **Destruction requires positive evidence.** See below — this is the constraint
  this codebase breaks most often.
- **A replay is not a byte log.** An attaching client is sent
  `screen::Screens::replay()`, not the raw ring. A full-screen app declares the
  alternate screen exactly once, so that declaration ages out of any bounded
  log and is tracked and re-emitted at attach time instead. Anything that
  changes what the pump stores has to keep that property, or exiting Claude
  paints over its own leftover frame again.
- Caps: ≤16 sessions per project, ≤50 buffers with unsaved changes (a buffer
  holds no text at all until it actually differs from its file, so this bounds
  dirty files, not open ones), 1 MB scrollback *per screen
  buffer* (normal and alternate are kept apart, so an app cannot evict the
  scrollback it hands back), 2 MB file
  cap for reads *and* buffer writes. Uploads are bounded per **request**, not
  per file: ≤16 parts and `config::max_upload_bytes` (100 MB default, global
  config or `RESH_MAX_UPLOAD` only — never per-project, or a cloned repo could
  raise its own disk ceiling). On the IDE socket: ≤16 proposals parked per
  project (`ide::MAX_PENDING`) — per project, not global, so one Claude in a
  loop cannot starve every other project — and 8 MB per websocket frame
  (`ide::MAX_FRAME_BYTES`), a coarse backstop against buffering an oversized
  frame at all. It is not the real limit: *both* sides of an `openDiff` are
  bounded by the same 2 MB `MAX_FILE_BYTES` as any other file, since a
  proposal is retained for the tab's life and broadcast whole to every
  connected browser.

## Absence of evidence is not evidence of absence

**Eleven separate defects in one feature were the same mistake**: code concluded
something was gone, dead, or empty because the *check* failed, then destroyed it.
Every one could kill a user's long-running shell or overwrite their file.

| What failed | What it was read as | What it destroyed |
|---|---|---|
| A form decoder turning `+` into a space | project `gtk+` doesn't exist | its live session |
| `pgrep -f` treating a path as a regex | nothing holds this socket | its live session |
| An unreadable root | every project is gone | every live session |
| A *partially* unreadable root | those projects are gone | their live sessions |
| A space in a path defeating word-splitting | nothing holds this socket | its live session |
| `ps` failing to spawn (empty `Vec`) | no process holds it | a live session's socket |
| The socket's holder not yet visible | the socket is an orphan | an unreachable shell |
| A key not resolving under *these* roots | the directory was deleted | every session outside them |
| An empty or truncated `.origin` marker | the directory was deleted | the recorded session |
| `Path::is_dir()` swallowing `EACCES` | the directory was deleted | the recorded session |
| `Path::exists()` on a dangling symlink | the destination is free | the symlink's target |

The last one is in a different module from the rest, so treat this as systemic,
not as a `registry.rs` quirk.

**The rule:** "I could not determine X" is a third outcome, never folded into
"X is false". Concretely:

- A subprocess has three results, not two: success, failure, and *ran but I
  cannot trust the output* (non-zero exit, or empty stdout where a live system
  must produce some). Check `status.success()`; never `unwrap_or_default()` a
  snapshot that gates a kill.
- `Path::exists()` / `is_dir()` collapse "not there" and "cannot look" into
  `false`, and both follow symlinks. Before anything destructive use
  `symlink_metadata` and match: `Err(NotFound)` → absent, `Err(_)` → **cannot
  tell, do nothing**, `Ok(_)` → present.
- Prefer suspending a sweep over guessing at one item. Stale rows in the UI are
  recoverable; a SIGKILLed shell is not.
- Write persistent evidence atomically (temp file with a pid-unique name, then
  `rename`), or a reader will see it half-written and act on the gap.

When a decision is destructive and irreversible, the burden of proof is on
destroying, not on keeping.

## Style

- Module-level `//!` doc explaining *why* the module exists.
- Implementation first, `#[cfg(test)] mod tests` at the bottom of the same file.
- Comments give rationale, not mechanics. Explain the non-obvious decision, not
  what the next line does.
- All HTML is built in Rust in `render.rs`; escape everything interpolated.
- `cargo test`, never `cargo test --release`.

## Testing: the lesson this codebase learned the hard way

**Tests that pass for the wrong reason are the dominant failure mode here.**
Seven separate reviews caught tests that could not fail:

- Path-confinement tests that failed with `ENOENT` before ever reaching the
  confinement check — which is why a symlink escape survived review.
- A mirroring test that could not distinguish "correctly skipped the author"
  from "wrongly deleted the author".
- Tests with a single subscriber, where `send_to` and `broadcast` are
  indistinguishable, so message privacy could regress silently.
- A test that asserted nothing and passed off its own 5-second read timeout.
- Strip tests with no glyph assertion, so swapping ● and ○ left them all green.
- An escaping test whose fixture contained no metacharacter to escape.
- A worktree self-parenting test whose buggy branch never executed, because the
  fixture never entered the state that triggers it.
- A test whose subject was `attach`'s call to `record_origin`, but which only
  asserted survival — also true with no marker written at all.

When writing a test, ask: **would this fail if I deleted the code it covers?**
If a negative test errors, assert on *why* — the message, not just `is_err()`.

**The technique that actually works: revert the fix and watch the test fail.**
Not a thought experiment — apply the broken version, run it, read the failure,
restore. This caught two tests that would have shipped green and vacuous, and
one whose whole design was non-discriminating (it measured client-visible
response ordering, which this project pipelines through a per-connection writer
thread, so it passed with the bug fully restored). Doing it also documents the
failure mode in the test's own comment for the next reader.

**A green suite proves less than it looks.** A deadlock *hangs* rather than
fails, so counting failures across repeated runs says nothing about lock
ordering — time the runs too. And a defect that only manifests between
concurrently-running tests will look like a flake in whichever test loses; a
"~1-in-8 flake" here turned out to be one test's `reconcile` reaping another
test's live session, which no amount of retrying would have revealed.

## The dev/prod substitution trap

Several real defects were invisible to a green test suite because tests
substitute something simpler for the real thing. Expect this class:

| Substitution | What it hid |
|---|---|
| `RESH_CMD=cat` instead of `dtach` | The dtach socket directory was never created — terminals would have died at spawn in production |
| macOS FSEvents instead of Linux inotify | Directories created after startup were never watched |
| No browser | Saving was completely broken (`base_hash` never initialised, so every save conflicted) |
| No systemd | `KillMode=control-group` killed every dtach session on restart, defeating the reason dtach is used |

So: **run the suite on the Linux host too** (`ssh` in and `cargo test`), and
**verify UI behavior in a real browser** before believing it works. Both have
caught defects that 100+ passing tests did not.

Some of that browser check is now automated: `deno run -A
tests/browser/reconnect.mjs` and `upload.mjs` drive a real Chromium against a
real resh with real dtach. It is deliberately outside `cargo test` (it needs a browser and
takes tens of seconds) and it skips when no browser is present. Anything
touching `static/app.js` should be checked there, since no Rust test can reach
that file. See [tests/browser/README.md](tests/browser/README.md) — especially
the four traps that make a browser test pass while asserting nothing.

## Verify, don't trust

- Check `git log` and `git status` against what a report claims. A commit has
  been reported that was never created.
- After deploying, confirm the *running* binary changed — `cargo build` alone
  updates neither path the service uses (see `docs/deploy.md`).
- **Build from one checkout.** This host points every cargo workspace at a
  single shared `target-dir`, and `build.rs` bakes *absolute* asset paths into
  its generated table. A `cargo build` from a second checkout of this repo — a
  git worktree, say — therefore rewrites that table with the other checkout's
  paths and leaves the shared binary built from the other checkout's source.
  Nothing announces this: cargo reports `Fresh resh`, the browser tests go on
  passing, and they are testing the wrong tree. Recover with
  `cargo clean -p resh`, and confirm with
  `grep -o '/home/[^\"]*static' $(cargo metadata --format-version 1 --no-deps |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/build/resh-*/out/assets_table.rs | head -1`.
  To compare against another branch, check it out in *this* directory.
- Check which branch the deploy host is on. `git pull --ff-only` will report
  "Already up to date" while sitting on a stale feature branch.

## Process

Design docs live in `docs/superpowers/specs/`, implementation plans in
`docs/superpowers/plans/`. For anything beyond a small fix, write the spec
first and get it reviewed, then the plan, then implement task-by-task with a
review between tasks.
