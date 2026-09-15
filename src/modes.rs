//! The terminal mode contract, kept on disk so it survives a roost restart.
//!
//! `screen::Screens` learns an app's mode contract by watching it declared,
//! once, in the app's first few hundred bytes. That works for every
//! attachment roost is alive for — a reload, a second browser, a ring
//! turnover — and not at all for a restart: the dtach session outlives roost
//! by design, so the app is still running, still holding a contract, and
//! will never mention it again. The new process therefore believes
//! `bracketedPaste: false` about a terminal that has bracketed paste on.
//!
//! The consequence is not cosmetic. Without bracketed paste a pasted
//! three-line prompt goes on the wire as `one\rtwo\rthree`, so **the first
//! line submits on its own** and the rest lands as follow-up input. It also
//! disguises itself twice over: a page reload does not fix it (a reload is
//! the same attach, replaying the same empty table), and Claude re-asserts
//! its *mouse* modes on interaction while emitting `?2004h` only at startup,
//! so the mouse half heals and paste never does.
//!
//! What is deliberately *not* here is the alternate-screen bit. A stale "on
//! the alternate screen" marker leaves the user a blank buffer with the
//! shell's output going somewhere invisible, and unlike a mode there is no
//! later event to reconcile it against. The asymmetry is the whole argument
//! for splitting them: a wrongly-asserted mouse mode is visible junk on the
//! command line that clears on the next Ctrl-C, where a wrongly-asserted
//! screen bit loses your output. Modes persist; the screen bit does not.
//!
//! # Where the file lives, and why the name starts with a dot
//!
//! Beside the session's dtach socket, as `.modes.<name>`. That directory is
//! swept by `registry::reconcile`, which reads every entry as a session
//! socket and reaps the ones no process holds — so a sidecar named anything
//! else would be reaped instants after being written, exactly as `.origin`
//! was. A leading `.` is the established marker for "metadata about the key,
//! not a socket", and it is unambiguous because a session name can never
//! contain one (`^[A-Za-z0-9_-]{1,32}$`).
//!
//! # Lifetime
//!
//! The file is a sidecar of the *socket*, not of roost's in-memory session,
//! and it is removed where the socket is removed. Tying it to the pump's exit
//! instead would delete it on a detach — which is precisely the case it
//! exists for, since a detached session is still running and roost may be
//! about to restart.

use std::path::{Path, PathBuf};

/// A wrong guess here is a stale contract replayed at a fresh shell, not a
/// parse failure, so the file is small on purpose: one `<mode> <0|1>` pair
/// per line, at most `PERSISTED.len()` of them.
const MAX_BYTES: u64 = 256;

/// `.modes.<name>` beside the socket for `project/name`.
pub fn path_for(project: &str, name: &str) -> PathBuf {
    sidecar_of(&crate::session::socket_path(project, name))
}

/// The sidecar belonging to a dtach socket path. Taken as a path rather than
/// as `(project, name)` so the reap sites, which hold a `DirEntry` and never
/// reconstruct the project key, can call it.
pub fn sidecar_of(sock: &Path) -> PathBuf {
    let name = sock.file_name().unwrap_or_default().to_string_lossy().into_owned();
    sock.with_file_name(format!(".modes.{name}"))
}

/// What a previous roost process recorded, or an empty list.
///
/// Every failure is the same answer — an empty list — and that is safe here
/// in a way it would not be in a reaping path: "I could not read the modes"
/// degrades to today's behaviour, an empty table, which is the bug this
/// module fixes rather than a new one. Nothing is destroyed on this route, so
/// there is no third outcome to carry.
///
/// Unparseable lines are skipped rather than failing the whole file, and
/// every mode is re-checked against `screen::PERSISTED`; see that constant
/// for why reading is where the allowlist has to be enforced.
pub fn load(project: &str, name: &str) -> Vec<(u16, bool)> {
    read_from(&path_for(project, name))
}

fn read_from(path: &Path) -> Vec<(u16, bool)> {
    // Bounded before reading: this file lives in a directory a user can write
    // to, and a huge one must not become a way to make roost allocate.
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && m.len() <= MAX_BYTES => {}
        _ => return Vec::new(),
    }
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(m), Some(v), None) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(mode), Ok(set)) = (m.parse::<u16>(), v.parse::<u8>()) else { continue };
        if set > 1 || !crate::screen::PERSISTED.contains(&mode) {
            continue;
        }
        out.push((mode, set == 1));
    }
    out
}

/// Records the contract. Best-effort: a failure here costs the next restart's
/// paste behaviour, which is what happens today anyway, and is not worth
/// failing a terminal over.
///
/// Write-then-rename with a pid-unique temp name, so a reader can never see a
/// half-written file and two roost processes sharing a state dir cannot
/// collide on the temp. The temp is itself dotted, so an interrupted write
/// leaves something the sweep skips rather than something it reaps.
pub fn save(project: &str, name: &str, modes: &[(u16, bool)]) {
    // The pump reads the table under the registry lock and writes it after
    // releasing — so a Close Tab or Close Project can unlink the socket in
    // between, and a write that went ahead anyway would re-create the
    // directory and the sidecar for a session that was explicitly ended.
    // Only a socket the filesystem positively reports as present is evidence
    // the session is still there; gone or unreadable both mean do nothing,
    // which is safe here because not writing costs at most one restart's
    // paste behaviour.
    if std::fs::symlink_metadata(crate::session::socket_path(project, name)).is_err() {
        return;
    }
    let path = path_for(project, name);
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let mut body = String::new();
    for (mode, set) in modes {
        if !crate::screen::PERSISTED.contains(mode) {
            continue;
        }
        body.push_str(&format!("{mode} {}\n", u8::from(*set)));
    }
    let tmp = dir.join(format!(".modes.{name}.tmp.{}", std::process::id()));
    if std::fs::write(&tmp, body.as_bytes()).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Drops the sidecar for a socket that has just been unlinked.
///
/// Called from the two places that remove a socket, both of which have
/// already established positive evidence that the session is over. It is
/// never called from the pump's exit path: that fires on a *detach* too,
/// where the shell is still running behind its dtach master and the contract
/// is still true.
///
/// Those two are **not** every way a session ends, which an earlier version
/// of this doc claimed. The commonest way is the user typing `exit`, and
/// **dtach unlinks its own socket** when the program exits — so neither of
/// them runs and the sidecar is orphaned. `sweep_orphans` collects those, and
/// until it existed the orphans were also what let a brand-new session
/// inherit a dead app's mode table.
pub fn forget(sock: &Path) {
    let _ = std::fs::remove_file(sidecar_of(sock));
}

/// Removes every `.modes.*` in a project's socket directory whose session is
/// positively gone, and any interrupted `.modes.*.tmp.<pid>`.
///
/// Needed because dtach unlinks its own socket on a normal exit, so the two
/// `forget` call sites miss the commonest ending. Without this the files
/// accumulate for the life of the state dir, and they also stop `reconcile`'s
/// `remove_dir` of an emptied key directory from ever succeeding.
///
/// *Positively* gone: `symlink_metadata` returning `NotFound` for the socket,
/// never `exists()`. An unreadable directory must not read as an absent
/// session — that conflation is the whole subject of CLAUDE.md's table, and
/// here it would delete the contract of a shell that is still running.
pub fn sweep_orphans(key_dir: &Path) {
    let Ok(rd) = std::fs::read_dir(key_dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(".modes.") else { continue };
        // An interrupted write: nothing will ever finish it, and no reader
        // looks at it.
        if rest.contains(".tmp.") {
            let _ = std::fs::remove_file(e.path());
            continue;
        }
        match std::fs::symlink_metadata(key_dir.join(rest)) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let _ = std::fs::remove_file(e.path());
            }
            _ => {} // present, or cannot tell: keep
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("roost-modes-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_sidecar_is_hidden_so_the_sweep_skips_it() {
        // Not cosmetic: `registry::reconcile` reads every non-dotted entry in
        // this directory as a session socket and reaps the ones nothing
        // holds. A sidecar named `main.modes` would be reaped seconds after
        // being written — and would additionally read as a *session* named
        // `main.modes` in the overview.
        let s = sidecar_of(Path::new("/state/sock/proj/main"));
        let leaf = s.file_name().unwrap().to_string_lossy().into_owned();
        assert!(leaf.starts_with('.'), "sidecar must be a dotfile, got {leaf}");
        assert_eq!(leaf, ".modes.main");
        assert_eq!(s.parent(), Path::new("/state/sock/proj/main").parent());
    }

    #[test]
    fn a_saved_table_reads_back_identically() {
        let d = tmpdir();
        let sock = d.join("main");
        let modes = vec![(2004u16, true), (1000, true), (7, false)];
        write_at(&sidecar_of(&sock), &modes);
        assert_eq!(read_from(&sidecar_of(&sock)), modes);
    }

    #[test]
    fn a_mode_outside_the_allowlist_is_refused_on_read() {
        // The load path is where this has to be enforced: the file outlives
        // the process that wrote it, so a table from a future version — or a
        // hand-edited one — must not be able to make this process replay
        // `?3l`, which clears the screen in this emulator, or `?1049h`,
        // which belongs to the screen logic and is deliberately not
        // persisted at all.
        let d = tmpdir();
        let p = d.join(".modes.main");
        std::fs::write(&p, b"2004 1\n3 0\n1049 1\n25 0\n1000 1\n").unwrap();
        assert_eq!(read_from(&p), vec![(2004, true), (1000, true)]);
    }

    #[test]
    fn junk_lines_are_skipped_without_losing_the_good_ones() {
        let d = tmpdir();
        let p = d.join(".modes.main");
        std::fs::write(&p, b"2004 1\nnonsense\n1000\n7 9\n1002 x\n1004 0\n").unwrap();
        assert_eq!(read_from(&p), vec![(2004, true), (1004, false)]);
    }

    #[test]
    fn a_missing_file_is_an_empty_table_not_an_error() {
        let d = tmpdir();
        assert!(read_from(&d.join(".modes.absent")).is_empty());
    }

    #[test]
    fn an_oversized_file_is_refused_before_it_is_read() {
        let d = tmpdir();
        let p = d.join(".modes.main");
        let mut big = String::from("2004 1\n");
        while big.len() <= MAX_BYTES as usize {
            big.push_str("1000 1\n");
        }
        std::fs::write(&p, big.as_bytes()).unwrap();
        // Refused wholesale rather than truncated: a file this size is not
        // one this module wrote, so nothing in it is trustworthy — including
        // the valid-looking first line.
        assert!(read_from(&p).is_empty());
    }

    #[test]
    fn forget_removes_the_sidecar_and_tolerates_it_being_gone() {
        let d = tmpdir();
        let sock = d.join("main");
        write_at(&sidecar_of(&sock), &[(2004, true)]);
        assert!(sidecar_of(&sock).exists());
        forget(&sock);
        assert!(!sidecar_of(&sock).exists());
        forget(&sock); // second call must not panic
    }

    #[test]
    fn a_fifo_where_the_file_belongs_does_not_block_the_pump_forever() {
        // This is what `is_file()` is actually for. A directory would fail
        // `read_to_string` anyway, so asserting on one proves nothing — the
        // first draft of this test did exactly that and passed with the type
        // check deleted. A fifo does not fail: `read_to_string` on one with
        // no writer blocks indefinitely, and this runs on the thread that
        // builds a `Session`, so the terminal would simply never open.
        //
        // Asserted with a deadline rather than by calling it directly,
        // because the regression here *hangs* instead of failing, and a hung
        // test tells you nothing until someone notices the suite stopped.
        let d = tmpdir();
        let p = d.join(".modes.main");
        let made = std::process::Command::new("mkfifo").arg(&p).status();
        match made {
            Ok(st) if st.success() => {}
            _ => return, // no mkfifo: skip rather than fail on tooling
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let probe = p.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read_from(&probe));
        });
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(v) => assert!(v.is_empty(), "a fifo must not be read as a table"),
            Err(_) => panic!("read_from blocked on a fifo — a terminal would never open"),
        }
    }

    #[test]
    fn a_symlink_pointing_at_a_valid_table_is_refused() {
        // The sidecar sits in a user-writable directory. Following a symlink
        // out of it would let anything on the filesystem that happens to
        // parse become a terminal's mode contract.
        let d = tmpdir();
        let real = d.join("real");
        std::fs::write(&real, b"2004 1\n").unwrap();
        assert_eq!(read_from(&real), vec![(2004, true)]);
        let link = d.join(".modes.main");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(read_from(&link).is_empty(), "a symlinked table must not be honoured");
    }

    /// `save` keys off the state dir, which is process-global; these tests
    /// only need the file format, so they write where they can see it.
    fn write_at(path: &Path, modes: &[(u16, bool)]) {
        let mut body = String::new();
        for (m, s) in modes {
            body.push_str(&format!("{m} {}\n", u8::from(*s)));
        }
        std::fs::write(path, body.as_bytes()).unwrap();
    }
}
