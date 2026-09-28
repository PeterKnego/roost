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
    /// Repository toplevel (absolute path) for running stash push from root.
    pub toplevel: std::path::PathBuf,
    /// Relative path from toplevel to project dir (with trailing slash).
    pub prefix: String,
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

    // Get the toplevel (repository root) for running stash from the root with root-relative paths.
    let toplevel = run(dir, &["rev-parse", "--show-toplevel"])
        .map_err(|e| format!("Could not read git toplevel: {e}. {NOTHING}"))?;
    let toplevel = std::path::PathBuf::from(toplevel.strip_suffix('\n').unwrap_or(&toplevel));

    // Get the prefix (relative path from repo root to current dir) to filter paths.
    // Use strip_suffix to avoid trimming leading whitespace (confinement hole).
    let prefix = run(dir, &["rev-parse", "--show-prefix"])
        .map_err(|e| format!("Could not read git prefix: {e}. {NOTHING}"))?;
    let prefix_str = prefix.strip_suffix('\n').unwrap_or(&prefix);

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

        // Strip the prefix to get the path relative to the current directory.
        let local_path = if prefix_str.is_empty() {
            path.to_string()
        } else if let Some(stripped) = path.strip_prefix(prefix_str) {
            stripped.to_string()
        } else {
            // Path is outside the current directory.
            // Only count as outside if no rel is given (we're not filtering for a specific file).
            if rel.is_none() {
                skipped.outside += 1;
            }
            continue;
        };

        // Filter by rel (project-relative path).
        if rel.is_some_and(|r| r != &local_path) {
            continue;
        }
        if !inside(&local_path) {
            skipped.outside += 1;
            continue;
        }
        match entry {
            Entry::Untracked { .. } => skipped.untracked += 1,
            Entry::Renamed { .. } => skipped.renamed += 1,
            Entry::Ordinary { xy, path: _ } => {
                let x = xy.chars().next().unwrap_or('.');
                if x == 'A' {
                    skipped.added += 1;
                } else {
                    staged |= x != '.';
                    paths.push(PlanPath { path: local_path, xy });
                }
            }
            Entry::Unmerged { .. } => unreachable!("returned above"),
        }
    }
    // `staged` is computed from `paths` only: the dialog's "includes staged changes"
    // is a statement about what it reverts, not about entries it leaves alone.
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
    Ok(Plan { paths, staged, skipped, diff, token, toplevel, prefix: prefix_str.to_string() })
}

/// The revert: one git command that saves the paths to a stash entry and
/// resets them to HEAD. `Ok` carries the message the user sees.
pub fn execute(_dir: &Path, plan: &Plan, run: GitRunner) -> Result<String, String> {
    let n = plan.paths.len();
    let label = format!("roost revert: {n} file{}", if n == 1 { "" } else { "s" });

    // Read before the push, and a failure refuses: without a trustworthy
    // "before" there is no way to tell afterwards whether a failed push left
    // an entry, and guessing "" would name the user's older stash as ours.
    let before = run(&plan.toplevel, &["stash", "list", "-1", "--format=%H"])
        .map_err(|e| format!("Could not read the stash list: {e}. {NOTHING}"))?;

    // Build root-relative paths for stash push (prefix + project-relative path).
    let mut root_paths = Vec::new();
    for p in &plan.paths {
        root_paths.push(format!("{}{}", plan.prefix, p.path));
    }

    // Run stash push from the toplevel with root-relative paths.
    let mut args = vec!["--literal-pathspecs", "stash", "push", "-m", label.as_str(), "--"];
    args.extend(root_paths.iter().map(|p| p.as_str()));
    let out = run(&plan.toplevel, &args).map_err(|e| {
        let first = e.lines().next().unwrap_or("").trim().to_string();
        // Three outcomes, never two: the after-check can fail, and "could not
        // look" is not "no entry" (CLAUDE.md).
        match run(&plan.toplevel, &["stash", "list", "-1", "--format=%H"]) {
            Ok(after) if after.trim().is_empty() => format!("git refused: {first}. {NOTHING}"),
            Ok(after) if after == before => format!("git refused: {first}. {NOTHING}"),
            Ok(_) => format!(
                "git refused: {first}. Nothing was reverted, but a stash entry \"{label}\" was created; `git stash drop` removes it."
            ),
            Err(_) => format!(
                "git refused: {first}. Nothing was reverted; whether a stash entry was left could not be checked; see `git stash list`."
            ),
        }
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

    /// Revert-checked: treating `x == 'A'` as eligible (allowing added files)
    /// fails this test.
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
        // `staged` describes only what is reverted: the staged added file and
        // the rename are skipped, so they must not make the dialog say
        // "includes staged changes".
        assert!(!p.staged, "only skipped entries are staged here");
    }

    /// Porcelain paths are relative to git's cwd, so a nested project sees its
    /// parent's changes as `../x`. The fixture has to be a real nested
    /// project, or a plan that took `../x` could not fail this.
    ///
    /// Porcelain paths are relative to git's cwd, so a nested project sees its
    /// parent's changes as `../x`. The fixture has to be a real nested
    /// project, or a plan that took `../x` could not fail this.
    ///
    /// Revert-checked: `inside` returning `true` does NOT fail this test.
    /// The `strip_prefix` check already filters outside paths, making the
    /// `inside` check redundant for regular subdirectories.
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
    ///
    /// Revert-checked: drops `--literal-pathspecs` from both arg lists fails
    /// this test.
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

    /// Finding 1: The pre-check must not fold "could not read" into "empty".
    /// If the pre-check fails, we cannot trustworthy compare afterwards, so refuse before pushing.
    /// Runner fails on all "list" commands (simulating read failure); real git otherwise.
    /// Assert error starts with "Could not read the stash list:", ends with "Nothing was reverted.",
    /// file unchanged, and no stash was created.
    ///
    /// Revert-checked: restore .unwrap_or_default() on pre-check fails this test.
    #[test]
    fn a_failed_stash_precheck_refuses_before_pushing() {
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let p = build_plan(d.path(), None, &real).unwrap();
        let list_fails = |dir: &Path, a: &[&str]| {
            if a.contains(&"list") {
                Err("fatal: could not open stash file".to_string())
            } else {
                real(dir, a)
            }
        };
        let err = execute(d.path(), &p, &list_fails).unwrap_err();
        assert!(err.starts_with("Could not read the stash list:"), "{err}");
        assert!(err.ends_with("Nothing was reverted."), "{err}");
        // File should be unchanged because push never ran
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "a2\n");
        // No stash created because push never ran
        assert_eq!(git(d.path(), &["stash", "list"]), "");
    }

    /// Finding 1: The post-check must distinguish "could not read" from "no entry".
    /// Repo has a change and NO prior stash. Push fails (simulated), but post-check
    /// cannot read stash list (also fails). The error must say "could not be checked",
    /// not "Nothing was reverted." alone.
    ///
    /// Revert-checked: return NOTHING in Err(_) arm instead of "could not be checked" fails this.
    #[test]
    fn a_failed_stash_postcheck_says_it_could_not_tell() {
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let p = build_plan(d.path(), None, &real).unwrap();
        // Pre-check succeeds (no stash), but post-check fails
        let list_count = std::cell::Cell::new(0u32);
        let pre_ok_post_fails = |dir: &Path, a: &[&str]| {
            if a.contains(&"list") {
                let count = list_count.get();
                list_count.set(count + 1);
                if count == 0 {
                    // First call (pre-check) succeeds
                    real(dir, a)
                } else {
                    // Second call (post-check) fails
                    Err("fatal: could not open stash file".to_string())
                }
            } else if a.contains(&"push") {
                Err("fatal: simulated".to_string())
            } else {
                real(dir, a)
            }
        };
        let err = execute(d.path(), &p, &pre_ok_post_fails).unwrap_err();
        assert!(err.contains("could not be checked"), "{err}");
        assert!(err.contains("Nothing was reverted"), "{err}");
    }

    /// Revert-checked: dropping the `--` + paths from stash push (stashes everything
    /// instead of just selected files) fails this test (b.txt assertion fails).
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

    /// Important (controller addition): stash entry detection via before/after hash comparison.
    /// Runner executes the real stash push, then returns an error to simulate failure AFTER saving.
    /// The message must report that an entry was created and be discoverable via stash drop.
    ///
    /// Revert-checked: making the after-check report equal hashes (after_stash == before_stash)
    /// fails this test. Output: panicked at assertion expecting "was created" in message, but got:
    /// "git refused: fatal: simulated error after stash. Nothing was reverted."
    #[test]
    fn a_push_that_saved_then_failed_names_the_stash_entry() {
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let p = build_plan(d.path(), None, &real).unwrap();
        let stash_then_fail = |dir: &Path, a: &[&str]| {
            if a.contains(&"push") {
                // Run the real stash push first (which succeeds and saves)
                let _ = real(dir, a);
                // Then return an error to simulate a failure after save
                Err("fatal: simulated error after stash".to_string())
            } else {
                real(dir, a)
            }
        };
        let msg = execute(d.path(), &p, &stash_then_fail).unwrap_err();
        assert!(msg.contains("was created"), "message must report stash entry created: {msg}");
        let list = git(d.path(), &["stash", "list"]);
        assert_eq!(list.lines().count(), 1, "exactly one stash entry should exist: {list}");
    }

    /// Important (controller addition): stash entry detection must not blame an older stash.
    /// The repo has a pre-existing stash with subject "roost revert: 1 file".
    /// When stash push fails WITHOUT actually running (no new entry), the error message
    /// must NOT mention "created".
    ///
    /// Revert-checked: using label matching (after.trim().ends_with(&label)) instead of hash
    /// comparison fails this test. Output: panicked at "error must end cleanly" assertion, got:
    /// "git refused: fatal: simulated error before stash. Nothing was reverted, but a stash entry
    /// \"roost revert: 1 file\" was created; `git stash drop` removes it."
    #[test]
    fn a_push_that_failed_without_saving_does_not_blame_an_older_stash() {
        let d = repo(&[("a.txt", "a\n")]);
        // Create a pre-existing stash with the same label format
        std::fs::write(d.path().join("a.txt"), "old\n").unwrap();
        git(d.path(), &["stash", "push", "-q", "-m", "roost revert: 1 file"]);
        // Make a new change for the plan
        std::fs::write(d.path().join("a.txt"), "new\n").unwrap();
        let p = build_plan(d.path(), None, &real).unwrap();
        let fail_without_running = |dir: &Path, a: &[&str]| {
            if a.contains(&"push") {
                // Return error WITHOUT running stash push (no new entry created)
                Err("fatal: simulated error before stash".to_string())
            } else {
                real(dir, a)
            }
        };
        let msg = execute(d.path(), &p, &fail_without_running).unwrap_err();
        // Must end with "Nothing was reverted." (no extra message about a stash)
        assert!(msg.ends_with("Nothing was reverted."), "error must end cleanly: {msg}");
        // Must NOT mention "created" (don't blame the old stash)
        assert!(!msg.contains("created"), "message must not mention stash creation: {msg}");
        // The stash list should still have exactly 1 entry (the old one, not a new one)
        let list = git(d.path(), &["stash", "list"]);
        assert_eq!(list.lines().count(), 1, "only the pre-existing stash should exist: {list}");
    }

    /// Revert-checked: `.unwrap_or_default()` on the status error (returning empty string
    /// instead of propagating the error) fails this test (plan would build with empty status).
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

    /// Revert-checked: deleting the `IN_PROGRESS` loop alone does NOT fail this test
    /// (caught by unmerged entry check). Deleting the unmerged-entry refusal alone also
    /// does NOT fail it (caught by IN_PROGRESS loop, which detects MERGE_HEAD). Deleting
    /// BOTH the loop and the unmerged-entry check fails this test.
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
    ///
    /// Revert-checked: deleting the `IN_PROGRESS` loop fails this test.
    #[test]
    fn merge_head_alone_refuses() {
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let head = git(d.path(), &["rev-parse", "HEAD"]);
        std::fs::write(d.path().join(".git/MERGE_HEAD"), head).unwrap();
        let err = build_plan(d.path(), None, &real).unwrap_err();
        assert!(err.contains("A merge is in progress"), "{err}");
    }

    /// Revert-checked: hashing only the path list, not the diff, fails this test.
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
    ///
    /// Revert-checked: deleting the `Saved working directory` check fails this
    /// test.
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

    /// Review Focus 2. Non-UTF8 diff handling.
    ///
    /// Revert-checked: mapping the diff error to empty string (returning Ok("") instead of
    /// propagating the error) fails this test (plan would build with empty diff).
    #[test]
    fn a_non_utf8_diff_refuses() {
        let d = repo(&[("l.txt", "caf\n")]);
        std::fs::write(d.path().join("l.txt"), b"caf\xe9\n").unwrap();
        let err = build_plan(d.path(), None, &real).unwrap_err();
        assert!(err.starts_with("Could not read the diff:") && err.contains("not valid UTF-8"), "{err}");
    }

    /// Review Focus 4. An empty identity stands in for a host with none.
    ///
    /// Revert-checked: treat stash Err as Ok fails this test.
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
        assert!(err.starts_with("git refused: "), "{err}");
        assert!(err.ends_with("Nothing was reverted."), "{err}");
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "a2\n");
    }

    /// Important 1: `prefix.trim()` also trims LEADING whitespace (confinement hole).
    /// A project dir named " sub" (space-prefixed) yields prefix " sub/" → trimmed "sub/"
    /// → a sibling `sub/` might have its files stripped and treated as inside.
    /// Test: repo with dirs " sub" and "sub" with different files; plan built from " sub"
    /// must include only its own file and count the sibling as outside.
    /// Files use different names so a mismatch is obvious: " sub/mine.txt" vs "sub/theirs.txt".
    ///
    /// Revert-checked: restoring .trim() (using it instead of strip_suffix) fails
    /// this test.
    #[test]
    fn prefix_with_leading_space_does_not_match_sibling() {
        let d = repo(&[(" sub/mine.txt", "m\n"), ("sub/theirs.txt", "t\n"), ("subway/x", "x\n")]);
        std::fs::write(d.path().join(" sub/mine.txt"), "m2\n").unwrap();
        std::fs::write(d.path().join("sub/theirs.txt"), "t2\n").unwrap();
        std::fs::write(d.path().join("subway/x"), "x2\n").unwrap();
        let p = build_plan(&d.path().join(" sub"), None, &real).unwrap();
        // With strip_suffix('\n'), prefix is " sub/", so " sub/mine.txt" matches and is
        // included as "mine.txt". With trim(), prefix is "sub/" (space trimmed), so " sub/mine.txt"
        // doesn't match, but "sub/theirs.txt" WOULD match and be wrongly included!
        assert_eq!(p.paths, vec![PlanPath { path: "mine.txt".into(), xy: ".M".into() }]);
        // Both the sibling "sub" and the string-prefix-lookalike "subway" are outside.
        assert_eq!(p.skipped.outside, 2, "should skip both 'sub/theirs.txt' and 'subway/x'");
    }

    /// Critical 1: from a nested project dir, `git --literal-pathspecs stash push -- <path>`
    /// saves the stash and then fails with "pathspec did not match", reverting nothing and
    /// leaving an orphan stash. Fix: run stash push from the toplevel with root-relative paths.
    /// Test: nested-project execute test asserting (a) the nested file is back at HEAD on disk,
    /// (b) exactly one `git stash list` entry, (c) the parent's changed file is untouched.
    ///
    /// Revert-checked: running stash from the project dir (using local paths) fails this test.
    #[test]
    fn a_nested_project_execute_reverts_from_toplevel() {
        let d = repo(&[("top.txt", "t\n"), ("sub/in.txt", "i\n")]);
        std::fs::write(d.path().join("top.txt"), "t2\n").unwrap();
        std::fs::write(d.path().join("sub/in.txt"), "i2\n").unwrap();
        let p = build_plan(&d.path().join("sub"), None, &real).unwrap();
        let msg = execute(&d.path().join("sub"), &p, &real).unwrap();
        assert!(msg.contains("stash@{0}") && msg.contains("roost revert: 1 file"), "{msg}");
        assert_eq!(std::fs::read_to_string(d.path().join("sub/in.txt")).unwrap(), "i\n", "nested file reverted");
        assert_eq!(std::fs::read_to_string(d.path().join("top.txt")).unwrap(), "t2\n", "parent file untouched");
        let list = git(d.path(), &["stash", "list"]);
        assert_eq!(list.lines().count(), 1, "{list}");
    }

    /// Important 2: `outside` is counted before the `rel` filter.
    /// For a single-file revert in a nested project every unrelated change elsewhere
    /// in the repo is counted as "not included: N outside".
    /// The brief's order is: rel filter first, then count. Compare `rel` against
    /// the project-relative (stripped) path; a root path that is outside the project
    /// and a `rel` is given → skip without counting.
    /// Test: nested fixture, `build_plan(sub, Some("in.txt"))` asserts `skipped.outside == 0`.
    ///
    /// Revert-checked: moving outside count before rel filter fails this test.
    #[test]
    fn rel_filter_does_not_count_unselected_outside_paths() {
        let d = repo(&[("top.txt", "t\n"), ("sub/in.txt", "i\n")]);
        std::fs::write(d.path().join("top.txt"), "t2\n").unwrap();
        std::fs::write(d.path().join("sub/in.txt"), "i2\n").unwrap();
        let p = build_plan(&d.path().join("sub"), Some("in.txt"), &real).unwrap();
        assert_eq!(p.paths, vec![PlanPath { path: "in.txt".into(), xy: ".M".into() }]);
        assert_eq!(p.skipped.outside, 0, "unselected outside paths should not be counted");
    }

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
                tab: crate::proto::Tab::File { rel: "a.txt".into(), mode: crate::proto::Mode::Edit },
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
    ///
    /// Parks on `push` specifically, not on every arg list containing
    /// `stash`: `execute` also runs a `stash list` pre-check before the push,
    /// so parking on every stash-bearing call would need two releases from
    /// one `go.send`, and deadlock instead of failing when the lock really is
    /// held.
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
                if a.contains(&"push") { let _ = wait.lock().unwrap().recv(); }
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
}
