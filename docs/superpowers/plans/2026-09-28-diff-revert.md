# Reverting a change from Changes and Diff: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the file-tree menu in the Changes pane and Diff tabs with a git menu whose one command, Revert, discards a change after a confirmation, saving it to `git stash` first.

**Architecture:** A new `src/revert.rs` builds a *plan* (what would be reverted, and a token over it) from `git status -z` and `git diff HEAD`, and executes it with one `git --literal-pathspecs stash push -- <paths>`. Two websocket intents, `RevertPreview` and `Revert`, are diverted in `wsconn` off the hub lock (the `RestoreWorkspace` shape) and answer only the requester. The client splits its context menu by tab kind and confirms through the existing `askChoice`.

**Tech Stack:** Rust (std, serde, tungstenite as already used), plain JS in `static/app.js` and `static/dialog.js`, deno + Chromium browser tests.

**Spec:** `docs/superpowers/specs/2026-09-28-diff-revert-design.md`, issue #125. Read it before any task.

## Global Constraints

- A revert never deletes a file: only paths with a version in `HEAD`. `??`, index-side `A`, and renames/copies (`R`, `C`) are skipped, each counted.
- Only paths inside the project: a porcelain path with any `..` component is skipped as `outside`.
- Every git call that takes paths runs with `--literal-pathspecs`.
- The revert is exactly one `git stash push -m "roost revert: N file(s)" -- <paths>`; its success is proven by git's output, never assumed from exit 0.
- "Could not determine" is never "nothing there": every git failure refuses with its own message ending "Nothing was reverted."
- No hub lock is held while git runs.
- `RevertPlan` and `Reverted` go to the requester only (`send_to`).
- The token is a hex `String`, never a JSON number.
- The confirmation focuses Cancel.
- HTTP surface unchanged: both intents are websocket.
- Tests: `cargo test -- --test-threads=1` (a parallel `cargo test` hangs on this host). Never `--release`.
- Every new test is revert-checked: apply the broken version, run it, read the failure, restore. Back up with `cp`, never restore with `git checkout <file>`. Record the result in the test's comment.
- Stage explicit paths in commits; never `git add -A`.

## Deviation from the spec

The spec says each path "must pass `safe_resolve` against the project".
`safe_resolve` canonicalises the target, and a deleted file (`D`, which the
spec says to revert) has no target, so it would be misfiled as *outside*;
`safe_resolve_parent` fails the same way for a file whose whole directory was
deleted. The plan confines lexically instead: a porcelain path is inside the
project exactly when it has no `..` component, because git reports paths
relative to its cwd (the project) and never lists one through a symlinked
directory (it tracks the link itself). roost never opens these paths itself;
git does, and `CloseBuffer` confines on its own. Task 2's `inside` carries
this rationale in its comment, and `a_nested_project_skips_paths_outside_it`
plus the deleted-file case in `a_plan_takes_only_paths_with_a_version_in_head`
pin both directions.

## Review Focus

Inputs the spec implies but does not name, most likely to bite first. Each has its test in the task that owns the code.

1. **The tree goes clean between the token check and the stash** (Claude or a terminal runs `git checkout`). `git stash push` then exits 0 saying "No local changes to save". The user expects "nothing was reverted", not "Reverted 1 file". Test: Task 2, `execute_refuses_when_git_saved_nothing`.
2. **A changed file whose content is not UTF-8** (a Latin-1 source file). Expected: a refusal naming the diff, not a panic and not a revert of unseen content. Test: Task 2, `a_non_utf8_diff_refuses`.
3. **A filename that is not UTF-8.** `-z` output carries it raw, `run_git` used to swallow the decode error as empty output, and empty status reads as a clean tree. Expected: "Could not read git status", not "Nothing to revert". Test: Task 1, `undecodable_output_is_an_error_not_empty`.
4. **No git identity configured.** `stash push` needs a committer. Expected: "git refused: …" with git's own reason, file untouched. Test: Task 2, `a_stash_git_refuses_reverts_nothing`.
5. **The token through JSON.** A `u64` above 2^53 loses precision as a JS number and every revert would read "changed since you looked". Expected: it round-trips. Test: Task 3, `the_token_travels_as_a_string`.

---

## File Structure

- `src/gitio.rs` (modify): `run_git` reports undecodable stdout as an error; new `Entry` + `parse_status_z`.
- `src/revert.rs` (create): plan building, token, execution, and the two hub-facing entry points. One responsibility: turning "revert this" into exactly one checked git command.
- `src/lib.rs` (modify): `pub mod revert;`.
- `src/render.rs` (modify): `data-xy` on change rows; `revert_list_html`.
- `src/proto.rs` (modify): two intents, two events.
- `src/wsconn.rs` (modify): divert both intents off the hub lock.
- `src/hub.rs` (modify): `unreachable!` arms, like `Search` and `RestoreWorkspace`.
- `static/dialog.js`, `static/style.css` (modify): disabled menu items with a hint.
- `static/app.js` (modify): menu split, `gitMenu`, preview/confirm/result handling.
- `tests/browser/revert.mjs` (create), `tests/browser/README.md` (modify).

---

### Task 1: `gitio`: a trustworthy status snapshot

**Files:**
- Modify: `src/gitio.rs` (`run_git` stdout thread ~line 43; add after `parse_status` ~line 119; tests at the bottom)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum Entry {
      Ordinary { xy: String, path: String },
      Renamed { xy: String, path: String, orig: String },
      Unmerged { xy: String, path: String },
      Untracked { path: String },
  }
  pub fn parse_status_z(out: &str) -> Vec<Entry>;
  ```
  and `run_git` returning `Err("git <arg> output was not valid UTF-8")` instead of `Ok("")`.

- [ ] **Step 1: Write the failing tests** (in `gitio`'s `mod tests`)

```rust
    /// Review Focus 3. `-z` output carries a filename's raw bytes, and the
    /// stdout thread used to drop `read_to_string`'s error, leaving an empty
    /// String: a run that succeeded with no output, which a status reader
    /// takes for a clean tree. "Could not read" must not become "nothing".
    #[test]
    fn undecodable_output_is_an_error_not_empty() {
        use std::os::unix::ffi::OsStrExt;
        let d = tempfile::tempdir().unwrap();
        run_git(d.path(), &["init", "-q"], false).unwrap();
        std::fs::write(d.path().join(std::ffi::OsStr::from_bytes(b"bad\xff")), "x").unwrap();
        let got = run_git(d.path(), &["status", "--porcelain=v2", "-z"], false);
        let err = got.expect_err("undecodable output must not read as success");
        assert!(err.contains("not valid UTF-8"), "{err}");
    }

    #[test]
    fn parse_status_z_reads_every_entry_kind_and_keeps_awkward_names() {
        // One of each kind; the ordinary name has a space and non-ASCII, and a
        // rename's two paths are separate NUL fields under -z (a tab without it).
        let out = "1 .M N... 100644 100644 100644 abc abc a b\u{e9}.rs\0\
                   2 R. N... 100644 100644 100644 abc abc R100 new.rs\0old.rs\0\
                   u UU N... 100644 100644 100644 100644 abc abc abc c.rs\0\
                   ? new dir/x.txt\0";
        assert_eq!(
            parse_status_z(out),
            vec![
                Entry::Ordinary { xy: ".M".into(), path: "a b\u{e9}.rs".into() },
                Entry::Renamed { xy: "R.".into(), path: "new.rs".into(), orig: "old.rs".into() },
                Entry::Unmerged { xy: "UU".into(), path: "c.rs".into() },
                Entry::Untracked { path: "new dir/x.txt".into() },
            ]
        );
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib -- --test-threads=1 gitio::tests::undecodable_output gitio::tests::parse_status_z`
Expected: compile error (`parse_status_z`/`Entry` not found). Comment the second test out and rerun: the first FAILS with "undecodable output must not read as success". Then restore it.

- [ ] **Step 3: Implement**

In `run_git`, make the stdout thread report its read error:

```rust
    let stdout_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        // The error is kept, not dropped: on invalid UTF-8 `read_to_string`
        // leaves `buf` empty, and an empty stdout from a successful git reads
        // as "nothing to report", a clean tree, when the truth is "could not
        // read the answer".
        let ok = match stdout_pipe.as_mut() {
            Some(s) => s.read_to_string(&mut buf).is_ok(),
            None => true,
        };
        (buf, ok)
    });
```

and where it is joined:

```rust
    let (stdout, stdout_ok) = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    let code = status.code().unwrap_or(-1);
    if code != 0 && !(allow_exit_1 && code == 1) {
        // git diff exits 1 when differences exist (only allowed if allow_exit_1 is true)
        return Err(stderr.trim().to_string());
    }
    if !stdout_ok {
        return Err(format!("git {} output was not valid UTF-8", args.first().unwrap_or(&"")));
    }
    Ok(stdout)
```

(`unwrap_or_default` on a `(String, bool)` gives `(String::new(), false)`, so a panicked reader thread is also an error. That's the correct direction.)

After `parse_status`, add:

```rust
/// One `git status --porcelain=v2 -z` entry, for the one caller that acts on
/// status rather than displaying it: `revert`. Separate from `parse_status`
/// because `-z` changes the framing (NUL-terminated, names unquoted, a
/// rename's two paths as two fields) and because the display parser drops
/// unmerged entries, which a destructive caller must see in order to refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Ordinary { xy: String, path: String },
    Renamed { xy: String, path: String, orig: String },
    Unmerged { xy: String, path: String },
    Untracked { path: String },
}

pub fn parse_status_z(out: &str) -> Vec<Entry> {
    let mut fields = out.split('\0');
    let mut entries = Vec::new();
    while let Some(f) = fields.next() {
        if let Some(rest) = f.strip_prefix("1 ") {
            // XY sub mH mI mW hH hI path
            let p: Vec<&str> = rest.splitn(8, ' ').collect();
            if p.len() == 8 {
                entries.push(Entry::Ordinary { xy: p[0].into(), path: p[7].into() });
            }
        } else if let Some(rest) = f.strip_prefix("2 ") {
            // XY sub mH mI mW hH hI Xscore path, then origPath as its own field
            let p: Vec<&str> = rest.splitn(9, ' ').collect();
            let orig = fields.next().unwrap_or("");
            if p.len() == 9 {
                entries.push(Entry::Renamed { xy: p[0].into(), path: p[8].into(), orig: orig.into() });
            }
        } else if let Some(rest) = f.strip_prefix("u ") {
            // XY sub m1 m2 m3 mW h1 h2 h3 path
            let p: Vec<&str> = rest.splitn(10, ' ').collect();
            if p.len() == 10 {
                entries.push(Entry::Unmerged { xy: p[0].into(), path: p[9].into() });
            }
        } else if let Some(rest) = f.strip_prefix("? ") {
            entries.push(Entry::Untracked { path: rest.into() });
        }
    }
    entries
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib -- --test-threads=1 gitio::`
Expected: PASS, including every existing `gitio` test.

- [ ] **Step 5: Revert-check**

`cp src/gitio.rs <scratchpad>/gitio.rs.good`, change the joined read back to ignore `stdout_ok`, run `undecodable_output_is_an_error_not_empty`, confirm it fails, then restore from the copy. Write the result into the test's doc comment ("Revert-checked: …").

- [ ] **Step 6: Commit**

```bash
git add src/gitio.rs
git commit -m "gitio: undecodable output is an error, and a -z status parser for acting on status"
```

---

### Task 2: `revert.rs`: the plan, the token, the one git command

**Files:**
- Create: `src/revert.rs`
- Modify: `src/lib.rs` (add `pub mod revert;` in alphabetical position, after `relaunch`)
- Modify: `src/render.rs` (add `revert_list_html` after `diff_html`)

**Interfaces:**
- Consumes: `gitio::{Entry, parse_status_z}` (Task 1), `worktree::GitRunner` (`&dyn Fn(&Path, &[&str]) -> Result<String, String>`), `workspace::hash_text(&str) -> u64`, `render::{diff_html, esc}`.
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
  pub struct PlanPath { pub path: String, pub xy: String }
  #[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
  pub struct Skipped { pub untracked: u32, pub added: u32, pub renamed: u32, pub outside: u32 }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Plan { pub paths: Vec<PlanPath>, pub staged: bool, pub skipped: Skipped, pub diff: String, pub token: String }
  pub fn build_plan(dir: &Path, rel: Option<&str>, run: GitRunner) -> Result<Plan, String>;
  pub fn execute(dir: &Path, plan: &Plan, run: GitRunner) -> Result<String, String>; // Ok(success message)
  // render.rs
  pub fn revert_list_html(paths: &[crate::revert::PlanPath]) -> String;
  ```

- [ ] **Step 1: Write the failing tests** (in `src/revert.rs`'s `mod tests`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A repository with an identity of its own, so `stash push` never
    /// depends on the host's global config, and `files` committed.
    fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        git(d.path(), &["config", "user.email", "t@example.com"]);
        git(d.path(), &["config", "user.name", "t"]);
        for (p, body) in files {
            let f = d.path().join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, body).unwrap();
        }
        git(d.path(), &["add", "-A"]);
        git(d.path(), &["commit", "-q", "-m", "base"]);
        d
    }

    fn real(d: &Path, a: &[&str]) -> Result<String, String> { crate::worktree::real_git(d, a) }

    #[test]
    fn a_plan_takes_only_paths_with_a_version_in_head() {
        let d = repo(&[("m.txt", "m\n"), ("gone.txt", "g\n"), ("old.txt", "o\n")]);
        std::fs::write(d.path().join("m.txt"), "m2\n").unwrap();
        std::fs::remove_file(d.path().join("gone.txt")).unwrap();
        std::fs::write(d.path().join("untracked.txt"), "u\n").unwrap();
        std::fs::write(d.path().join("added.txt"), "a\n").unwrap();
        git(d.path(), &["add", "added.txt"]);
        git(d.path(), &["mv", "old.txt", "new.txt"]);
        let p = build_plan(d.path(), None, &real).unwrap();
        let mut got: Vec<(String, String)> = p.paths.iter().map(|x| (x.path.clone(), x.xy.clone())).collect();
        got.sort();
        assert_eq!(got, vec![("gone.txt".into(), ".D".into()), ("m.txt".into(), ".M".into())]);
        assert_eq!(p.skipped, Skipped { untracked: 1, added: 1, renamed: 1, outside: 0 });
        assert!(p.staged, "the added file and the rename are staged");
    }

    /// Porcelain paths are relative to git's cwd, so a nested project sees its
    /// parent's changes as `../x`. The fixture has to be a real nested
    /// project, or a plan that took `../x` could not fail this.
    #[test]
    fn a_nested_project_skips_paths_outside_it() {
        let d = repo(&[("top.txt", "t\n"), ("sub/in.txt", "i\n")]);
        std::fs::write(d.path().join("top.txt"), "t2\n").unwrap();
        std::fs::write(d.path().join("sub/in.txt"), "i2\n").unwrap();
        let p = build_plan(&d.path().join("sub"), None, &real).unwrap();
        assert_eq!(p.paths, vec![PlanPath { path: "in.txt".into(), xy: ".M".into() }]);
        assert_eq!(p.skipped.outside, 1);
    }

    /// A pathspec is a pattern: without `--literal-pathspecs`, reverting `a*`
    /// also reverts `ab`. The unselected file is the whole assertion.
    #[test]
    fn a_glob_character_in_a_name_reverts_only_that_file() {
        let d = repo(&[("a*", "1\n"), ("ab", "1\n")]);
        std::fs::write(d.path().join("a*"), "2\n").unwrap();
        std::fs::write(d.path().join("ab"), "2\n").unwrap();
        let p = build_plan(d.path(), Some("a*"), &real).unwrap();
        execute(d.path(), &p, &real).unwrap();
        assert_eq!(std::fs::read_to_string(d.path().join("a*")).unwrap(), "1\n");
        assert_eq!(std::fs::read_to_string(d.path().join("ab")).unwrap(), "2\n", "ab was not selected");
    }

    #[test]
    fn a_revert_restores_head_saves_a_stash_and_touches_nothing_else() {
        let d = repo(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
        std::fs::write(d.path().join("a.txt"), "staged\n").unwrap();
        git(d.path(), &["add", "a.txt"]);
        std::fs::write(d.path().join("a.txt"), "worktree\n").unwrap();
        std::fs::write(d.path().join("b.txt"), "b2\n").unwrap();
        let p = build_plan(d.path(), Some("a.txt"), &real).unwrap();
        let msg = execute(d.path(), &p, &real).unwrap();
        assert!(msg.contains("stash@{0}") && msg.contains("roost revert: 1 file"), "{msg}");
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "a\n");
        assert_eq!(git(d.path(), &["diff", "--cached", "--name-only"]), "", "the index is back at HEAD too");
        assert_eq!(std::fs::read_to_string(d.path().join("b.txt")).unwrap(), "b2\n", "b.txt was not selected");
        let list = git(d.path(), &["stash", "list"]);
        assert_eq!(list.lines().count(), 1, "{list}");
        assert!(list.contains("roost revert: 1 file"), "{list}");
        git(d.path(), &["stash", "apply", "-q"]);
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "worktree\n", "the stash brings it back");
    }

    #[test]
    fn a_failed_status_refuses_and_says_so() {
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let broken = |dir: &Path, a: &[&str]| {
            if a.contains(&"status") { Err("fatal: index file corrupt".to_string()) } else { real(dir, a) }
        };
        let err = build_plan(d.path(), None, &broken).unwrap_err();
        assert!(err.starts_with("Could not read git status: fatal: index file corrupt"), "{err}");
        assert!(err.ends_with("Nothing was reverted."), "{err}");
    }

    #[test]
    fn a_merge_in_progress_refuses() {
        let d = repo(&[("a.txt", "a\n")]);
        git(d.path(), &["checkout", "-q", "-b", "other"]);
        std::fs::write(d.path().join("a.txt"), "other\n").unwrap();
        git(d.path(), &["commit", "-qam", "other"]);
        git(d.path(), &["checkout", "-q", "-"]);
        std::fs::write(d.path().join("a.txt"), "mine\n").unwrap();
        git(d.path(), &["commit", "-qam", "mine"]);
        let _ = Command::new("git").arg("-C").arg(d.path()).args(["merge", "-q", "other"]).output();
        assert!(d.path().join(".git/MERGE_HEAD").exists(), "fixture: the merge must be in progress");
        let err = build_plan(d.path(), None, &real).unwrap_err();
        assert!(err.contains("A merge is in progress"), "{err}");
    }

    /// The git-dir check on its own: a clean status with MERGE_HEAD present.
    /// Without this the test above could pass on the unmerged entry alone.
    #[test]
    fn merge_head_alone_refuses() {
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let head = git(d.path(), &["rev-parse", "HEAD"]);
        std::fs::write(d.path().join(".git/MERGE_HEAD"), head).unwrap();
        let err = build_plan(d.path(), None, &real).unwrap_err();
        assert!(err.contains("A merge is in progress"), "{err}");
    }

    #[test]
    fn the_token_changes_when_the_diff_does() {
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
        let t1 = build_plan(d.path(), None, &real).unwrap().token;
        std::fs::write(d.path().join("a.txt"), "two\n").unwrap();
        let t2 = build_plan(d.path(), None, &real).unwrap().token;
        assert_ne!(t1, t2, "same path and XY, different content: the token must see the diff");
    }

    /// Review Focus 1. `stash push` exits 0 with "No local changes to save"
    /// when the paths went clean after the plan was built.
    #[test]
    fn execute_refuses_when_git_saved_nothing() {
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let p = build_plan(d.path(), None, &real).unwrap();
        std::fs::write(d.path().join("a.txt"), "a\n").unwrap(); // clean again
        let err = execute(d.path(), &p, &real).unwrap_err();
        assert!(err.contains("Nothing was reverted"), "{err}");
        assert_eq!(git(d.path(), &["stash", "list"]), "");
    }

    /// Review Focus 2.
    #[test]
    fn a_non_utf8_diff_refuses() {
        let d = repo(&[("l.txt", "caf\n")]);
        std::fs::write(d.path().join("l.txt"), b"caf\xe9\n").unwrap();
        let err = build_plan(d.path(), None, &real).unwrap_err();
        assert!(err.starts_with("Could not read the diff:") && err.contains("not valid UTF-8"), "{err}");
    }

    /// Review Focus 4. An empty identity stands in for a host with none.
    #[test]
    fn a_stash_git_refuses_reverts_nothing() {
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let p = build_plan(d.path(), None, &real).unwrap();
        let no_ident = |dir: &Path, a: &[&str]| {
            let mut v = vec!["-c", "user.name=", "-c", "user.email="];
            v.extend_from_slice(a);
            real(dir, &v)
        };
        let err = execute(d.path(), &p, &no_ident).unwrap_err();
        assert!(err.starts_with("git refused: ") && err.ends_with("Nothing was reverted."), "{err}");
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "a2\n");
    }
}
```

In `render.rs`'s tests:

```rust
    #[test]
    fn revert_list_html_escapes_paths() {
        let h = revert_list_html(&[crate::revert::PlanPath { path: "<b>.rs".into(), xy: ".M".into() }]);
        assert!(h.contains("&lt;b&gt;.rs") && !h.contains("<b>"), "{h}");
        assert!(h.contains(".M"), "{h}");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib -- --test-threads=1 revert:: render::tests::revert_list`
Expected: compile errors (module and functions missing).

- [ ] **Step 3: Implement `src/revert.rs`** (hub-facing functions are added in Task 3)

```rust
//! Reverting what the Changes pane and a Diff tab show (#125).
//!
//! The first place roost overwrites file content with git, so everything here
//! is shaped by CLAUDE.md's "destruction requires positive evidence". A plan
//! is built from a fresh `git status -z` and `git diff HEAD`, and a token over
//! it is what the dialog was shown; the revert rebuilds the plan and proceeds
//! only if the token still matches, so a change made after the user looked is
//! never discarded unseen. The revert itself is one `git stash push`, which
//! saves and reverts in a single git operation (no half-state), and its success
//! is read from git's own output rather than assumed from exit 0.
//!
//! A revert never deletes: only paths with a version in HEAD are taken, which
//! is why untracked, added, and renamed entries are counted and left alone.
use crate::gitio::{parse_status_z, Entry};
use crate::worktree::GitRunner;
use std::path::{Component, Path};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PlanPath {
    pub path: String,
    pub xy: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Skipped {
    pub untracked: u32,
    pub added: u32,
    pub renamed: u32,
    pub outside: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub paths: Vec<PlanPath>,
    pub staged: bool,
    pub skipped: Skipped,
    pub diff: String,
    /// Hex, not a number: a u64 above 2^53 does not survive a JS number.
    pub token: String,
}

const NOTHING: &str = "Nothing was reverted.";

/// Git's in-progress markers, with how to name each. Checked in the git dir
/// with `symlink_metadata`: `NotFound` is absent, any other error is *cannot
/// tell*, and cannot-tell refuses like present does.
const IN_PROGRESS: [(&str, &str); 5] = [
    ("MERGE_HEAD", "A merge"),
    ("rebase-merge", "A rebase"),
    ("rebase-apply", "A rebase"),
    ("CHERRY_PICK_HEAD", "A cherry-pick"),
    ("REVERT_HEAD", "A revert"),
];

fn refuse_in_progress(dir: &Path, run: GitRunner) -> Result<(), String> {
    let git_dir = run(dir, &["rev-parse", "--absolute-git-dir"])
        .map_err(|e| format!("Could not check for a merge in progress: {e}. {NOTHING}"))?;
    let git_dir = Path::new(git_dir.trim());
    for (marker, what) in IN_PROGRESS {
        match std::fs::symlink_metadata(git_dir.join(marker)) {
            Ok(_) => return Err(format!("{what} is in progress; finish or abort it first. {NOTHING}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Could not check for a merge in progress: {e}. {NOTHING}")),
        }
    }
    Ok(())
}

/// Porcelain paths are relative to git's cwd, which is the project, so a path
/// is inside it exactly when it has no `..` (a nested project sees its
/// parent's changes as `../x`). Lexical on purpose: git names tree entries and
/// never lists a path *through* a symlinked directory (it tracks the link
/// itself), so there is no link to resolve; and a deleted file, or a whole
/// deleted directory, has no parent to canonicalise, which `safe_resolve_parent`
/// would refuse.
fn inside(path: &str) -> bool {
    Path::new(path).components().all(|c| matches!(c, Component::Normal(_)))
}

pub fn build_plan(dir: &Path, rel: Option<&str>, run: GitRunner) -> Result<Plan, String> {
    refuse_in_progress(dir, run)?;
    let status = run(dir, &["status", "--porcelain=v2", "-z"])
        .map_err(|e| format!("Could not read git status: {e}. {NOTHING}"))?;
    let mut paths = Vec::new();
    let mut skipped = Skipped::default();
    let mut staged = false;
    for entry in parse_status_z(&status) {
        let path = match &entry {
            Entry::Ordinary { path, .. } | Entry::Renamed { path, .. } | Entry::Unmerged { path, .. } | Entry::Untracked { path } => path,
        };
        if matches!(entry, Entry::Unmerged { .. }) {
            // Checked before `rel` filters: a conflict anywhere means a merge
            // is being resolved, whichever file was right-clicked.
            return Err(format!("A merge is in progress; finish or abort it first. {NOTHING}"));
        }
        if rel.is_some_and(|r| r != path) {
            continue;
        }
        if !inside(path) {
            skipped.outside += 1;
            continue;
        }
        match entry {
            Entry::Untracked { .. } => skipped.untracked += 1,
            Entry::Renamed { .. } => skipped.renamed += 1,
            Entry::Ordinary { xy, path } => {
                let x = xy.chars().next().unwrap_or('.');
                if x == 'A' {
                    skipped.added += 1;
                } else {
                    staged |= x != '.';
                    paths.push(PlanPath { path, xy });
                }
            }
            Entry::Unmerged { .. } => unreachable!("returned above"),
        }
    }
    // Staged-ness of skipped entries matters too: the dialog says "includes
    // staged changes" only about what it reverts, so it is computed from
    // `paths` above, not from every entry.
    paths.sort_by(|a, b| a.path.cmp(&b.path));
    let diff = if paths.is_empty() {
        String::new()
    } else {
        let mut args = vec!["--literal-pathspecs", "diff", "HEAD", "--"];
        args.extend(paths.iter().map(|p| p.path.as_str()));
        run(dir, &args).map_err(|e| format!("Could not read the diff: {e}. {NOTHING}"))?
    };
    let mut basis = String::new();
    for p in &paths {
        basis.push_str(&p.xy);
        basis.push(' ');
        basis.push_str(&p.path);
        basis.push('\0');
    }
    basis.push_str(&diff);
    let token = format!("{:016x}", crate::workspace::hash_text(&basis));
    Ok(Plan { paths, staged, skipped, diff, token })
}

/// The revert: one git command that saves the paths to a stash entry and
/// resets them to HEAD. `Ok` carries the message the user sees.
pub fn execute(dir: &Path, plan: &Plan, run: GitRunner) -> Result<String, String> {
    let n = plan.paths.len();
    let label = format!("roost revert: {n} file{}", if n == 1 { "" } else { "s" });
    let mut args = vec!["--literal-pathspecs", "stash", "push", "-m", label.as_str(), "--"];
    args.extend(plan.paths.iter().map(|p| p.path.as_str()));
    let out = run(dir, &args).map_err(|e| {
        let first = e.lines().next().unwrap_or("").trim();
        format!("git refused: {first}. {NOTHING}")
    })?;
    // Exit 0 is not the evidence: with nothing left to save, git says "No
    // local changes to save" and exits 0 having reverted nothing. The line it
    // prints when it did save is.
    if !out.contains("Saved working directory") {
        return Err(format!("These changes were already gone. {NOTHING}"));
    }
    Ok(format!(
        "Reverted {n} file{}; saved as stash@{{0}} (\"{label}\"). `git stash apply` brings them back.",
        if n == 1 { "" } else { "s" }
    ))
}
```

Add to `src/render.rs` after `diff_html`:

```rust
/// The "Revert all" confirmation's detail: which paths, with their XY codes.
/// Escaped like every other interpolation; `askChoice` sets it as innerHTML.
pub fn revert_list_html(paths: &[crate::revert::PlanPath]) -> String {
    paths
        .iter()
        .map(|p| format!("<div class=\"dl ctx\">{} {}</div>", esc(&p.xy), esc(&p.path)))
        .collect()
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib -- --test-threads=1 revert:: render::tests::revert_list`
Expected: all PASS.

- [ ] **Step 5: Revert-check each, restoring from a `cp` backup each time, and record results in the doc comments**

| Break | Must fail |
|---|---|
| Drop `--literal-pathspecs` from both arg lists | `a_glob_character_in_a_name_reverts_only_that_file` |
| `inside` returns `true` | `a_nested_project_skips_paths_outside_it` |
| Treat `x == 'A'` as eligible | `a_plan_takes_only_paths_with_a_version_in_head` |
| Delete the `Saved working directory` check | `execute_refuses_when_git_saved_nothing` |
| Delete the `IN_PROGRESS` loop | `merge_head_alone_refuses` (and `a_merge_in_progress_refuses` must stay green: it is caught by the unmerged entry. Say so in its comment.) |
| Hash only the path list, not the diff | `the_token_changes_when_the_diff_does` |

- [ ] **Step 6: Commit**

```bash
git add src/revert.rs src/lib.rs src/render.rs
git commit -m "revert: build a checked plan and revert it with one stash push"
```

---

### Task 3: The protocol, off the hub lock, to the requester only

**Files:**
- Modify: `src/proto.rs` (`Intent` ~line 81, `Event` ~line 433, encode tests ~line 571)
- Modify: `src/revert.rs` (add `run_preview`, `run_revert`, tests)
- Modify: `src/wsconn.rs` (divert after the `RestoreWorkspace` block, ~line 381)
- Modify: `src/hub.rs` (`unreachable!` arms next to `RestoreWorkspace`, ~line 779)

**Interfaces:**
- Consumes: `build_plan`, `execute`, `Plan`, `PlanPath`, `Skipped` (Task 2); `render::{diff_html, revert_list_html}`; `Hub::{lock, send_to, broadcast, snapshot_event, handle}`; `Intent::CloseBuffer { rel }` (discard = re-read from disk, sends `BufferText`).
- Produces:
  ```rust
  // proto::Intent
  RevertPreview { #[serde(default)] rel: Option<String> },
  Revert { #[serde(default)] rel: Option<String>, token: String, #[serde(default)] discard_buffers: Vec<String> },
  // proto::Event
  RevertPlan { rel: Option<String>, paths: Vec<crate::revert::PlanPath>, staged: bool,
               skipped: crate::revert::Skipped, dirty: Vec<String>, detail_html: String, token: String },
  Reverted { rel: Option<String>, ok: bool, msg: String, stale: bool },
  // revert.rs
  pub fn run_preview(hub: &Arc<Mutex<Hub>>, id: &ConnId, rel: Option<String>, run: GitRunner);
  pub fn run_revert(hub: &Arc<Mutex<Hub>>, id: &ConnId, rel: Option<String>, token: String, discard: Vec<String>, run: GitRunner);
  ```
  Wire names (client, Task 4): `{t:"RevertPreview", rel}`, `{t:"Revert", rel, token, discard_buffers}`; events `t:"RevertPlan"` and `t:"Reverted"` with the fields above in snake_case.

- [ ] **Step 1: Write the failing tests** (append to `revert.rs`'s `mod tests`)

```rust
    use crate::hub::Hub;
    use std::sync::{Arc, Mutex};

    fn hub_on(d: &Path) -> Arc<Mutex<Hub>> {
        Arc::new(Mutex::new(Hub::new("revertproj", d.to_path_buf())))
    }
    fn drain(rx: &std::sync::mpsc::Receiver<String>) -> Vec<String> {
        let mut v = vec![];
        while let Ok(m) = rx.try_recv() { v.push(m); }
        v
    }
    fn token_of(msgs: &[String]) -> String {
        let m = msgs.iter().find(|m| m.contains(r#""t":"RevertPlan""#)).expect("a RevertPlan");
        let v: serde_json::Value = serde_json::from_str(m).unwrap();
        v["token"].as_str().expect("the token is a JSON string").to_string()
    }

    /// Two subscribers, or `send_to` and `broadcast` look the same.
    #[test]
    fn plan_and_result_reach_only_the_requester() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let hub = hub_on(d.path());
        let (me, rx_me) = Hub::lock(&hub).subscribe();
        let (_other, rx_other) = Hub::lock(&hub).subscribe();
        drain(&rx_me); drain(&rx_other);

        run_preview(&hub, &me, None, &real);
        let token = token_of(&drain(&rx_me));
        run_revert(&hub, &me, None, token, vec![], &real);
        let mine = drain(&rx_me);
        let theirs = drain(&rx_other);
        std::env::remove_var("ROOST_STATE_DIR");

        assert!(mine.iter().any(|m| m.contains(r#""t":"Reverted""#) && m.contains(r#""ok":true"#)), "{mine:?}");
        assert!(!theirs.iter().any(|m| m.contains("RevertPlan") || m.contains(r#""t":"Reverted""#)), "{theirs:?}");
    }

    /// Review Focus 5.
    #[test]
    fn the_token_travels_as_a_string() {
        let ev = crate::proto::Event::RevertPlan {
            rel: None, paths: vec![], staged: false, skipped: Skipped::default(),
            dirty: vec![], detail_html: String::new(), token: format!("{:016x}", u64::MAX),
        };
        let s = crate::proto::encode(&ev);
        assert!(s.contains(r#""token":"ffffffffffffffff""#), "{s}");
    }

    #[test]
    fn a_change_made_after_the_preview_is_not_reverted() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "seen\n").unwrap();
        let hub = hub_on(d.path());
        let (me, rx) = Hub::lock(&hub).subscribe();
        drain(&rx);
        run_preview(&hub, &me, None, &real);
        let token = token_of(&drain(&rx));
        std::fs::write(d.path().join("a.txt"), "unseen\n").unwrap();
        run_revert(&hub, &me, None, token, vec![], &real);
        let msgs = drain(&rx);
        std::env::remove_var("ROOST_STATE_DIR");
        assert!(msgs.iter().any(|m| m.contains("changed since you looked") && m.contains(r#""stale":true"#)), "{msgs:?}");
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "unseen\n");
    }

    /// The tree went clean between preview and revert: refused as changed,
    /// never reported as a success.
    #[test]
    fn a_tree_cleaned_after_the_preview_is_refused_not_reported_reverted() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let hub = hub_on(d.path());
        let (me, rx) = Hub::lock(&hub).subscribe();
        drain(&rx);
        run_preview(&hub, &me, None, &real);
        let token = token_of(&drain(&rx));
        std::fs::write(d.path().join("a.txt"), "a\n").unwrap();
        run_revert(&hub, &me, None, token, vec![], &real);
        let msgs = drain(&rx);
        std::env::remove_var("ROOST_STATE_DIR");
        assert!(msgs.iter().any(|m| m.contains(r#""t":"Reverted""#) && m.contains(r#""ok":false"#)), "{msgs:?}");
    }

    #[test]
    fn an_unnamed_dirty_buffer_refuses_and_a_named_one_is_discarded() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let hub = hub_on(d.path());
        let (me, rx) = Hub::lock(&hub).subscribe();
        {
            let mut h = Hub::lock(&hub);
            h.handle(&me, crate::proto::Intent::OpenTab {
                pane: crate::proto::MIDDLE,
                tab: crate::workspace::Tab::File { rel: "a.txt".into(), mode: crate::proto::Mode::Edit },
            });
            h.handle(&me, crate::proto::Intent::EditBuffer { rel: "a.txt".into(), text: "typed\n".into() });
        }
        drain(&rx);
        run_preview(&hub, &me, None, &real);
        let plan = drain(&rx);
        assert!(plan.iter().any(|m| m.contains(r#""dirty":["a.txt"]"#)), "{plan:?}");
        let token = token_of(&plan);

        run_revert(&hub, &me, None, token.clone(), vec![], &real);
        let refused = drain(&rx);
        assert!(refused.iter().any(|m| m.contains("unsaved edits")), "{refused:?}");
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "a2\n");
        assert!(Hub::lock(&hub).ws.buffers["a.txt"].dirty(), "an unnamed buffer is not discarded");

        run_revert(&hub, &me, None, token, vec!["a.txt".into()], &real);
        let done = drain(&rx);
        std::env::remove_var("ROOST_STATE_DIR");
        assert!(done.iter().any(|m| m.contains(r#""ok":true"#)), "{done:?}");
        assert!(!Hub::lock(&hub).ws.buffers["a.txt"].dirty(), "a named buffer is discarded to the file");
        assert!(done.iter().any(|m| m.contains(r#""t":"BufferText""#) && m.contains("a\\n")), "{done:?}");
    }

    /// No hub lock while git runs. The runner parks inside `stash push` until
    /// told to go; while it is parked the lock must be takeable. A lock-held
    /// implementation scores 0 here. Timed rather than counted on failure,
    /// because a deadlock hangs.
    #[test]
    fn the_hub_lock_is_free_while_git_runs() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let hub = hub_on(d.path());
        let (me, rx) = Hub::lock(&hub).subscribe();
        drain(&rx);
        run_preview(&hub, &me, None, &real);
        let token = token_of(&drain(&rx));

        let (go, wait) = std::sync::mpsc::channel::<()>();
        let (h2, me2) = (hub.clone(), me.clone());
        let worker = std::thread::spawn(move || {
            let wait = Mutex::new(wait);
            let slow = move |p: &Path, a: &[&str]| {
                if a.contains(&"stash") { let _ = wait.lock().unwrap().recv(); }
                real(p, a)
            };
            run_revert(&h2, &me2, None, token, vec![], &slow);
        });
        std::thread::sleep(std::time::Duration::from_millis(200)); // let it reach the parked stash
        let mut free = 0;
        for _ in 0..50 {
            if hub.try_lock().is_ok() { free += 1; }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        go.send(()).unwrap();
        let started = std::time::Instant::now();
        worker.join().unwrap();
        std::env::remove_var("ROOST_STATE_DIR");
        assert!(started.elapsed() < std::time::Duration::from_secs(20), "the revert hung after release");
        assert!(free > 40, "the hub lock was free only {free}/50 times while git was running");
    }
```

The `OpenTab`/`EditBuffer` calls mirror `hub.rs`'s
`discarding_a_buffer_reloads_the_file_rather_than_leaving_nothing` (~line
3272); take `Tab`, `Mode` and `MIDDLE` from wherever that test imports them,
since the paths above are written from memory of it.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib -- --test-threads=1 revert::tests`
Expected: compile errors (`run_preview`, `run_revert`, the new `Event` variants).

- [ ] **Step 3: Implement**

`src/proto.rs`, in `Intent` beside `RestoreWorkspace`:

```rust
    /// What a revert would do (#125). Answered to the requester only, with a
    /// token over exactly what the dialog will show. `None` is "all".
    /// Diverted in wsconn: a plan is several git calls.
    RevertPreview {
        #[serde(default)]
        rel: Option<String>,
    },
    /// Do it, if nothing changed since the preview: the server rebuilds the
    /// plan and refuses on a different token. `discard_buffers` names the
    /// unsaved roost buffers the dialog said would be discarded; a dirty one
    /// it did not name refuses.
    Revert {
        #[serde(default)]
        rel: Option<String>,
        token: String,
        #[serde(default)]
        discard_buffers: Vec<String>,
    },
```

In `Event`:

```rust
    RevertPlan {
        rel: Option<String>,
        paths: Vec<crate::revert::PlanPath>,
        staged: bool,
        skipped: crate::revert::Skipped,
        dirty: Vec<String>,
        /// Server-rendered and escaped: a diff (one file) or a path list.
        detail_html: String,
        token: String,
    },
    /// `stale`: refused because what the user saw no longer holds; the client
    /// re-asks for a preview rather than leaving them with a dead dialog.
    Reverted { rel: Option<String>, ok: bool, msg: String, stale: bool },
```

If `Event` derives more than `Serialize` (e.g. `Debug, Clone`), add those derives to `PlanPath` and `Skipped` too; Task 2 gave them `Debug, Clone, PartialEq, Eq, Serialize`.

`src/revert.rs`, above `mod tests`:

```rust
use crate::hub::{ConnId, Hub};
use crate::proto::Event;
use std::sync::{Arc, Mutex};

/// The paths among `paths` with an unsaved roost buffer. Buffer keys are
/// project-relative, the same space as porcelain paths run in the project.
fn dirty_among(hub: &Arc<Mutex<Hub>>, paths: &[PlanPath]) -> Vec<String> {
    let h = Hub::lock(hub);
    paths.iter().filter(|p| h.ws.buffers.get(&p.path).is_some_and(|b| b.dirty())).map(|p| p.path.clone()).collect()
}

fn project_dir(hub: &Arc<Mutex<Hub>>) -> std::path::PathBuf {
    Hub::lock(hub).dir.clone()
}

pub fn run_preview(hub: &Arc<Mutex<Hub>>, id: &ConnId, rel: Option<String>, run: GitRunner) {
    let dir = project_dir(hub); // lock released at the end of this statement
    let ev = match build_plan(&dir, rel.as_deref(), run) {
        Err(msg) => Event::Reverted { rel, ok: false, msg, stale: false },
        Ok(plan) => {
            let dirty = dirty_among(hub, &plan.paths);
            let detail_html = if rel.is_some() {
                crate::render::diff_html(&plan.diff)
            } else {
                crate::render::revert_list_html(&plan.paths)
            };
            Event::RevertPlan {
                rel, paths: plan.paths, staged: plan.staged, skipped: plan.skipped,
                dirty, detail_html, token: plan.token,
            }
        }
    };
    Hub::lock(hub).send_to(id, &ev);
}

pub fn run_revert(
    hub: &Arc<Mutex<Hub>>, id: &ConnId, rel: Option<String>, token: String,
    discard: Vec<String>, run: GitRunner,
) {
    let refuse = |msg: String, stale: bool| {
        Hub::lock(hub).send_to(id, &Event::Reverted { rel: rel.clone(), ok: false, msg, stale });
    };
    let dir = project_dir(hub);
    let plan = match build_plan(&dir, rel.as_deref(), run) {
        Ok(p) => p,
        Err(msg) => return refuse(msg, false),
    };
    if plan.token != token || plan.paths.is_empty() {
        return refuse("These changes changed since you looked; review them again.".into(), true);
    }
    if let Some(p) = dirty_among(hub, &plan.paths).into_iter().find(|p| !discard.contains(p)) {
        return refuse(format!("{p} gained unsaved edits; review again."), true);
    }
    match execute(&dir, &plan, run) {
        Err(msg) => refuse(msg, false),
        Ok(msg) => {
            let mut h = Hub::lock(hub);
            // Only buffers the dialog named, and only for paths reverted. One
            // that went dirty after the check was never named, so it is left
            // alone: its file changed under it, the existing conflict path.
            for rel in discard.iter().filter(|r| plan.paths.iter().any(|p| &p.path == *r)) {
                h.handle(id, crate::proto::Intent::CloseBuffer { rel: rel.clone() });
            }
            h.send_to(id, &Event::Reverted { rel: rel.clone(), ok: true, msg, stale: false });
            let snap = h.snapshot_event(id);
            h.broadcast(&snap);
        }
    }
}
```

(Check `Hub`'s `dir` and `ws` field visibility; if they are private, add `pub(crate)` to exactly those two fields and nothing else.)

`src/wsconn.rs`, after the `RestoreWorkspace` block:

```rust
                // Diverted like a restore, and for the same reason: a revert
                // plan is several git calls with a 15 s deadline each, and
                // this lock is what every other socket on the project waits
                // on. Inline, one at a time per connection. Wrapped because a
                // panic here would escape a socket thread (CLAUDE.md).
                if let Ok(proto::Intent::RevertPreview { rel }) = decoded {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        crate::revert::run_preview(&hub, &id, rel, &crate::worktree::real_git)
                    }));
                    if r.is_err() {
                        Hub::lock(&hub).send_to(&id, &proto::Event::Error { msg: "revert preview failed".into() });
                    }
                    continue;
                }
                if let Ok(proto::Intent::Revert { rel, token, discard_buffers }) = decoded {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        crate::revert::run_revert(&hub, &id, rel, token, discard_buffers, &crate::worktree::real_git)
                    }));
                    if r.is_err() {
                        Hub::lock(&hub).send_to(&id, &proto::Event::Error { msg: "revert failed; check git stash list".into() });
                    }
                    continue;
                }
```

`src/hub.rs`, after the `RestoreWorkspace` arm:

```rust
            // Diverted in wsconn for the same reason as the two above: git
            // under this lock would stall every browser on the project.
            Intent::RevertPreview { .. } | Intent::Revert { .. } => {
                unreachable!("revert intents are diverted in wsconn before this lock is taken")
            }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib -- --test-threads=1 revert:: proto:: wsconn::`
Expected: PASS.

- [ ] **Step 5: Revert-check, restoring from `cp` backups, recording results in comments**

| Break | Must fail |
|---|---|
| `send_to` → `broadcast` for `Reverted` | `plan_and_result_reach_only_the_requester` |
| Skip the token comparison | `a_change_made_after_the_preview_is_not_reverted` |
| Skip the dirty-buffer check | `an_unnamed_dirty_buffer_refuses_and_a_named_one_is_discarded` |
| Hold `Hub::lock(hub)` in a binding across `execute` | `the_hub_lock_is_free_while_git_runs` |
| Make `token` a `u64` field | `the_token_travels_as_a_string` (compile or assertion) |

- [ ] **Step 6: Full suite, then commit**

Run: `cargo test -- --test-threads=1`
Expected: every binary `ok`, 0 failed.

```bash
git add src/proto.rs src/revert.rs src/wsconn.rs src/hub.rs
git commit -m "revert: preview and revert intents, off the hub lock, answered to the requester"
```

---

### Task 4: Client: the git menu, the confirmation, the result

**Files:**
- Modify: `src/render.rs` (`changes_fragment` ~line 920: add `data-xy`; its tests)
- Modify: `static/dialog.js` (`askMenu` ~line 249)
- Modify: `static/style.css` (menu item styles, next to `.dlg-item`)
- Modify: `static/app.js` (`mountTab` ~line 1583, `wireFileLinks` ~line 2071, `wireFragment` ~line 2219, event switch ~line 843)

**Interfaces:**
- Consumes: wire shapes from Task 3.
- Produces: `gitMenu(e, rel, xy)`, `confirmRevert(ev)` in `app.js`; `askMenu` items accept `{ id, label, disabled?, hint? }`.

- [ ] **Step 1: Write the failing Rust test** (render tests)

```rust
    /// The client decides the menu's disabled state from this; a row without
    /// it would offer Revert on an untracked file and let the server say no.
    /// `"` in the fixture because `esc` is what stands between an XY code and
    /// the attribute (XY never holds one, but the rule is "escape everything").
    #[test]
    fn change_rows_carry_their_xy() {
        let st = Status { changes: vec![crate::gitio::Change { xy: "??".into(), path: "n.txt".into() }], ..Default::default() };
        let h = changes_fragment("p", &st);
        assert!(h.contains(r#"data-rel="n.txt""#) && h.contains(r#"data-xy="??""#), "{h}");
    }
```

- [ ] **Step 2: Run it**: `cargo test --lib -- --test-threads=1 render::tests::change_rows_carry` → FAIL (no `data-xy`).

- [ ] **Step 3: Implement**

`changes_fragment`'s row format gains `data-xy="{}"` right after `data-rel="{}"`, with `esc(&c.xy)` as the matching argument. The "full diff" row gets no `data-xy`.

`static/dialog.js` `askMenu`, replacing the item loop and the key handler's list:

```js
    for (const it of items) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "dlg-item";
      b.textContent = it.label;
      // A disabled item still says why: a menu that silently lacks the
      // command does not answer "why can't I do this here".
      if (it.disabled) b.disabled = true;
      if (it.hint) {
        const s = document.createElement("small");
        s.className = "dlg-hint";
        s.textContent = it.hint;
        b.append(s);
      }
      b.onclick = () => finish(it.id);
      list.appendChild(b);
    }
    el.onkeydown = (e) => {
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      e.preventDefault();
      const btns = [...list.querySelectorAll(".dlg-item:not(:disabled)")];
      if (!btns.length) return;
      const i = btns.indexOf(document.activeElement);
      const n = (e.key === "ArrowDown" ? i + 1 : i - 1 + btns.length) % btns.length;
      btns[n].focus();
    };
```

and in the returned focus function, `list.querySelector(".dlg-item:not(:disabled)") || el` in place of `list.querySelector(".dlg-item")`, so a menu with only a disabled item still takes focus and Escape still closes it.

`static/style.css`, beside the `.dlg-item` rules:

```css
/* A command that cannot run here still names itself and why (#125). */
.dlg-item:disabled { opacity: .55; cursor: default; }
.dlg-item .dlg-hint { display: block; font-size: 11px; color: var(--muted); }
```

`static/app.js`:

In `mountTab`, before `content.dataset.url = url;`:

```js
  // What wireFragment keys its context menu on: a tree gets the file menu,
  // Changes and Diff the git menu (#125). On the element, not passed along,
  // because htmx:afterSwap also calls wireFragment with only the element.
  content.dataset.kind = t.k;
  content.dataset.rel = t.k === "Diff" ? (t.rel || "") : "";
```

`wireFileLinks(root)` → `wireFileLinks(root, menu = fileMenu)`, with its context-menu line:

```js
    a.oncontextmenu = (e) => { e.preventDefault(); menu(e, a.dataset.rel, a.dataset.xy); };
```

`wireFragment`:

```js
function wireFragment(content) {
  // Changes and Diff show changes, not places: New file, Rename and Delete
  // mean nothing there, and Delete on a change row read as "drop this
  // change" while deleting the file (#125).
  if (content.dataset.kind === "Changes" || content.dataset.kind === "Diff") {
    wireFileLinks(content, gitMenu);
    content.oncontextmenu = (e) => {
      if (e.target.closest("a[data-rel]")) return;
      e.preventDefault();
      gitMenu(e, content.dataset.rel || "", undefined);
    };
    return;
  }
  wireFileLinks(content);
  // right-clicking blank space in a tree targets the project root
  content.oncontextmenu = (e) => {
    if (e.target.closest("a[data-rel]")) return;
    e.preventDefault();
    fileMenu(e, "");
  };
}
```

After `fileMenu`:

```js
/// Why a change row cannot be reverted, or "" if it can. A hint for the menu
/// only: the server decides again from its own status (revert.rs), so an
/// unknown XY (a Diff tab has none) is offered and the server answers.
function revertBlock(rel, xy) {
  if (rel.startsWith("../")) return "outside this project";
  if (!xy) return "";
  if (xy === "??") return "untracked, no committed version";
  if (xy[0] === "A") return "added, no committed version";
  if (xy[0] === "R" || xy[0] === "C") return "renamed";
  return "";
}

let revertRel = null;

async function gitMenu(e, rel, xy) {
  const all = !rel;
  const why = all ? "" : revertBlock(rel, xy);
  const choice = await askMenu({
    items: [{ id: "revert", label: all ? "Revert all…" : "Revert…", disabled: !!why, hint: why }],
    x: e.clientX, y: e.clientY,
  });
  if (choice !== "revert") return;
  revertRel = all ? null : rel;
  send({ t: "RevertPreview", rel: revertRel });
}

function skippedText(s) {
  const parts = [];
  if (s.untracked) parts.push(`${s.untracked} untracked`);
  if (s.added) parts.push(`${s.added} added`);
  if (s.renamed) parts.push(`${s.renamed} renamed`);
  if (s.outside) parts.push(`${s.outside} outside this project`);
  return parts.join(", ");
}

async function confirmRevert(ev) {
  const skip = skippedText(ev.skipped);
  if (!ev.paths.length) { showBanner(skip ? `Nothing to revert: ${skip}.` : "Nothing to revert."); return; }
  const n = ev.paths.length;
  const what = ev.rel || `${n} file${n === 1 ? "" : "s"}`;
  const lines = ["They are saved to git stash first; `git stash apply` brings them back."];
  if (ev.staged) lines.push("This includes staged changes.");
  if (skip) {
    const k = ev.skipped.untracked + ev.skipped.added + ev.skipped.renamed + ev.skipped.outside;
    lines.push(`${k} file${k === 1 ? " is" : "s are"} not included: ${skip}.`);
  }
  for (const d of ev.dirty) lines.push(`${d} has unsaved edits in roost; they are discarded too.`);
  // Cancel holds focus: Enter must never be the thing that discards work.
  const choice = await askChoice({
    title: `Discard changes to ${what}?`, lines, detailHtml: ev.detail_html, focus: "cancel",
    choices: [{ id: "discard", label: `Discard changes to ${what}` }],
  });
  if (choice === "discard") {
    send({ t: "Revert", rel: ev.rel ?? null, token: ev.token, discard_buffers: ev.dirty });
  }
}
```

In the event switch, beside `GitInit`:

```js
    case "RevertPlan":
      confirmRevert(ev);
      break;
    case "Reverted":
      if (ev.ok) { showBanner(ev.msg); break; }
      showError(ev.msg);
      // The dialog was built on something that no longer holds; show the
      // user what is true now instead of leaving them to right-click again.
      if (ev.stale) send({ t: "RevertPreview", rel: ev.rel ?? null });
      break;
```

- [ ] **Step 4: Run**

`cargo test --lib -- --test-threads=1 render::` → PASS.
`deno run -A tests/browser/dialogs.mjs` → ALL PASS (askMenu regressions).
`deno run -A tests/browser/touchfiles.mjs` and `download.mjs` → ALL PASS (the tree's file menu is unchanged).

- [ ] **Step 5: Revert-check** `change_rows_carry_their_xy` by removing `data-xy` from the format; confirm FAIL, restore.

- [ ] **Step 6: Commit**

```bash
git add src/render.rs static/dialog.js static/style.css static/app.js
git commit -m "Changes and Diff get a git menu with Revert, confirmed, instead of the file menu"
```

---

### Task 5: Browser test

**Files:**
- Create: `tests/browser/revert.mjs`
- Modify: `tests/browser/README.md` (run line after `nodtach.mjs`/`paste.mjs`; a revert-check paragraph under *Writing another one*)

**Interfaces:**
- Consumes: `fixture`, `freePort`, `openPage`, `profileDir`, `sleep`, `startBrowser`, `startRoost`, `until` from `harness.mjs`; `fixture()` already runs `git init` in the project.

- [ ] **Step 1: Write the test**

```js
//! Revert from the Changes pane and a Diff tab (#125).
//!
//! Right-clicking a change used to open the tree's file menu, Delete
//! included. This drives the real menus against a real repository and
//! asserts on the file on disk, never on the UI alone: a dialog that
//! confirmed and did nothing would pass any assertion about dialogs.
//!
//! Run: deno run -A tests/browser/revert.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };
const enc = new TextEncoder();
const git = async (dir, ...args) => {
  const o = await new Deno.Command("git", { args: ["-C", dir, ...args], stdout: "piped", stderr: "piped" }).output();
  if (!o.success) throw new Error(`git ${args.join(" ")}: ${new TextDecoder().decode(o.stderr)}`);
  return new TextDecoder().decode(o.stdout);
};
const read = (p) => Deno.readTextFile(p);

const fx = await fixture();
await git(fx.dir, "config", "user.email", "t@example.com");
await git(fx.dir, "config", "user.name", "t");
await Deno.writeFile(`${fx.dir}/a.txt`, enc.encode("base\n"));
await Deno.writeFile(`${fx.dir}/b.txt`, enc.encode("base\n"));
await git(fx.dir, "add", "-A");
await git(fx.dir, "commit", "-qm", "base");
await Deno.writeFile(`${fx.dir}/a.txt`, enc.encode("changed\n"));
await Deno.writeFile(`${fx.dir}/b.txt`, enc.encode("changed\n"));
await Deno.writeFile(`${fx.dir}/new.txt`, enc.encode("untracked\n"));

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;
try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { evalIn } = page;
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"), 30, "app.js");

  // Open Changes in pane 0 and wait for its rows.
  await evalIn(`send({ t: "OpenTab", pane: 0, tab: { k: "Changes" } })`);
  const row = (rel) => `document.querySelector('.content[data-kind="Changes"] a[data-rel=${JSON.stringify(rel)}]')`;
  ok(await until(() => evalIn(`!!${row("a.txt")} && !!${row("new.txt")}`), 20, "change rows"), "Changes lists the three files");

  const rightClick = (sel) => evalIn(`(() => { const el = ${sel}; const r = el.getBoundingClientRect();
    el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: r.x + 5, clientY: r.y + 5 })); })()`);
  const menuItems = () => evalIn(`[...document.querySelectorAll("#dlg-menu[open] .dlg-item")].map((b) => ({ t: b.firstChild.textContent, d: b.disabled, h: b.querySelector(".dlg-hint")?.textContent || "" }))`);
  const closeMenu = () => evalIn(`document.querySelector("#dlg-menu[open]")?.close()`);

  console.log("A. the menu on a change row");
  await rightClick(row("a.txt"));
  await until(() => evalIn(`!!document.querySelector("#dlg-menu[open]")`), 5, "menu");
  let items = await menuItems();
  ok(items.some((i) => i.t === "Revert…" && !i.d), `a change row offers Revert… (${JSON.stringify(items)})`);
  ok(!items.some((i) => /Delete|New file|Rename/.test(i.t)), "and none of the file menu");
  await closeMenu();

  console.log("\nB. an untracked row says why not");
  await rightClick(row("new.txt"));
  await until(() => evalIn(`!!document.querySelector("#dlg-menu[open]")`), 5, "menu");
  items = await menuItems();
  ok(items.some((i) => i.t === "Revert…" && i.d && i.h.includes("untracked")), `disabled with its reason (${JSON.stringify(items)})`);
  await closeMenu();

  const openConfirm = async (rel) => {
    await rightClick(row(rel));
    await until(() => evalIn(`!!document.querySelector("#dlg-menu[open] .dlg-item:not(:disabled)")`), 5, "menu");
    await evalIn(`document.querySelector("#dlg-menu[open] .dlg-item:not(:disabled)").click()`);
    return await until(() => evalIn(`!!document.querySelector("#dlg-choice[open]")`), 10, "confirmation");
  };

  console.log("\nC. cancel, and Enter, change nothing");
  ok(await openConfirm("a.txt"), "Revert… opens a confirmation");
  ok(await evalIn(`document.activeElement === document.querySelector("#dlg-choice .dlg-cancel")`), "focus is on Cancel");
  ok(await evalIn(`document.querySelector("#dlg-choice .dlg-detail").textContent.includes("changed")`), "it shows the diff");
  await evalIn(`document.activeElement.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })); document.activeElement.click();`);
  await sleep(800);
  ok((await read(`${fx.dir}/a.txt`)) === "changed\n", "Enter/Cancel left the file on disk untouched");

  console.log("\nD. confirm reverts on disk and names the stash");
  await openConfirm("a.txt");
  await evalIn(`document.querySelector('#dlg-choice .dlg-choice[data-choice="discard"]').click()`);
  ok(await until(async () => (await read(`${fx.dir}/a.txt`)) === "base\n", 15, "reverted"), "a.txt is back at HEAD on disk");
  ok((await read(`${fx.dir}/b.txt`)) === "changed\n", "b.txt, not selected, is untouched");
  ok((await git(fx.dir, "stash", "list")).includes("roost revert: 1 file"), "the change is in git stash");
  ok(await until(() => evalIn(`[...document.querySelectorAll(".error-banner")].some((b) => b.textContent.includes("stash@{0}"))`), 10, "banner"),
     "the banner names the stash");
  ok(await until(() => evalIn(`!${row("a.txt")}`), 15, "row gone"), "a.txt left the Changes list");

  console.log("\nE. a Diff tab gets the git menu, not the file menu");
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "Diff", rel: "b.txt" } })`);
  await until(() => evalIn(`!!document.querySelector('.content[data-kind="Diff"] .diffview')`), 15, "diff tab");
  await rightClick(`document.querySelector('.content[data-kind="Diff"] .diffview')`);
  await until(() => evalIn(`!!document.querySelector("#dlg-menu[open]")`), 5, "menu");
  items = await menuItems();
  ok(items.some((i) => i.t === "Revert…") && !items.some((i) => /New file/.test(i.t)), `Diff: ${JSON.stringify(items)}`);
  await closeMenu();

  console.log("\nF. the tree keeps its file menu");
  await evalIn(`send({ t: "OpenTab", pane: 0, tab: { k: "Tree" } })`);
  const treeRow = `document.querySelector('ul.tree a[data-rel="b.txt"]')`;
  await until(() => evalIn(`!!${treeRow}`), 15, "tree row");
  await rightClick(treeRow);
  await until(() => evalIn(`!!document.querySelector("#dlg-menu[open]")`), 5, "menu");
  items = await menuItems();
  ok(items.some((i) => i.t === "Delete") && !items.some((i) => i.t.startsWith("Revert")), `tree: ${JSON.stringify(items)}`);
  await closeMenu();
} finally {
  page?.close();
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nALL PASS" : `\n${fail} FAILED`);
Deno.exit(fail === 0 ? 0 : 1);
```

(If `OpenTab` with `{k:"Changes"}` or `{k:"Tree"}` opens a duplicate tab rather than activating an existing one, use the `find`/`ActivateTab` pattern from `reconnect.mjs` instead. Check `workspace::apply_layout` before choosing.)

- [ ] **Step 2: Run it**: `deno run -A tests/browser/revert.mjs` → ALL PASS.

- [ ] **Step 3: Revert-check, restoring from `cp` backups** (`ROOST_STATIC` serves `static/` live, so JS/CSS changes need no rebuild):

| Break | Must fail |
|---|---|
| `wireFragment` without the Changes/Diff branch | A (Delete present), E |
| `focus: "first"` in `confirmRevert` | C (focus and file untouched) |
| `revertBlock` returns `""` always | B |
| `send` of `Revert` removed | D |

Record the counts in the README paragraph.

- [ ] **Step 4: Full verification and commit**

Confirm the build is from this checkout (CLAUDE.md, *Build from one checkout*):
`grep -o '/home/[^"]*static' $(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/build/roost-*/out/assets_table.rs | sort -u`
Expected: only `…/projects/roost/static`.

Run: `cargo test -- --test-threads=1` (all ok) and `deno run -A tests/browser/revert.mjs`, `dialogs.mjs`, `touchfiles.mjs`, `download.mjs` (all ALL PASS).

```bash
git add tests/browser/revert.mjs tests/browser/README.md
git commit -m "Browser test: revert from Changes and Diff, asserted on disk"
```
