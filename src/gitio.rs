//! Git working-tree state via the git binary. Porcelain v2 parsing includes
//! rename lines ("2 ..."), which v1 flagged as its most suspect code.
use std::path::Path;
use std::process::Command;

pub struct Change {
    pub xy: String,
    pub path: String,
}

pub struct Status {
    pub branch: String,
    pub changes: Vec<Change>,
}

fn run_git(repo: &Path, args: &[&str], allow_exit_1: bool) -> Result<String, String> {
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
    let mut changes = Vec::new();
    for line in porcelain.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            branch = rest.to_string();
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
    Status { branch, changes }
}

pub fn status(repo: &Path) -> Result<Status, String> {
    if !repo.join(".git").exists() {
        return Err("not a git repository".into());
    }
    run_git(repo, &["status", "--porcelain=v2", "-b"], false).map(|s| parse_status(&s))
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
                 1 .M N... 100644 100644 100644 abc def a.txt\n\
                 ? b.txt\n";
        let st = parse_status(p);
        assert_eq!(st.branch, "main");
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
}
