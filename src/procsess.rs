//! Which session a process belongs to, and who else is in it.
//!
//! `registry::kill_and_unlink` kills whatever holds a session's dtach socket.
//! That reaches the dtach master and nothing else: dtach `setsid`s the slave
//! side, so the user's shell leads a *different* session, and the master's
//! death arrives there only as a `SIGHUP`. Anything that handles the hangup —
//! Claude Code does — survives, reparents to init, and becomes unreachable.
//! This module is how the sweep finds it: the session id survives reparenting,
//! which is the one property a process tree does not have.
//!
//! Three outcomes everywhere, never two. "I could not read this" is not "this
//! is gone", and folding them together on a path that gates a kill or a
//! confirmation is the mistake `CLAUDE.md`'s table catalogues eleven times.
//! `idesess.rs` is the same shape for the neighbouring question.
use std::path::Path;

/// The session a pid is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sid {
    /// Read and parsed cleanly.
    In(u32),
    /// `ENOENT` — the process exited. Evidence, not a gap.
    Gone,
    /// Could not read, or could not parse. Never folded into `Gone`.
    Unknown,
}

/// `/proc/<pid>/stat`'s session id (field 6).
///
/// Split on the **last** `)`, never on whitespace and never on the first `)`:
/// field 2 is `comm`, which the kernel prints in parentheses without escaping,
/// so a process named `foo) bar (baz` puts both characters inside it. Every
/// field this function wants comes after the whole of `comm`, so the last `)`
/// is the only reliable anchor. After it: state, ppid, pgrp, session.
pub fn session_of(proc_root: &Path, pid: u32) -> Sid {
    let raw = match std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")) {
        Ok(s) => s,
        // The two outcomes that must stay apart: the process exited, versus
        // roost could not look. Only the first is evidence.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Sid::Gone,
        Err(_) => return Sid::Unknown,
    };
    let Some((_, tail)) = raw.rsplit_once(')') else { return Sid::Unknown };
    match tail.split_whitespace().nth(3).and_then(|f| f.parse().ok()) {
        Some(sid) => Sid::In(sid),
        None => Sid::Unknown,
    }
}

/// Every pid in `sid`'s session, or `None` when some entry could not be
/// classified.
///
/// `None` is not "empty". This is the function a sweep asks "is the session
/// gone yet", and the answer gates unlinking a socket and reporting a session
/// ended — so a `/proc` entry roost could not read has to stop the sweep
/// concluding, not be skipped past. A pid that vanished between the `read_dir`
/// and the `stat` is a different matter: `Sid::Gone` is the outcome the sweep
/// wants, so it is dropped rather than treated as doubt.
pub fn members_of(proc_root: &Path, sid: u32) -> Option<Vec<u32>> {
    let rd = std::fs::read_dir(proc_root).ok()?;
    let mut out = Vec::new();
    for e in rd.flatten() {
        // `/proc` holds non-pid entries (`self`, `sys`, `uptime`); they are
        // not processes and are not doubt either.
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        match session_of(proc_root, pid) {
            Sid::In(s) if s == sid => out.push(pid),
            Sid::In(_) | Sid::Gone => {}
            Sid::Unknown => return None,
        }
    }
    out.sort_unstable();
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes one fake `/proc/<pid>/stat`. `comm` is inserted verbatim between
    /// the parentheses, so a test can hand it a name containing spaces and
    /// parentheses — which is the whole hazard this parse exists for.
    fn stat(dir: &Path, pid: u32, comm: &str, ppid: u32, pgrp: u32, sid: u32) {
        let p = dir.join(pid.to_string());
        std::fs::create_dir_all(&p).unwrap();
        // pid (comm) state ppid pgrp session tty_nr … — the fields after
        // `session` are padding; nothing here reads them.
        std::fs::write(
            p.join("stat"),
            format!("{pid} ({comm}) S {ppid} {pgrp} {sid} 34816 1 4194304 100 0 0\n"),
        )
        .unwrap();
    }

    #[test]
    fn reads_the_session_id() {
        let d = tempfile::tempdir().unwrap();
        stat(d.path(), 1601267, "bash", 1601266, 1601267, 1601267);
        assert_eq!(session_of(d.path(), 1601267), Sid::In(1601267));
    }

    // Revert-checked: changing rsplit_once(')') to split_once(')') makes this
    // fail — test panicked at src/procsess.rs:89:9: assertion `left == right`
    // failed
    //   left: In(1)
    //  right: In(77)
    #[test]
    fn a_comm_containing_spaces_and_parens_does_not_shift_the_fields() {
        // The reason this parse splits on the LAST ')' rather than counting
        // whitespace: both of these are legal process names.
        let d = tempfile::tempdir().unwrap();
        stat(d.path(), 42, "my prog", 1, 42, 99);
        stat(d.path(), 43, "foo) bar (baz", 1, 43, 77);
        assert_eq!(session_of(d.path(), 42), Sid::In(99));
        assert_eq!(session_of(d.path(), 43), Sid::In(77));
    }

    #[test]
    fn a_missing_pid_is_gone_not_unknown() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(session_of(d.path(), 12345), Sid::Gone);
    }

    #[test]
    fn an_unparseable_stat_is_unknown_not_gone() {
        // The distinction that matters: `Gone` would let a sweep conclude the
        // session is empty and unlink a socket whose shell is still running.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("7");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("stat"), "not a stat line at all\n").unwrap();
        assert_eq!(session_of(d.path(), 7), Sid::Unknown);
    }

    #[test]
    fn members_of_finds_every_pid_in_the_session_and_no_others() {
        let d = tempfile::tempdir().unwrap();
        // The measured shape: a dtach master leading its own session, the shell
        // leading a second one, and a backgrounded job in the shell's session but
        // in a process group of its own — which is what makes the process group
        // the wrong unit and the session the right one.
        stat(d.path(), 1601266, "dtach", 1, 1601266, 1601266);
        stat(d.path(), 1601267, "bash", 1601266, 1601267, 1601267);
        stat(d.path(), 1601290, "claude", 1601267, 1601290, 1601267);
        stat(d.path(), 999, "unrelated", 1, 999, 999);
        assert_eq!(members_of(d.path(), 1601267), Some(vec![1601267, 1601290]));
        // Asserted explicitly: an empty session and an undeterminable one are
        // different answers, and only this one means "nothing left".
        assert_eq!(members_of(d.path(), 555), Some(vec![]));
    }

    // Revert-checked: changing `Sid::Unknown => return None` to `Sid::Unknown
    // => {}` makes this fail — test panicked at src/procsess.rs:164:9:
    // assertion `left == right` failed
    //   left: Some([100])
    //  right: None
    #[test]
    fn one_unreadable_entry_makes_the_whole_membership_unknown() {
        // A sweep uses this to decide it is finished. If an entry roost could not
        // classify were skipped, an unreadable survivor would read as an empty
        // session — the socket would be unlinked and the session reported ended
        // while the shell was still running.
        let d = tempfile::tempdir().unwrap();
        stat(d.path(), 100, "bash", 1, 100, 100);
        let p = d.path().join("101");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("stat"), "garbage\n").unwrap();
        assert_eq!(members_of(d.path(), 100), None);
    }

    #[test]
    fn a_non_pid_directory_entry_is_skipped_not_fatal() {
        // /proc really contains these: `self`, `thread-self`, `sys`, `net`.
        let d = tempfile::tempdir().unwrap();
        stat(d.path(), 100, "bash", 1, 100, 100);
        std::fs::create_dir_all(d.path().join("self")).unwrap();
        std::fs::write(d.path().join("uptime"), "1 2\n").unwrap();
        assert_eq!(members_of(d.path(), 100), Some(vec![100]));
    }
}
