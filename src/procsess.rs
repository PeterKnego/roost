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

/// A pid's direct children, or `None` when the list could not be read.
///
/// `None` covers a kernel built without `CONFIG_PROC_CHILDREN` as well as a
/// process that exited. Both mean roost cannot see what this holder was
/// parenting, which is not the same as it having parented nothing — and the
/// caller must not proceed to kill on that.
pub fn children_of(proc_root: &Path, pid: u32) -> Option<Vec<u32>> {
    let p = proc_root.join(pid.to_string()).join("task").join(pid.to_string()).join("children");
    let raw = std::fs::read_to_string(p).ok()?;
    Some(raw.split_whitespace().filter_map(|w| w.parse().ok()).collect())
}

/// The sessions to sweep for a set of socket holders, or `None` when they
/// could not be determined.
///
/// **Must be called before the holders are killed.** Once a dtach master dies
/// its children reparent to init and the `children` file this reads is gone;
/// deriving the target afterwards is deriving it from nothing.
///
/// A target is a holder's direct child that *leads its own session* — dtach
/// `setsid`s the slave side, so the shell is a session leader in a session the
/// master is not in. An ordinary child (same session as its parent) is not the
/// slave side and is left alone. roost's own attach clients have no children
/// at all and so contribute nothing, which is why they need no special case
/// even though `pids_holding` returns them.
///
/// `own_sid` is roost's own session. `None` means roost could not establish
/// it, and then there is no target this function is willing to name: it cannot
/// promise the sweep would not kill the server answering the click.
pub fn target_sessions(
    proc_root: &Path,
    holders: &[u32],
    own_sid: Option<u32>,
) -> Option<Vec<u32>> {
    // Nothing held the socket, so there is nothing to derive and nothing this
    // function could name to kill. Checked before `own_sid`, because refusing
    // the vacuous case would make every kill fail whenever roost could not
    // read its own `/proc` entry — including the ordinary "the session was
    // already gone" path, which must stay a success.
    if holders.is_empty() {
        return Some(Vec::new());
    }
    let own = own_sid?;
    let mut out = Vec::new();
    for holder in holders {
        let holder_sid = match session_of(proc_root, *holder) {
            Sid::In(s) => s,
            // Died between the snapshot and now: nothing to derive, and not a
            // reason to abandon the other holders.
            Sid::Gone => continue,
            Sid::Unknown => return None,
        };
        let Some(kids) = children_of(proc_root, *holder) else { return None };
        for kid in kids {
            match session_of(proc_root, kid) {
                Sid::In(s)
                    // A session leader (`s == kid`), in a session that is
                    // neither the holder's nor ours, and not init's.
                    if s == kid && s != holder_sid && s != own && s > 1 =>
                {
                    out.push(s)
                }
                Sid::In(_) | Sid::Gone => {}
                Sid::Unknown => return None,
            }
        }
    }
    out.sort_unstable();
    out.dedup();
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
    // => {}` makes this fail — test panicked at src/procsess.rs:168:9:
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

    /// Writes a fake `/proc/<pid>/task/<pid>/children`.
    fn children(dir: &Path, pid: u32, kids: &[u32]) {
        let p = dir.join(pid.to_string()).join("task").join(pid.to_string());
        std::fs::create_dir_all(&p).unwrap();
        let list: Vec<String> = kids.iter().map(|k| k.to_string()).collect();
        std::fs::write(p.join("children"), format!("{} ", list.join(" "))).unwrap();
    }

    #[test]
    fn a_master_contributes_its_shells_session_and_a_client_contributes_nothing() {
        let d = tempfile::tempdir().unwrap();
        // Measured shape: master 1601266 -> shell 1601267 (its own session);
        // roost's attach client 134273 for the same socket, with no children.
        stat(d.path(), 1601266, "dtach", 1, 1601266, 1601266);
        children(d.path(), 1601266, &[1601267]);
        stat(d.path(), 1601267, "bash", 1601266, 1601267, 1601267);
        children(d.path(), 1601267, &[]);
        stat(d.path(), 134273, "dtach", 134227, 134273, 134227);
        children(d.path(), 134273, &[]);
        assert_eq!(
            target_sessions(d.path(), &[1601266, 134273], Some(134227)),
            Some(vec![1601267])
        );
    }

    // Revert-checked: dropping `&& s != own` from the match guard makes this
    // fail — test panicked at src/procsess.rs:292:9: assertion `left ==
    // right` failed
    //   left: Some([501])
    //  right: Some([])
    #[test]
    fn roosts_own_session_is_never_a_target() {
        // The guard that matters most: roost is itself a process with children,
        // and a mis-derivation here would have it kill the server answering the
        // click.
        let d = tempfile::tempdir().unwrap();
        stat(d.path(), 500, "dtach", 1, 500, 500);
        children(d.path(), 500, &[501]);
        stat(d.path(), 501, "bash", 500, 501, 501);
        assert_eq!(target_sessions(d.path(), &[500], Some(501)), Some(vec![]));
    }

    #[test]
    fn init_and_pid_zero_are_never_targets() {
        let d = tempfile::tempdir().unwrap();
        stat(d.path(), 500, "dtach", 1, 500, 500);
        children(d.path(), 500, &[1]);
        stat(d.path(), 1, "systemd", 0, 1, 1);
        assert_eq!(target_sessions(d.path(), &[500], Some(999)), Some(vec![]));
    }

    #[test]
    fn a_child_that_is_not_a_session_leader_is_not_a_target() {
        // Only the slave side leads its own session. An ordinary child shares its
        // parent's session and killing that session would be killing the holder's,
        // which is a different and much wider thing.
        let d = tempfile::tempdir().unwrap();
        stat(d.path(), 500, "dtach", 1, 500, 500);
        children(d.path(), 500, &[502]);
        stat(d.path(), 502, "helper", 500, 500, 500);
        assert_eq!(target_sessions(d.path(), &[500], Some(999)), Some(vec![]));
    }

    // Revert-checked: changing `let own = own_sid?;` to `let own =
    // own_sid.unwrap_or(0);` makes this fail — test panicked at
    // src/procsess.rs:329:9: assertion `left == right` failed
    //   left: Some([501])
    //  right: None
    #[test]
    fn an_unknown_own_session_refuses_every_target() {
        // Without knowing which session is ours we cannot promise not to kill it,
        // and the safe direction is to do nothing and report it.
        let d = tempfile::tempdir().unwrap();
        stat(d.path(), 500, "dtach", 1, 500, 500);
        children(d.path(), 500, &[501]);
        stat(d.path(), 501, "bash", 500, 501, 501);
        assert_eq!(target_sessions(d.path(), &[500], None), None);
    }

    #[test]
    fn an_unreadable_child_makes_the_whole_derivation_unknown() {
        let d = tempfile::tempdir().unwrap();
        stat(d.path(), 500, "dtach", 1, 500, 500);
        children(d.path(), 500, &[501]);
        let p = d.path().join("501");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("stat"), "garbage\n").unwrap();
        assert_eq!(target_sessions(d.path(), &[500], Some(999)), None);
    }

    #[test]
    fn no_holders_is_no_targets_even_with_an_unknown_own_session() {
        // The vacuous case must stay a success: `kill_and_unlink` reaches here for
        // a socket nothing holds, and refusing it would turn "already gone" into
        // "could not determine" and leave the socket behind forever.
        let d = tempfile::tempdir().unwrap();
        assert_eq!(target_sessions(d.path(), &[], None), Some(vec![]));
    }

    #[test]
    fn a_holder_that_already_exited_is_skipped_not_doubt() {
        // The snapshot is a moment old; a holder that died on its own in between
        // has achieved what was wanted and must not stall the sweep.
        let d = tempfile::tempdir().unwrap();
        assert_eq!(target_sessions(d.path(), &[404], Some(999)), Some(vec![]));
    }

    #[test]
    fn a_missing_children_file_is_unknown_not_childless() {
        // Some kernels build without CONFIG_PROC_CHILDREN. "No file" there means
        // roost cannot see the shell at all, not that there is no shell.
        let d = tempfile::tempdir().unwrap();
        stat(d.path(), 500, "dtach", 1, 500, 500);
        assert_eq!(children_of(d.path(), 500), None);
        assert_eq!(target_sessions(d.path(), &[500], Some(999)), None);
    }
}
