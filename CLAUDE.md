# Working in deadlight

Single Rust binary, no async runtime, thread per connection, hand-rolled
GET-only HTTP plus websockets, server-rendered HTML, plain JS with no
framework. See [README.md](README.md) for what it does and
[docs/deploy.md](docs/deploy.md) for running and deploying it.

## Hard constraints

These are load-bearing. Breaking one is a defect, not a style choice.

- **Bind `127.0.0.1` only.** The websocket spawns a shell; the loopback bind is
  the security boundary.
- **HTTP stays GET-only.** Every state change is a websocket intent. This is
  why there is no CSRF surface — keep it that way.
- **Every websocket checks `Origin`** in its handshake. Handshakes bypass the
  same-origin policy, so a socket without this check is drive-by RCE.
- **Every filesystem path is confined** before use: `projects::safe_resolve`
  for existing targets, `projects::safe_resolve_parent` for creation and rename
  destinations (it canonicalises the parent and validates the final component,
  because the target does not exist yet).
- **Session names match `^[A-Za-z0-9_-]{1,32}$`** — they land in a dtach socket
  path and a command line.
- **Project storage keys are percent-encoded** (`karpie%2Fsrc`) while URLs keep
  readable slashes. Existing top-level keys must stay byte-for-byte identical.
- **Never hold a lock across blocking I/O.** This project has already shipped
  one deadlock that way (the global session registry held across a PTY write,
  which wedged every session in every project).
- **No panics may escape a socket or watcher thread.**
- Caps: ≤16 sessions per project, ≤50 open buffers, 1 MB scrollback, 2 MB file
  cap for reads *and* writes.

## Style

- Module-level `//!` doc explaining *why* the module exists.
- Implementation first, `#[cfg(test)] mod tests` at the bottom of the same file.
- Comments give rationale, not mechanics. Explain the non-obvious decision, not
  what the next line does.
- All HTML is built in Rust in `render.rs`; escape everything interpolated.
- `cargo test`, never `cargo test --release`.

## Testing: the lesson this codebase learned the hard way

**Tests that pass for the wrong reason are the dominant failure mode here.**
Four separate reviews caught tests that could not fail:

- Path-confinement tests that failed with `ENOENT` before ever reaching the
  confinement check — which is why a symlink escape survived review.
- A mirroring test that could not distinguish "correctly skipped the author"
  from "wrongly deleted the author".
- Tests with a single subscriber, where `send_to` and `broadcast` are
  indistinguishable, so message privacy could regress silently.
- A test that asserted nothing and passed off its own 5-second read timeout.

When writing a test, ask: **would this fail if I deleted the code it covers?**
If a negative test errors, assert on *why* — the message, not just `is_err()`.

## The dev/prod substitution trap

Several real defects were invisible to a green test suite because tests
substitute something simpler for the real thing. Expect this class:

| Substitution | What it hid |
|---|---|
| `DEADLIGHT_CMD=cat` instead of `dtach` | The dtach socket directory was never created — terminals would have died at spawn in production |
| macOS FSEvents instead of Linux inotify | Directories created after startup were never watched |
| No browser | Saving was completely broken (`base_hash` never initialised, so every save conflicted) |
| No systemd | `KillMode=control-group` killed every dtach session on restart, defeating the reason dtach is used |

So: **run the suite on the Linux host too** (`ssh` in and `cargo test`), and
**verify UI behavior in a real browser** before believing it works. Both have
caught defects that 100+ passing tests did not.

## Verify, don't trust

- Check `git log` and `git status` against what a report claims. A commit has
  been reported that was never created.
- After deploying, confirm the *running* binary changed — `cargo build` alone
  updates neither path the service uses (see `docs/deploy.md`).
- Check which branch the deploy host is on. `git pull --ff-only` will report
  "Already up to date" while sitting on a stale feature branch.

## Process

Design docs live in `docs/superpowers/specs/`, implementation plans in
`docs/superpowers/plans/`. For anything beyond a small fix, write the spec
first and get it reviewed, then the plan, then implement task-by-task with a
review between tasks.
