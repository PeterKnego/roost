# Rename `roost` to `remloft` — Implementation Plan

**Status: CANCELLED, never started.** Written 2026-09-06, cancelled 2026-09-08.
The name stays `roost`. Kept as the record of a considered and rejected
decision, so that the reasoning below is not rediscovered from scratch — but
nothing here is to be executed.

What the cancellation costs, and how each cost is paid, is in
[docs/roost-packaging-handover.md](../../roost-packaging-handover.md) §6.1. The
short version: the `PATH` collision with `navbytes/roost` is now permanent and
is stated in every package description; the Homebrew tap is namespaced and
needs no change; the AUR package needs `conflicts=('roost')`; crates.io is
unaffected.

**Tech Stack:** Rust 2021 (no async runtime), plain JS, Deno browser tests,
systemd user units, tailscale serve. GNU sed (`\b` supported in both basic and
extended mode).

**Spec:** None, matching the precedent of
[2026-09-02-rename-to-roost.md](2026-09-02-rename-to-roost.md): a rename
implements a naming decision, it does not design anything.

**The decision, and why.** `roost` collides head-on with
[`navbytes/roost`](https://github.com/navbytes/roost) — a Rust, MIT,
*"session-native terminal multiplexer for AI agent CLIs"*, repo created
2026-07-20 (four weeks before ours), actively developed (pushed 2026-09-06),
installable today via `brew install navbytes/tap/roost`. Both projects declare
`name = "roost"` in `Cargo.toml` and both install a binary literally named
`roost`, so a user with both gets whichever `$PATH` entry comes first, silently.
Thirteen further agent tools carry the name. Verified 2026-09-06: `remloft` is
free on crates.io (and `remloft-cli`/`remloft_cli`/`remloft-rs`), npm, PyPI,
Docker Hub and Debian; has **zero** GitHub repositories bearing the name; scores
0 on `remloft terminal`, `remloft claude code`, `remloft agent` and
`remloft workspace`; is absent from homebrew-core and every tap; and
`github.com/remloft`, `remloft.dev`, `remloft.sh`, `remloft.io` and
`remloft.com` are all unregistered.

**This rename takes the clean migration.** The 2026-09-02 rename kept
`~/.local/state/resh/` by pinning `ROOST_STATE_DIR` in the unit, to avoid
ending live sessions. That pin is why the host today runs a binary called
`roost` out of a directory called `resh`. This plan ends every session once,
on purpose, and lands on `~/.local/state/remloft/` with no pin — collapsing
three product names in the on-disk contract down to one.

## Inventory (measured 2026-09-06)

Occurrences of `roost`, any case, excluding `.git/` and `target/`:

| Area | Count | Treatment |
|---|---|---|
| `src/` | 926 | sweep |
| `tests/` | 492 | sweep |
| `docs/` (living) | 173 | sweep, then hand-edit two paragraphs |
| `static/` | 50 | sweep |
| root files | 47 | sweep (`Cargo.toml`, `README.md`, `CLAUDE.md`, `SECURITY.md`, `Cargo.lock`) |
| **`docs/superpowers/`** | **552** | **DO NOT TOUCH** — historical record |

Environment contract, 16 distinct variables:

```
ROOST_STATE_DIR 299   ROOST_CMD 122   ROOST_ROOTS 68   ROOST_NOTIFY 39
ROOST_CONFIG 38   ROOST_PROJECT 35   ROOST_SESSION 33   ROOST_STATIC 21
ROOST_PING_SECS 15   ROOST_MAX_UPLOAD 13   ROOST_HEALTH_SECS 5
ROOST_ORIGINS 4   ROOST_DEBOUNCE_MS 4   ROOST_MISSING 1   ROOST_FOUND 1
```

Host state, read 2026-09-06:

```
~/.local/bin/roost              (binary, Sep 6 09:35)
~/.local/bin/resh -> roost      (legacy symlink from the last rename)
~/.local/bin/resh.pre-roost     (stale backup binary, Aug 31)
~/.config/systemd/user/roost.service
  Environment=ROOST_ROOTS=/home/claude/ultima:/home/claude/projects
  Environment=ROOST_STATE_DIR=%h/.local/state/resh
  ExecStart=%h/.local/bin/roost
  WorkingDirectory=/home/claude/projects/roost
~/.local/state/resh/            6 live sockets  <- the service's, pinned
~/.local/state/roost/           0 sockets       <- default path, dev/test runs
~/.config/roost/
```

**Six live sessions** will be ended by this plan:

```
sock/roost/term            sock/roost/term1
sock/explore/term          sock/ste_skill/term
sock/ultima_cluster/term   sock/ultima_cluster%2F.claude%2Fworktrees%2Fclaude-1/term
```

No project under either root carries a `.roost/` or `.resh/` directory, so
there is no per-project config to migrate. Exactly one deployed Claude settings
file carries the hook command: `/home/claude/projects/roost/.claude/settings.local.json`.

## Global Constraints

- The new name is exactly `remloft`, lowercase, everywhere: crate, binary, env
  prefix (`REMLOFT_`), default state dir, config dir, per-project dir, systemd
  unit, log prefix, rendered chrome, lock `ideName`, hook command.
- **`docs/superpowers/` keeps every old name.** Specs and plans record what was
  true when written. A sweep that touches them is a defect.
- **The sweep is whole-word for `roost`, but prefix-literal for `ROOST_`.**
  `grep -w ROOST` matches *nothing* — the underscore is a word character — so
  a `\bROOST\b` sweep silently misses all 682 environment references. Use two
  sweeps and verify each independently.
- **Build from one checkout** (CLAUDE.md): this host shares one cargo
  `target-dir` and `build.rs` bakes absolute asset paths into its generated
  table. Do not build this from a worktree.
- `cargo test -- --test-threads=1`. A bare `cargo test` hangs on this host.
- Every task ends green, with a commit.

---

### Task 1: Crate and binary

- [ ] **Step 1** — `Cargo.toml`: `name = "roost"` → `"remloft"`. Update
      `repository`/`homepage` to `https://github.com/PeterKnego/remloft` (the
      GitHub rename lands in Task 7; the URL redirects either way).
- [ ] **Step 2 — watch it fail.** `cargo build` must fail on the `roost::`
      paths in `tests/` and `src/main.rs` before any are fixed. Record the
      first error. A build that succeeds here means the crate name is not
      actually load-bearing and the sweep in Task 3 is doing this work
      silently, which is the failure mode to avoid.
- [ ] **Step 3** — rewrite `roost::` → `remloft::` across `src/` and `tests/`.
- [ ] **Step 4** — `cargo build`, regenerate `Cargo.lock`, run the suite.
- [ ] **Step 5** — confirm the binary is renamed:
      `ls $(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/remloft`
      and confirm the asset table points at *this* checkout (CLAUDE.md's
      shared-target-dir check).
- [ ] **Step 6** — commit: `rename: crate and binary become remloft`.

### Task 2: The environment contract

682 references across 16 variables. `ROOST_SESSION` and `ROOST_PROJECT` are
**exported into the shell of every session**, so any user script reading them
breaks — that is a real user-visible break, called out in Task 5's docs.

- [ ] **Step 1** — rewrite the discriminating test at `src/wsstate.rs:338-339`
      (`"default state dir must follow the product name"`) to expect
      `REMLOFT_*` and `.local/state/remloft`.
- [ ] **Step 2 — run it to see it fail.** Not a thought experiment: apply,
      run, read the failure, keep it.
- [ ] **Step 3** — sweep `ROOST_` → `REMLOFT_` across `src/ tests/ static/
      docs/ *.toml *.md`, **excluding `docs/superpowers/`**.
- [ ] **Step 4 — verify no old prefix survives outside the historical record:**
      `grep -rIn --exclude-dir=.git --exclude-dir=target --exclude-dir=superpowers 'ROOST_' .`
      must return nothing.
- [ ] **Step 5** — full suite, `--test-threads=1`.
- [ ] **Step 6** — commit: `rename: ROOST_* environment contract becomes REMLOFT_*`.

### Task 3: On-disk names and user-visible strings

Default state dir, `~/.config/roost/`, per-project `.roost/`, systemd unit
name, log prefix, lock `ideName`, and the 27 capitalised `Roost` strings in
rendered chrome.

- [ ] **Step 1** — repoint the two discriminating tests: the default state dir
      literal at `src/wsstate.rs:106`, and the lock `ideName` check at
      `src/idelock.rs:203` with its fixture at `:235`. The `ideName` test is
      already known-discriminating — its own comment at `:296` records
      *"Dropping the `ideName` check makes this the only failing test —
      verified by doing it."*
- [ ] **Step 2 — run both to see them fail.**
- [ ] **Step 3** — whole-word sweep, historical record excluded:
      `grep -rIl --exclude-dir=.git --exclude-dir=target --exclude-dir=superpowers -w roost . | xargs sed -i 's/\broost\b/remloft/g'`
      and the same for `\bRoost\b` → `Remloft`.
- [ ] **Step 4 — verify the sweep hit only what it should.** `git diff --stat`
      must show zero files under `docs/superpowers/`. Confirm the count of
      changed lines is within sight of the inventory above (1,496 + 27 − 552
      historical ≈ 971 lowercase + capitalised); a wildly different number
      means the word boundary matched something unintended.
- [ ] **Step 5** — full suite plus the two tests from Step 2.
- [ ] **Step 6 — check the rendered chrome and log prefix by eye.** No Rust
      test reaches `static/app.js`; run `deno run -A tests/browser/reconnect.mjs`
      against a scratch instance (never the live one — it clamps every terminal
      to the smallest client's geometry).
- [ ] **Step 7** — commit: `rename: state dir, config paths, lock ideName, log prefix and chrome become remloft`.

### Task 4: The Claude hook command — NEW, no precedent

The 2026-09-02 rename did not face this: `claudehooks` landed 2026-09-03, so
`resh claude-hook` never existed on disk. `roost claude-hook` does.

`src/claudehooks.rs:26` is `pub const COMMAND: &str = "roost claude-hook";`
and `:100` tests ownership by **exact string equality**:

```rust
entry.get("command").and_then(|c| c.as_str()) == Some(COMMAND)
```

A grep for legacy-name handling in that module returns nothing. So changing
`COMMAND` orphans every deployed entry: `remloft` will not recognise it, will
not rewrite it, will not remove it, and the entry keeps invoking a binary by
the old name. CLAUDE.md's constraint — a `Stop` hook that exits non-zero shows
an error in every Claude session in that checkout — is what makes this louder
than a stale row in a UI.

- [ ] **Step 1** — add a failing test asserting `COMMAND == "remloft claude-hook"`
      and that `set` leaves a hand-written `remloft notify` hook alone (the
      "ownership is this exact string, nothing looser" property at `:25`).
- [ ] **Step 2 — watch it fail.**
- [ ] **Step 3** — change `COMMAND`, and the three test fixtures at `:321`,
      `:374`, `:376`.
- [ ] **Step 4 — sweep the one deployed settings file by hand:**
      `/home/claude/projects/roost/.claude/settings.local.json`. Re-run the
      search over both roots afterwards to confirm no `roost claude-hook`
      entry survives anywhere:
      `grep -rl 'roost claude-hook' ~/projects ~/ultima --include=settings.local.json`
- [ ] **Step 5** — commit: `rename: the Claude hook command becomes remloft claude-hook`.

### Task 5: Living documentation

- [ ] **Step 1** — sweep `docs/` (excluding `superpowers/`), `README.md`,
      `CLAUDE.md`, `SECURITY.md`.
- [ ] **Step 2** — rewrite the rename note in `docs/deploy.md` by hand: it
      currently explains the `resh`→`roost` state-dir pin, which this plan
      **removes**. Replace with the clean-migration history: state dir moved to
      `~/.local/state/remloft/` on 2026-09-06, no pin, sessions ended once.
- [ ] **Step 3** — rewrite the hooks paragraph in `docs/notifications.md`
      ("Existing hooks and scripts must be updated by hand"), naming
      `REMLOFT_NOTIFY`, `REMLOFT_SESSION`, `REMLOFT_PROJECT` and
      `remloft claude-hook`, and stating that scripts written against
      `ROOST_*` or `RESH_*` break.
- [ ] **Step 4 — verify the historical record is untouched:**
      `git diff --name-only | grep superpowers` must be empty.
- [ ] **Step 5** — commit and push.

### Task 6: Host cutover — the clean migration

**Ordering is load-bearing.** Sessions must end *before* the directory moves.
A dtach master carries its socket path in its argv (`src/registry.rs:208`
runs `ps -Aww -o pid=,args=` and matches the path as a whole argument), so a
socket reached through a new path — moved *or* symlinked — reads as held by
nothing, gets unlinked, and leaves its shell alive and unreachable forever.

- [ ] **Step 1 — preflight.** Record what is about to be destroyed, so the
      report afterwards is checkable: `find ~/.local/state/resh -type s`,
      `ps -Aww -o pid=,args= | grep '[d]tach'`, and copy
      `~/.config/systemd/user/roost.service` and `~/.local/bin/roost` aside
      as `.pre-remloft`.
- [ ] **Step 2 — tell the user, then end all six sessions deliberately.** This
      is the one irreversible step in the plan and it is not recoverable by
      rollback. Close each from the UI, or `systemctl --user stop roost` then
      kill the six masters by pid. Confirm zero sockets remain:
      `find ~/.local/state/resh ~/.local/state/roost -type s` returns nothing.
- [ ] **Step 3 — build and install:** `cargo build --release`, install to
      `~/.local/bin/remloft`.
- [ ] **Step 4 — move the state.** `mv ~/.local/state/resh ~/.local/state/remloft`.
      Project storage keys are the *filenames* inside it (`aeron-go.json`,
      `ultima_cluster%2F….json`) and CLAUDE.md requires they stay byte-for-byte
      identical — moving the directory preserves them; do not rename anything
      inside. Then fold in the stray default-path dir: `~/.local/state/roost/`
      holds three dev-run project files, an `ide/` dir and `error.log`, and no
      sockets. Merge or discard deliberately, do not leave it.
      `mv ~/.config/roost ~/.config/remloft`.
- [ ] **Step 5 — write the new unit** `remloft.service`, with **no**
      `Environment=*_STATE_DIR` line at all — that omission is the point of
      this rename. Keep `KillMode=process` and its comment (load-bearing:
      the default cgroup kill takes every session with it). Update
      `Environment=REMLOFT_ROOTS=…`, `ExecStart=%h/.local/bin/remloft`,
      `WorkingDirectory=/home/claude/projects/remloft`.
- [ ] **Step 6 — cut over:** `systemctl --user disable --now roost`, enable and
      start `remloft`, in one command so the gap is seconds.
- [ ] **Step 7 — verify the running binary actually changed** (CLAUDE.md: a
      `cargo build` updates neither path the service uses). Check
      `systemctl --user show remloft -p ExecMainPID` and the `/proc/<pid>/exe`
      link, not just that the unit is active. Open the workspace in a real
      browser and start a new session; confirm its socket appears under
      `~/.local/state/remloft/sock/`.
- [ ] **Step 8 — clean up the legacy layer**, now that nothing depends on it:
      remove `~/.local/bin/resh`, `~/.local/bin/resh.pre-roost`,
      `~/.config/systemd/user/resh.service.bak-20260820-035220`, and (after
      Step 7 passes) `~/.local/bin/roost`. Leaving a `roost` symlink is
      tempting and would keep stale hooks quiet — resist it; Task 4 already
      swept the only deployed hook, and the symlink is how a fourth name layer
      starts.
- [ ] **Step 9 — update the host notes** in `docs/deploy.md` with the observed
      cutover, including the six ended sessions.

### Task 7: GitHub and crates.io

- [ ] **Step 1** — `gh repo rename remloft`. Redirects are permanent for repo,
      git and release-asset URLs, and stars/issues/releases carry over
      (verified against `facebook/jest` → `jestjs/jest`). **Never** create a
      new `PeterKnego/roost` afterwards — that is the only thing that breaks
      the redirect.
- [ ] **Step 2** — `git remote set-url origin git@github.com:PeterKnego/remloft.git`,
      prove a push works.
- [ ] **Step 3** — update the 12 hardcoded URLs (`Cargo.toml:8,9`;
      `README.md:14,15,50,57`; `docs/roost-packaging-handover.md:4,282`; the
      four in the 2026-09-02 plan stay, historical).
- [ ] **Step 4** — publish `remloft` to crates.io starting at **0.6.0**, so the
      version line reads as continuous with `roost` 0.5.0.
- [ ] **Step 5** — yank `roost` 0.4.0 and 0.5.0. Yanking blocks new dependency
      resolution and deletes nothing; the 22 existing downloads keep working.
      Do **not** attempt to delete the crate — crates.io is permanent, and
      holding the name keeps it away from `navbytes/roost`, whose `Cargo.toml`
      declares `name = "roost"`.
- [ ] **Step 6** — tick this plan's boxes and commit it.

---

## Rollback

Tasks 1–5 and 7 are ordinary commits and revert cleanly; the GitHub rename
reverses by renaming back.

**Task 6 Step 2 does not roll back.** Six live shells end. Everything after it
is recoverable from the `.pre-remloft` copies; that step is not. Do not begin
Task 6 until Tasks 1–5 are green and the user has confirmed the sessions can go.

## Deferred

- **The checkout directory** `~/projects/roost` → `~/projects/remloft`. Renaming
  it removes project `roost` from the roots and ends its sessions — including
  the terminal this plan is likely being executed from. Same deferral as the
  last rename, same reason. Do it manually after Task 6, from outside the
  checkout, and update `WorkingDirectory=` in the unit.
- **`remloft.dev` / `.sh` / `.com`** are unregistered as of 2026-09-06.
  Registering one is not part of this plan.
- **Homebrew tap.** `homebrew-remloft` and a `Formula/remloft.rb` are free.
  Out of scope; noted because `navbytes/roost` reaching brew first is part of
  why this rename is happening.
