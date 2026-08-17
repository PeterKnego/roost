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
/// starting with `worktree <path>`. The first record is the main worktree.
pub fn parse_porcelain(out: &str) -> Vec<Worktree> {
    let mut ws = Vec::new();
    for record in out.split("\n\n") {
        let mut path: Option<PathBuf> = None;
        let mut branch = String::new();
        for line in record.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p.trim()));
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = b.trim().rsplit('/').next().unwrap_or("").to_string();
            } else if line.trim() == "detached" {
                branch = "(detached)".to_string();
            }
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

pub fn list(repo: &Path) -> Vec<Worktree> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "list", "--porcelain"])
        .output();
    match out {
        Ok(o) if o.status.success() => parse_porcelain(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
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
        // records, `branch refs/heads/<name>`, and `bare`/`detached` variants.
        let out = "worktree /r/main\nHEAD abc\nbranch refs/heads/main\n\n\
                   worktree /r/.claude/worktrees/feat\nHEAD def\nbranch refs/heads/feat\n\n";
        let ws = parse_porcelain(out);
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0].path, PathBuf::from("/r/main"));
        assert_eq!(ws[0].branch, "main");
        assert!(ws[0].is_main, "the first record is the main worktree");
        assert_eq!(ws[1].path, PathBuf::from("/r/.claude/worktrees/feat"));
        assert_eq!(ws[1].branch, "feat");
        assert!(!ws[1].is_main);
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

        let ws = list(&repo);
        assert_eq!(ws.len(), 2, "git must report both worktrees");
        assert!(ws.iter().any(|w| w.branch == "feat"));

        let roots = vec![root.path().to_path_buf()];
        // The dot-segment path is vouched for, so it resolves...
        assert!(is_vouched_worktree(&roots, "repo/.claude/worktrees/feat"));
        assert!(crate::projects::resolve_project(&roots, "repo/.claude/worktrees/feat").is_some());
        // ...while an unvouched dot path is still refused.
        assert!(!is_vouched_worktree(&roots, "repo/.claude"));
        assert!(crate::projects::resolve_project(&roots, "repo/.claude").is_none());
        assert!(crate::projects::resolve_project(&roots, "repo/.git").is_none());
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
}
