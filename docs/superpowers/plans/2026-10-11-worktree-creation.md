# roost creates every worktree, and the terminal follows — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a Claude in a roost terminal asks for a worktree, the user approves it in the browser, roost creates it in Claude Code's naming convention, and the asking terminal is re-homed into the worktree's project without the process moving; roost can also start new work in a worktree itself (fork / fresh / shell).

**Architecture:** A `WorktreeCreate` hook (`roost worktree-hook`) asks the server over a new 0600 Unix socket; the hub parks the request, asks the browsers, and on approval creates the worktree and writes a re-home record under `state/home/`. The session never moves — socket path, `SESSIONS` key and env keep naming the main checkout (its *home*) — and every ownership lookup (live list, Close Project, removal gate, Claude evidence, notifications, IDE delivery, overview) reads the record to find its *owner*.

**Tech Stack:** Rust (std only for the socket: `std::os::unix::net`, `libc` for `SO_PEERCRED` if not already a dependency — check `Cargo.toml` first), plain JS in `static/app.js`, Deno + Chromium browser tests in `tests/browser/`.

**Spec:** `docs/superpowers/specs/2026-10-11-worktree-creation-design.md` (issue #156; corrections in https://github.com/PeterKnego/roost/issues/156#issuecomment-6111548159). Read it before any task; the tables in "Why re-homing is an alias" and §4 are the contract.

## Global Constraints

- Worktree location `.claude/worktrees/worktree-<name>`, branch `worktree-<name>`, base = HEAD of the main checkout, recorded with `worktree::write_base`.
- `<name>` matches `^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$`; a taken name is refused, never suffixed.
- A session's socket path, `SESSIONS` key and environment name its **home** forever. Re-homing changes ownership through `state/home/<home storage key>/<name>` and nothing else. `registry::reconcile` is not modified by this plan.
- Re-home records are written atomically (pid-unique temp + `rename`) and read three-valued: `NotFound` → not re-homed, unreadable/empty → **unknown** (destroy nothing, show in home), contents → owner URL.
- Hook socket: `state/hook.sock`, mode 0600, `SO_PEERCRED` uid must equal ours, one request + one reply, newline-delimited JSON, 64 KiB read bound.
- HTTP surface unchanged: still exactly `POST /upload` and `POST /paste`.
- `roost worktree-hook` is installed **only** on `WorktreeCreate`, with `"timeout": 120`; exits non-zero only on a `refuse` reply or an unreadable reply. `roost claude-hook` behaviour is unchanged.
- Ask defaults: no browser connected → refuse at once ("no roost tab is open on this project to approve it"); no answer in 60 s → refuse ("nobody approved it in roost within 60 s"); one pending ask per session.
- Never hold a lock across blocking I/O (git, socket reads, waiting for a browser). No panic may escape a socket thread.
- Tests: `cargo test -- --test-threads=1` (a bare `cargo test` hangs on this host); never `--release`. Every new test is watched failing with its code reverted, and the observed failure is noted in the test's comment.
- Build only from `/home/claude/projects/roost` (shared `target-dir`); never in a git worktree.

## Review Focus

1. **Close Project on the main checkout while a re-homed Claude runs** — the user expects that Claude to keep running in the worktree tab. Pinned in Task 4 (`kill_project_of_home_spares_a_resident`).
2. **A session name that exists natively in the worktree and as a resident** (`term3` in both) — a reasonable person expects two distinct tabs, never one attaching the other's shell. Pinned in Task 4 (`owner_next_free_name_skips_resident_names`) and Task 6 (tab carries `home`).
3. **Worktree deleted by hand (`git worktree remove --force`) under a resident** — the terminal must reappear in the main checkout, still alive. Pinned in Task 3 (`owner_gone_falls_back_to_home_and_keeps_record`).
4. **roost restarted while an ask is pending** — the hook must get an answer (connection reset → unreadable reply → refuse), not hang Claude for 120 s silently. Pinned in Task 8 (`hook_exits_1_when_server_drops_connection`).
5. **Two browsers answer the same ask** — the first answer wins, the second is a no-op, the worktree is created once. Pinned in Task 9 (`second_answer_to_an_ask_is_ignored`).

---

### Task 0: Re-probe Claude Code in interactive mode, and the fork directory

No code. The spec's probe ran in `-p` mode only, and "a fork starts in its launch directory" is from docs.

**Files:** none committed. Notes go in the PR description at the end.

- [ ] **Step 1: Set up a scratch repo with a logging hook**

```bash
P=$(mktemp -d)/probe && mkdir -p "$P/.claude" "$P/log" && cd "$P" && git init -q -b main . && git -c user.email=p@x -c user.name=p commit -q --allow-empty -m init
cat > "$P/hook.sh" <<'EOF'
#!/bin/sh
d=$(dirname "$0"); in=$(cat)
ev=$(printf '%s' "$in" | python3 -c 'import json,sys;print(json.load(sys.stdin)["hook_event_name"])')
printf '%s\n' "$in" > "$d/log/$ev.$(date +%s%N).json"
if [ "$ev" = WorktreeCreate ]; then
  n=$(printf '%s' "$in" | python3 -c 'import json,sys;print(json.load(sys.stdin)["name"])')
  git -C "$d" worktree add -q -b "worktree-$n" "$d/.claude/worktrees/worktree-$n" HEAD >/dev/null 2>&1
  echo "$d/.claude/worktrees/worktree-$n"
fi
EOF
chmod +x "$P/hook.sh"
printf '{"hooks":{"WorktreeCreate":[{"hooks":[{"type":"command","command":"%s/hook.sh"}]}],"CwdChanged":[{"hooks":[{"type":"command","command":"%s/hook.sh"}]}],"Stop":[{"hooks":[{"type":"command","command":"%s/hook.sh"}]}]}}\n' "$P" "$P" "$P" > "$P/.claude/settings.local.json"
echo "$P"
```

- [ ] **Step 2: Interactive EnterWorktree**

In a real terminal (`tmux new -s probe`), `cd "$P" && claude --model haiku`, then type: `Use EnterWorktree with name iw1, then run pwd.` Approve the tool prompts. In another terminal, while it is alive: `readlink /proc/$(pgrep -nx claude)/cwd`.

Expected: `log/` holds `WorktreeCreate` and `Stop` (whose `cwd` is `…/worktree-iw1`), **no** `CwdChanged`; the readlink is the worktree. Any difference: stop and report to the user before Task 1 — the spec rests on these.

- [ ] **Step 3: Fork directory**

Note the session id from `Stop`'s payload (`session_id`). `/exit`. Then `cd "$P/.claude/worktrees/worktree-iw1" && claude --resume <id> --fork-session`, ask `run pwd`.

Expected: pwd is the worktree, and the conversation history is present. If `--resume` cannot find the session from that directory ("No conversation found"), stop and report: Task 11's Fork option depends on it.

- [ ] **Step 4: Record the results** in a scratch note for the PR description. Nothing to commit.

---

### Task 1: Worktree names — validation and generator

**Files:**
- Create: `src/wtname.rs`
- Modify: `src/lib.rs` (add `pub mod wtname;` in alphabetical position)

**Interfaces:**
- Produces: `wtname::valid(name: &str) -> bool`; `wtname::check(name: &str) -> Result<(), String>` (message quotes the rule); `wtname::generate(taken: &dyn Fn(&str) -> bool) -> Option<String>`; `wtname::dir_name(name: &str) -> String` (= `format!("worktree-{name}")`).

- [ ] **Step 1: Write the failing tests** at the bottom of `src/wtname.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_names_that_could_be_an_argument_or_a_path() {
        for bad in ["", "--foo", "-x", "_x", "../x", "a/b", "a b", "a.b", &"a".repeat(33)] {
            let e = check(bad).unwrap_err();
            assert!(e.contains("[A-Za-z0-9][A-Za-z0-9_-]"), "message must quote the rule, got {e:?} for {bad:?}");
        }
    }

    #[test]
    fn accepts_claude_codes_own_shapes() {
        for good in ["bright-running-fox", "Foo-bar_2", "a", &"a".repeat(32)] {
            assert!(check(good).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn generator_skips_taken_names_and_gives_up_when_all_are_taken() {
        let first = generate(&|_| false).unwrap();
        assert!(valid(&first) && first.split('-').count() == 3, "{first}");
        let again = generate(&|n| n == first).unwrap();
        assert_ne!(again, first);
        assert!(generate(&|_| true).is_none());
    }

    #[test]
    fn dir_name_is_claude_codes_prefix() {
        assert_eq!(dir_name("bright-fox"), "worktree-bright-fox");
    }
}
```

- [ ] **Step 2: Run, expect a compile failure** (`check` etc. undefined)

Run: `cargo test --lib wtname -- --test-threads=1`
Expected: FAIL, `cannot find function check`.

- [ ] **Step 3: Implement**

```rust
//! Worktree names. roost and Claude Code put worktrees in the same place, so
//! they use one convention — `worktree-<name>` for the directory and the
//! branch — and a name from either side passes the same gate before it
//! reaches a path or a `git` argument. A leading `-` would read as an option
//! and a leading `_` buys nothing, so both are refused.

const RULE: &str = "^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$";
const MAX_TRIES: u32 = 64;

// Small on purpose: three words from these give 16*16*16 names, and the
// generator only needs to avoid what this one repository already has.
const ADJ: [&str; 16] = [
    "amber", "brave", "bright", "calm", "clever", "eager", "gentle", "golden",
    "lucky", "mellow", "nimble", "quiet", "rapid", "silent", "steady", "swift",
];
const VERB: [&str; 16] = [
    "dancing", "drifting", "flying", "gliding", "humming", "jumping", "leaping", "racing",
    "roaming", "rolling", "running", "sailing", "singing", "soaring", "spinning", "wandering",
];
const NOUN: [&str; 16] = [
    "badger", "comet", "falcon", "fox", "heron", "lynx", "maple", "otter",
    "panda", "pebble", "raven", "river", "spruce", "tiger", "walrus", "willow",
];

pub fn valid(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_alphanumeric()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
}

pub fn check(name: &str) -> Result<(), String> {
    if valid(name) { Ok(()) } else { Err(format!("worktree name {name:?} must match {RULE}")) }
}

pub fn dir_name(name: &str) -> String {
    format!("worktree-{name}")
}

/// A fresh `adjective-verb-noun`, skipping any name `taken` reports. Seeded
/// from the clock and pid: uniqueness comes from `taken`, not from the seed.
pub fn generate(taken: &dyn Fn(&str) -> bool) -> Option<String> {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ (std::process::id() as u64).rotate_left(32);
    for _ in 0..MAX_TRIES {
        // xorshift64: no dependency, and quality is irrelevant here.
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        let n = format!("{}-{}-{}", ADJ[(x & 15) as usize], VERB[((x >> 4) & 15) as usize], NOUN[((x >> 8) & 15) as usize]);
        if !taken(&n) {
            return Some(n);
        }
    }
    None
}
```

- [ ] **Step 4: Run, expect PASS**

Run: `cargo test --lib wtname -- --test-threads=1`
Expected: 4 passed.

- [ ] **Step 5: Revert check.** Change `b[0].is_ascii_alphanumeric()` to `true`, rerun, confirm `refuses_names_that_could_be_an_argument_or_a_path` fails on `"--foo"`; restore. Note the failure in that test's comment.

- [ ] **Step 6: Commit**

```bash
git add src/wtname.rs src/lib.rs
git commit -m "wtname: one naming rule for roost's and Claude Code's worktrees (#156)"
```

---

### Task 2: `worktree::create_named`

**Files:**
- Modify: `src/worktree.rs` (add `create_named` next to `create`, tests at the bottom)

**Interfaces:**
- Consumes: `wtname::check`, `wtname::dir_name` (Task 1); existing `GitRunner`, `write_base`, `base_file`, `list`, `crate::gitio::is_inside_work_tree`, `crate::projects::safe_resolve_parent`.
- Produces: `pub fn create_named(repo: &Path, state_dir: &Path, name: &str, wt_key_of: &dyn Fn(&str) -> String, run: GitRunner) -> Result<Created, String>` — `Created.name` is the directory name (`worktree-<name>`), `wt_key_of` receives that directory name.

- [ ] **Step 1: Write the failing tests** (real git, like the existing `known_projects_groups_a_repos_worktrees…` test in `registry.rs`):

```rust
    fn git_repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for a in [&["init", "-q", "-b", "main"][..], &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "i"][..]] {
            assert!(std::process::Command::new("git").args(a).current_dir(d.path()).status().unwrap().success());
        }
        d
    }

    #[test]
    fn create_named_uses_claude_codes_convention_and_records_head() {
        let repo = git_repo();
        let st = tempfile::tempdir().unwrap();
        let c = create_named(repo.path(), st.path(), "bright-fox", &|n| format!("k-{n}"), &real_git).unwrap();
        assert_eq!(c.name, "worktree-bright-fox");
        assert!(c.path.ends_with(".claude/worktrees/worktree-bright-fox"));
        let br = real_git(&c.path, &["symbolic-ref", "--short", "HEAD"]).unwrap();
        assert_eq!(br.trim(), "worktree-bright-fox");
        assert_eq!(read_base(st.path(), "k-worktree-bright-fox").as_deref(), Some("main"));
    }

    #[test]
    fn create_named_refuses_a_taken_name_instead_of_suffixing() {
        let repo = git_repo();
        let st = tempfile::tempdir().unwrap();
        create_named(repo.path(), st.path(), "x", &|n| n.to_string(), &real_git).unwrap();
        let e = create_named(repo.path(), st.path(), "x", &|n| n.to_string(), &real_git).unwrap_err();
        assert!(e.contains("worktree-x") && e.contains("exists"), "{e}");
        assert_eq!(list(repo.path()).len(), 2, "no second worktree");
    }

    #[test]
    fn create_named_refuses_an_invalid_name_before_running_git() {
        let ran = std::cell::Cell::new(false);
        let run = |_: &Path, _: &[&str]| -> Result<String, String> { ran.set(true); Ok(String::new()) };
        let st = tempfile::tempdir().unwrap();
        let e = create_named(Path::new("/nonexistent"), st.path(), "--x", &|n| n.to_string(), &run).unwrap_err();
        assert!(e.contains("must match"), "{e}");
        assert!(!ran.get(), "git ran for a refused name");
    }
```

- [ ] **Step 2: Run, expect compile failure.** `cargo test --lib worktree::tests::create_named -- --test-threads=1` → `cannot find function create_named`.

- [ ] **Step 3: Implement** below `create`:

```rust
/// `create`, with the name supplied (by Claude Code, or by roost's own
/// generator) instead of minted as `claude-N`. A taken name is refused, not
/// suffixed: the asker named this worktree, and giving it another one is the
/// disagreement between roost and Claude Code that this exists to remove.
pub fn create_named(
    repo: &Path,
    state_dir: &Path,
    name: &str,
    wt_key_of: &dyn Fn(&str) -> String,
    run: GitRunner,
) -> Result<Created, String> {
    crate::wtname::check(name)?;
    if !crate::gitio::is_inside_work_tree(repo) {
        return Err("not a git repository".into());
    }
    let canon = repo.canonicalize().map_err(|e| format!("cannot resolve project directory: {e}"))?;
    let ws = list(repo);
    if ws.is_empty() {
        return Err("git did not answer (worktree list)".into());
    }
    match ws.iter().find(|w| w.path.canonicalize().ok().as_deref() == Some(canon.as_path())) {
        Some(w) if w.is_main => {}
        _ => return Err("start worktrees from the main checkout".into()),
    }
    let dir = crate::wtname::dir_name(name);
    let out = run(repo, &["branch", "--list", &dir])
        .map_err(|e| format!("cannot tell whether branch {dir} exists: {e}"))?;
    if !out.trim().is_empty() {
        return Err(format!("branch {dir} already exists"));
    }
    match std::fs::symlink_metadata(repo.join(".claude/worktrees").join(&dir)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => return Err(format!(".claude/worktrees/{dir} already exists")),
        Err(e) => return Err(format!("cannot tell whether .claude/worktrees/{dir} exists: {e}")),
    }
    std::fs::create_dir_all(repo.join(".claude/worktrees")).map_err(|e| e.to_string())?;
    let rel = format!(".claude/worktrees/{dir}");
    let path = crate::projects::safe_resolve_parent(repo, &rel)?;
    let base = match run(repo, &["symbolic-ref", "--short", "HEAD"]) {
        Ok(b) if !b.trim().is_empty() => b.trim().to_string(),
        _ => run(repo, &["rev-parse", "HEAD"]).map_err(|e| format!("cannot read HEAD: {e}"))?.trim().to_string(),
    };
    let key = wt_key_of(&dir);
    write_base(state_dir, &key, &base)?;
    if let Err(e) = run(repo, &["worktree", "add", "-b", &dir, &rel, "HEAD"]) {
        let _ = std::fs::remove_file(base_file(state_dir, &key));
        return Err(format!("git worktree add failed: {e}"));
    }
    Ok(Created { name: dir, path, base })
}
```

- [ ] **Step 4: Run, expect PASS** (3 tests).

- [ ] **Step 5: Revert check.** Delete the `branch --list` early return; rerun; `create_named_refuses_a_taken_name_instead_of_suffixing` must fail (git's own `worktree add -b` error message lacks "exists"? — read the actual failure and note it; if git's message happens to contain both words, the test is non-discriminating: tighten it to assert `e.starts_with("branch worktree-x already exists")`). Restore.

- [ ] **Step 6: Commit** `git add src/worktree.rs && git commit -m "worktree: create_named, Claude Code's convention, refuse taken names (#156)"`

---

### Task 3: `home.rs` — the re-home record

**Files:**
- Create: `src/home.rs`
- Modify: `src/lib.rs` (`pub mod home;`)

**Interfaces:**
- Consumes: `crate::projects::storage_key`, `crate::registry::decode_key`, `crate::session::valid_name`, `crate::wsstate::state_dir`.
- Produces:
  ```rust
  pub enum Owner { Here, Elsewhere(String), Unknown(String) }
  pub fn owner_of_in(state: &Path, home: &str, name: &str) -> Owner;
  pub fn record_in(state: &Path, home: &str, name: &str, owner: &str) -> Result<(), String>;
  pub fn forget_in(state: &Path, home: &str, name: &str);
  /// (home, name) of every session recorded as owned by `owner`.
  pub fn residents_of_in(state: &Path, owner: &str) -> Vec<(String, String)>;
  /// Names of `home`'s sessions recorded as owned elsewhere OR unknown.
  pub fn away_from_in(state: &Path, home: &str) -> Vec<(String, Owner)>;
  /// Removes records whose socket is NotFound; `sock_state(home, name)`
  /// answers Some(true)=present, Some(false)=NotFound, None=cannot tell.
  pub fn sweep_in(state: &Path, sock_state: &dyn Fn(&str, &str) -> Option<bool>);
  // and the same five without `_in`, using wsstate::state_dir().
  ```

- [ ] **Step 1: Write the failing tests:**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_record_is_here() {
        let st = tempfile::tempdir().unwrap();
        assert!(matches!(owner_of_in(st.path(), "foo", "term3"), Owner::Here));
    }

    #[test]
    fn recorded_owner_round_trips_and_lists_as_resident() {
        let st = tempfile::tempdir().unwrap();
        let wt = "foo/.claude/worktrees/worktree-x";
        record_in(st.path(), "foo", "term3", wt).unwrap();
        assert!(matches!(owner_of_in(st.path(), "foo", "term3"), Owner::Elsewhere(o) if o == wt));
        assert_eq!(residents_of_in(st.path(), wt), vec![("foo".to_string(), "term3".to_string())]);
        assert!(residents_of_in(st.path(), "foo").is_empty(), "the home is not the owner");
    }

    #[test]
    fn empty_or_unreadable_record_is_unknown_not_here() {
        // Reverting the empty-file arm to `Here` must fail this: an empty
        // file is a half-written record, and `Here` would let Close Project
        // on the home kill a session that belongs to a worktree.
        let st = tempfile::tempdir().unwrap();
        let d = st.path().join("home").join(crate::projects::storage_key("foo"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("term3"), "").unwrap();
        assert!(matches!(owner_of_in(st.path(), "foo", "term3"), Owner::Unknown(_)));
        std::fs::create_dir(d.join("term4")).unwrap(); // read_to_string on a dir errors
        assert!(matches!(owner_of_in(st.path(), "foo", "term4"), Owner::Unknown(_)));
        let away: Vec<_> = away_from_in(st.path(), "foo").into_iter().map(|(n, _)| n).collect();
        assert_eq!(away, vec!["term3".to_string(), "term4".to_string()]);
    }

    #[test]
    fn sweep_removes_only_on_positive_absence() {
        let st = tempfile::tempdir().unwrap();
        for n in ["gone", "alive", "unsure"] {
            record_in(st.path(), "foo", n, "foo/.claude/worktrees/worktree-x").unwrap();
        }
        sweep_in(st.path(), &|_, n| match n { "gone" => Some(false), "alive" => Some(true), _ => None });
        assert!(matches!(owner_of_in(st.path(), "foo", "gone"), Owner::Here));
        assert!(matches!(owner_of_in(st.path(), "foo", "alive"), Owner::Elsewhere(_)));
        assert!(matches!(owner_of_in(st.path(), "foo", "unsure"), Owner::Elsewhere(_)), "cannot tell → keep");
    }

    #[test]
    fn record_refuses_a_bad_session_name() {
        let st = tempfile::tempdir().unwrap();
        let e = record_in(st.path(), "foo", "../x", "foo/w").unwrap_err();
        assert!(e.contains("session name"), "{e}");
    }
}
```

- [ ] **Step 2: Run, expect compile failure.** `cargo test --lib home -- --test-threads=1`.

- [ ] **Step 3: Implement:**

```rust
//! Which project a terminal belongs to, when that is not the project it was
//! started in.
//!
//! A session cannot move. Its dtach socket path is baked into the master's
//! argv and every liveness check matches on it (`registry::pids_holding`), its
//! `SESSIONS` key is captured by the pump thread, and its environment is the
//! process's own. So a terminal re-homed into a worktree keeps all of those
//! naming its *home*, and this one record names its *owner*: the project it is
//! shown in, counted in, and closed with. `registry::reconcile` never reads
//! it, which is the point — the physical layout it sweeps does not change.
//!
//! Three outcomes on read, never two. An empty or unreadable record is
//! `Unknown`: it may be half-written, and treating it as `Here` would let
//! Close Project on the home end a Claude that belongs to a worktree.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    Here,
    Elsewhere(String),
    Unknown(String),
}

fn dir_in(state: &Path, home: &str) -> PathBuf {
    state.join("home").join(crate::projects::storage_key(home))
}

pub fn owner_of_in(state: &Path, home: &str, name: &str) -> Owner {
    match std::fs::read_to_string(dir_in(state, home).join(name)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Owner::Here,
        Err(e) => Owner::Unknown(format!("cannot read re-home record: {e}")),
        Ok(s) => {
            let s = s.trim_end_matches('\n');
            if s.is_empty() { Owner::Unknown("empty re-home record".into()) } else { Owner::Elsewhere(s.to_string()) }
        }
    }
}

pub fn record_in(state: &Path, home: &str, name: &str, owner: &str) -> Result<(), String> {
    if !crate::session::valid_name(name) {
        return Err(format!("bad session name {name:?}"));
    }
    let dir = dir_in(state, home);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    std::fs::write(&tmp, format!("{owner}\n")).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(name)).map_err(|e| { let _ = std::fs::remove_file(&tmp); e.to_string() })
}

pub fn forget_in(state: &Path, home: &str, name: &str) {
    if crate::session::valid_name(name) {
        let _ = std::fs::remove_file(dir_in(state, home).join(name));
    }
}

/// Every record, as (home, name, owner). Dot-files (temps) skipped.
fn all_in(state: &Path) -> Vec<(String, String, Owner)> {
    let mut out = Vec::new();
    let Ok(homes) = std::fs::read_dir(state.join("home")) else { return out };
    for h in homes.flatten() {
        let home = crate::registry::decode_key(&h.file_name().to_string_lossy());
        let Ok(names) = std::fs::read_dir(h.path()) else { continue };
        for n in names.flatten() {
            let name = n.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !crate::session::valid_name(&name) { continue; }
            let owner = owner_of_in(state, &home, &name);
            out.push((home.clone(), name, owner));
        }
    }
    out.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    out
}

pub fn residents_of_in(state: &Path, owner: &str) -> Vec<(String, String)> {
    all_in(state).into_iter()
        .filter_map(|(h, n, o)| matches!(&o, Owner::Elsewhere(x) if x == owner).then_some((h, n)))
        .collect()
}

pub fn away_from_in(state: &Path, home: &str) -> Vec<(String, Owner)> {
    all_in(state).into_iter().filter(|(h, _, _)| h == home).map(|(_, n, o)| (n, o)).collect()
}

pub fn sweep_in(state: &Path, sock_state: &dyn Fn(&str, &str) -> Option<bool>) {
    for (home, name, _) in all_in(state) {
        if sock_state(&home, &name) == Some(false) {
            forget_in(state, &home, &name);
        }
    }
}

pub fn owner_of(home: &str, name: &str) -> Owner { owner_of_in(&crate::wsstate::state_dir(), home, name) }
pub fn record(home: &str, name: &str, owner: &str) -> Result<(), String> { record_in(&crate::wsstate::state_dir(), home, name, owner) }
pub fn forget(home: &str, name: &str) { forget_in(&crate::wsstate::state_dir(), home, name) }
pub fn residents_of(owner: &str) -> Vec<(String, String)> { residents_of_in(&crate::wsstate::state_dir(), owner) }
pub fn away_from(home: &str) -> Vec<(String, Owner)> { away_from_in(&crate::wsstate::state_dir(), home) }
pub fn sweep(sock_state: &dyn Fn(&str, &str) -> Option<bool>) { sweep_in(&crate::wsstate::state_dir(), sock_state) }
```

Note `away_from_in` returns `Here` entries too only if a record exists (it never does for `Here`, since `Here` = NotFound). Fine.

Owner-gone fallback (Review Focus 3) lives in the consumer (Task 4: `owned_live_names` treats an `Elsewhere(o)` whose `o` no longer resolves as `Here` for display, and never forgets the record). Add this test here, as it is about the record surviving:

```rust
    #[test]
    fn owner_gone_falls_back_to_home_and_keeps_record() {
        let st = tempfile::tempdir().unwrap();
        record_in(st.path(), "foo", "term3", "foo/.claude/worktrees/worktree-gone").unwrap();
        sweep_in(st.path(), &|_, _| Some(true)); // socket still there
        assert!(matches!(owner_of_in(st.path(), "foo", "term3"), Owner::Elsewhere(_)),
            "a vanished owner is not evidence the session ended");
    }
```

- [ ] **Step 4: Run, expect PASS** (6 tests).

- [ ] **Step 5: Revert check.** Change the empty arm to `Owner::Here`; `empty_or_unreadable_record_is_unknown_not_here` must fail. Change `sweep_in`'s `== Some(false)` to `!= Some(true)`; `sweep_removes_only_on_positive_absence` must fail on `unsure`. Restore both; note the failures in comments.

- [ ] **Step 6: Commit** `git add src/home.rs src/lib.rs && git commit -m "home: the re-home record, three-valued (#156)"`

---

### Task 4: Session ownership — live list, names, Close Project, end

**Files:**
- Modify: `src/session.rs` — add `owned_live_names`, `resolves`-aware owner check, change `kill_project`, `end_session`; tests at bottom.
- Modify: `src/hub.rs:1472` `refresh_live_sessions` → `owned_live_names`; `do_new_terminal`/`do_start_terminal` pass resident names as `also_taken`.
- Modify: `src/registry.rs` — call `home::sweep` right after `modes::sweep_orphans` (`:1015`), with `sock_state` from `symlink_metadata(session::socket_path(home, name))`. This is a call *after* reconcile's own logic; reconcile's decisions are untouched.

**Interfaces:**
- Consumes: `home::{owner_of, residents_of, away_from, forget, Owner}` (Task 3).
- Produces: `session::owned_live_names(project: &str) -> Vec<(String, Option<String>)>` — `(name, home)` where `home` is `None` for a native session and `Some(home)` for a resident; `session::kill_project` keeps its signature and return value (count ended) and additionally returns nothing new — the skipped-unknown count is logged via `eprintln!` and surfaced by Task 6's view field.

- [ ] **Step 1: Write the failing tests.** These need real dtach (not `ROOST_CMD=cat` — the dev/prod table). Use the existing test helper that spawns a real session (`reserve_and_attach`) with a `dtach`-backed command, as the existing `kill_project` tests in `session.rs` do; read them first and reuse their fixture (state dir isolation via `ROOST_STATE_DIR` and the per-test project names).

```rust
    /// The test the whole re-home design rests on. Reverting the
    /// `Owner::Elsewhere` skip in `kill_project` must kill the session —
    /// watch that failure before trusting this test.
    #[test]
    fn kill_project_of_home_spares_a_resident() {
        let fx = Fixture::new("rehome-spare");          // existing helper: temp state dir + project dir
        let home = fx.project();                          // e.g. "rehome-spare"
        let owner = format!("{home}/.claude/worktrees/worktree-x");
        reserve_and_attach(&home, "term3", fx.dir()).unwrap();
        let pid = master_pid_of(&home, "term3");          // pids_holding_path on socket_path
        crate::home::record(&home, "term3", &owner).unwrap();

        kill_project(&home);

        assert!(process_alive(pid), "Close Project on the home killed a re-homed session");
        assert!(matches!(probe_socket(&socket_path(&home, "term3")), SocketState::Held(_)));
        let owned: Vec<_> = owned_live_names(&owner);
        assert_eq!(owned, vec![("term3".to_string(), Some(home.clone()))]);
        assert!(owned_live_names(&home).is_empty(), "still listed under its home");

        kill_project(&owner);
        assert!(!process_alive(pid), "closing the owner must end its residents");
    }

    #[test]
    fn kill_project_of_home_spares_an_unknown_record() {
        let fx = Fixture::new("rehome-unknown");
        let home = fx.project();
        reserve_and_attach(&home, "term3", fx.dir()).unwrap();
        let pid = master_pid_of(&home, "term3");
        let d = crate::wsstate::state_dir().join("home").join(crate::projects::storage_key(&home));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("term3"), "").unwrap();
        kill_project(&home);
        assert!(process_alive(pid), "an unreadable owner is not permission to kill");
        assert_eq!(owned_live_names(&home), vec![("term3".to_string(), None)], "unknown shows in its home");
    }

    #[test]
    fn end_session_forgets_the_record() {
        let fx = Fixture::new("rehome-end");
        let home = fx.project();
        reserve_and_attach(&home, "term3", fx.dir()).unwrap();
        crate::home::record(&home, "term3", "x/w").unwrap();
        end_session(&home, "term3");
        assert!(matches!(crate::home::owner_of(&home, "term3"), crate::home::Owner::Here));
    }

    #[test]
    fn owner_next_free_name_skips_resident_names() {
        // In the owner, `term3` is taken by the resident even though the
        // owner's own sock/ directory has no term3.
        let fx = Fixture::new("rehome-names");
        let home = fx.project();
        let owner = format!("{home}/.claude/worktrees/worktree-x");
        reserve_and_attach(&home, "term", fx.dir()).unwrap();
        crate::home::record(&home, "term", &owner).unwrap();
        let taken: Vec<String> = owned_live_names(&owner).into_iter().map(|(n, _)| n).collect();
        assert_eq!(next_free_name(&owner, &taken).as_deref(), Some("term1"));
    }
```

If `Fixture`, `master_pid_of`, `process_alive` do not exist under those names in `session.rs`'s test module, write them there from the existing `kill_project` tests' inline code (do not invent a parallel fixture in another file).

- [ ] **Step 2: Run, expect failures** (`owned_live_names` undefined).

- [ ] **Step 3: Implement** in `session.rs`:

```rust
/// What `project`'s workspace shows: its own live sessions minus those
/// re-homed elsewhere, plus sessions re-homed into it from their home.
/// `None` home = native. Fork-free, like `live_names`: it runs under the hub
/// registry lock. A resident whose owner no longer resolves is shown in its
/// home instead (the worktree was removed by hand); its record is kept.
pub fn owned_live_names(project: &str) -> Vec<(String, Option<String>)> {
    let away: std::collections::HashMap<String, crate::home::Owner> =
        crate::home::away_from(project).into_iter().collect();
    let mut out: Vec<(String, Option<String>)> = live_names(project)
        .into_iter()
        .filter(|n| match away.get(n) {
            Some(crate::home::Owner::Elsewhere(o)) => !owner_resolves(o),
            _ => true, // Here, or Unknown: shown in its home
        })
        .map(|n| (n, None))
        .collect();
    for (home, name) in crate::home::residents_of(project) {
        if live_names(&home).contains(&name) && owner_resolves(project) {
            out.push((name, Some(home)));
        }
    }
    out.sort();
    out.dedup();
    out
}

fn owner_resolves(owner: &str) -> bool {
    crate::projects::resolve_project(&crate::projects::roots(), owner).is_some()
}
```

(Check the exact signature of `projects::resolve_project`/`roots` before writing — the inventory found `resolve_project(roots, url)` in `registry.rs:678`; use whatever that call uses.)

In `kill_project`, after building `all_names`:

```rust
    // A re-homed session belongs to its owner, and an unreadable record is
    // not permission to end it (`home.rs`). Only `Here` is ours to close.
    let mut skipped_unknown = 0usize;
    all_names.retain(|n| match crate::home::owner_of(project, n) {
        crate::home::Owner::Here => true,
        crate::home::Owner::Elsewhere(_) => false,
        crate::home::Owner::Unknown(why) => {
            skipped_unknown += 1;
            eprintln!("roost: Close Project left {project}/{n} running: {why}");
            false
        }
    });
    // ...and residents re-homed *into* this project are closed with it.
    let residents = crate::home::residents_of(project);
```

and after the existing `for name in &all_names` loop:

```rust
    for (home, name) in &residents {
        if end_session(home, name) {
            ended += 1;
        }
    }
```

Note: `begin_close(project)` drained map entries for `project`'s key, which include the re-homed sessions (their map key is the home's). Those map entries must not be drained for an away session: change `begin_close` to take the set of names to keep, or — simpler and lock-safe — compute `away` *before* `begin_close` and pass it in: `begin_close_except(project, &away_names)`. Write `begin_close_except` beside `begin_close` (same body, `retain` the excepted keys) and make `begin_close` call it with `&[]`.

In `end_session`, after `crate::relaunch::forget(project, name);` add `crate::home::forget(project, name);`.

In `hub.rs` `refresh_live_sessions`: `self.ws.live_sessions` stays `Vec<String>` (names) — set it from `owned_live_names(&self.project).into_iter().map(|(n, _)| n).collect()`, and store the homes in a new `self.ws.session_homes: BTreeMap<String, String>` (resident name → home), added to `WsState` with `#[serde(default)]` (Task 6 sends it to the client). Every `next_free_name(_with)` call in `hub.rs` passes those resident names in `also_taken`.

In `registry.rs` after `modes::sweep_orphans(...)`:

```rust
        crate::home::sweep(&|home, name| match std::fs::symlink_metadata(crate::session::socket_path(home, name)) {
            Ok(_) => Some(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
            Err(_) => None,
        });
```

(Place it once per reconcile pass, outside the per-key loop.)

- [ ] **Step 4: Run** `cargo test --lib session -- --test-threads=1` and `cargo test --lib hub -- --test-threads=1`. Expected: new tests pass, existing pass. Time the run (`time`); compare with a baseline run on `develop` — a hang is a deadlock, not a slow test.

- [ ] **Step 5: Revert check.** Make the `Elsewhere` arm in `kill_project`'s retain return `true`; `kill_project_of_home_spares_a_resident` must fail on "killed a re-homed session". Revert `begin_close_except` to plain `begin_close`; the same test must fail (the pump's map entry is drained → session killed by `child.kill()`) — if it does *not* fail, the map path is not exercised and the test needs the session attached in this process (it is, via `reserve_and_attach`; investigate before moving on). Restore.

- [ ] **Step 6: Commit** `git add src/session.rs src/hub.rs src/registry.rs src/workspace.rs src/wsstate.rs && git commit -m "session: ownership via re-home records; Close Project spares residents (#156)"`

---

### Task 5: Ownership consumers — removal gate, Claude evidence, notifications, IDE, overview

**Files:**
- Modify: `src/hub.rs` `do_remove_worktree` (`:1810-1821`), `publish` (`:2625`)
- Modify: `src/registry.rs` `removable` callers / `worktree_rows` (`:1262-1285`), `known_projects_inner` live count
- Modify: `src/claudes.rs` — `names_for` / `claude_evidence_with_scan` / `tick`: map `(project, session)` through `home::owner_of`
- Modify: `src/ide.rs` — delivery of `openDiff`/selection for a session to the owner's hub (`notify_selected` `:542`, the proposal broadcast)
- Modify: `src/routes.rs:277` `build_overview_sessions`

**Interfaces:**
- Consumes: `home::owner_of`, `session::owned_live_names` (Tasks 3–4).
- Produces: `home::effective_owner(home: &str, name: &str) -> String` — the owner URL for an `Elsewhere` whose owner resolves, else `home`. Add it to `home.rs` (it calls `session`'s `owner_resolves`; move `owner_resolves` to `home.rs` as `pub fn resolves(owner:&str)->bool` and have `session.rs` call it).

- [ ] **Step 1: Write the failing tests:**

In `hub.rs` tests, next to `remove_refuses_a_worktree_with_a_live_terminal` (reuse `repo_with_worktree`):

```rust
    #[test]
    fn remove_refuses_a_worktree_with_a_live_resident() {
        // Reverting the residents check must let removal delete the worktree
        // under a running re-homed Claude.
        let tmp = tempfile::tempdir().unwrap();
        let (mut h, home, _wt_dir) = repo_with_worktree(tmp.path());
        let wt_url = format!("{home}/.claude/worktrees/feat"); // match the fixture's worktree
        crate::session::reserve_and_attach(&home, "term3", &h.dir).unwrap();
        crate::home::record(&home, "term3", &wt_url).unwrap();
        let a = conn(&mut h);
        h.handle(&a, Intent::RemoveWorktree { key: crate::projects::storage_key(&wt_url) });
        let msg = last_error(&h, &a);
        assert!(msg.contains("terminal") , "{msg}");
    }
```

In `claudes.rs` tests (fake `/proc`, existing `mk` helper):

```rust
    #[test]
    fn a_rehomed_claude_counts_for_its_owner_not_its_home() {
        let st = tempfile::tempdir().unwrap();
        crate::home::record_in(st.path(), "karpie", "term3", "karpie/.claude/worktrees/worktree-x").unwrap();
        mk(100, "claude", "ROOST_PROJECT=karpie\nROOST_SESSION=term3\n");
        let procs = claude_terminals(root);
        let owned = names_for_owned(st.path(), "karpie/.claude/worktrees/worktree-x", &procs);
        assert_eq!(owned, vec!["term3".to_string()]);
        assert!(names_for_owned(st.path(), "karpie", &procs).is_empty());
    }
```

`names_for_owned(state, project, procs)` is the new `_in` form of `names_for` that maps through `home::owner_of_in(state, p.project, p.session)` (with `resolves` injected as always-true in tests via a `&dyn Fn(&str)->bool` parameter, so the fake has no project roots). Production `names_for` calls it with the real state dir and `home::resolves`.

For `publish` and IDE: one hub test that `publish(home, "term3", notice)` with a record pointing at `owner` lands in the owner hub's notices and not the home's. Write it with two `Hub::for_project` hubs (two subscribers — a single-subscriber test cannot tell send_to from broadcast; CLAUDE.md testing list).

- [ ] **Step 2: Run, expect failures.**

- [ ] **Step 3: Implement.**
  - `do_remove_worktree`: where it computes `session::live_names(&url)` for the gate, use `session::owned_live_names(&url)` (residents included). The Claude-evidence gate uses the owner-mapped evidence from `claudes`.
  - `claudes.rs`: `names_for` maps each `ClaudeProc` to `home::effective_owner(&p.project, &p.session)` before comparing to `project`; `tick`'s changed-projects comparison uses the mapped project, so a re-home triggers a broadcast for both.
  - `hub::publish(project, session, …)`: `let target = home::effective_owner(project, session);` and route to `target`'s hub.
  - `ide.rs`: wherever a message is delivered to `conn.project`'s browsers *for a specific session* (`set_session` result `Sess::In(s)`), deliver to `home::effective_owner(&conn.project, &s)` instead. `openDiff` confinement (`conn.workspace`) is unchanged — read the comment at `ide.rs:954` and leave it.
  - `registry::worktree_rows` / `known_projects_inner` / `routes::build_overview_sessions`: count and list via `owned_live_names`; for a resident row, ages come from `pids_holding_path(holders, socket_path(home, name))` (the *home* path — that is where the socket is).

- [ ] **Step 4: Run** `cargo test --lib -- --test-threads=1`, timed. All pass.

- [ ] **Step 5: Revert checks.** Each new test, against its one-line revert (gate back to `live_names`; `names_for` without the mapping; `publish` without `effective_owner`). Note each failure in its test's comment.

- [ ] **Step 6: Commit** `git commit -m "ownership: removal gate, Claude evidence, notices, IDE and overview follow re-homing (#156)"` (stage the files listed above by path).

---

### Task 6: The client — tabs that carry their home

**Files:**
- Modify: `src/proto.rs:25` `Tab::Terminal { session: String }` → add `#[serde(default, skip_serializing_if = "Option::is_none")] home: Option<String>`; `WorkspaceView` gains `session_homes: BTreeMap<String,String>`; `Event::Rehomed { session: String, owner: String }`.
- Modify: every `Tab::Terminal { session }` pattern (29 sites, `grep -n "Terminal { session" src/*.rs`) → `Tab::Terminal { session, .. }` where `home` is irrelevant; construction sites set `home: None` except where noted.
- Modify: `src/hub.rs` — when `refresh_live_sessions` finds a resident with no tab, add `Tab::Terminal { session, home: Some(home) }` to the first pane (same placement rule `NewTerminal` uses); when the home finds a session now owned elsewhere that it has a tab for, drop the tab and `send` `Event::Rehomed` to its browsers.
- Modify: `static/app.js` — terminal socket URL `:3046` uses `tab.home ?? PROJECT`; the paste endpoint (`:5365`, `:5397`) likewise; handle `Rehomed` with a transient banner `"${session} moved to ${owner basename} → "` plus a link (same shape as the `WorktreeReady` popup-blocked fallback at `:848`).
- Test: `tests/browser/worktree-rehome.mjs` (created in Task 13; this task adds a Rust test only).

**Interfaces:**
- Consumes: `owned_live_names` (Task 4), `effective_owner` (Task 5).
- Produces: wire field `Tab::Terminal.home`; `Event::Rehomed`.

- [ ] **Step 1: Failing test** in `hub.rs`:

```rust
    #[test]
    fn rehoming_moves_the_tab_and_tells_the_home() {
        // Two hubs, two connections — so a broadcast to the wrong project is
        // visible (single-subscriber tests cannot tell).
        let (home_hub, owner_hub, home, owner) = two_hubs_with_session("term3"); // write this helper
        crate::home::record(&home, "term3", &owner).unwrap();
        let (ah, ao) = (conn(&mut Hub::lock(&home_hub)), conn(&mut Hub::lock(&owner_hub)));
        Hub::lock(&home_hub).refresh_and_broadcast();
        Hub::lock(&owner_hub).refresh_and_broadcast();
        assert!(!has_terminal_tab(&Hub::lock(&home_hub), "term3"));
        assert_eq!(terminal_tab_home(&Hub::lock(&owner_hub), "term3"), Some(home.clone()));
        assert!(received(&Hub::lock(&home_hub), &ah).iter().any(|e| matches!(e, Event::Rehomed { session, .. } if session == "term3")));
        assert!(!received(&Hub::lock(&owner_hub), &ao).iter().any(|e| matches!(e, Event::Rehomed { .. })));
    }

    #[test]
    fn a_saved_layout_without_home_still_loads() {
        let t: Tab = serde_json::from_str(r#"{"k":"Terminal","session":"term"}"#).unwrap();
        assert_eq!(t, Tab::Terminal { session: "term".into(), home: None });
        assert_eq!(serde_json::to_string(&t).unwrap(), r#"{"k":"Terminal","session":"term"}"#, "byte-identical when native");
    }
```

`refresh_and_broadcast` is whatever existing method pairs `refresh_live_sessions` with a broadcast (read `hub.rs` around `:1472` and its callers; if none exists as one call, call the two in sequence in the test).

- [ ] **Step 2–4:** run (fail), implement as listed under Files, run `cargo test --lib -- --test-threads=1` (pass).

- [ ] **Step 5: Revert check.** Remove the tab-drop in the home; the first test fails on `has_terminal_tab`. Restore.

- [ ] **Step 6: Manual browser check** with `scripts/testroost.sh` (never the live instance — project memory): open the main checkout, start a terminal, write the record by hand (`printf 'proj/.claude/worktrees/worktree-x\n' > $STATE/home/<key>/term`), reload both pages; the terminal shows in the worktree page with its scrollback; typing reaches the same shell (`echo $$` matches before/after).

- [ ] **Step 7: Commit** `git commit -m "tabs carry their home; re-homed terminals follow to the owner's page (#156)"`

---

### Task 7: The hook socket server

**Files:**
- Create: `src/hooksock.rs`
- Modify: `src/lib.rs` (`pub mod hooksock;` and start it next to the IDE listeners at startup, `lib.rs:81` area, after `reconcile`)

**Interfaces:**
- Consumes: hub entry point from Task 9: `hub::ask_worktree(project: &str, req: AskRequest) -> std::sync::mpsc::Receiver<AskReply>`. In this task, define the types and a stub that returns `Refuse("not implemented")` so the server is testable alone.
- Produces:
  ```rust
  #[derive(Deserialize)] pub struct AskRequest { pub op: String, pub project: String, pub session: String, pub name: String, pub cwd: String, pub session_id: String }
  #[derive(Serialize, Debug, PartialEq)] #[serde(untagged)] pub enum AskReply { Path { path: String }, Refuse { refuse: String } }
  pub fn socket_path() -> PathBuf;                     // state_dir()/hook.sock
  pub fn start() -> Result<(), String>;                 // binds, spawns accept thread
  pub fn peer_allowed(peer_uid: u32, our_uid: u32) -> bool;
  pub fn serve_one(stream: UnixStream, ask: &dyn Fn(AskRequest) -> AskReply);
  ```

- [ ] **Step 1: Failing tests:**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn a_foreign_uid_is_refused() {
        assert!(peer_allowed(1000, 1000));
        assert!(!peer_allowed(0, 1000));
        assert!(!peer_allowed(1001, 1000));
    }

    #[test]
    fn one_request_one_reply() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || serve_one(b, &|r| AskReply::Path { path: format!("/x/{}", r.name) }));
        let mut a2 = a.try_clone().unwrap();
        writeln!(a2, r#"{{"op":"worktree_create","project":"p","session":"term","name":"fox","cwd":"/p","session_id":"s"}}"#).unwrap();
        let mut line = String::new();
        BufReader::new(a).read_line(&mut line).unwrap();
        t.join().unwrap();
        assert_eq!(line.trim(), r#"{"path":"/x/fox"}"#);
    }

    #[test]
    fn an_oversized_request_is_refused_without_asking() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let asked2 = asked.clone();
        let t = std::thread::spawn(move || serve_one(b, &move |_| { asked2.store(true, std::sync::atomic::Ordering::SeqCst); AskReply::Path { path: "/x".into() } }));
        let mut a2 = a.try_clone().unwrap();
        let _ = a2.write_all(&vec![b'x'; 70 * 1024]);
        let _ = a2.write_all(b"\n");
        let mut line = String::new();
        let _ = BufReader::new(a).read_line(&mut line);
        t.join().unwrap();
        assert!(!asked.load(std::sync::atomic::Ordering::SeqCst));
        assert!(line.contains("refuse") && line.contains("too large"), "{line}");
    }

    #[test]
    fn garbage_is_refused_with_a_reason() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || serve_one(b, &|_| AskReply::Path { path: "/x".into() }));
        let mut a2 = a.try_clone().unwrap();
        writeln!(a2, "not json").unwrap();
        let mut line = String::new();
        BufReader::new(a).read_line(&mut line).unwrap();
        t.join().unwrap();
        assert!(line.contains("refuse") && line.contains("request"), "{line}");
    }
}
```

- [ ] **Step 2:** run → compile failure.

- [ ] **Step 3: Implement:**

```rust
//! The channel `roost worktree-hook` asks the server over.
//!
//! `roost claude-hook` never talks to the server — it writes to its tty and
//! to files. Approving a worktree needs a round trip (hook → hub → a browser
//! → back), so this is the one place a Claude-spawned process reaches roost.
//!
//! A Unix socket, not HTTP: CLAUDE.md fixes the HTTP state-change surface at
//! two Origin-checked POSTs, and an endpoint that creates directories and
//! moves terminals is exactly what a drive-by page would want. No browser can
//! open a Unix socket. The trust boundary is the same as `state/` itself:
//! file mode 0600, and the peer's uid from `SO_PEERCRED` must be ours.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

const MAX_REQUEST: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
pub struct AskRequest { pub op: String, pub project: String, pub session: String, pub name: String, pub cwd: String, pub session_id: String }

#[derive(Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum AskReply { Path { path: String }, Refuse { refuse: String } }

pub fn socket_path() -> PathBuf { crate::wsstate::state_dir().join("hook.sock") }

pub fn peer_allowed(peer_uid: u32, our_uid: u32) -> bool { peer_uid == our_uid }

fn peer_uid(s: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe { libc::getsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, &mut cred as *mut _ as *mut libc::c_void, &mut len) };
    (rc == 0).then_some(cred.uid)
}

fn reply(mut s: &UnixStream, r: &AskReply) {
    if let Ok(mut line) = serde_json::to_string(r) { line.push('\n'); let _ = s.write_all(line.as_bytes()); }
}

pub fn serve_one(stream: UnixStream, ask: &dyn Fn(AskRequest) -> AskReply) {
    let mut line = String::new();
    let n = BufReader::new((&stream).take(MAX_REQUEST)).read_line(&mut line).unwrap_or(0);
    if n as u64 >= MAX_REQUEST && !line.ends_with('\n') {
        return reply(&stream, &AskReply::Refuse { refuse: "request too large".into() });
    }
    let req: AskRequest = match serde_json::from_str(line.trim_end()) {
        Ok(r) => r,
        Err(e) => return reply(&stream, &AskReply::Refuse { refuse: format!("unreadable request: {e}") }),
    };
    if req.op != "worktree_create" {
        return reply(&stream, &AskReply::Refuse { refuse: format!("unknown op {:?}", req.op) });
    }
    reply(&stream, &ask(req));
}

pub fn start() -> Result<(), String> {
    let p = socket_path();
    match std::fs::symlink_metadata(&p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot stat {}: {e}", p.display())),
        Ok(_) => match UnixStream::connect(&p) {
            // Someone answers: another roost holds it. Never unlink that.
            Ok(_) => return Err(format!("{} is in use by another roost", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => { let _ = std::fs::remove_file(&p); }
            Err(e) => return Err(format!("cannot tell whether {} is in use: {e}", p.display())),
        },
    }
    let l = UnixListener::bind(&p).map_err(|e| format!("bind {}: {e}", p.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    let me = unsafe { libc::getuid() };
    std::thread::Builder::new().name("hooksock".into()).spawn(move || {
        for s in l.incoming().flatten() {
            if !peer_uid(&s).is_some_and(|u| peer_allowed(u, me)) { continue; }
            let _ = std::thread::Builder::new().name("hooksock-conn".into()).spawn(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    serve_one(s, &|req| crate::hub::ask_worktree_blocking(req))
                }));
            });
        }
    }).map_err(|e| e.to_string())?;
    Ok(())
}
```

Add a stub in `hub.rs` for now:

```rust
/// Filled in by Task 9. Blocks the calling (socket) thread, never a hub lock.
pub fn ask_worktree_blocking(_req: crate::hooksock::AskRequest) -> crate::hooksock::AskReply {
    crate::hooksock::AskReply::Refuse { refuse: "not implemented".into() }
}
```

Check `Cargo.toml` for `libc`; if absent, add it (it is almost certainly a transitive dependency already — `cargo tree -i libc`), and say so in the commit message. A bind window between `bind` and `set_permissions` exists; close it by setting the process umask to `0o177` around `bind` (`libc::umask`) and restoring it, and comment why.

In `lib.rs` startup: `if let Err(e) = crate::hooksock::start() { eprintln!("roost: worktree hook socket not started: {e}"); }` — roost runs without it; the hook then falls back (Task 8).

- [ ] **Step 4:** run `cargo test --lib hooksock -- --test-threads=1` → 4 pass.

- [ ] **Step 5: Revert check.** Remove the size check; `an_oversized_request_is_refused_without_asking` must fail (it will reach `ask` or fail to parse with a different message — read which, note it). Restore.

- [ ] **Step 6: Commit** `git commit -m "hooksock: a 0600 Unix socket for the worktree hook, peer uid checked (#156)"`

---

### Task 8: `roost worktree-hook`

**Files:**
- Modify: `src/main.rs:7` — `Some("worktree-hook") => std::process::exit(roost::cli::run_worktree_hook()),`
- Modify: `src/cli.rs` — `run_worktree_hook` and a testable core `worktree_hook_core`.

**Interfaces:**
- Consumes: `hooksock::{socket_path, AskRequest, AskReply}`, `worktree::create_named`, `wtname`.
- Produces:
  ```rust
  pub enum HookOutcome { Print(String), Refuse(String) }
  pub fn worktree_hook_core(payload: &serde_json::Value, env_project: Option<String>, env_session: Option<String>,
                            connect: &dyn Fn() -> std::io::Result<std::os::unix::net::UnixStream>,
                            fallback_create: &dyn Fn(&std::path::Path, &str) -> Result<String, String>) -> HookOutcome;
  pub fn run_worktree_hook() -> i32;   // 0 + path on stdout, or 1 + reason on stderr
  ```

- [ ] **Step 1: Failing tests** (in `cli.rs` tests):

```rust
    fn payload(name: &str) -> serde_json::Value {
        serde_json::json!({"hook_event_name":"WorktreeCreate","session_id":"s1","cwd":"/repo","name":name})
    }
    fn no_socket() -> std::io::Result<std::os::unix::net::UnixStream> { Err(std::io::ErrorKind::NotFound.into()) }

    #[test]
    fn outside_roost_creates_without_asking() {
        let out = worktree_hook_core(&payload("fox"), None, None, &|| panic!("must not connect"), &|_, n| Ok(format!("/repo/.claude/worktrees/worktree-{n}")));
        assert!(matches!(out, HookOutcome::Print(p) if p.ends_with("worktree-fox")));
    }

    #[test]
    fn roost_unreachable_falls_back_to_creating() {
        let out = worktree_hook_core(&payload("fox"), Some("p".into()), Some("term".into()), &no_socket, &|_, n| Ok(format!("/w/{n}")));
        assert!(matches!(out, HookOutcome::Print(_)));
    }

    #[test]
    fn a_refuse_reply_is_refused_with_its_reason() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || crate::hooksock::serve_one(b, &|_| crate::hooksock::AskReply::Refuse { refuse: "nobody approved".into() }));
        let a = std::sync::Mutex::new(Some(a));
        let out = worktree_hook_core(&payload("fox"), Some("p".into()), Some("term".into()), &|| Ok(a.lock().unwrap().take().unwrap()), &|_, _| panic!("must not fall back"));
        server.join().unwrap();
        assert!(matches!(out, HookOutcome::Refuse(r) if r.contains("nobody approved")));
    }

    #[test]
    fn hook_exits_1_when_server_drops_connection() {
        // roost restarted mid-ask: the connection closes with no reply.
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(b);
        let a = std::sync::Mutex::new(Some(a));
        let out = worktree_hook_core(&payload("fox"), Some("p".into()), Some("term".into()), &|| Ok(a.lock().unwrap().take().unwrap()), &|_, _| panic!("must not fall back"));
        assert!(matches!(out, HookOutcome::Refuse(r) if r.contains("no answer")));
    }

    #[test]
    fn an_invalid_name_is_refused_before_anything() {
        let out = worktree_hook_core(&payload("--evil"), None, None, &|| panic!(), &|_, _| panic!());
        assert!(matches!(out, HookOutcome::Refuse(r) if r.contains("must match")));
    }
```

- [ ] **Step 2:** run → compile failure.

- [ ] **Step 3: Implement:**

```rust
pub enum HookOutcome { Print(String), Refuse(String) }

/// `roost worktree-hook`'s decision, separated from its I/O so every row of
/// the spec's table (§2) is a test. Falling back to creating when roost
/// cannot be reached is deliberate and is the one permissive default in this
/// codebase: creating a worktree destroys nothing, and refusing would break
/// `claude --worktree` outside roost and across every roost restart.
pub fn worktree_hook_core(
    payload: &serde_json::Value,
    env_project: Option<String>,
    env_session: Option<String>,
    connect: &dyn Fn() -> std::io::Result<std::os::unix::net::UnixStream>,
    fallback_create: &dyn Fn(&std::path::Path, &str) -> Result<String, String>,
) -> HookOutcome {
    use std::io::{BufRead, BufReader, Write};
    let s = |k: &str| payload.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let (name, cwd, sid) = (s("name"), s("cwd"), s("session_id"));
    if let Err(e) = crate::wtname::check(&name) { return HookOutcome::Refuse(e); }
    let fallback = || match fallback_create(std::path::Path::new(&cwd), &name) {
        Ok(p) => HookOutcome::Print(p),
        Err(e) => HookOutcome::Refuse(e),
    };
    let (Some(project), Some(session)) = (env_project, env_session) else { return fallback() };
    let stream = match connect() {
        Ok(s) => s,
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused) => return fallback(),
        Err(e) => return HookOutcome::Refuse(format!("roost: cannot reach roost to approve a worktree: {e}")),
    };
    let req = serde_json::json!({"op":"worktree_create","project":project,"session":session,"name":name,"cwd":cwd,"session_id":sid});
    let mut w = &stream;
    if writeln!(w, "{req}").is_err() { return HookOutcome::Refuse("roost: could not send the request".into()); }
    let mut line = String::new();
    match BufReader::new(&stream).read_line(&mut line) {
        Ok(0) | Err(_) => return HookOutcome::Refuse("roost: no answer from roost (it may have restarted); not creating".into()),
        Ok(_) => {}
    }
    let v: serde_json::Value = match serde_json::from_str(line.trim_end()) {
        Ok(v) => v,
        Err(_) => return HookOutcome::Refuse("roost: unreadable answer from roost; not creating".into()),
    };
    if let Some(p) = v.get("path").and_then(|p| p.as_str()) { return HookOutcome::Print(p.to_string()); }
    let why = v.get("refuse").and_then(|r| r.as_str()).unwrap_or("refused");
    HookOutcome::Refuse(format!("roost: {why}"))
}

pub fn run_worktree_hook() -> i32 {
    use std::io::Read;
    let mut input = String::new();
    if std::io::stdin().take(1 << 20).read_to_string(&mut input).is_err() {
        eprintln!("roost worktree-hook: could not read the hook payload"); return 1;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else {
        eprintln!("roost worktree-hook: payload is not JSON"); return 1;
    };
    let outcome = worktree_hook_core(
        &v,
        std::env::var("ROOST_PROJECT").ok(),
        std::env::var("ROOST_SESSION").ok(),
        &|| std::os::unix::net::UnixStream::connect(crate::hooksock::socket_path()),
        &|repo, name| {
            let key_of = |d: &str| crate::projects::storage_key(&format!("{}/.claude/worktrees/{d}", repo.display()));
            crate::worktree::create_named(repo, &crate::wsstate::state_dir(), name, &key_of, &crate::worktree::real_git)
                .map(|c| c.path.to_string_lossy().into_owned())
        },
    );
    match outcome {
        HookOutcome::Print(p) => { println!("{p}"); 0 }
        HookOutcome::Refuse(r) => { eprintln!("{r}"); 1 }
    }
}
```

Note the fallback's `key_of` uses an absolute path, unlike the hub's project-relative URL key: the hook does not know the roost project URL outside roost. Task 9's approve path uses the hub's key. Add a comment saying a fallback-created worktree's `.base` is keyed differently and its state shows "base unknown" in the switcher until #157 — or, better, when `ROOST_PROJECT` *is* set (the "roost unreachable" row), key it as the hub would (`format!("{project}/.claude/worktrees/{d}")`). Implement the better one: pass `env_project` into the fallback closure.

- [ ] **Step 4:** run `cargo test --lib cli -- --test-threads=1` → pass.

- [ ] **Step 5: Revert check.** Map `Ok(0)` to `fallback()`; `hook_exits_1_when_server_drops_connection` must fail. Restore.

- [ ] **Step 6: End-to-end check against real Claude Code** in the Task 0 scratch repo: point its `WorktreeCreate` hook at `target/debug/roost worktree-hook`, run `claude --model haiku --worktree e2e -p "Reply OK" < /dev/null` with no `ROOST_*` env; expect `git worktree list` to show `worktree-e2e` on branch `worktree-e2e`.

- [ ] **Step 7: Commit** `git commit -m "cli: roost worktree-hook, with the spec's fallback table (#156)"`

---

### Task 9: The ask — hub parks a request, browsers answer

**Files:**
- Modify: `src/hub.rs` — replace the Task 7 stub; `Hub` gains `asks: Vec<PendingAsk>`; handlers for `Intent::WorktreeAnswer`; expiry.
- Modify: `src/proto.rs` — `Event::WorktreeAsk { id: String, session: String, name: String }`, `Event::WorktreeAskClosed { id: String }`, `Intent::WorktreeAnswer { id: String, approve: bool }`.

**Interfaces:**
- Consumes: `hooksock::{AskRequest, AskReply}`, `worktree::create_named`, `home::record`, `claudehooks` (Task 10's `set_worktree` — call it if present; this task can land before Task 10 by leaving that call to Task 10).
- Produces: `pub fn ask_worktree_blocking(req: AskRequest) -> AskReply` (real); internal `PendingAsk { id, session, name, reply: mpsc::Sender<AskReply>, from_session_id: String }`.

Flow (all locks released before blocking):
1. `ask_worktree_blocking` validates `req.session` (`session::valid_name`) and that `req.project` resolves; finds the hub with `hub::open_hub(&req.project)` (existing; do not *create* a hub — a project nobody has open has no browser, refuse "no roost tab is open on this project to approve it").
2. Under the hub lock: if no connections → refuse at once; if an ask for this session is pending → refuse "an earlier request from this terminal is still waiting". Else push a `PendingAsk`, broadcast `WorktreeAsk`, release the lock.
3. Wait on the receiver with `recv_timeout(60s)`. On timeout: lock, remove the ask if still present, broadcast `WorktreeAskClosed`, reply refuse "nobody approved it in roost within 60 s".
4. `Intent::WorktreeAnswer { id, approve }` under the lock: remove the ask (if absent → no-op: someone else answered); broadcast `WorktreeAskClosed`. If `approve == false` send `Refuse("refused in roost")`. If approve: spawn the create thread (the `do_new_worktree` shape: work off-lock, `catch_unwind`), which runs `create_named(self.dir, state_dir, name, key_of, real_git)`, then `home::record(project, session, &owner_url)`, then (Task 10) installs the worktree hook into the new worktree, then sends `Path { path }`, then `broadcast_all(ProjectsChanged { project: owner_url })`, and `send_to(from, WorktreeReady { url: owner_url, launch: None })` so the approving browser's `pendingTab` navigates. Any failure → `Refuse(msg)` and no record written (record is written only after create succeeded; if `record` itself fails, reply refuse — Claude must not move into a worktree whose terminal roost will show in the wrong place).

- [ ] **Step 1: Failing tests:**

```rust
    #[test]
    fn an_ask_with_no_browser_is_refused_at_once() {
        let (hub, project) = registered_hub_with_session("term3"); // Hub::for_project, no conns
        let r = ask_worktree_blocking(req(&project, "term3", "fox"));
        assert_eq!(r, AskReply::Refuse { refuse: "no roost tab is open on this project to approve it".into() });
        drop(hub);
    }

    #[test]
    fn approve_creates_records_and_replies_path() {
        let (hub, project, repo) = registered_hub_in_git_repo_with_session("term3");
        let a = conn(&mut Hub::lock(&hub));
        let t = std::thread::spawn({ let p = project.clone(); move || ask_worktree_blocking(req(&p, "term3", "fox")) });
        let id = wait_for_ask(&hub, &a);                        // polls received() for WorktreeAsk, ≤5 s
        Hub::lock(&hub).handle(&a, Intent::WorktreeAnswer { id, approve: true });
        let r = t.join().unwrap();
        let owner = format!("{project}/.claude/worktrees/worktree-fox");
        assert_eq!(r, AskReply::Path { path: repo.join(".claude/worktrees/worktree-fox").to_string_lossy().into() });
        assert!(matches!(crate::home::owner_of(&project, "term3"), crate::home::Owner::Elsewhere(o) if o == owner));
    }

    #[test]
    fn second_answer_to_an_ask_is_ignored() {
        let (hub, project, repo) = registered_hub_in_git_repo_with_session("term3");
        let (a, b) = { let mut h = Hub::lock(&hub); (conn(&mut h), conn(&mut h)) };
        let t = std::thread::spawn({ let p = project.clone(); move || ask_worktree_blocking(req(&p, "term3", "fox")) });
        let id = wait_for_ask(&hub, &a);
        Hub::lock(&hub).handle(&a, Intent::WorktreeAnswer { id: id.clone(), approve: true });
        Hub::lock(&hub).handle(&b, Intent::WorktreeAnswer { id, approve: true });
        t.join().unwrap();
        assert_eq!(crate::worktree::list(&repo).len(), 2, "created exactly once");
    }

    #[test]
    fn refuse_writes_nothing() {
        let (hub, project, repo) = registered_hub_in_git_repo_with_session("term3");
        let a = conn(&mut Hub::lock(&hub));
        let t = std::thread::spawn({ let p = project.clone(); move || ask_worktree_blocking(req(&p, "term3", "fox")) });
        let id = wait_for_ask(&hub, &a);
        Hub::lock(&hub).handle(&a, Intent::WorktreeAnswer { id, approve: false });
        assert!(matches!(t.join().unwrap(), AskReply::Refuse { .. }));
        assert_eq!(crate::worktree::list(&repo).len(), 1);
        assert!(matches!(crate::home::owner_of(&project, "term3"), crate::home::Owner::Here));
    }
```

The 60 s timeout: make it a `const ASK_TIMEOUT: Duration` and a `#[cfg(test)]` override via a function parameter (`ask_worktree_blocking_with(req, timeout)`), and test it at 200 ms: `an_unanswered_ask_is_refused_and_closed` asserting the refuse text and that `WorktreeAskClosed` was broadcast.

Write the helpers (`registered_hub_with_session`, `registered_hub_in_git_repo_with_session`, `req`, `wait_for_ask`) in `hub.rs`'s test module next to `repo_with_worktree`, reusing it.

- [ ] **Step 2–4:** fail, implement per the flow, pass (`cargo test --lib hub -- --test-threads=1`, timed).

- [ ] **Step 5: Revert checks.** Remove the "already removed → no-op" branch: `second_answer_to_an_ask_is_ignored` fails. Move `home::record` before `create_named`: `refuse_writes_nothing` still passes but a new assertion in `approve_creates…` can't see it — instead, make `create_named` fail (name already taken) and assert no record: add `a_failed_create_writes_no_record` and watch it fail under the moved order. Restore.

- [ ] **Step 6: Commit** `git commit -m "hub: park a worktree ask, first browser answer wins, 60 s refuse (#156)"`

---

### Task 10: Installing the worktree hook

**Files:**
- Modify: `src/claudehooks.rs` — generalise to two kinds.
- Modify: `src/proto.rs` — `Intent::SetWorktreeHooks { on: bool }`; `WorkspaceView.worktree_hooks: Option<bool>`.
- Modify: `src/hub.rs` — handle the intent (mirror `SetClaudeHooks` at `:633`, including its cache invalidation), and in Task 9's approve path install into the new worktree when the main checkout has it.
- Modify: `static/app.js` — a toggle in the same settings place as the bell's `SetClaudeHooks` (`:4787-4825`), label "Let roost create Claude's worktrees".

**Interfaces:**
- Produces: `claudehooks::Kind::{Notify, Worktree}`; `claudehooks::state_of(dir, Kind) -> HookState`; `claudehooks::set_kind(dir, Kind, on) -> Result<(), String>`; existing `state`/`set` become `state_of(dir, Kind::Notify)` / `set_kind(dir, Kind::Notify, on)`. `pub const WORKTREE_COMMAND: &str = "roost worktree-hook";`

Kind table:

| Kind | command | events | entry |
|---|---|---|---|
| Notify | `roost claude-hook` | SessionStart, Notification, Stop | `{"type":"command","command":…,"timeout":5}` |
| Worktree | `roost worktree-hook` | WorktreeCreate | `{"type":"command","command":…,"timeout":120}` |

`load` validates that every event of **both** kinds is an array when present. `is_ours` takes the kind's command. A `Worktree` entry found under any event other than `WorktreeCreate` is left alone on enable and removed on disable (never installed elsewhere).

- [ ] **Step 1: Failing tests:**

```rust
    #[test]
    fn worktree_kind_installs_only_on_worktree_create_with_a_long_timeout() {
        let d = tempfile::tempdir().unwrap();
        set_kind(d.path(), Kind::Worktree, true).unwrap();
        let v = read_json(d.path());
        let hooks = v["hooks"].as_object().unwrap();
        assert_eq!(hooks.keys().collect::<Vec<_>>(), vec!["WorktreeCreate"]);
        assert_eq!(v["hooks"]["WorktreeCreate"][0]["hooks"][0]["command"], "roost worktree-hook");
        assert_eq!(v["hooks"]["WorktreeCreate"][0]["hooks"][0]["timeout"], 120);
    }

    #[test]
    fn kinds_are_independent() {
        let d = tempfile::tempdir().unwrap();
        set_kind(d.path(), Kind::Notify, true).unwrap();
        set_kind(d.path(), Kind::Worktree, true).unwrap();
        set_kind(d.path(), Kind::Notify, false).unwrap();
        assert_eq!(state_of(d.path(), Kind::Worktree), HookState::Present);
        assert_eq!(state_of(d.path(), Kind::Notify), HookState::Absent);
    }

    #[test]
    fn a_non_array_worktree_create_is_unknown_and_refuses_to_write() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), r#"{"hooks": {"WorktreeCreate": 5}}"#);
        assert!(matches!(state_of(d.path(), Kind::Worktree), HookState::Unknown(_)));
        assert!(set_kind(d.path(), Kind::Worktree, true).is_err());
        assert_eq!(read(d.path()), r#"{"hooks": {"WorktreeCreate": 5}}"#);
    }
```

(`read_json`, `read`, `write` exist in the module's tests already — reuse; check names.)

- [ ] **Step 2–4:** fail, implement, pass; also run the whole existing `claudehooks` test module unchanged — every existing test must still pass, since `state`/`set` keep their behaviour.

- [ ] **Step 5: Revert check.** Use `EVENTS` (notify's) for the `load` validation only; `a_non_array_worktree_create_is_unknown…` fails. Restore.

- [ ] **Step 6: Commit** `git commit -m "claudehooks: a second owned command, roost worktree-hook, on WorktreeCreate only (#156)"`

---

### Task 11: The client — ask dialog, `+` dialog, ✻ shortcut

**Files:**
- Modify: `static/app.js`: handle `WorktreeAsk` / `WorktreeAskClosed`; `+` control in the worktree switcher (rendered server-side in `render.rs` `worktrees_strip` — add the button there, escaped, main checkout only); `showClaudeHere`'s "worktree" choice opens the `+` dialog with Fork preselected.
- Modify: `src/proto.rs` — `Intent::NewWorktree { name: Option<String>, launch: Option<WorktreeLaunch> }` with `enum WorktreeLaunch { Fork { session: String }, Claude, Shell }`; `Event::WorktreeReady { url, launch: Option<Launch>, typed: Option<String> }`.
- Modify: `src/hub.rs` `do_new_worktree` → `create_named` with `name` (generated via `wtname::generate` with `taken` = branch-or-dir exists, when `None`); for `Fork`, look up `claudesess::recorded(project, session)` and refuse if absent; the reply's `typed` is the exact keystrokes.
- Modify: `src/launch.rs` — `ClaudeSession::Fork(id)` variant; `keystrokes(Launch::Claude, Some(&Fork(id)))` → `claude --resume {id} --fork-session\r`, gated by `claudesess::valid_session_id` like `Resume`.
- Modify: `src/render.rs` — the `+` button.

**Interfaces:**
- Consumes: `create_named`, `wtname::generate`, `claudesess::recorded`.
- Produces: wire shapes above; `launch::ClaudeSession::Fork`.

- [ ] **Step 1: Failing Rust tests:**

```rust
    // launch.rs
    #[test]
    fn fork_types_resume_with_fork_session() {
        let id = "864ee734-e3ab-434e-8278-745a850b16ad";
        assert_eq!(keystrokes(Launch::Claude, Some(&ClaudeSession::Fork(id.into()))), format!("claude --resume {id} --fork-session\r").into_bytes());
    }
    #[test]
    fn a_fork_id_that_could_be_an_argument_degrades_to_bare_claude() {
        assert_eq!(keystrokes(Launch::Claude, Some(&ClaudeSession::Fork("--x".into()))), b"claude\r".to_vec());
    }

    // hub.rs
    #[test]
    fn fork_without_a_recorded_session_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut h, _home, _) = repo_with_worktree(tmp.path());
        let a = conn(&mut h);
        h.handle(&a, Intent::NewWorktree { name: Some("fox".into()), launch: Some(WorktreeLaunch::Fork { session: "term9".into() }) });
        let msg = last_error(&h, &a);
        assert!(msg.contains("term9") && msg.contains("no recorded Claude session"), "{msg}");
    }
```

- [ ] **Step 2–4:** fail, implement, pass. In `app.js`, the `+` dialog uses `askChoice` (the in-page dialog) with a name input prefilled from a server-provided suggestion: add `suggested_worktree_name: Option<String>` to the view only when the switcher is shown, generated per state build (cheap) — or simpler: the client sends `name: null` and the server generates; the dialog shows "name: generated" with an optional text field. Use the simpler form.
   Opening: the `+` click and the ask-approve click each do `pendingTab = window.open("about:blank")` *inside the click handler* (user activation), exactly as `showClaudeHere` does; `WorktreeReady` navigates it. For `typed`, the URL carries `?launch=` today; extend with `?type=fork&session=<id>` validated server-side again at terminal start — **do not** let the URL carry raw keystrokes. Implement: `WorktreeReady.launch = Some(Launch::Claude)` plus `fork: Option<String>` (the session id), the new page's first terminal is started with `NewTerminal { launch: claude, fork: Some(id) }`, and the server re-validates the id with `claudesess::valid_session_id` before typing.

- [ ] **Step 5: Revert check** for the two launch tests (drop the validity gate → second test fails). Restore.

- [ ] **Step 6: Commit** `git commit -m "worktrees: + dialog with fork/fresh/shell; ask dialog; ✻ opens it (#156)"`

---

### Task 12: Moves roost was not asked about

**Files:**
- Modify: `src/claudes.rs` — `ClaudeProc` gains `cwd: Option<PathBuf>` (from `readlink /proc/<pid>/cwd`; `None` = unreadable = unknown); `tick` computes, per home project, `Moved { session, worktree_url }` for Claudes whose cwd is inside a worktree of the home (via `worktree::list(home_dir)`, canonicalised, never string prefix) and whose `home::owner_of` is not that worktree; and `Back { session }` for re-homed Claudes whose cwd is the home's main checkout.
- Modify: `src/proto.rs` — `Event::ClaudeElsewhere { session, worktree: Option<String> }` (`None` = back in the main checkout); `Intent::Rehome { session: String, to: Option<String> }` (`None` = move back).
- Modify: `src/hub.rs` — handle `Rehome`: validate `to` is a worktree of this project via `worktree::list`, then `home::record` / `home::forget`, then refresh + broadcast both projects.
- Modify: `static/app.js` — dismissible banner per spec §7 with [Move tab there] / [Move back]; when `worktree_hooks` is off, the banner also offers "Let roost create Claude's worktrees" once per project per browser (localStorage, try/catch).

`ClaudeProc` ordering/dedup: `cwd` must be excluded from `changed_projects`' comparison (it projects the port out already — read its doc comment and do the same for cwd, or every `cd` rebroadcasts every project).

**Also (spec §7, added after Task 0):** a re-homed terminal whose worktree Claude Code removed on exit. In `hub.rs`, when `refresh_live_sessions` finds a resident record whose owner no longer resolves, check `worktree::list(home_dir)`: if the list is non-empty (git answered) and does not contain the owner, send the home's browsers `Event::WorktreeGone { worktree: String, session: String }` once per (owner, session) per hub lifetime (a `HashSet` on `Hub`); an empty list (git did not answer) sends nothing. Client: transient banner "`worktree-x` was removed (Claude Code removes a clean worktree when its session exits); `term3` is back here". Test (two hubs, real git worktree removed with `git worktree remove` behind roost's back):

```rust
    #[test]
    fn a_removed_owner_is_announced_once_in_the_home() {
        let (home_hub, home, repo, wt_url, wt_dir) = home_hub_with_resident_in_real_worktree("term3");
        let a = conn(&mut Hub::lock(&home_hub));
        assert!(std::process::Command::new("git").args(["worktree", "remove", wt_dir.to_str().unwrap()]).current_dir(&repo).status().unwrap().success());
        Hub::lock(&home_hub).refresh_live_sessions();
        Hub::lock(&home_hub).refresh_live_sessions();
        let gone: Vec<_> = received(&Hub::lock(&home_hub), &a).into_iter().filter(|e| matches!(e, Event::WorktreeGone { .. })).collect();
        assert_eq!(gone.len(), 1, "announced exactly once");
        assert!(matches!(crate::home::owner_of(&home, "term3"), crate::home::Owner::Elsewhere(o) if o == wt_url), "record kept");
    }
```

Revert check: drop the once-per-hub `HashSet`; the test fails with 2.

- [ ] **Step 1: Failing tests** (fake `/proc` with a `cwd` symlink per pid — `std::os::unix::fs::symlink(target, pid_dir.join("cwd"))`):

```rust
    #[test]
    fn a_claude_in_a_worktree_of_its_home_is_reported_moved() {
        let (root, repo, wt) = fake_proc_with_repo_and_worktree();   // real git worktree on disk
        mk_with_cwd(&root, 100, "ROOST_PROJECT=karpie\nROOST_SESSION=term3\n", &wt);
        let moves = detect_moves(&root, &|p| (p == "karpie").then(|| repo.clone()), &|_, _| crate::home::Owner::Here);
        assert_eq!(moves, vec![Move::Into { home: "karpie".into(), session: "term3".into(), worktree: "karpie/.claude/worktrees/feat".into() }]);
    }

    #[test]
    fn an_unreadable_cwd_reports_nothing() {
        let (root, repo, _wt) = fake_proc_with_repo_and_worktree();
        mk(100, "claude", "ROOST_PROJECT=karpie\nROOST_SESSION=term3\n"); // no cwd link
        assert!(detect_moves(&root, &|_| Some(repo.clone()), &|_, _| crate::home::Owner::Here).is_empty());
    }

    #[test]
    fn a_sibling_directory_with_a_matching_prefix_is_not_a_worktree() {
        // /repo/.claude/worktrees/feat-old (a plain directory, not a worktree)
        let (root, repo, _wt) = fake_proc_with_repo_and_worktree();
        let impostor = repo.join(".claude/worktrees/feat-old");
        std::fs::create_dir_all(&impostor).unwrap();
        mk_with_cwd(&root, 100, "ROOST_PROJECT=karpie\nROOST_SESSION=term3\n", &impostor);
        assert!(detect_moves(&root, &|_| Some(repo.clone()), &|_, _| crate::home::Owner::Here).is_empty());
    }
```

`detect_moves(proc_root, home_dir_of, owner_of) -> Vec<Move>` with `enum Move { Into { home, session, worktree }, Back { home, session } }` — injectable so tests need no roots or state dir.

- [ ] **Step 2–4:** fail, implement, pass.

- [ ] **Step 5: Revert check.** Replace the `worktree::list` match with `cwd.starts_with(repo.join(".claude/worktrees"))`; the impostor test fails. Restore.

- [ ] **Step 6: Commit** `git commit -m "claudes: notice a Claude working in another worktree; offer, never move (#156)"`

---

### Task 13: Browser test

**Files:**
- Create: `tests/browser/worktree-rehome.mjs` (copy the harness pattern from `worktree-launch.mjs` and read `tests/browser/README.md`'s four traps first)
- Create: a fake `claude` that is a **real binary** (project memory: a script is not seen as `claude` by `comm`). Build it from a tiny Rust or C file in the test's temp dir, named `claude`, whose behaviour is: read lines from stdin; on `worktree <name>`, exec `roost worktree-hook` with the Claude Code payload on stdin (`{"hook_event_name":"WorktreeCreate","session_id":"t1","cwd":<cwd>,"name":<name>}`), print the hook's stdout and `chdir` into it; on `pwd`, print cwd. This mimics the observed behaviour (process chdir) without the network.

Scenarios (each with a real mouse click for the user activation; assertion on a marker, never on absence alone):

- [ ] **A. Approve:** start a terminal, run the fake `claude`, type `worktree fox`. The dialog appears in the page; click "Create and open in new tab". Assert: a new page opens at `/<project>/.claude/worktrees/worktree-fox`; its terminal tab shows the scrollback marker typed before the move; typing `pwd` there prints `…/worktree-fox`; `echo $PPID` (or the fake's pid) is identical before and after; the main page's terminal tab is gone and the `moved to` notice is visible.
- [ ] **B. Close Project on main** after A: confirm; the worktree page's terminal still answers `pwd`.
- [ ] **C. No click:** `worktree slow`; wait 61 s; the fake prints the refuse reason containing "within 60 s"; no `worktree-slow` in `git worktree list`.
- [ ] **D. No browser:** close all pages, drive the fake through a direct `dtach -a` (or the test's PTY helper), `worktree lonely`; refuse reason "no roost tab is open"; returns in < 2 s.
- [ ] **E. `+` → Fork:** with a recorded session id (write `state/claude/<key>/term.json` as `claudesess::record` would), `+` → Fork → Create; the new page's terminal shows `claude --resume <id> --fork-session` typed.

- [ ] **Run:** `deno run -A tests/browser/worktree-rehome.mjs` against `scripts/testroost.sh` (never the live instance). Run it 3 times; per project memory, a back-to-back flake under contention is not a regression signal — but a scenario that fails 3/3 is.
- [ ] **Revert check:** with Task 4's `kill_project` skip reverted, scenario B must fail. Restore.
- [ ] **Commit** `git commit -m "browser: worktree-rehome.mjs drives ask, approve, re-home, close and fork (#156)"`

---

### Task 14: CLAUDE.md, docs, and the Linux host

**Files:**
- Modify: `CLAUDE.md` — the settings-file bullet (two owned commands; `worktree-hook` only on `WorktreeCreate`, non-zero only on refuse/unreadable); a new hard-constraint bullet: *a session's socket path, its `SESSIONS` key and its environment name its home forever; re-homing changes ownership through `state/home/` and nothing else — moving the socket or re-keying the map is the vulnerability, not the cleanup*; the caps bullet gains "the 16-per-project cap counts a session's home, not its owner"; mention `state/hook.sock` next to the HTTP-surface bullet ("not HTTP, so not part of the CSRF surface; 0600 + peer uid").
- Modify: `README.md` / `docs/` — wherever worktrees are described (grep `worktree` in `README.md` and `docs/*.md`, not specs/plans).

- [ ] **Step 1:** edit; `git diff CLAUDE.md` reads as a rule, not a changelog.
- [ ] **Step 2: Full suite** on this host: `cargo test -- --test-threads=1`, timed; then on the Linux deploy host (CLAUDE.md: "run the suite on the Linux host too") from a checkout of this branch.
- [ ] **Step 3:** `cargo build` here, then confirm the shared target dir's `assets_table.rs` names this checkout (CLAUDE.md "Build from one checkout" grep).
- [ ] **Step 4: Commit** `git commit -m "CLAUDE.md: re-homing and the worktree hook are constraints (#156)"`
- [ ] **Step 5:** push the branch and open **one** PR into `develop` for #156 containing spec, plan and code (project memory: never a spec-only PR), with Task 0's interactive-probe results in the description.
