//! Claude's working directory, from its pid.
//!
//! The IDE protocol never sends a path. On connect the CLI sends exactly
//! `ide_connected {pid}`, and MCP's `initialize` adds a client name and
//! version — nothing that says where the process is. So resh asks the kernel.
//!
//! This matters for worktrees, which is the case that makes the question
//! worth asking at all: `worktree.rs` records that the dominant worktree
//! location is `{repo}/.claude/worktrees/{name}`, a directory Claude Code
//! creates for itself. resh knows the directory it *spawned* a shell in
//! (`session.rs`), but that is where the session started, not where Claude is
//! now. Every absolute path in an `openDiff` is relative to the latter.
//!
//! Three outcomes, not two. "I could not read /proc/<pid>/cwd" is not "the
//! process is gone", and only the second may drop a connection.
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum Cwd {
    At(PathBuf),
    /// Positive evidence the process no longer exists.
    Gone,
    /// resh cannot tell. Never a reason to destroy anything.
    Unknown,
}

pub fn cwd_of_in(proc_root: &Path, pid: u32) -> Cwd {
    // read_link, not canonicalize: a process whose directory was deleted
    // under it still has a truthful cwd, and resolving would discard it.
    let pdir = proc_root.join(pid.to_string());
    match std::fs::read_link(pdir.join("cwd")) {
        Ok(p) => Cwd::At(p),
        Err(_) => {
            // The link is unreadable. Which of the two reasons applies is
            // decided by what is definitely present, never by the same
            // failure that just happened.
            match std::fs::symlink_metadata(&pdir) {
                Ok(_) => Cwd::Unknown, // the process is there; we just cannot look
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Distinguish "no such process" from "no /proc at all".
                    match std::fs::symlink_metadata(proc_root) {
                        Ok(_) => Cwd::Gone,
                        Err(_) => Cwd::Unknown,
                    }
                }
                Err(_) => Cwd::Unknown,
            }
        }
    }
}

pub fn cwd_of(pid: u32) -> Cwd {
    cwd_of_in(Path::new("/proc"), pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_own_pid_resolves_to_our_own_directory() {
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        match cwd_of(std::process::id()) {
            Cwd::At(p) => assert_eq!(p.canonicalize().unwrap(), here),
            other => panic!("expected At, got {other:?}"),
        }
    }

    #[test]
    fn a_pid_that_cannot_exist_is_gone_not_unknown() {
        // The distinction is the whole point: Gone drops the connection,
        // Unknown must not.
        assert!(matches!(cwd_of(u32::MAX), Cwd::Gone));
    }

    #[test]
    fn an_unreadable_proc_is_unknown_not_gone() {
        // /proc/<pid> exists but its cwd entry cannot be read. Folding this
        // into Gone is how a live Claude gets disconnected because a check
        // failed. Reverting the Unknown branch to Gone fails only this test.
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("1234")).unwrap();
        assert!(matches!(cwd_of_in(d.path(), 1234), Cwd::Unknown));
    }

    #[test]
    fn a_missing_proc_entry_is_gone() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(cwd_of_in(d.path(), 1234), Cwd::Gone));
    }

    #[test]
    fn a_missing_proc_filesystem_is_unknown() {
        // Not Linux, or a container without /proc. resh cannot tell, so it
        // must not claim the process is gone.
        let d = tempfile::tempdir().unwrap();
        let absent = d.path().join("no-proc-here");
        assert!(matches!(cwd_of_in(&absent, 1234), Cwd::Unknown));
    }

    #[test]
    fn a_dangling_cwd_symlink_still_reports_the_path() {
        // A process whose directory was deleted under it. readlink answers
        // regardless of whether the target exists, and that answer is the
        // truth about the process — resolving it would turn a valid answer
        // into a wrong one.
        let d = tempfile::tempdir().unwrap();
        let pdir = d.path().join("1234");
        std::fs::create_dir(&pdir).unwrap();
        std::os::unix::fs::symlink("/tmp/deleted-under-it", pdir.join("cwd")).unwrap();
        match cwd_of_in(d.path(), 1234) {
            Cwd::At(p) => assert_eq!(p, PathBuf::from("/tmp/deleted-under-it")),
            other => panic!("expected At, got {other:?}"),
        }
    }
}
