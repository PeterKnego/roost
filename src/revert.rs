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

    // Get the prefix (relative path from repo root to current dir) to filter paths.
    let prefix = run(dir, &["rev-parse", "--show-prefix"])
        .map_err(|e| format!("Could not read git prefix: {e}. {NOTHING}"))?;
    let prefix = prefix.trim();

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
        let local_path = if prefix.is_empty() {
            path.to_string()
        } else if let Some(stripped) = path.strip_prefix(prefix) {
            stripped.to_string()
        } else {
            // Path is outside the current directory.
            skipped.outside += 1;
            continue;
        };

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

    /// Revert-checked: deleting the `Saved working directory` check fails this
    /// test.
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

    /// Revert-checked: none (test verifies error handling).
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

    /// Revert-checked: deleting the `IN_PROGRESS` loop fails this test.
    /// Also, `a_merge_in_progress_refuses` must stay green: it is caught by the
    /// unmerged entry, so it does not test the loop.
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
    /// Revert-checked: none (depends on UTF-8 validation in run_git).
    #[test]
    fn a_non_utf8_diff_refuses() {
        let d = repo(&[("l.txt", "caf\n")]);
        std::fs::write(d.path().join("l.txt"), b"caf\xe9\n").unwrap();
        let err = build_plan(d.path(), None, &real).unwrap_err();
        assert!(err.starts_with("Could not read the diff:") && err.contains("not valid UTF-8"), "{err}");
    }

    /// Review Focus 4. An empty identity stands in for a host with none.
    ///
    /// Revert-checked: none (depends on git's ident handling).
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
