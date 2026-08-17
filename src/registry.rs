//! The project registry: what deadlight knows about, and what is running.
//!
//! Rebuilt at startup rather than accumulated in memory, because dtach
//! sessions deliberately outlive deadlight. An in-memory-only registry would
//! forget every running shell on restart — which is exactly how nine
//! orphaned sessions for deleted directories accumulated unnoticed in
//! production on 2026-08-17.
use std::path::PathBuf;

pub struct ProjectStatus {
    /// Storage key, percent-encoded (`karpie%2Fsrc`).
    pub key: String,
    /// URL form, readable slashes (`karpie/src`).
    pub url: String,
    pub live: usize,
    pub oldest_age_secs: u64,
    pub has_layout: bool,
}

pub struct ReapReport {
    pub dead_sockets: usize,
    pub gone_projects: usize,
}

/// Inverse of the storage-key encoding used by `wsstate` and `session`.
pub fn decode_key(key: &str) -> String {
    crate::http::percent_decode(key)
}

fn sock_root() -> PathBuf {
    crate::wsstate::state_dir().join("sock")
}

/// Pids of every live process whose command line contains this socket path.
/// `pgrep -f` matches the full command line, which is where dtach carries
/// its socket path. Returns an empty vec (not a panic) on any `pgrep`
/// failure — including "no match", which `pgrep` reports as a nonzero exit.
fn socket_pids(path: &std::path::Path) -> Vec<u32> {
    let Ok(out) = std::process::Command::new("pgrep").arg("-f").arg(path.to_string_lossy().as_ref()).output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect()
}

/// True when some live process holds this socket path.
fn socket_has_process(path: &std::path::Path) -> bool {
    !socket_pids(path).is_empty()
}

/// What to do with one socket file, given what's known about it. Kept as a
/// pure function, separate from the `kill`/`pgrep`/`remove_file` calls that
/// gather its inputs, so every combination — including "a kill attempt
/// failed and the process survived" — can be exercised in a fast, ordinary
/// unit test rather than only via a real process that may or may not
/// actually die on schedule.
#[derive(Debug, PartialEq, Eq)]
enum ReapAction {
    /// Nothing holds the socket; always safe to remove.
    RemoveDeadSocket,
    /// The project is gone, a kill was attempted, and no process still
    /// holds the socket: safe to remove.
    RemoveKilled,
    /// A live process, and either the project still exists, or (project
    /// gone) a kill attempt failed and something still holds the socket.
    /// Leave the file in place: a socket that still exists remains
    /// discoverable by the next `reconcile`, so leaving it is strictly
    /// safer than deleting the only path back to an orphaned process.
    Leave,
}

fn reap_decision(project_gone: bool, held_before: bool, held_after_kill_attempt: bool) -> ReapAction {
    if !held_before {
        return ReapAction::RemoveDeadSocket;
    }
    if !project_gone {
        return ReapAction::Leave;
    }
    if held_after_kill_attempt {
        ReapAction::Leave
    } else {
        ReapAction::RemoveKilled
    }
}

/// Removes sockets whose process is gone and sessions whose project directory
/// no longer exists. Runs at startup and on every enumeration, so orphans
/// cannot accumulate silently the way they did before this existed.
///
/// Deliberately narrow about what it deletes: only a dead socket file (no
/// process holds it — dtach itself died and left it behind) or a live
/// session's socket file when its project directory is gone *and* the
/// process(es) holding that socket were actually killed and confirmed gone.
/// It never touches a saved-layout state file (`wsstate`'s `<key>.json`): a
/// layout for a project that has moved is still the user's data, and losing
/// unsaved buffer text embedded in it would be unrecoverable. A moved-back or
/// re-created project directory simply picks its layout back up.
///
/// The kill-then-confirm step for the project-gone case matters because this
/// function's primary caller is startup: `session::kill_project` only ends
/// sessions present in *this* process's in-memory map, which is empty right
/// after a restart. A `dtach` session that outlived the previous deadlight
/// process has no map entry, so without directly signalling its pid, the
/// socket file would be deleted while the process kept running — a shell
/// that is now both unreachable (no socket to attach through) and
/// unreapable (no socket for the next `reconcile` to find it by). That is
/// worse than the orphan accumulation this function exists to fix, so the
/// socket is removed only once the pid(s) are confirmed dead; if a kill
/// fails or a process survives, the socket is left in place on purpose.
pub fn reconcile(roots: &[PathBuf]) -> ReapReport {
    let mut report = ReapReport { dead_sockets: 0, gone_projects: 0 };
    let Ok(rd) = std::fs::read_dir(sock_root()) else { return report };
    for entry in rd.flatten() {
        // Directory entry names under sock/ are storage keys (percent-encoded:
        // `karpie%2Fsrc`), not the raw project form. `session`'s functions
        // take the raw form and encode internally via storage_key to build
        // their map key, so passing the encoded `key` straight through would
        // double-encode a nested project's `%` and match nothing — the exact
        // invisibility bug this registry exists to prevent. `url` (the
        // decoded, raw form) is what every `session::*` call below must use.
        let key = entry.file_name().to_string_lossy().into_owned();
        let url = decode_key(&key);
        let project_gone = crate::projects::resolve_project(roots, &url).is_none();
        let Ok(inner) = std::fs::read_dir(entry.path()) else { continue };
        for sock in inner.flatten() {
            let pids = socket_pids(&sock.path());
            let held_before = !pids.is_empty();
            let name = sock.file_name().to_string_lossy().into_owned();

            if held_before && project_gone {
                // In-process first (handles a session created and orphaned
                // within this same run), then the OS-level pids directly —
                // that second step is what makes the startup case work,
                // where the in-memory map is empty and kill_project alone
                // is a no-op.
                let in_process_ended = crate::session::kill_project(&url);
                for pid in &pids {
                    let _ =
                        std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
                }
                // Bounded wait for the kill to take effect rather than an
                // instant recheck: SIGKILL is not synchronous. Bounded so a
                // process that refuses to die can't hang reconcile forever.
                let mut still_alive = socket_has_process(&sock.path());
                for _ in 0..20 {
                    if !still_alive {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                    still_alive = socket_has_process(&sock.path());
                }
                match reap_decision(project_gone, held_before, still_alive) {
                    ReapAction::RemoveKilled => {
                        let _ = std::fs::remove_file(sock.path());
                        report.gone_projects += 1;
                        eprintln!(
                            "deadlight: reaped session {key}/{name} — project directory is gone (killed pids {pids:?}, {in_process_ended} in-process)"
                        );
                    }
                    ReapAction::Leave => {
                        eprintln!(
                            "deadlight: could not end session {key}/{name} for a missing project — pids {pids:?} did not die; socket left in place so it stays discoverable"
                        );
                    }
                    ReapAction::RemoveDeadSocket => unreachable!("held_before is true here"),
                }
                continue;
            }

            match reap_decision(project_gone, held_before, held_before) {
                ReapAction::RemoveDeadSocket => {
                    // No process holds this socket: safe to remove
                    // regardless of whether the project still exists.
                    let _ = std::fs::remove_file(sock.path());
                    report.dead_sockets += 1;
                    eprintln!("deadlight: reaped dead socket {}", sock.path().display());
                }
                ReapAction::Leave => {} // live process, project still exists: untouched
                ReapAction::RemoveKilled => unreachable!("only reached via the project_gone branch above"),
            }
        }
        // An emptied directory is noise; ignore failure when it is not empty
        // (a live session for a project that still exists must survive).
        let _ = std::fs::remove_dir(entry.path());
    }
    report
}

/// Every project deadlight knows about: those with a saved layout, those with
/// live sessions, and those with both.
pub fn known_projects(roots: &[PathBuf]) -> Vec<ProjectStatus> {
    let _ = reconcile(roots);
    let mut by_key: std::collections::BTreeMap<String, ProjectStatus> = Default::default();

    if let Ok(rd) = std::fs::read_dir(crate::wsstate::state_dir()) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(key) = name.strip_suffix(".json") else { continue };
            by_key.insert(
                key.to_string(),
                ProjectStatus {
                    key: key.to_string(),
                    url: decode_key(key),
                    live: 0,
                    oldest_age_secs: 0,
                    has_layout: true,
                },
            );
        }
    }

    if let Ok(rd) = std::fs::read_dir(sock_root()) {
        for e in rd.flatten() {
            // `key` is the encoded directory name; `session::list_sessions`
            // takes the raw form and encodes internally, so it must be
            // decoded here too, or a nested project's sessions never match
            // and the project shows idle while shells are actually running.
            let key = e.file_name().to_string_lossy().into_owned();
            let sessions = crate::session::list_sessions(&decode_key(&key));
            let live = sessions.len();
            let oldest = sessions.iter().map(|s| s.age_secs).max().unwrap_or(0);
            let slot = by_key.entry(key.clone()).or_insert(ProjectStatus {
                key: key.clone(),
                url: decode_key(&key),
                live: 0,
                oldest_age_secs: 0,
                has_layout: false,
            });
            slot.live = live;
            slot.oldest_age_secs = oldest;
        }
    }

    by_key.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn with_state<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path());
        let out = f(d.path());
        std::env::remove_var("DEADLIGHT_STATE_DIR");
        out
    }

    #[test]
    fn decode_key_reverses_the_storage_encoding() {
        assert_eq!(decode_key("karpie"), "karpie");
        assert_eq!(decode_key("karpie%2Fsrc"), "karpie/src");
        assert_eq!(decode_key("a%2Fb%2Fc"), "a/b/c");
    }

    #[test]
    fn a_saved_layout_alone_makes_a_project_known_but_idle() {
        with_state(|state| {
            fs::create_dir_all(state).unwrap();
            fs::write(state.join("karpie.json"), "{}").unwrap();
            let roots = vec![PathBuf::from("/nonexistent-root")];
            let ps = known_projects(&roots);
            let p = ps.iter().find(|p| p.key == "karpie").expect("saved layout must be listed");
            assert!(p.has_layout);
            assert_eq!(p.live, 0, "no sessions means idle, not live");
            assert_eq!(p.url, "karpie");
        });
    }

    #[test]
    fn a_socket_with_no_live_process_is_reaped() {
        with_state(|state| {
            let sock = state.join("sock/ghost");
            fs::create_dir_all(&sock).unwrap();
            // A plain file stands in for a stale socket: no dtach process holds it.
            fs::write(sock.join("shell"), "").unwrap();
            let report = reconcile(&[PathBuf::from("/nonexistent-root")]);
            assert!(report.dead_sockets >= 1, "a socket with no process must be removed");
            assert!(!sock.join("shell").exists(), "the stale socket file must be gone");
        });
    }

    #[test]
    fn nested_project_keys_produce_slashed_urls() {
        with_state(|state| {
            fs::create_dir_all(state).unwrap();
            fs::write(state.join("karpie%2Fsrc.json"), "{}").unwrap();
            let ps = known_projects(&[PathBuf::from("/nonexistent-root")]);
            let p = ps.iter().find(|p| p.key == "karpie%2Fsrc").expect("nested project must list");
            assert_eq!(p.url, "karpie/src", "the URL keeps readable slashes");
        });
    }

    // Exhaustive over every (project_gone, held_before, held_after_kill)
    // combination reap_decision can actually be called with by `reconcile`
    // (held_after_kill is only meaningful when held_before is true), so the
    // "kill failed, keep the socket" path is proven without needing a
    // process that refuses to die on cue.
    #[test]
    fn reap_decision_never_deletes_a_socket_whose_process_survived() {
        // No process holds it: always safe to remove, regardless of project.
        assert_eq!(reap_decision(true, false, false), ReapAction::RemoveDeadSocket);
        assert_eq!(reap_decision(false, false, false), ReapAction::RemoveDeadSocket);
        // Live process, project still exists: never touched.
        assert_eq!(reap_decision(false, true, true), ReapAction::Leave);
        // Live process, project gone, kill confirmed successful: safe to remove.
        assert_eq!(reap_decision(true, true, false), ReapAction::RemoveKilled);
        // Live process, project gone, but something still holds the socket
        // after the kill attempt (kill failed, or it hasn't died yet): the
        // socket must survive so the next reconcile can still find it.
        assert_eq!(
            reap_decision(true, true, true),
            ReapAction::Leave,
            "a failed kill must never lose the only path back to an orphaned process"
        );
    }

    // Regression for the startup case: a session that outlived a previous
    // deadlight process has no entry in `session`'s in-memory map, so
    // `session::kill_project` alone is a no-op. Uses a real `dtach` process
    // (a runtime prerequisite of this project, see README/docs/deploy.md) so
    // this actually proves the OS-level pid is killed, not just forgotten
    // about by having its socket file deleted out from under it.
    #[test]
    fn a_live_session_for_a_deleted_project_is_actually_killed_not_just_forgotten() {
        with_state(|state| {
            let sock_dir = state.join("sock/ghost-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            // `dtach -n` creates the socket, forks a detached session running
            // `sleep 30`, and its own foreground process exits once that
            // setup is done — no in-memory `session::SESSIONS` entry is ever
            // created, mirroring exactly what a restart leaves behind.
            let status = std::process::Command::new("dtach")
                .args(["-n", sock_path.to_str().unwrap(), "sleep", "30"])
                .status();
            let Ok(status) = status else {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            };
            assert!(status.success(), "dtach -n setup must succeed for this test to mean anything");
            assert!(
                socket_has_process(&sock_path),
                "test setup: the detached process must be observable via pgrep before reconcile runs"
            );

            let report = reconcile(&[PathBuf::from("/nonexistent-root")]);

            assert_eq!(report.gone_projects, 1, "the gone-project session must be counted as reaped");
            assert!(
                !socket_has_process(&sock_path),
                "the dtach process itself must be killed, not merely forgotten by deleting its socket"
            );
            assert!(!sock_path.exists(), "the socket must be removed only once the process is confirmed gone");
        });
    }
}
