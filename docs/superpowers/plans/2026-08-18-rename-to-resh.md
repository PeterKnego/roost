# Rename `deadlight` to `resh` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rename the project, its binary, its environment contract and its state
directory from `deadlight` to `resh`, on both the repo and the deploy host,
leaving no working reference to the old name.

**Architecture:** A rename is mechanical but has three sharp edges, and each one
gets its own task rather than being folded into a sweep: the **crate/binary**
(compile-time, caught by the compiler), the **environment contract**
(`RESH_*` variables that live inside every terminal and in any hook the user
wrote — silently broken, never caught by a compiler), and **on-disk state**
(`~/.local/state/deadlight/`, which a rename orphans). A blanket
find-and-replace is explicitly rejected: the string `deadlight` appears in
historical design documents and in test fixtures where it means "a project
called deadlight", not "this product".

**Tech Stack:** Rust 2021 (no async runtime), plain JS, systemd user units,
tailscale serve.

**Spec:** None. This plan implements a naming decision taken in conversation on
2026-08-18, after checking that `resh` is absent from apt and Homebrew, is not
a shell builtin, and collides only with a dormant `curl|bash`-installed shell
history tool ([curusarn/resh](https://github.com/curusarn/resh)). `rush` was
rejected (GNU Rush ships a `rush` binary in both apt and Homebrew) and `rehash`
was rejected (a zsh/tcsh builtin, so it can never be reached from `$PATH`).

## Global Constraints

- The new name is exactly `resh`, lowercase, everywhere: crate, binary, env
  prefix (`RESH_`), state dir, systemd unit, tailnet hostname.
- **Runtime state is disposable.** The user confirmed the existing sessions and
  saved layouts are test data, so there is NO state migration: the old state
  directory is deleted, not read. This is the one shortcut this plan takes, and
  it is only safe because it was explicitly authorised.
- **Historical design documents are not rewritten.** Everything under
  `docs/superpowers/specs/` and `docs/superpowers/plans/` dated before today
  records what was true when written; renaming inside them would falsify the
  record. They keep their filenames and contents. Living docs (`README.md`,
  `CLAUDE.md`, `docs/deploy.md`, `docs/notifications.md`, `docs/backlog.md`) are
  updated.
- `cargo test`, never `cargo test --release`.
- Every task ends green: `cargo test` passes before the commit.
- The deploy host is reached as `ssh claude@<deploy-host-ip>` (the tailnet name works
  but stops at a Tailscale SSH browser check — see `docs/deploy.md`).
- Deploy verification is `running md5 == built md5`; `cargo build` alone does not
  deploy.

---

### Task 1: Crate and binary

**Files:**
- Modify: `Cargo.toml:2`
- Modify: `Cargo.lock` (regenerated, not hand-edited)
- Modify: every `src/*.rs` and `tests/integration.rs` using `deadlight::`

**Interfaces:**
- Produces: crate `resh`, binary `resh`, external test path `resh::…`.

- [ ] **Step 1: Rename the package**

In `Cargo.toml`, line 2:

```toml
name = "resh"
```

- [ ] **Step 2: Watch it fail**

Run: `cargo build 2>&1 | head -20`
Expected: FAIL — `tests/integration.rs` and `src/main.rs` still say
`deadlight::`, e.g. ``error[E0433]: failed to resolve: use of undeclared crate
or module `deadlight` ``.

- [ ] **Step 3: Update the 22 crate references**

```bash
grep -rIl 'deadlight::' src/ tests/ | xargs sed -i '' 's/deadlight::/resh::/g'
```

(GNU sed: drop the `''` after `-i`.)

- [ ] **Step 4: Regenerate the lockfile and build**

Run: `cargo build && cargo test 2>&1 | grep -E '^test result'`
Expected: PASS, 231 lib + 37 integration. `Cargo.lock` now names `resh`.

- [ ] **Step 5: Confirm the binary is renamed**

Run: `ls target/debug/resh && ! ls target/debug/deadlight 2>/dev/null && echo ok`
Expected: `ok`

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src tests
git commit -m "rename: crate and binary become resh"
```

---

### Task 2: The environment contract

**Files:**
- Modify: `src/wsstate.rs:44` (`DEADLIGHT_STATE_DIR`)
- Modify: `src/projects.rs` (`DEADLIGHT_ROOTS`)
- Modify: `src/config.rs` (`DEADLIGHT_ORIGINS`)
- Modify: `src/session.rs` (`DEADLIGHT_CMD`, and the three it exports)
- Modify: `src/watch.rs` (`DEADLIGHT_DEBOUNCE_MS`)
- Modify: `tests/integration.rs`, `src/*.rs` test modules (~245 occurrences total)

**Interfaces:**
- Produces: `RESH_ROOTS`, `RESH_STATE_DIR`, `RESH_ORIGINS`, `RESH_CMD`,
  `RESH_DEBOUNCE_MS`, and the three exported into every terminal:
  `RESH_NOTIFY`, `RESH_PROJECT`, `RESH_SESSION`.

- [ ] **Step 1: Write the failing test**

Add to `src/session.rs`'s test module. This reads the environment back **out of
a real PTY child**, because the three exported variables are invisible to the
compiler and to any assertion made on strings this test wrote itself — a test
that merely checks its own literals would pass with the rename half-done.

```rust
#[test]
fn a_terminal_carries_the_resh_environment_contract() {
    let _g = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // `env` prints the child's environment to its stdout, which is the PTY —
    // so it arrives back through this attachment's own subscriber channel.
    std::env::set_var("RESH_CMD", "env");
    let d = tempfile::tempdir().unwrap();
    let att = attach("envproj", "shell", d.path()).expect("attach");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut seen = String::new();
    while std::time::Instant::now() < deadline {
        match att.rx.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(chunk) => {
                seen.push_str(&String::from_utf8_lossy(&chunk));
                if seen.contains("RESH_SESSION") {
                    break;
                }
            }
            Err(_) => {}
        }
    }
    kill_project("envproj");
    std::env::remove_var("RESH_CMD");

    assert!(seen.contains("RESH_NOTIFY=1"), "child env lacked RESH_NOTIFY: {seen:?}");
    assert!(seen.contains("RESH_PROJECT=envproj"), "child env lacked RESH_PROJECT: {seen:?}");
    assert!(seen.contains("RESH_SESSION=shell"), "child env lacked RESH_SESSION: {seen:?}");
    assert!(
        !seen.contains("DEADLIGHT_"),
        "a terminal still exports the old prefix, so hooks would see both: {seen:?}"
    );
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test --lib a_terminal_carries_the_resh`
Expected: FAIL — `attach` still reads `DEADLIGHT_CMD`, so with only `RESH_CMD`
set it spawns real `dtach` instead of `env`, nothing matching arrives on the
channel, and the first assertion reports the child env lacked `RESH_NOTIFY`.

- [ ] **Step 3: Rename the prefix everywhere**

```bash
grep -rIl 'DEADLIGHT_' src/ tests/ | xargs sed -i '' 's/DEADLIGHT_/RESH_/g'
```

- [ ] **Step 4: Verify no old name survives in code**

Run: `! grep -rI 'DEADLIGHT_' src/ tests/ && echo clean`
Expected: `clean`

- [ ] **Step 5: Run the suite**

Run: `cargo test 2>&1 | grep -E '^test result'`
Expected: PASS on all four result lines.

- [ ] **Step 6: Commit**

```bash
git add src tests
git commit -m "rename: DEADLIGHT_* environment contract becomes RESH_*"
```

---

### Task 3: State directory and user-visible strings

**Files:**
- Modify: `src/wsstate.rs:49` (default state dir)
- Modify: `src/render.rs:188,190,300,303` (breadcrumb, `<title>`, header)
- Modify: `src/session.rs`, `src/notify.rs`, `src/registry.rs`, `src/term.rs`,
  `src/watch.rs`, `src/hub.rs`, `src/fileops.rs`, `src/routes.rs`,
  `src/origin.rs`, `src/lib.rs` (the `eprintln!("deadlight: …")` log prefix)

**Interfaces:**
- Produces: state at `~/.local/state/resh/`, log lines prefixed `resh:`, and
  the picker/workspace chrome reading `resh`.

- [ ] **Step 1: Write the failing test**

Add to `src/wsstate.rs`'s test module:

```rust
#[test]
fn the_default_state_dir_is_named_for_the_product() {
    let _g = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("RESH_STATE_DIR");
    let d = state_dir();
    assert!(
        d.ends_with(".local/state/resh"),
        "default state dir must follow the product name, got {d:?}"
    );
    assert!(
        !d.to_string_lossy().contains("deadlight"),
        "the old name must not survive in a path users will find on disk: {d:?}"
    );
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test --lib the_default_state_dir_is_named`
Expected: FAIL — `got ".../.local/state/deadlight"`.

- [ ] **Step 3: Rename the state dir and the chrome**

In `src/wsstate.rs:49`, `.join(".local/state/deadlight")` becomes
`.join(".local/state/resh")`. Then the log prefix and visible strings:

```bash
grep -rIl 'deadlight' src/ static/ | xargs sed -i '' 's/deadlight/resh/g'
```

This also rewrites the test fixtures that use `"deadlight"` as a *project*
name (`src/notify.rs`, `src/projects.rs`); that is harmless — they are
arbitrary names — and keeps them realistic.

- [ ] **Step 4: Verify and run the suite**

Run: `! grep -rI 'deadlight' src/ static/ && cargo test 2>&1 | grep -E '^test result'`
Expected: no output from grep, then PASS on all four result lines.

- [ ] **Step 5: Check the rendered chrome by eye**

```bash
rm -rf /tmp/resh-fix && mkdir -p /tmp/resh-fix/proj && (cd /tmp/resh-fix/proj && git init -q)
RESH_ROOTS=/tmp/resh-fix RESH_STATE_DIR=/tmp/resh-state ./target/debug/resh 8470 &
sleep 3; curl -s http://127.0.0.1:8470/ | grep -o '<title>[^<]*</title>\|crumb-current">[^<]*'
pkill -f 'target/debug/resh 8470'; rm -rf /tmp/resh-fix /tmp/resh-state
```

Expected: `<title>resh</title>` and `crumb-current">resh` — no `deadlight`.

- [ ] **Step 6: Commit**

```bash
git add src static
git commit -m "rename: state directory, log prefix and page chrome become resh"
```

---

### Task 4: Living documentation

**Files:**
- Modify: `README.md`, `CLAUDE.md`, `docs/deploy.md`, `docs/notifications.md`,
  `docs/backlog.md`
- Do NOT modify: `docs/superpowers/specs/*`, `docs/superpowers/plans/*` (except
  this plan)

**Interfaces:**
- Consumes: the `RESH_*` names from Task 2 and the `~/.local/state/resh/` path
  from Task 3 — the docs must quote them exactly.

- [ ] **Step 1: Rename in the living docs only**

```bash
sed -i '' 's/DEADLIGHT_/RESH_/g; s/deadlight/resh/g' \
  README.md CLAUDE.md docs/deploy.md docs/notifications.md docs/backlog.md
```

- [ ] **Step 2: Add the rename note to `docs/deploy.md`**

Insert after the "Running" heading:

```markdown
**The project was called `deadlight` until 2026-08-18.** Anything on disk from
before then — `~/.local/state/deadlight/`, a `deadlight.service` unit, a
`~/.local/bin/deadlight` binary — is from the old name and is not read by this
build. The historical design documents under `docs/superpowers/` keep the old
name deliberately: they record what was true when they were written.
```

- [ ] **Step 3: Verify the historical record is untouched**

Run: `git diff --name-only | grep -c 'superpowers/specs\|superpowers/plans/2026-08-1[67]'`
Expected: `0`

- [ ] **Step 4: Verify no stale instructions remain**

Run: `grep -rn 'DEADLIGHT_\|deadlight' README.md CLAUDE.md docs/deploy.md docs/notifications.md docs/backlog.md`
Expected: only the rename note added in Step 2.

- [ ] **Step 5: Commit**

```bash
git add README.md CLAUDE.md docs/
git commit -m "docs: rename to resh, keeping the historical record intact"
```

- [ ] **Step 6: Push, so the host has something to pull**

Task 5 Step 3 runs `git pull --ff-only` on the deploy host. Without this push it
would pull the pre-rename tree, build a binary still called `deadlight`, and the
`install` would fail on a missing path.

```bash
git push origin master
```

Expected: the rename commits land on `origin/master` (still under the old repo
name at this point — the repository itself is renamed in Task 6).

---

### Task 5: Deploy host cutover

**Files:** none in the repo — this task changes the deploy host.

**Interfaces:**
- Consumes: the `resh` binary from Task 1 and `~/.local/state/resh/` from Task 3.

- [ ] **Step 1: Stop the old service and remove its state**

The user confirmed this state is disposable test data.

```bash
ssh claude@<deploy-host-ip> 'systemctl --user stop deadlight; systemctl --user disable deadlight; \
  rm -rf ~/.local/state/deadlight ~/.local/bin/deadlight ~/.config/systemd/user/deadlight.service; \
  systemctl --user daemon-reload; echo "old service gone: $(systemctl --user is-active deadlight 2>&1)"'
```

Expected: `old service gone: inactive`

- [ ] **Step 2: Write the new unit**

```bash
ssh claude@<deploy-host-ip> 'cat > ~/.config/systemd/user/resh.service <<UNIT
[Unit]
Description=resh — remote web workspace
After=network.target

[Service]
# KillMode=process is load-bearing: resh spawns dtach sessions as children so
# shells survive a restart. The default control-group KillMode kills the whole
# cgroup and takes every session with it.
KillMode=process
ExecStart=/home/claude/.local/bin/resh
Restart=on-failure
WorkingDirectory=/home/claude/projects/resh

[Install]
WantedBy=default.target
UNIT
systemctl --user daemon-reload && systemctl --user enable resh && echo unit-written'
```

Expected: `unit-written`

- [ ] **Step 3: Rename the checkout and build**

```bash
ssh claude@<deploy-host-ip> 'mv /home/claude/projects/deadlight /home/claude/projects/resh && \
  cd /home/claude/projects/resh && git pull --ff-only && cargo build --release && \
  install -m 755 ~/.cache/cargo-target/release/resh ~/.local/bin/resh && echo installed'
```

Expected: `installed`

- [ ] **Step 4: Start it and verify the running binary is the built one**

```bash
ssh claude@<deploy-host-ip> 'systemctl --user start resh; sleep 4; \
  PID=$(systemctl --user show -p MainPID --value resh); \
  echo "active=$(systemctl --user is-active resh)"; \
  echo "running=$(md5sum /proc/$PID/exe|cut -d" " -f1)"; \
  echo "built  =$(md5sum ~/.cache/cargo-target/release/resh|cut -d" " -f1)"'
```

Expected: `active=active` and the two md5s identical.

- [ ] **Step 5: Point the tailnet name at it**

```bash
ssh claude@<deploy-host-ip> 'tailscale set --hostname=resh && sleep 5 && \
  tailscale serve --bg --https=443 http://127.0.0.1:8444 && \
  tailscale serve --https=8444 off; tailscale serve status'
```

Expected: a single route, `https://resh.<tailnet>.ts.net/` → `127.0.0.1:8444`.
The old `:8444` route and the dead `:443 → 8082` zellij route are both gone.

- [ ] **Step 6: Verify end to end**

```bash
curl -s -o /dev/null -w '%{http_code}\n' https://resh.<tailnet>.ts.net/
```

Expected: `200`

---

### Task 6: Rename the GitHub repository

**Files:** none — this changes the remote.

- [ ] **Step 1: Rename on GitHub**

```bash
gh repo rename resh --repo PeterKnego/deadlight --yes
```

- [ ] **Step 2: Update the local remote**

```bash
git remote set-url origin git@github.com:PeterKnego/resh.git
git remote -v
```

Expected: both lines show `PeterKnego/resh.git`.

- [ ] **Step 3: Verify push still works**

Run: `git push origin master && git remote show origin | head -3`
Expected: `Everything up-to-date`, tracking `PeterKnego/resh`.

- [ ] **Step 4: Update the host's remote too**

```bash
ssh claude@<deploy-host-ip> 'cd /home/claude/projects/resh && \
  git remote set-url origin git@github.com:PeterKnego/resh.git && git remote -v | head -1'
```

Expected: `origin  git@github.com:PeterKnego/resh.git (fetch)`

- [ ] **Step 5: Rename the local checkout**

Do this last — it changes the directory this session is running in.

```bash
cd /Users/peter/Projects && mv deadlight resh && cd resh && cargo test 2>&1 | grep -E '^test result'
```

Expected: PASS on all four result lines.

---

## Rollback

Nothing here is one-way except the two deletions in Task 5 Step 1, which the
user authorised. If the rename needs undoing: `gh repo rename deadlight`,
`git remote set-url` back, `tailscale set --hostname=<deploy-host>`, and
revert the commits — the crate rename is a normal code change.
