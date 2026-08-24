//! Git working-tree state via the git binary. Porcelain v2 parsing includes
//! rename lines ("2 ..."), which v1 flagged as its most suspect code.
use std::path::Path;
use std::process::Command;

pub struct Change {
    pub xy: String,
    pub path: String,
}

#[derive(Default)]
pub struct Status {
    pub branch: String,
    pub changes: Vec<Change>,
    /// Commits the local branch is ahead of / behind its upstream, and the
    /// upstream's name. All from the same `--porcelain=v2 -b` output already
    /// fetched — `# branch.ab +A -B` and `# branch.upstream NAME` — so this
    /// costs no extra `git` invocation. Zero and empty when there is no
    /// upstream (a fresh branch never pushed), which reads correctly as "no
    /// divergence to report".
    pub ahead: u32,
    pub behind: u32,
    pub upstream: String,
}

pub(crate) fn run_git(repo: &Path, args: &[&str], allow_exit_1: bool) -> Result<String, String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    // Drain stdout/stderr on dedicated threads concurrently with the wait loop below:
    // git can fill the ~64KB pipe buffer and block on write while we poll try_wait,
    // which would deadlock if we only read after the process exits.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(s) = stdout_pipe.as_mut() {
            let _ = s.read_to_string(&mut buf);
        }
        buf
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(s) = stderr_pipe.as_mut() {
            let _ = s.read_to_string(&mut buf);
        }
        buf
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(st) => break st,
            None if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("git {} timed out after 15s", args.first().unwrap_or(&"")));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    };
    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    let code = status.code().unwrap_or(-1);
    if code != 0 && !(allow_exit_1 && code == 1) {
        // git diff exits 1 when differences exist (only allowed if allow_exit_1 is true)
        return Err(stderr.trim().to_string());
    }
    Ok(stdout)
}

pub fn parse_status(porcelain: &str) -> Status {
    let mut branch = String::new();
    let mut upstream = String::new();
    let mut ahead = 0u32;
    let mut behind = 0u32;
    let mut changes = Vec::new();
    for line in porcelain.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            branch = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            upstream = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            // "+A -B" — a plus/minus token pair. Parse defensively: a token
            // that does not match leaves its count at zero rather than
            // guessing, since these gate what the header tells the user.
            for tok in rest.split_whitespace() {
                if let Some(n) = tok.strip_prefix('+') {
                    ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = tok.strip_prefix('-') {
                    behind = n.parse().unwrap_or(0);
                }
            }
        } else if line.starts_with("1 ") {
            // "1 XY sub mH mI mW hH hI path"
            let parts: Vec<&str> = line.splitn(9, ' ').collect();
            if parts.len() == 9 {
                changes.push(Change { xy: parts[1].into(), path: parts[8].into() });
            }
        } else if line.starts_with("2 ") {
            // "2 XY sub mH mI mW hH hI Xscore path\torigPath"
            let parts: Vec<&str> = line.splitn(10, ' ').collect();
            if parts.len() == 10 {
                let path = parts[9].split('\t').next().unwrap_or("");
                changes.push(Change { xy: parts[1].into(), path: path.into() });
            }
        } else if let Some(rest) = line.strip_prefix("? ") {
            changes.push(Change { xy: "??".into(), path: rest.to_string() });
        }
    }
    Status { branch, changes, ahead, behind, upstream }
}

/// Gated on being inside *any* work tree, matching `is_inside_work_tree` and
/// therefore `Hub`'s `is_git` — not on this directory having its own `.git`.
///
/// A nested project (`karpie/src`) has no `.git` of its own but is genuinely
/// inside its parent's work tree, and `git status -C` there reports the
/// subtree's changes perfectly well. Keying this check off the project's own
/// `.git` while `is_git` asked the broader question left the three git surfaces
/// disagreeing: the terminal placeholder correctly declined to offer
/// `git init`, `diff` (which has no such pre-check) happily produced diffs, and
/// the changes pane in between insisted "not a git repository". One definition
/// for all three.
///
/// The pre-check earns its keep by keeping the *common* non-repo case free of a
/// subprocess — `run_git`'s error would be equivalent, but every workspace load
/// hits this path.
pub fn status(repo: &Path) -> Result<Status, String> {
    if !is_inside_work_tree(repo) {
        return Err("not a git repository".into());
    }
    run_git(repo, &["status", "--porcelain=v2", "-b"], false).map(|s| parse_status(&s))
}

/// Whether `dir` sits inside *any* git work tree — its own, or an ancestor's.
/// What `git rev-parse --is-inside-work-tree` answers, computed by walking
/// ancestors instead of forking, because the only caller is
/// `Hub::refresh_live_sessions`, which runs under the process-global
/// hub-registry lock: a subprocess there would stall every other project's
/// connection setup, the constraint CLAUDE.md states outright.
///
/// A project's *own* `.git` is not the question. A **nested** project
/// (`karpie/src` — explicitly supported) has none of its own while sitting
/// squarely inside its parent's work tree, and answering "not a git
/// repository" there made the terminal placeholder offer `git init`, one click
/// from creating an embedded repository that silently detaches that subtree
/// from the parent's history. The changes pane beside it is already showing
/// the *parent's* status, because `git status -C dir` succeeds anywhere inside
/// a work tree — so the wrong offer looked entirely credible.
///
/// Walks to the filesystem root rather than stopping at a project root, which
/// is not over-reach: if an ancestor outside the roots is a repository, then
/// this directory really is inside its work tree and `git init` really would
/// embed one. `symlink_metadata` rather than `exists()` for the usual reason —
/// `exists()` follows symlinks and folds every error into "absent" — and a
/// stat error that is not `NotFound` counts as *present*, since the safe
/// direction here is to withhold the `git init` offer rather than to make it
/// on the strength of a failed check.
pub fn is_inside_work_tree(dir: &Path) -> bool {
    // `.git` is a directory in a normal clone and a *file* in a linked
    // worktree, so this must not test for a directory specifically.
    for anc in dir.ancestors() {
        match std::fs::symlink_metadata(anc.join(".git")) {
            Ok(_) => return true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return true,
        }
    }
    false
}

/// `git init`, with no arguments beyond that: `run_git` fixes `-C repo` as
/// the only path input and this passes nothing else, so there is no room
/// for a caller to smuggle extra git arguments through here. Routed through
/// `run_git` rather than a bare `Command`, so it gets the same 15s
/// deadline-and-kill as every other git call in this module — callers that
/// run this under a long-lived lock (the hub does) depend on that bound.
pub fn init(repo: &Path) -> Result<String, String> {
    run_git(repo, &["init"], false)
}

pub fn diff(repo: &Path, path: Option<&str>) -> Result<String, String> {
    match path {
        None => run_git(repo, &["diff", "HEAD"], true),
        Some(p) => {
            let tracked = Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["ls-files", "--error-unmatch", p])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if tracked {
                run_git(repo, &["diff", "HEAD", "--", p], true)
            } else {
                // untracked: synthesize an all-new diff; confine the read
                let abs = crate::projects::safe_resolve(repo, p)?;
                let body = crate::projects::read_text_file(&abs)?;
                let lines: Vec<&str> = body.lines().collect();
                let mut d = format!("--- /dev/null\n+++ b/{p}\n@@ -0,0 +1,{} @@\n", lines.len());
                for l in &lines {
                    d.push('+');
                    d.push_str(l);
                    d.push('\n');
                }
                Ok(d)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn repo_fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = Command::new("git").arg("-C").arg(d.path()).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        fs::write(d.path().join("a.txt"), "one\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        d
    }

    #[test]
    fn parses_ordinary_and_untracked_lines() {
        let p = "# branch.head main\n\
                 # branch.upstream origin/main\n\
                 # branch.ab +2 -1\n\
                 1 .M N... 100644 100644 100644 abc def a.txt\n\
                 ? b.txt\n";
        let st = parse_status(p);
        assert_eq!(st.branch, "main");
        assert_eq!(st.upstream, "origin/main");
        assert_eq!(st.ahead, 2);
        assert_eq!(st.behind, 1);
        assert_eq!(st.changes.len(), 2);
        assert_eq!(st.changes[0].xy, ".M");
        assert_eq!(st.changes[0].path, "a.txt");
        assert_eq!(st.changes[1].xy, "??");
        assert_eq!(st.changes[1].path, "b.txt");
    }

    #[test]
    fn parses_rename_lines() {
        let p = "# branch.head main\n\
                 2 R. N... 100644 100644 100644 abc def R100 new.txt\told.txt\n";
        let st = parse_status(p);
        assert_eq!(st.changes.len(), 1);
        assert_eq!(st.changes[0].xy, "R.");
        assert_eq!(st.changes[0].path, "new.txt");
    }

    #[test]
    fn status_against_real_repo() {
        let d = repo_fixture();
        fs::write(d.path().join("a.txt"), "two\n").unwrap();
        fs::write(d.path().join("b.txt"), "bee\n").unwrap();
        let st = status(d.path()).unwrap();
        assert_eq!(st.branch, "main");
        assert_eq!(st.changes.len(), 2);
    }

    #[test]
    fn diff_tracked_untracked_and_escape() {
        let d = repo_fixture();
        fs::write(d.path().join("a.txt"), "two\n").unwrap();
        fs::write(d.path().join("b.txt"), "bee\n").unwrap();
        let full = diff(d.path(), None).unwrap();
        assert!(full.contains("-one"));
        assert!(full.contains("+two"));
        let untracked = diff(d.path(), Some("b.txt")).unwrap();
        assert!(untracked.contains("+++ b/b.txt"));
        assert!(untracked.contains("+bee"));
        assert!(diff(d.path(), Some("../../etc/passwd")).is_err());
    }

    #[test]
    fn status_errors_outside_a_repo() {
        let d = tempfile::tempdir().unwrap();
        assert!(status(d.path()).is_err());
    }

    #[test]
    fn diff_untracked_binary_file_errors() {
        let d = repo_fixture();
        std::fs::write(d.path().join("bin.bin"), b"\x00\x01\x02").unwrap();
        let result = diff(d.path(), Some("bin.bin"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_lowercase().contains("binary"));
    }

    /// A **nested** project inside a real repository is the case that matters:
    /// it has no `.git` of its own, so a bare `dir.join(".git").exists()` said
    /// "not a git repository" and the terminal placeholder offered `git init` —
    /// one click from embedding a repo inside its parent and detaching that
    /// subtree from the parent's history. A fixture whose parent is not a repo
    /// would pass either way and prove nothing, so the parent here is a real
    /// one, created by a real `git init`.
    #[test]
    fn a_nested_directory_inside_a_repo_is_inside_a_work_tree() {
        let d = repo_fixture();
        let nested = d.path().join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();

        assert!(
            !nested.join(".git").exists(),
            "fixture must have no .git of its own, or this proves nothing"
        );
        assert!(
            is_inside_work_tree(&nested),
            "a directory inside a repo's work tree must count as git, or the placeholder \
             offers `git init` and embeds a repository inside its parent"
        );
        // The repo root itself, the easy case, must still work.
        assert!(is_inside_work_tree(d.path()));
        // All three git surfaces must agree on one definition. `status` used to
        // key off the project's own `.git`, so a nested project got no
        // `git init` offer (right) but a changes pane reading "not a git
        // repository" — while `diff`, which never had that pre-check, produced
        // diffs happily. Three surfaces, three answers, for one directory.
        assert!(
            status(&nested).is_ok(),
            "the changes pane must agree with is_git: a nested project is inside a work tree"
        );
        assert!(
            diff(&nested, None).is_ok(),
            "and diff, which never had a .git pre-check, must keep working"
        );
    }

    /// A linked worktree's `.git` is a *file*, not a directory — worktrees are
    /// first-class projects here, so a check that tested specifically for a
    /// directory would offer `git init` inside every one of them.
    #[test]
    fn a_linked_worktrees_git_file_counts_as_a_work_tree() {
        let d = tempfile::tempdir().unwrap();
        let fake_worktree = d.path().join("wt");
        std::fs::create_dir_all(&fake_worktree).unwrap();
        std::fs::write(fake_worktree.join(".git"), "gitdir: /elsewhere/.git/worktrees/wt\n")
            .unwrap();
        assert!(
            is_inside_work_tree(&fake_worktree),
            "a .git *file* marks a linked worktree and must count"
        );
    }

    /// The negative case, so the two above cannot pass by the function simply
    /// returning true. A tempdir under the OS temp root has no repository
    /// anywhere above it.
    #[test]
    fn a_directory_with_no_repo_anywhere_above_it_is_not_a_work_tree() {
        let d = tempfile::tempdir().unwrap();
        let deep = d.path().join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        assert!(
            !is_inside_work_tree(&deep),
            "with no repo in any ancestor this must be false, or `git init` is never offered \
             and the escape hatch for a plain directory is unreachable"
        );
    }
}
