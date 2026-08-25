//! Git worktrees. A worktree is its own project — separate directory, rel
//! path, sessions and layout — and only its *display* is parent-and-child.
//!
//! Discovery asks git rather than walking the filesystem, because the
//! dominant real location is a dot-directory (`{repo}/.claude/worktrees/{name}`,
//! which Claude Code creates) that the picker hides and `resolve_project`
//! refuses. A path convention would also miss a worktree placed in a sibling
//! directory; `git worktree list` would not.
use std::path::{Path, PathBuf};

pub struct Worktree {
    pub path: PathBuf,
    /// Branch name, or `"(detached)"` — a worktree always needs a label
    /// because worktrees of one repo differ only by branch.
    pub branch: String,
    pub is_main: bool,
}

/// Parse `git worktree list --porcelain`: blank-line separated records, each
/// starting with `worktree <path>`. The first non-bare record is the main
/// worktree.
pub fn parse_porcelain(out: &str) -> Vec<Worktree> {
    let mut ws = Vec::new();
    for record in out.split("\n\n") {
        let mut path: Option<PathBuf> = None;
        let mut branch = String::new();
        let mut bare = false;
        for line in record.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p.trim()));
            } else if let Some(b) = line.strip_prefix("branch ") {
                // `branch` carries the full ref (`refs/heads/feature/x`), not
                // just the leaf component — branch is the *entire* label
                // grouping worktrees are keyed on, so truncating it to the
                // last `/`-segment (the old `rsplit('/').next()`) would
                // silently collide `feature/a` and `bugfix/a` into the same
                // displayed "a".
                let b = b.trim();
                branch = b.strip_prefix("refs/heads/").unwrap_or(b).to_string();
            } else if line.trim() == "detached" {
                branch = "(detached)".to_string();
            } else if line.trim() == "bare" {
                bare = true;
            }
        }
        // A bare repository's own record (a real, common worktree layout is
        // `repo/.bare`, git's own main entry for it) has a path and no
        // branch — it is not a working tree, nothing is checked out, there
        // is nothing to open as a project. Narrowing the dot-segment
        // exception to genuine worktrees is the whole point of this module,
        // so this record is dropped entirely rather than merely mislabelled
        // "(detached)": keeping it out of the returned list is what keeps
        // `is_vouched_worktree` from ever vouching for it.
        if bare {
            continue;
        }
        if let Some(p) = path {
            let is_main = ws.is_empty();
            if branch.is_empty() {
                branch = "(detached)".to_string();
            }
            ws.push(Worktree { path: p, branch, is_main });
        }
    }
    ws
}

/// Runs through `gitio::run_git`, not a bare `Command::output()`: this is
/// reachable from an HTTP request path (`/frag/_projects`, polled by
/// `hx-trigger="load, refresh from:body"`), so a wedged repository or
/// filesystem must not hang the request indefinitely — the same 15s
/// deadline (with stdout/stderr drained concurrently so a full pipe buffer
/// can't itself deadlock the wait) that every other git call in this
/// codebase already gets.
pub fn list(repo: &Path) -> Vec<Worktree> {
    match crate::gitio::run_git(repo, &["worktree", "list", "--porcelain"], false) {
        Ok(out) => parse_porcelain(&out),
        Err(_) => Vec::new(),
    }
}

pub type GitRunner<'a> = &'a dyn Fn(&Path, &[&str]) -> Result<String, String>;

/// The production runner: the 15 s-deadline `gitio::run_git`, exit 0 only.
pub fn real_git(repo: &Path, args: &[&str]) -> Result<String, String> {
    crate::gitio::run_git(repo, args, false)
}

pub const MAX_WORKTREES: u32 = 64;

#[derive(Debug)]
pub struct Created {
    pub name: String,
    pub path: PathBuf,
    /// The branch (or commit, when detached) the worktree was cut from.
    pub base: String,
}

pub fn base_file(state_dir: &Path, wt_key: &str) -> PathBuf {
    state_dir.join("worktrees").join(format!("{wt_key}.base"))
}

/// `None` for absent, unreadable, or empty: an empty base is not a base,
/// and "ahead unknown" is the direction that failure must fall.
pub fn read_base(state_dir: &Path, wt_key: &str) -> Option<String> {
    let s = std::fs::read_to_string(base_file(state_dir, wt_key)).ok()?;
    let s = s.trim_end_matches('\n');
    if s.is_empty() { None } else { Some(s.to_string()) }
}

/// Temp file with a pid-unique name, then rename: a reader never sees half.
pub fn write_base(state_dir: &Path, wt_key: &str, base: &str) -> Result<(), String> {
    let path = base_file(state_dir, wt_key);
    let dir = path.parent().ok_or("no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".{wt_key}.base.tmp.{}", std::process::id()));
    std::fs::write(&tmp, format!("{base}\n")).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| { let _ = std::fs::remove_file(&tmp); e.to_string() })
}

/// Mint `claude-N`, record its base, `git worktree add`. Every failure
/// returns before anything later runs; a failed check is a refusal, never a
/// skip to N+1 — "I could not tell whether claude-1 exists" is not "it does".
pub fn create(
    repo: &Path,
    state_dir: &Path,
    wt_key_of: &dyn Fn(&str) -> String,
    run: GitRunner,
) -> Result<Created, String> {
    if !crate::gitio::is_inside_work_tree(repo) {
        return Err("not a git repository".into());
    }
    let canon = repo.canonicalize().map_err(|e| format!("cannot resolve project directory: {e}"))?;
    let ws = list(repo);
    if ws.is_empty() {
        return Err("git did not answer (worktree list)".into());
    }
    let me = ws.iter().find(|w| w.path.canonicalize().ok().as_deref() == Some(canon.as_path()));
    match me {
        Some(w) if w.is_main => {}
        _ => return Err("start worktrees from the main checkout".into()),
    }
    let mut name = None;
    for n in 1..=MAX_WORKTREES {
        let cand = format!("claude-{n}");
        let out = run(repo, &["branch", "--list", &cand])
            .map_err(|e| format!("cannot tell whether branch {cand} exists: {e}"))?;
        if !out.trim().is_empty() {
            continue;
        }
        match std::fs::symlink_metadata(repo.join(".claude/worktrees").join(&cand)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => { name = Some(cand); break; }
            Ok(_) => continue,
            Err(e) => return Err(format!("cannot tell whether .claude/worktrees/{cand} exists: {e}")),
        }
    }
    let name = name.ok_or_else(|| format!("too many worktrees ({MAX_WORKTREES})"))?;
    std::fs::create_dir_all(repo.join(".claude/worktrees")).map_err(|e| e.to_string())?;
    let path = crate::projects::safe_resolve_parent(repo, &format!(".claude/worktrees/{name}"))?;
    let base = match run(repo, &["symbolic-ref", "--short", "HEAD"]) {
        Ok(b) if !b.trim().is_empty() => b.trim().to_string(),
        _ => run(repo, &["rev-parse", "HEAD"]).map_err(|e| format!("cannot read HEAD: {e}"))?.trim().to_string(),
    };
    let key = wt_key_of(&name);
    write_base(state_dir, &key, &base)?;
    let rel = format!(".claude/worktrees/{name}");
    if let Err(e) = run(repo, &["worktree", "add", "-b", &name, &rel, "HEAD"]) {
        let _ = std::fs::remove_file(base_file(state_dir, &key));
        return Err(format!("git worktree add failed: {e}"));
    }
    Ok(Created { name, path, base })
}

/// True when git itself reports `rel` as a worktree of some repository under
/// `roots`. This is the sole exception to the dot-segment rule, and it is an
/// exception to *naming* only — the caller still confines the path.
pub fn is_vouched_worktree(roots: &[PathBuf], rel: &str) -> bool {
    let Some(candidate) = confined_path(roots, rel) else { return false };
    // Walk up looking for the repository that owns this path, then ask it.
    let mut probe = candidate.as_path();
    while let Some(parent) = probe.parent() {
        if parent.join(".git").exists() {
            let owned = list(parent).into_iter().any(|w| {
                w.path.canonicalize().map(|p| p == candidate).unwrap_or(false)
            });
            if owned {
                return true;
            }
        }
        probe = parent;
        // `probe` descends from `candidate`, which is already canonical
        // (confined_path canonicalizes it); `roots` themselves may not be
        // (e.g. a tempdir under a symlinked /tmp on macOS, or `/home` itself
        // symlinked in production) — comparing against the raw root here
        // would stop the walk one level too early and never reach the
        // repository's `.git`, so each root is canonicalized for this check
        // too, same as `confined_path` does before comparing.
        if !roots.iter().any(|r| r.canonicalize().map(|cr| probe.starts_with(&cr)).unwrap_or(false)) {
            break;
        }
    }
    false
}

/// Canonicalise `rel` under some root without applying the dot-segment rule —
/// confinement only. Returns None when it escapes every root.
fn confined_path(roots: &[PathBuf], rel: &str) -> Option<PathBuf> {
    for root in roots {
        let Ok(base) = root.canonicalize() else { continue };
        let Ok(canon) = base.join(rel).canonicalize() else { continue };
        if canon.starts_with(&base) && canon.is_dir() {
            return Some(canon);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_main_and_linked_worktrees_with_branches() {
        // Real `git worktree list --porcelain` shape: blank-line separated
        // records, `branch refs/heads/<name>`, and a leading bare record —
        // git itself emits one whenever the repo has a bare gitdir (the
        // common `repo/.bare` worktree layout), before any real worktree.
        let out = "worktree /r/.bare\nbare\n\n\
                   worktree /r/main\nHEAD abc\nbranch refs/heads/main\n\n\
                   worktree /r/.claude/worktrees/feat\nHEAD def\nbranch refs/heads/feat\n\n";
        let ws = parse_porcelain(out);
        // The bare record is dropped entirely, not merely skipped over for
        // is_main purposes — it never becomes a Worktree at all.
        assert!(!ws.iter().any(|w| w.path == PathBuf::from("/r/.bare")));
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0].path, PathBuf::from("/r/main"));
        assert_eq!(ws[0].branch, "main");
        assert!(ws[0].is_main, "the first non-bare record is the main worktree");
        assert_eq!(ws[1].path, PathBuf::from("/r/.claude/worktrees/feat"));
        assert_eq!(ws[1].branch, "feat");
        assert!(!ws[1].is_main);
    }

    // A slashed branch name is common (`feature/x`, `bugfix/x`) and branch is
    // the *entire* label the grouping UI keys on — truncating to the last
    // `/`-segment would silently collide `feature/a` and `bugfix/a`.
    #[test]
    fn parses_a_slashed_branch_name_without_truncating_it() {
        let out = "worktree /r/main\nHEAD abc\nbranch refs/heads/main\n\n\
                   worktree /r/wt\nHEAD def\nbranch refs/heads/feature/nested/thing\n\n";
        let ws = parse_porcelain(out);
        assert_eq!(ws[1].branch, "feature/nested/thing");
    }

    #[test]
    fn parses_a_detached_worktree_without_panicking() {
        let out = "worktree /r/main\nHEAD abc\ndetached\n\n";
        let ws = parse_porcelain(out);
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].branch, "(detached)", "a detached head still needs a label");
    }

    #[test]
    fn malformed_output_yields_nothing_rather_than_panicking() {
        assert!(parse_porcelain("").is_empty());
        assert!(parse_porcelain("garbage\nmore garbage\n").is_empty());
    }

    // The porcelain format is the thing under test, so this uses a real
    // `git worktree add` — a hand-written fixture would not prove we parse
    // git's actual output.
    #[test]
    fn a_real_worktree_in_a_dot_directory_is_vouched_and_resolvable() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&repo, &["init", "-q", "-b", "main"]);
        run(&repo, &["config", "user.email", "t@t"]);
        run(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "x").unwrap();
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "-qm", "init"]);
        run(&repo, &["worktree", "add", "-q", "-b", "feat", ".claude/worktrees/feat"]);
        // A slashed branch name is common (`feature/x`) — real git output,
        // not just the hand-written fixture, must not truncate it either.
        run(&repo, &["worktree", "add", "-q", "-b", "feature/nested", ".claude/worktrees/nested"]);

        let ws = list(&repo);
        assert_eq!(ws.len(), 3, "git must report every worktree");
        assert!(ws.iter().any(|w| w.branch == "feat"));
        assert!(
            ws.iter().any(|w| w.branch == "feature/nested"),
            "a real slashed branch name must reach here intact, not truncated to its last segment"
        );

        let roots = vec![root.path().to_path_buf()];
        // The dot-segment path is vouched for, so it resolves...
        assert!(is_vouched_worktree(&roots, "repo/.claude/worktrees/feat"));
        assert!(crate::projects::resolve_project(&roots, "repo/.claude/worktrees/feat").is_some());
        // ...while an unvouched dot path is still refused.
        assert!(!is_vouched_worktree(&roots, "repo/.claude"));
        assert!(crate::projects::resolve_project(&roots, "repo/.claude").is_none());
        assert!(crate::projects::resolve_project(&roots, "repo/.git").is_none());
    }

    // A bare clone placed in a dot directory (`repo/.bare`) is a real,
    // common worktree layout — and, without M7's fix, would itself become a
    // vouched, openable "worktree" (a path with no branch, mislabelled
    // "(detached)"), which is exactly the kind of thing the dot-segment
    // exception must stay narrow enough to exclude: nothing is checked out
    // in it, there's no working tree to open.
    #[test]
    fn a_bare_repo_in_a_dot_directory_is_never_vouched_for() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&repo, &["init", "-q", "-b", "main"]);
        run(&repo, &["config", "user.email", "t@t"]);
        run(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "x").unwrap();
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "-qm", "init"]);
        run(&repo, &["clone", "--bare", "-q", ".", ".bare"]);

        let ws = list(&repo.join(".bare"));
        assert!(ws.is_empty(), "a bare repository's own record must never surface as a worktree");

        let roots = vec![root.path().to_path_buf()];
        assert!(!is_vouched_worktree(&roots, "repo/.bare"));
        assert!(crate::projects::resolve_project(&roots, "repo/.bare").is_none());
    }

    #[test]
    fn a_worktree_outside_the_roots_is_never_resolvable() {
        // Confinement, not the allowlist, is what forbids this: a cloned repo
        // could name a worktree anywhere.
        let root = tempfile::tempdir().unwrap();
        let roots = vec![root.path().to_path_buf()];
        assert!(!is_vouched_worktree(&roots, "../escape"));
        assert!(crate::projects::resolve_project(&roots, "../escape").is_none());
    }

    fn repo_with_commit(root: &Path) -> PathBuf {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "x").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        repo
    }
    fn key_of(n: &str) -> String { format!("repo%2F.claude%2Fworktrees%2F{n}") }

    #[test]
    fn create_mints_the_next_free_name_and_records_the_base() {
        // Revert-checked: writing `.base` after `worktree add` instead of
        // before passes this test but fails `base_is_written_before_git_runs`.
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let state = root.path().join("state");
        let c1 = create(&repo, &state, &key_of, &real_git).unwrap();
        assert_eq!(c1.name, "claude-1");
        assert_eq!(c1.path, repo.join(".claude/worktrees/claude-1"));
        assert_eq!(c1.base, "main");
        assert!(c1.path.join("a.txt").is_file(), "checked out");
        assert_eq!(read_base(&state, &key_of("claude-1")).as_deref(), Some("main"));
        assert!(list(&repo).iter().any(|w| w.branch == "claude-1" && !w.is_main));
        let c2 = create(&repo, &state, &key_of, &real_git).unwrap();
        assert_eq!(c2.name, "claude-2");
    }

    // Revert-checked: ignoring the branch-existence check's output (minting
    // straight off the directory check) still picks "claude-1" as the free
    // name, and `git worktree add -b claude-1` then collides with the branch
    // this test pre-created — observed: `create` returns
    // `Err("git worktree add failed: ... fatal: a branch named 'claude-1' already exists")`,
    // and `.unwrap()` on it panics.
    #[test]
    fn a_branch_without_a_directory_still_takes_its_number() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        real_git(&repo, &["branch", "claude-1"]).unwrap();
        let c = create(&repo, &root.path().join("state"), &key_of, &real_git).unwrap();
        assert_eq!(c.name, "claude-2");
    }

    // Revert-checked: dropping the directory `symlink_metadata` check (minting
    // straight off the branch check) makes this test mint "claude-1" again —
    // observed: `assertion 'left == right' failed ... left: "claude-1", right: "claude-2"`.
    #[test]
    fn a_directory_without_a_branch_still_takes_its_number() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        std::fs::create_dir_all(repo.join(".claude/worktrees/claude-1")).unwrap();
        let c = create(&repo, &root.path().join("state"), &key_of, &real_git).unwrap();
        assert_eq!(c.name, "claude-2");
    }

    // Revert-checked: replacing the `run(...).map_err(...)?` on the branch
    // check with `run(...).unwrap_or_default()` (folding "cannot tell" into
    // "empty, so absent") makes this test mint claude-1 anyway instead of
    // refusing — observed: panic "called `Result::unwrap_err()` on an `Ok`
    // value: Created { name: \"claude-1\", path: \"...claude-1\", base: \"main\" }".
    #[test]
    fn a_failed_branch_check_refuses_rather_than_skipping() {
        // "Could not tell whether claude-1 exists" must not become claude-2.
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let flaky = |r: &Path, args: &[&str]| -> Result<String, String> {
            if args.first() == Some(&"branch") { Err("fatal: index locked".into()) } else { real_git(r, args) }
        };
        let err = create(&repo, &root.path().join("state"), &key_of, &flaky).unwrap_err();
        assert!(err.contains("cannot tell") && err.contains("claude-1"), "{err}");
        assert!(list(&repo).len() == 1, "nothing was created");
    }

    // Revert-checked: swapping the order — calling `worktree add` before
    // `write_base` — makes `seen` false (the base file does not exist yet
    // when git runs) — observed: `assertion failed: seen.get()`, message
    // "\".base existed when git ran\"".
    #[test]
    fn base_is_written_before_git_runs_and_removed_when_git_fails() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let state = root.path().join("state");
        let seen = std::cell::Cell::new(false);
        let failing = |r: &Path, args: &[&str]| -> Result<String, String> {
            if args.first() == Some(&"worktree") {
                seen.set(read_base(&state, &key_of("claude-1")).is_some());
                Err("fatal: disk full".into())
            } else { real_git(r, args) }
        };
        let err = create(&repo, &state, &key_of, &failing).unwrap_err();
        assert!(err.contains("disk full"), "{err}");
        assert!(seen.get(), ".base existed when git ran");
        assert!(read_base(&state, &key_of("claude-1")).is_none(), "…and is gone after git failed");
    }

    // Revert-checked: replacing `Some(w) if w.is_main` with `Some(_)` (never
    // checking is_main) lets the linked worktree mint its own nested
    // "claude-2" instead of erroring — observed: panic "called
    // `Result::unwrap_err()` on an `Ok` value: Created { name: \"claude-2\",
    // path: \"...claude-1/.claude/worktrees/claude-2\", base: \"claude-1\" }".
    #[test]
    fn a_linked_worktree_cannot_create_worktrees() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_commit(root.path());
        let c = create(&repo, &root.path().join("state"), &key_of, &real_git).unwrap();
        let err = create(&c.path, &root.path().join("state"), &key_of, &real_git).unwrap_err();
        assert!(err.contains("main checkout"), "{err}");
    }

    // Revert-checked: dropping the `is_inside_work_tree` guard makes this
    // test fail differently, not pass — `list()` on a non-repo returns an
    // empty Vec, so the next check ("git did not answer") fires instead, and
    // the message no longer contains "not a git repository" — observed:
    // panic with message "git did not answer (worktree list)".
    #[test]
    fn a_non_repository_is_refused_by_name() {
        let root = tempfile::tempdir().unwrap();
        let err = create(root.path(), &root.path().join("state"), &key_of, &real_git).unwrap_err();
        assert!(err.contains("not a git repository"), "{err}");
    }

    // Revert-checked: replacing `read_base`'s
    // `if s.is_empty() { None } else { Some(s.to_string()) }` with an
    // unconditional `Some(s.to_string())` makes an empty `.base` file read
    // back as a base — observed: panic "assertion `left == right` failed:
    // empty is not a base\n  left: Some(\"\")\n right: None".
    #[test]
    fn write_base_is_atomic_and_read_base_ignores_a_torn_file() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        write_base(&state, "k", "main").unwrap();
        assert_eq!(read_base(&state, "k").as_deref(), Some("main"));
        assert!(std::fs::read_dir(state.join("worktrees")).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().contains(".tmp")), "no temp file left");
        std::fs::write(base_file(&state, "torn"), "").unwrap();
        assert_eq!(read_base(&state, "torn"), None, "empty is not a base");
    }
}
