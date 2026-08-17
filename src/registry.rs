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

/// True when some live process holds this socket path. `pgrep -f` matches the
/// full command line, which is where dtach carries its socket path.
fn socket_has_process(path: &std::path::Path) -> bool {
    std::process::Command::new("pgrep")
        .arg("-f")
        .arg(path.to_string_lossy().as_ref())
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

/// Removes sockets whose process is gone and sessions whose project directory
/// no longer exists. Runs at startup and on every enumeration, so orphans
/// cannot accumulate silently the way they did before this existed.
///
/// Deliberately narrow about what it deletes: only a dead socket file (no
/// process holds it — dtach itself died and left it behind) or a live
/// session's socket file when its project directory is gone. It never
/// touches a saved-layout state file (`wsstate`'s `<key>.json`): a layout for
/// a project that has moved is still the user's data, and losing unsaved
/// buffer text embedded in it would be unrecoverable. A moved-back or
/// re-created project directory simply picks its layout back up.
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
            let live = socket_has_process(&sock.path());
            if !live {
                // No process holds this socket: safe to remove regardless of
                // whether the project still exists.
                let _ = std::fs::remove_file(sock.path());
                report.dead_sockets += 1;
                eprintln!("deadlight: reaped dead socket {}", sock.path().display());
            } else if project_gone {
                let name = sock.file_name().to_string_lossy().into_owned();
                // Raw form, not the encoded `key` — see the comment above.
                let ended = crate::session::kill_project(&url);
                let _ = std::fs::remove_file(sock.path());
                report.gone_projects += 1;
                eprintln!(
                    "deadlight: reaped session {key}/{name} — project directory is gone ({ended} in-process)"
                );
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
}
