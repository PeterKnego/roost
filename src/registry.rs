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

/// Inverse of the storage-key encoding used by `wsstate` and `session`. Not
/// `http::percent_decode` — see `projects::decode_storage_key`'s doc comment
/// for why that general form-decoder is the wrong inverse for this specific
/// encoding (it would mangle a project literally named `gtk+`).
pub fn decode_key(key: &str) -> String {
    crate::projects::decode_storage_key(key)
}

fn sock_root() -> PathBuf {
    crate::wsstate::state_dir().join("sock")
}

/// One snapshot of every process's pid and its raw command-line text, taken
/// with a single `ps` call. Passed around so that sweeping every socket
/// under `sock/` to see which are still held costs one subprocess total, not
/// one per socket. Deliberately not `pgrep -f`: that treats its pattern as
/// an extended regex matched anywhere in the command line, which is wrong
/// two different ways for a filesystem path — (1) a project name may contain
/// regex metacharacters (`valid_project` allows them; only *session* names
/// are restricted to `[A-Za-z0-9_-]`), which can make the "pattern" an
/// invalid ERE and silently match nothing, so a live socket's process looks
/// dead; and (2) unanchored substring matching means session `claude`'s
/// socket path matches inside session `claude-2`'s command line too, so an
/// actually-dead `claude` socket is never reaped once `claude-2` exists.
/// `pids_holding` matches each raw line as plain text (no regex ever built
/// from untrusted input) and requires the match be a whole argument, which
/// avoids both hazards without needing to split the line into words first —
/// splitting on whitespace would break on a socket path that itself contains
/// a space, which a project directory name may (`list_projects`,
/// `resolve_project`, and `valid_project` all permit spaces; only *session*
/// names are restricted).
fn process_snapshot() -> Vec<(u32, String)> {
    let Ok(out) = std::process::Command::new("ps").args(["-Ao", "pid=,args="]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let (pid_str, args) = trimmed.split_once(char::is_whitespace)?;
            let pid: u32 = pid_str.parse().ok()?;
            Some((pid, args.trim_start().to_string()))
        })
        .collect()
}

/// True when `target` appears in `line` as one whole command-line argument:
/// bounded by whitespace, or the line's own start/end, on both sides — never
/// as a fragment of a longer word, but with any internal spaces `target`
/// itself contains left intact. Plain substring matching alone (a bare
/// `line.contains(target)`) would have the same prefix-collision hazard as
/// `pgrep -f` (`claude` inside `claude-2`); splitting the line into words
/// first (`==` against each) would instead break on a `target` that itself
/// contains a space, silently treating a live socket as unheld — see
/// `process_snapshot`'s doc comment for why that matters here. This is the
/// middle path: a bounded substring search.
fn line_has_whole_arg(line: &str, target: &str) -> bool {
    if target.is_empty() {
        return false;
    }
    line.match_indices(target).any(|(start, matched)| {
        let end = start + matched.len();
        let before_ok = start == 0 || line.as_bytes()[start - 1].is_ascii_whitespace();
        let after_ok = end == line.len() || line.as_bytes()[end].is_ascii_whitespace();
        before_ok && after_ok
    })
}

/// Pids in `snapshot` whose command line has this socket path as one whole
/// argument (see `line_has_whole_arg`). `dtach` always carries the socket
/// path as a single, whole argument (see `session::default_command`) —
/// never embedded in a larger word.
fn pids_holding(snapshot: &[(u32, String)], path: &std::path::Path) -> Vec<u32> {
    let target = path.to_string_lossy();
    snapshot.iter().filter(|(_, line)| line_has_whole_arg(line, &target)).map(|(pid, _)| *pid).collect()
}

/// True when some live process holds this socket path right now. Takes its
/// own fresh snapshot — used for one-off checks (tests, and re-polling after
/// a kill to see whether it has taken effect yet), where the whole point is
/// to observe current state rather than a sweep's frozen-in-time one.
fn socket_has_process(path: &std::path::Path) -> bool {
    !pids_holding(&process_snapshot(), path).is_empty()
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
///
/// "Cannot verify whether a project exists" must never be treated as
/// "the project is gone": `resolve_project` returns `None` for every
/// project alike when a root fails to canonicalize (a mount not up yet, a
/// permissions problem, a `DEADLIGHT_ROOTS` typo, or just running on a host
/// where a configured root doesn't exist there). Without a guard, that
/// transient condition would make this function SIGKILL every live session
/// on the machine — strictly worse than doing nothing, for a tool whose
/// whole premise is that sessions survive a restart. So the project-gone
/// branch only ever runs once *every* configured root is confirmed
/// reachable — not merely one of them (the default deploy config ships two
/// roots; if only one of them is a mount that isn't up, every session under
/// the down root must not be declared gone just because the other root
/// resolved fine) — otherwise every socket is left alone (dead-socket
/// reaping, which needs no project information, still runs).
pub fn reconcile(roots: &[PathBuf]) -> ReapReport {
    let mut report = ReapReport { dead_sockets: 0, gone_projects: 0 };
    let roots_ok = !roots.is_empty() && roots.iter().all(|r| r.canonicalize().is_ok());
    if !roots_ok {
        eprintln!(
            "deadlight: none of the configured roots could be read ({roots:?}) — \
             reaping sessions for missing projects is suspended this pass"
        );
    }
    let Ok(rd) = std::fs::read_dir(sock_root()) else { return report };
    // One process listing for the whole sweep (see process_snapshot's doc
    // comment) rather than one `ps` per socket file.
    let snapshot = process_snapshot();
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
        let project_gone = roots_ok && crate::projects::resolve_project(roots, &url).is_none();
        let Ok(inner) = std::fs::read_dir(entry.path()) else { continue };
        for sock in inner.flatten() {
            let pids = pids_holding(&snapshot, &sock.path());
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
                    // `held_before` came from `snapshot`, taken once before
                    // this whole sweep for efficiency — by the time this
                    // particular socket is reached (especially after another
                    // socket's kill-confirmation sleep earlier in the same
                    // pass) it can be stale, and a new process may have
                    // attached since. Re-check right now, immediately before
                    // deleting, rather than trust the sweep's frozen-in-time
                    // view: a socket that survives one sweep costs nothing,
                    // but a live one deleted out from under a fresh session
                    // is unrecoverable. This matters once this runs on more
                    // than just startup — `known_projects` (below) already
                    // calls `reconcile` on every enumeration, by design.
                    if !socket_has_process(&sock.path()) {
                        let _ = std::fs::remove_file(sock.path());
                        report.dead_sockets += 1;
                        eprintln!("deadlight: reaped dead socket {}", sock.path().display());
                    }
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
            // `reconcile` (just run, above) guarantees every socket file
            // still here is process-backed: a socket with no process is
            // reaped outright, and a live one for a since-deleted project is
            // killed before its socket is removed. So right after a
            // restart — when this process's in-memory session map is empty
            // and `list_sessions` can only ever report 0 — the number of
            // socket files under this key is a truthful floor for "how many
            // are live", even though it can't yet supply real ages or
            // attachment counts. Once something in this process actually
            // attaches, the in-memory list becomes authoritative again.
            let (live, oldest) = if !sessions.is_empty() {
                (sessions.len(), sessions.iter().map(|s| s.age_secs).max().unwrap_or(0))
            } else {
                let floor = std::fs::read_dir(e.path()).map(|rd| rd.flatten().count()).unwrap_or(0);
                (floor, 0)
            };
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
        // Regression: http::percent_decode (a form decoder) would turn `+`
        // into a space; decode_key must not, or a project literally named
        // `gtk+` would look gone and have its live session killed.
        assert_eq!(decode_key("gtk+"), "gtk+");
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

    /// Spawns `dtach -n <sock_path> sleep 30`: it creates the socket, forks a
    /// detached session, and its own foreground process exits once that
    /// setup is done — so `Command::status()` returning is itself the
    /// synchronization point (no arbitrary sleep needed to wait for the
    /// socket to appear), and no in-memory `session::SESSIONS` entry is ever
    /// created, mirroring exactly what a session that outlived a previous
    /// deadlight process leaves behind. Returns `None` (rather than
    /// panicking) if `dtach` genuinely isn't installed, since its own setup
    /// step could not have succeeded either. `dtach` is a hard runtime
    /// prerequisite of this project (see README.md, docs/deploy.md), so this
    /// isn't introducing a new kind of test dependency.
    fn spawn_detached_dtach(sock_path: &std::path::Path) -> Option<()> {
        let status = std::process::Command::new("dtach")
            .args(["-n", sock_path.to_str().unwrap(), "sleep", "30"])
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        Some(())
    }

    // Regression for the startup case: a session that outlived a previous
    // deadlight process has no entry in `session`'s in-memory map, so
    // `session::kill_project` alone is a no-op. Uses a real `dtach` process
    // so this actually proves the OS-level pid is killed, not just forgotten
    // about by having its socket file deleted out from under it. Roots point
    // at a real, existing directory that genuinely does not contain
    // "ghost-proj" — not a nonexistent path — so `project_gone` becomes true
    // for the right reason (the project really isn't there) rather than by
    // accident of the C3 "roots unverifiable" guard also forcing it false.
    #[test]
    fn a_live_session_for_a_deleted_project_is_actually_killed_not_just_forgotten() {
        with_state(|state| {
            let sock_dir = state.join("sock/ghost-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            let root = tempfile::tempdir().unwrap(); // exists, but has no "ghost-proj" child

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }
            assert!(
                socket_has_process(&sock_path),
                "test setup: the detached process must be observable before reconcile runs"
            );

            let report = reconcile(&[root.path().to_path_buf()]);

            assert_eq!(report.gone_projects, 1, "the gone-project session must be counted as reaped");
            assert!(
                !socket_has_process(&sock_path),
                "the dtach process itself must be killed, not merely forgotten by deleting its socket"
            );
            assert!(!sock_path.exists(), "the socket must be removed only once the process is confirmed gone");
        });
    }

    // I5: the end-to-end guard the review asked for. Without this, a future
    // change to the project_gone condition could start deleting live
    // sockets for projects that still exist with every other test green.
    #[test]
    fn a_live_session_for_a_project_that_still_exists_is_never_touched() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("still-here")).unwrap();
            let sock_dir = state.join("sock/still-here");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }
            assert!(socket_has_process(&sock_path), "test setup: process must be observable");

            let report = reconcile(&[root.path().to_path_buf()]);

            assert_eq!(report.gone_projects, 0, "the project still exists; nothing should be reaped");
            assert!(sock_path.exists(), "the socket of a live session for an existing project must survive");
            assert!(
                socket_has_process(&sock_path),
                "the process itself must survive — the project was never gone"
            );

            // Clean up: this process was never in any in-memory map, so
            // nothing else will ever end it.
            for pid in pids_holding(&process_snapshot(), &sock_path) {
                let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        });
    }

    // C3: a root that cannot be verified must never be mistaken for "the
    // project is gone". Without this guard, a transiently unreadable mount
    // or a DEADLIGHT_ROOTS typo would make startup reconcile SIGKILL every
    // live session on the machine.
    #[test]
    fn unreadable_roots_suspend_gone_project_reaping_but_dead_sockets_still_reap() {
        with_state(|state| {
            let sock_dir = state.join("sock/whatever-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }

            // Every root fails to canonicalize: roots_ok must be false.
            let report = reconcile(&[PathBuf::from("/nonexistent-root-a"), PathBuf::from("/nonexistent-root-b")]);

            assert_eq!(report.gone_projects, 0, "must not reap anything when roots can't be verified");
            assert!(sock_path.exists(), "a live session must survive when its project can't be checked");
            assert!(socket_has_process(&sock_path), "the process itself must survive");

            for pid in pids_holding(&process_snapshot(), &sock_path) {
                let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }

            // Dead-socket reaping needs no project information at all, and
            // must still work even while gone-project reaping is suspended.
            let dead = state.join("sock/other-proj");
            fs::create_dir_all(&dead).unwrap();
            fs::write(dead.join("shell"), "").unwrap();
            let report2 = reconcile(&[PathBuf::from("/nonexistent-root-a")]);
            assert!(report2.dead_sockets >= 1, "a socket with no process must still be reaped");
        });
    }

    // I4: after a restart, `session::list_sessions` can only ever report 0
    // (its map is in-memory and starts empty), but `reconcile` guarantees
    // every socket file left standing is process-backed — so `live` must
    // reflect that floor rather than falsely reporting an active project as
    // idle, which is exactly the invisibility this whole module exists to
    // remove.
    #[test]
    fn known_projects_reports_a_live_floor_from_socket_files_when_the_in_memory_map_is_empty() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("survivor")).unwrap();
            let sock_dir = state.join("sock/survivor");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }

            let ps = known_projects(&[root.path().to_path_buf()]);
            let p = ps.iter().find(|p| p.key == "survivor").expect("survivor must be listed");
            assert_eq!(
                p.live, 1,
                "a process-backed socket with no in-memory record must still count as live, not idle"
            );

            for pid in pids_holding(&process_snapshot(), &sock_path) {
                let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        });
    }

    // C4 regression: a project directory name may contain a space
    // (list_projects/resolve_project/valid_project all permit it; only
    // *session* names are restricted). storage_key doesn't escape spaces,
    // so the socket directory name is the literal "my project". A
    // word-split match can never equal a target containing a space, so this
    // is the direct proof that matching must not first split the `ps` line
    // on whitespace.
    #[test]
    fn a_socket_path_containing_a_space_is_still_correctly_matched() {
        with_state(|state| {
            let sock_dir = state.join("sock/my project");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }

            assert!(
                socket_has_process(&sock_path),
                "a socket path containing a space must still be recognized as held"
            );

            for pid in pids_holding(&process_snapshot(), &sock_path) {
                let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        });
    }

    // Same regression, exercised end to end through `reconcile`: before the
    // fix, a spaced socket path's `held_before` came back false, so it fell
    // into the *unconditional* dead-socket branch and was deleted without
    // ever attempting a kill first — worse than a plain missed reap, since
    // it happened even though the process was alive and the project existed.
    #[test]
    fn a_live_session_for_a_spaced_project_name_survives_reconcile() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("my project")).unwrap();
            let sock_dir = state.join("sock/my project");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }

            let report = reconcile(&[root.path().to_path_buf()]);

            assert_eq!(report.dead_sockets, 0, "a live, spaced-path socket must not look dead");
            assert_eq!(report.gone_projects, 0, "the project exists; nothing should be reaped");
            assert!(sock_path.exists(), "the socket of a live session must survive");
            assert!(socket_has_process(&sock_path), "the process itself must survive");

            for pid in pids_holding(&process_snapshot(), &sock_path) {
                let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        });
    }

    // I6 regression: the default deploy config ships two roots. `.any()`
    // let one resolving root paper over a second, genuinely-down one, so
    // every session under the down root looked gone and got SIGKILLed —
    // C3's failure mode, just narrowed to a partial outage instead of a
    // total one. `.all()` must refuse to treat any project as gone unless
    // every configured root is verifiable.
    #[test]
    fn one_unreadable_root_among_several_still_suspends_gone_project_reaping() {
        with_state(|state| {
            let good_root = tempfile::tempdir().unwrap(); // resolves fine
            let sock_dir = state.join("sock/partial-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }

            let roots =
                vec![good_root.path().to_path_buf(), PathBuf::from("/nonexistent-root-partial")];
            let report = reconcile(&roots);

            assert_eq!(
                report.gone_projects, 0,
                "one unreadable root among several must still suspend gone-project reaping"
            );
            assert!(sock_path.exists());
            assert!(socket_has_process(&sock_path));

            for pid in pids_holding(&process_snapshot(), &sock_path) {
                let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        });
    }

    // R8: the primitive `reconcile`'s dead-socket branch now rechecks with,
    // immediately before every delete, must reflect state *at call time* —
    // not some memoized value from an earlier snapshot in the same sweep —
    // or the recheck would be no fix at all. A true reproduction of the
    // race itself (a process attaching to a specific socket in the exact
    // window between the sweep's one-time snapshot and reconcile reaching
    // that socket) isn't practical to make deterministic from a black-box
    // test without internal hooks into reconcile's loop; this instead pins
    // down the property the fix depends on: two calls bracketing a process
    // actually attaching must disagree.
    #[test]
    fn socket_has_process_reflects_state_at_call_time_not_a_stale_snapshot() {
        with_state(|state| {
            let sock_dir = state.join("sock/timing-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            assert!(!socket_has_process(&sock_path), "nothing holds this path yet");

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }

            assert!(
                socket_has_process(&sock_path),
                "a fresh check must observe a process that just attached, not a cached earlier answer"
            );

            for pid in pids_holding(&process_snapshot(), &sock_path) {
                let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        });
    }
}
