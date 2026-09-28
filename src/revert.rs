//! Reverting what the Changes pane and a Diff tab show (#125).
//!
//! The first place roost overwrites file content with git, so everything here
//! is shaped by CLAUDE.md's "destruction requires positive evidence". A plan
//! is built from a fresh `git status -z` and `git diff HEAD`, and a token over
//! it is what the dialog was shown; the revert rebuilds the plan and proceeds
//! only if the token still matches, so a change made after the user looked is
//! never discarded unseen. The revert itself is one `git stash push`, which
//! saves the entry first and then resets the paths, so it can stop partway
//! with some files already reverted and the entry their only copy. What
//! happened is read from the stash list before and after, never from git's
//! wording (translated) or its exit status (silent on how far it got), and
//! a push stopped partway is reported as exactly that.
//!
//! A revert never deletes: only paths with a version in HEAD are taken, which
//! is why untracked, added, and renamed entries are counted and left alone.
use crate::gitio::{parse_status_z, Entry};
use crate::worktree::GitRunner;
use std::path::Path;

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

pub fn build_plan(dir: &Path, rel: Option<&str>, run: GitRunner) -> Result<Plan, String> {
    refuse_in_progress(dir, run)?;

    // The stash runs from the toplevel with root-relative paths; see `execute`.
    let toplevel = run(dir, &["rev-parse", "--show-toplevel"])
        .map_err(|e| format!("Could not read git toplevel: {e}. {NOTHING}"))?;
    let toplevel = std::path::PathBuf::from(toplevel.strip_suffix('\n').unwrap_or(&toplevel));

    // `strip_suffix`, not `trim`: a project named " sub" has the prefix
    // " sub/", and trimming it would confine the plan to a sibling `sub/`.
    let prefix = run(dir, &["rev-parse", "--show-prefix"])
        .map_err(|e| format!("Could not read git prefix: {e}. {NOTHING}"))?;
    let prefix_str = prefix.strip_suffix('\n').unwrap_or(&prefix);

    let status = run(dir, &["status", "--porcelain=v2", "-z"])
        .map_err(|e| format!("Could not read git status: {e}. {NOTHING}"))?;
    let mut paths = Vec::new();
    let mut skipped = Skipped::default();
    let mut staged = false;
    let mut matched = false;
    for entry in parse_status_z(&status) {
        let path = match &entry {
            Entry::Ordinary { path, .. } | Entry::Renamed { path, .. } | Entry::Unmerged { path, .. } | Entry::Untracked { path } => path,
        };
        if matches!(entry, Entry::Unmerged { .. }) {
            // Checked before `rel` filters: a conflict anywhere means a merge
            // is being resolved, whichever file was right-clicked.
            return Err(format!("A merge is in progress; finish or abort it first. {NOTHING}"));
        }

        // With `-z` git names paths from the repository root, whatever its
        // cwd, so stripping the project's prefix is the confinement: a path
        // without it is outside the project. Lexical on purpose: git never
        // lists a path through a symlinked directory, and a deleted file has
        // no parent for `safe_resolve_parent` to canonicalise.
        let local_path = if prefix_str.is_empty() {
            path.to_string()
        } else if let Some(stripped) = path.strip_prefix(prefix_str) {
            stripped.to_string()
        } else {
            // Counted only for "all": a single-file dialog has no use for
            // how much else changed elsewhere in the repository.
            if rel.is_none() {
                skipped.outside += 1;
            }
            continue;
        };

        if rel.is_some_and(|r| r != local_path) {
            continue;
        }
        matched = true;
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
    // "Matched nothing" is not "nothing to revert": the name the client sent
    // is not one git lists, so the list it came from is stale or mismatched.
    if let (Some(r), false) = (rel, matched) {
        return Err(format!("{r} is not in git status; refresh the Changes list. {NOTHING}"));
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

/// The top stash entry as `hash TAB subject`, or "" for an empty list.
fn top_stash(dir: &Path, run: GitRunner) -> Result<String, String> {
    run(dir, &["stash", "list", "-1", "--format=%H%x09%s"]).map(|s| s.trim_end_matches('\n').to_string())
}

/// What the stash list says happened, which is the only evidence read:
/// git's own wording is translated, and its exit status says nothing about
/// how far a push got before failing.
enum Saved {
    Nothing,
    Ours,
    /// A new top entry this revert did not make: a stash taken concurrently
    /// hides whether ours saved anything.
    SomeoneElses,
}

/// "Ours" needs both halves: a new hash alone could be a concurrent stash,
/// and the label alone could be an older roost revert.
fn saved(before: &str, after: &str, label: &str) -> Saved {
    if after == before {
        return Saved::Nothing;
    }
    let subject = after.split_once('\t').map_or("", |(_, s)| s);
    if subject.ends_with(label) { Saved::Ours } else { Saved::SomeoneElses }
}

const CHANGED: &str = "The stash changed while this ran, so whether this revert saved anything could not be told \
                       — see `git stash list` before dropping anything.";

/// The revert: one `git stash push` that saves the paths to a stash entry and
/// resets them to HEAD. `Ok` carries the message the user sees.
///
/// `push` runs only the push, and must not kill it: git saves the entry
/// first and resets paths one step at a time, so a push stopped partway has
/// already overwritten some files, and the entry is their only copy. `run`
/// reads the stash list around it.
pub fn execute(plan: &Plan, run: GitRunner, push: GitRunner) -> Result<String, String> {
    let n = plan.paths.len();
    let label = format!("roost revert: {n} file{}", if n == 1 { "" } else { "s" });

    // Read before the push, and a failure refuses: without a trustworthy
    // "before" there is no way to tell afterwards whether the push saved,
    // and guessing "" would name the user's older stash as ours.
    let before = top_stash(&plan.toplevel, run)
        .map_err(|e| format!("Could not read the stash list: {e}. {NOTHING}"))?;

    // Root-relative, from the toplevel: `--literal-pathspecs stash push` run
    // in a subdirectory saves and then fails to match its own pathspec.
    let root_paths: Vec<String> = plan.paths.iter().map(|p| format!("{}{}", plan.prefix, p.path)).collect();
    let mut args = vec!["--literal-pathspecs", "stash", "push", "-m", label.as_str(), "--"];
    args.extend(root_paths.iter().map(|p| p.as_str()));
    let pushed = push(&plan.toplevel, &args);
    let after = top_stash(&plan.toplevel, run);

    match pushed {
        Err(e) => {
            let first = e.lines().next().unwrap_or("").trim().to_string();
            // Four outcomes: the after-check can fail, and "could not look"
            // is not "no entry" (CLAUDE.md).
            Err(match after.as_deref().map(|a| saved(&before, a, &label)) {
                Ok(Saved::Nothing) => format!("git refused: {first}. {NOTHING}"),
                Ok(Saved::Ours) => format!(
                    "git stopped partway after saving the changes as stash@{{0}} (\"{label}\"): some files may \
                     already be reverted — check `git status`. `git stash apply` brings them back."
                ),
                Ok(Saved::SomeoneElses) => format!("git refused: {first}. {CHANGED}"),
                Err(_) => format!(
                    "git refused: {first}. Whether a stash entry was left could not be checked — see \
                     `git stash list` before dropping anything."
                ),
            })
        }
        Ok(_) => match after.as_deref().map(|a| saved(&before, a, &label)) {
            Ok(Saved::Ours) => Ok(format!(
                "Reverted {n} file{}; saved as stash@{{0}} (\"{label}\"). `git stash apply` brings them back.",
                if n == 1 { "" } else { "s" }
            )),
            // Exit 0 with nothing saved: "No local changes to save", the
            // paths went clean after the plan was built.
            Ok(Saved::Nothing) => Err(format!("These changes were already gone. {NOTHING}")),
            Ok(Saved::SomeoneElses) => Err(format!("git did not report an error. {CHANGED}")),
            Err(_) => Err(
                "git did not report an error, but whether it saved anything could not be checked — see \
                 `git stash list` and `git status`."
                    .to_string(),
            ),
        },
    }
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
    discard: Vec<String>, run: GitRunner, push: GitRunner,
) {
    let dir = project_dir(hub);
    let refuse = |msg: String, stale: bool| {
        // A git refusal is logged as well as shown: the dialog is gone once
        // the user dismisses it, and this is the first place roost overwrites
        // files with git. Stale is not logged; it is the user's own race.
        if !stale {
            eprintln!("roost: revert in {}: {msg}", dir.display());
        }
        Hub::lock(hub).send_to(id, &Event::Reverted { rel: rel.clone(), ok: false, msg, stale });
    };
    let plan = match build_plan(&dir, rel.as_deref(), run) {
        Ok(p) => p,
        Err(msg) => return refuse(msg, false),
    };
    if plan.token != token || plan.paths.is_empty() {
        return refuse("These changes changed since you looked; review them again.".into(), true);
    }
    // Among the reverted paths by construction, so the close below never
    // reaches a buffer outside the revert however `discard` was built.
    let dirty = dirty_among(hub, &plan.paths);
    if let Some(p) = dirty.iter().find(|p| !discard.contains(p)) {
        return refuse(format!("{p} gained unsaved edits; review again."), true);
    }
    match execute(&plan, run, push) {
        Err(msg) => refuse(msg, false),
        Ok(msg) => {
            let mut h = Hub::lock(hub);
            // Only buffers that were dirty at the check: a clean one needs
            // no discarding, and one that went dirty after the check was
            // never named, so it keeps its edits against the changed file,
            // the existing conflict path.
            for rel in dirty.iter().filter(|r| discard.contains(r)) {
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

    /// With `-z`, porcelain paths are relative to the repository root, so a
    /// nested project sees its parent's change as `top.txt`, not `../top.txt`,
    /// and only the prefix strip keeps it out. The fixture has to be a real
    /// nested project, or a plan that took the parent's file could not fail
    /// this.
    ///
    /// Revert-checked: taking a path that lacks the prefix as-is, instead of
    /// counting it outside, fails this test.
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
        execute(&p, &real, &real).unwrap();
        assert_eq!(std::fs::read_to_string(d.path().join("a*")).unwrap(), "1\n");
        assert_eq!(std::fs::read_to_string(d.path().join("ab")).unwrap(), "2\n", "ab was not selected");
    }

    /// The pre-check must not fold "could not read" into "empty": without a
    /// trustworthy "before" nothing afterwards can be told, so it refuses
    /// before pushing.
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
        let err = execute(&p, &list_fails, &list_fails).unwrap_err();
        assert!(err.starts_with("Could not read the stash list:"), "{err}");
        assert!(err.ends_with("Nothing was reverted."), "{err}");
        // File should be unchanged because push never ran
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "a2\n");
        // No stash created because push never ran
        assert_eq!(git(d.path(), &["stash", "list"]), "");
    }

    /// The after-check can fail too, and "could not look" is not "no entry":
    /// the message must say it could not tell, and must not claim nothing was
    /// reverted, which it cannot know.
    ///
    /// Revert-checked: the `Err(_)` arm answering "git refused: {first}.
    /// Nothing was reverted." fails this with exactly that message.
    #[test]
    fn a_failed_stash_postcheck_says_it_could_not_tell() {
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let p = build_plan(d.path(), None, &real).unwrap();
        let lists = std::cell::Cell::new(0u32);
        let second_list_fails = |dir: &Path, a: &[&str]| {
            lists.set(lists.get() + 1);
            if lists.get() == 1 { real(dir, a) } else { Err("fatal: could not open stash file".to_string()) }
        };
        let push_fails = |_: &Path, _: &[&str]| Err("fatal: simulated".to_string());
        let err = execute(&p, &second_list_fails, &push_fails).unwrap_err();
        assert!(err.contains("could not be checked") && err.contains("git stash list"), "{err}");
        assert!(!err.contains("Nothing was reverted") && !err.contains("stash drop"), "{err}");
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
        let msg = execute(&p, &real, &real).unwrap();
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

    /// An older roost revert on top carries the same label, so the label alone
    /// would claim it: a push that saved nothing must not be reported as one
    /// that stopped partway.
    ///
    /// Revert-checked: `saved` ignoring the hash (label alone is ours) fails
    /// this with `error must end cleanly: git stopped partway after saving the
    /// changes as stash@{0} ("roost revert: 1 file") ...`.
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
        let msg = execute(&p, &real, &fail_without_running).unwrap_err();
        // Must end with "Nothing was reverted." (no extra message about a stash)
        assert!(msg.ends_with("Nothing was reverted."), "error must end cleanly: {msg}");
        assert!(!msg.contains("stopped partway"), "{msg}");
        // The stash list should still have exactly 1 entry (the old one, not a new one)
        let list = git(d.path(), &["stash", "list"]);
        assert_eq!(list.lines().count(), 1, "only the pre-existing stash should exist: {list}");
    }

    fn real_push(d: &Path, a: &[&str]) -> Result<String, String> { crate::worktree::real_git_unbounded(d, a) }

    /// Puts a directory's mode back even when the test panics, or TempDir
    /// cannot delete what is inside it.
    struct Writable<'a>(&'a Path);
    impl Drop for Writable<'_> {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    /// `stash push` saves the entry first and resets paths afterwards, so it
    /// can stop partway: here `d/` is read-only, git saves, reverts `a.txt`
    /// and `z.txt`, fails on `d/b.txt`, and exits 1. The stash then holds the
    /// only copy of what was reverted, so a message saying nothing was
    /// reverted and offering `git stash drop` is an instruction to lose work.
    ///
    /// Revert-checked: the partway arm emitting the former "Nothing was
    /// reverted, but a stash entry ... `git stash drop` removes it." fails
    /// this with `git refused: warning: unable to unlink 'd/b.txt':
    /// Permission denied. Nothing was reverted, but a stash entry "roost
    /// revert: 3 files" was created; ...`.
    #[test]
    fn a_push_that_stops_partway_says_so_and_never_suggests_drop() {
        use std::os::unix::fs::PermissionsExt;
        let d = repo(&[("a.txt", "a\n"), ("d/b.txt", "b\n"), ("z.txt", "z\n")]);
        for (f, body) in [("a.txt", "a2\n"), ("d/b.txt", "b2\n"), ("z.txt", "z2\n")] {
            std::fs::write(d.path().join(f), body).unwrap();
        }
        let p = build_plan(d.path(), None, &real).unwrap();
        let sub = d.path().join("d");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        let _restore = Writable(&sub);
        if std::fs::write(sub.join("probe"), "").is_ok() {
            eprintln!("skipped: a read-only directory does not stop this user (root?)");
            return;
        }
        let msg = execute(&p, &real, &real_push).unwrap_err();
        assert!(msg.contains("stopped partway") && msg.contains("git stash apply"), "{msg}");
        assert!(!msg.contains("Nothing was reverted") && !msg.contains("drop"), "{msg}");
        let list = git(d.path(), &["stash", "list"]);
        assert_eq!(list.lines().count(), 1, "{list}");
    }

    /// The same, from a runner that saves for real and then reports failure,
    /// so it does not depend on permissions.
    ///
    /// Revert-checked: the same break fails this with `git refused: error:
    /// simulated failure after saving. Nothing was reverted, but a stash
    /// entry "roost revert: 1 file" was created; ...`.
    #[test]
    fn a_push_that_saved_then_failed_names_the_stash_entry() {
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let p = build_plan(d.path(), None, &real).unwrap();
        let stash_then_fail = |dir: &Path, a: &[&str]| {
            let _ = real(dir, a);
            Err("error: simulated failure after saving".to_string())
        };
        let msg = execute(&p, &real, &stash_then_fail).unwrap_err();
        assert!(msg.contains("stopped partway") && msg.contains("roost revert: 1 file"), "{msg}");
        assert!(msg.contains("git stash apply"), "{msg}");
        assert!(!msg.contains("Nothing was reverted") && !msg.contains("drop"), "{msg}");
        let list = git(d.path(), &["stash", "list"]);
        assert_eq!(list.lines().count(), 1, "{list}");
    }

    /// A new top entry that is not ours is someone else's stash, made while
    /// the push ran: whether ours saved anything cannot be told.
    ///
    /// Revert-checked: `saved` ignoring the label (a new hash alone is ours)
    /// fails this with `git stopped partway after saving the changes as
    /// stash@{0} ("roost revert: 1 file") ...`.
    #[test]
    fn a_stash_made_by_someone_else_during_a_failed_push_is_not_claimed() {
        let d = repo(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let p = build_plan(d.path(), Some("a.txt"), &real).unwrap();
        std::fs::write(d.path().join("b.txt"), "b2\n").unwrap();
        let other_then_fail = |dir: &Path, _: &[&str]| {
            real(dir, &["stash", "push", "-q", "-m", "other", "--", "b.txt"]).unwrap();
            Err("error: simulated".to_string())
        };
        let msg = execute(&p, &real, &other_then_fail).unwrap_err();
        assert!(msg.contains("stash changed while this ran"), "{msg}");
        assert!(msg.starts_with("git refused: error: simulated."), "{msg}");
        // "before dropping anything" is a warning; `stash drop` is the advice
        // that loses work.
        assert!(!msg.contains("stash drop") && !msg.contains("stopped partway"), "{msg}");
    }

    /// Git translates "Saved working directory", so reading success from it
    /// reports a real revert as "already gone" under another locale. The
    /// stash list is the evidence; the runner stands in for a translated git.
    ///
    /// Revert-checked: reinstating `out.contains("Saved working directory")`
    /// fails this with `called Result::unwrap() on an Err value: "These
    /// changes were already gone. Nothing was reverted."`.
    #[test]
    fn success_is_read_from_the_stash_list_not_from_git_s_wording() {
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let p = build_plan(d.path(), None, &real).unwrap();
        let translated = |dir: &Path, a: &[&str]| {
            real(dir, a).map(|_| "Arbeitsverzeichnis und Index-Status gespeichert\n".to_string())
        };
        let msg = execute(&p, &real, &translated).unwrap();
        assert!(msg.starts_with("Reverted 1 file; saved as stash@{0}"), "{msg}");
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "a\n");
    }

    /// Exit 0 with someone else's entry on top: git said nothing was wrong,
    /// but the entry is not evidence that this revert saved anything.
    ///
    /// Revert-checked: `saved` ignoring the label fails this with `called
    /// Result::unwrap_err() on an Ok value: "Reverted 1 file; saved as
    /// stash@{0} ("roost revert: 1 file")..."`.
    #[test]
    fn a_stash_made_by_someone_else_during_a_clean_push_is_not_claimed() {
        let d = repo(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let p = build_plan(d.path(), Some("a.txt"), &real).unwrap();
        std::fs::write(d.path().join("b.txt"), "b2\n").unwrap();
        let other_ok = |dir: &Path, _: &[&str]| real(dir, &["stash", "push", "-q", "-m", "other", "--", "b.txt"]);
        let msg = execute(&p, &real, &other_ok).unwrap_err();
        assert!(msg.starts_with("git did not report an error."), "{msg}");
        assert!(msg.contains("stash changed while this ran") && !msg.contains("stash drop"), "{msg}");
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

    /// `stash push` exits 0 with "No local changes to save" when the paths
    /// went clean after the plan was built.
    ///
    /// Revert-checked: making `saved` answer `Ours` unconditionally fails this
    /// with `called Result::unwrap_err() on an Ok value: "Reverted 1 file;
    /// saved as stash@{0} ..."`.
    #[test]
    fn execute_refuses_when_git_saved_nothing() {
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let p = build_plan(d.path(), None, &real).unwrap();
        std::fs::write(d.path().join("a.txt"), "a\n").unwrap(); // clean again
        let err = execute(&p, &real, &real).unwrap_err();
        assert!(err.contains("Nothing was reverted"), "{err}");
        assert_eq!(git(d.path(), &["stash", "list"]), "");
    }

    /// A diff roost cannot read is not an empty diff.
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

    /// An empty identity stands in for a host with none.
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
        let err = execute(&p, &no_ident, &no_ident).unwrap_err();
        assert!(err.starts_with("git refused: "), "{err}");
        assert!(err.ends_with("Nothing was reverted."), "{err}");
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "a2\n");
    }

    /// `trim` on the prefix also trims leading whitespace: a project named
    /// " sub" would be confined to its sibling `sub/`. Different file names
    /// in each make a mismatch obvious.
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

    /// From a nested project dir, `git --literal-pathspecs stash push -- <path>`
    /// saves the entry and then fails "pathspec did not match", so the push
    /// runs from the toplevel with root-relative paths.
    ///
    /// Revert-checked: running stash from the project dir (using local paths) fails this test.
    #[test]
    fn a_nested_project_execute_reverts_from_toplevel() {
        let d = repo(&[("top.txt", "t\n"), ("sub/in.txt", "i\n")]);
        std::fs::write(d.path().join("top.txt"), "t2\n").unwrap();
        std::fs::write(d.path().join("sub/in.txt"), "i2\n").unwrap();
        let p = build_plan(&d.path().join("sub"), None, &real).unwrap();
        let msg = execute(&p, &real, &real).unwrap();
        assert!(msg.contains("stash@{0}") && msg.contains("roost revert: 1 file"), "{msg}");
        assert_eq!(std::fs::read_to_string(d.path().join("sub/in.txt")).unwrap(), "i\n", "nested file reverted");
        assert_eq!(std::fs::read_to_string(d.path().join("top.txt")).unwrap(), "t2\n", "parent file untouched");
        let list = git(d.path(), &["stash", "list"]);
        assert_eq!(list.lines().count(), 1, "{list}");
    }

    /// A single-file dialog must not report every unrelated change elsewhere
    /// in the repository as "not included: N outside".
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

    /// A single-file revert whose name git does not list is a stale or
    /// mismatched Changes list, not a file with nothing to revert.
    ///
    /// Revert-checked: deleting the matched-nothing refusal fails this with
    /// `called Result::unwrap_err() on an Ok value: Plan { paths: [], ... }`.
    #[test]
    fn a_rel_git_does_not_list_is_refused_as_not_in_status() {
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let err = build_plan(d.path(), Some("nope.txt"), &real).unwrap_err();
        assert_eq!(err, "nope.txt is not in git status; refresh the Changes list. Nothing was reverted.");
    }

    /// `-z` names are raw bytes; the `rel` the Changes list sends is raw too.
    #[test]
    fn a_non_ascii_name_is_planned_by_its_raw_name() {
        let d = repo(&[("café.txt", "c\n")]);
        std::fs::write(d.path().join("café.txt"), "c2\n").unwrap();
        let p = build_plan(d.path(), Some("café.txt"), &real).unwrap();
        assert_eq!(p.paths, vec![PlanPath { path: "café.txt".into(), xy: ".M".into() }]);
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
        run_revert(&hub, &me, None, token, vec![], &real, &real);
        let mine = drain(&rx_me);
        let theirs = drain(&rx_other);
        std::env::remove_var("ROOST_STATE_DIR");

        assert!(mine.iter().any(|m| m.contains(r#""t":"Reverted""#) && m.contains(r#""ok":true"#)), "{mine:?}");
        assert!(!theirs.iter().any(|m| m.contains("RevertPlan") || m.contains(r#""t":"Reverted""#)), "{theirs:?}");
    }

    /// A u64 token as a JSON number loses bits in the browser.
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
        run_revert(&hub, &me, None, token, vec![], &real, &real);
        let msgs = drain(&rx);
        std::env::remove_var("ROOST_STATE_DIR");
        assert!(msgs.iter().any(|m| m.contains("changed since you looked") && m.contains(r#""stale":true"#)), "{msgs:?}");
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "unseen\n");
    }

    /// Part of what the dialog showed went clean before the click: the rest
    /// is still revertable, but it is not what the user agreed to, so only
    /// the token refuses. With both files cleaned the empty-plan guard would
    /// refuse on its own and hide a missing token check.
    ///
    /// Revert-checked: deleting `plan.token != token` fails this with
    /// `"ok":true,"msg":"Reverted 1 file; saved as stash@{0} ..."`; with the
    /// earlier fixture (both files cleaned) the same break stayed green.
    #[test]
    fn a_tree_cleaned_after_the_preview_is_refused_not_reported_reverted() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let d = repo(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        std::fs::write(d.path().join("b.txt"), "b2\n").unwrap();
        let hub = hub_on(d.path());
        let (me, rx) = Hub::lock(&hub).subscribe();
        drain(&rx);
        run_preview(&hub, &me, None, &real);
        let token = token_of(&drain(&rx));
        std::fs::write(d.path().join("a.txt"), "a\n").unwrap();
        run_revert(&hub, &me, None, token, vec![], &real, &real);
        let msgs = drain(&rx);
        std::env::remove_var("ROOST_STATE_DIR");
        assert!(msgs.iter().any(|m| m.contains("changed since you looked") && m.contains(r#""stale":true"#)), "{msgs:?}");
        assert_eq!(git(d.path(), &["stash", "list"]), "");
        assert_eq!(std::fs::read_to_string(d.path().join("b.txt")).unwrap(), "b2\n");
    }

    /// An empty plan is never pushed: `stash push --` with no paths stashes
    /// the whole repository, and from a nested project the whole repository
    /// is exactly what the plan excluded. Here the token matches, so only
    /// the empty-plan guard stands between the click and the parent's file.
    ///
    /// Revert-checked: deleting `plan.paths.is_empty()` fails this with
    /// `"ok":true,"msg":"Reverted 0 files; saved as stash@{0} ..."`, the
    /// parent's `top.txt` stashed.
    #[test]
    fn an_empty_plan_is_never_pushed() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let d = repo(&[("top.txt", "t\n"), ("sub/in.txt", "i\n")]);
        std::fs::write(d.path().join("top.txt"), "t2\n").unwrap();
        let hub = hub_on(&d.path().join("sub"));
        let (me, rx) = Hub::lock(&hub).subscribe();
        drain(&rx);
        run_preview(&hub, &me, None, &real);
        let token = token_of(&drain(&rx));
        run_revert(&hub, &me, None, token, vec![], &real, &real);
        let msgs = drain(&rx);
        std::env::remove_var("ROOST_STATE_DIR");
        assert!(msgs.iter().any(|m| m.contains(r#""t":"Reverted""#) && m.contains(r#""ok":false"#)), "{msgs:?}");
        assert_eq!(std::fs::read_to_string(d.path().join("top.txt")).unwrap(), "t2\n");
        assert_eq!(git(d.path(), &["stash", "list"]), "");
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

        run_revert(&hub, &me, None, token.clone(), vec![], &real, &real);
        let refused = drain(&rx);
        assert!(refused.iter().any(|m| m.contains("unsaved edits")), "{refused:?}");
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "a2\n");
        assert!(Hub::lock(&hub).ws.buffers["a.txt"].dirty(), "an unnamed buffer is not discarded");

        run_revert(&hub, &me, None, token, vec!["a.txt".into()], &real, &real);
        let done = drain(&rx);
        std::env::remove_var("ROOST_STATE_DIR");
        assert!(done.iter().any(|m| m.contains(r#""ok":true"#)), "{done:?}");
        assert!(!Hub::lock(&hub).ws.buffers["a.txt"].dirty(), "a named buffer is discarded to the file");
        assert!(done.iter().any(|m| m.contains(r#""t":"BufferText""#) && m.contains("a\\n")), "{done:?}");
    }

    /// `discard_buffers` comes from the client; only a buffer that was dirty
    /// at the check, and is among the reverted paths, is the server's to
    /// close. A clean one named anyway is left for the watcher.
    ///
    /// Revert-checked: closing `discard ∩ plan.paths` (dirty or not) fails
    /// this with a `{"t":"BufferText","rel":"a.txt","text":"a\n",...}` in
    /// the messages.
    #[test]
    fn a_clean_buffer_named_for_discard_is_not_closed() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let d = repo(&[("a.txt", "a\n")]);
        std::fs::write(d.path().join("a.txt"), "a2\n").unwrap();
        let hub = hub_on(d.path());
        let (me, rx) = Hub::lock(&hub).subscribe();
        Hub::lock(&hub).handle(&me, crate::proto::Intent::OpenTab {
            pane: crate::proto::MIDDLE,
            tab: crate::proto::Tab::File { rel: "a.txt".into(), mode: crate::proto::Mode::Edit },
        });
        drain(&rx);
        run_preview(&hub, &me, None, &real);
        let token = token_of(&drain(&rx));
        run_revert(&hub, &me, None, token, vec!["a.txt".into()], &real, &real);
        let done = drain(&rx);
        std::env::remove_var("ROOST_STATE_DIR");
        assert!(done.iter().any(|m| m.contains(r#""t":"Reverted""#) && m.contains(r#""ok":true"#)), "{done:?}");
        assert!(!done.iter().any(|m| m.contains(r#""t":"BufferText""#)), "{done:?}");
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
            run_revert(&h2, &me2, None, token, vec![], &real, &slow);
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
