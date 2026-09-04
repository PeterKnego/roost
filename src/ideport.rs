//! Which TCP port a project's IDE listener last bound.
//!
//! `ide::start_in` binds port 0 and takes whatever the OS gives it, so every
//! roost start hands every project a different port. That is invisible until
//! you notice what else it means: `session_env` bakes `CLAUDE_CODE_SSE_PORT`
//! into a shell at spawn time and dtach sessions outlive roost, so a
//! surviving shell holds a port number that a *later* roost start can hand to
//! a different project. Claude Code matches a lock file by port **before** it
//! tries to match by path (`gBt` in the 2.1.260 bundle: `else if (v.port ===
//! r) R = true`), so that Claude would connect to the other project's
//! listener, with the other project's token, and never compare a path at all.
//!
//! Remembering the port removes the draw. This is state roost writes about
//! itself, not configuration: no config key, no per-project override.
//!
//! Advisory, never authoritative. A recorded port that cannot be bound is not
//! an error — the caller falls back to an OS-assigned one and records that
//! instead. Two roosts sharing a `ROOST_STATE_DIR` will alternate, which is
//! the correct outcome for a hint.
use std::path::{Path, PathBuf};

fn record_path(dir: &Path, project: &str) -> PathBuf {
    dir.join(format!("{}.port", crate::projects::storage_key(project)))
}

/// The recorded port, or `None` when there is no usable hint.
///
/// Every failure is `None`: no file, an unreadable one, a value that is not a
/// number, one outside `u16`, or `0`. A hint roost cannot make sense of is
/// worth exactly as much as no hint, and the caller's fallback is the same in
/// both cases — so unlike the `/proc` readers elsewhere in this crate there is
/// no third outcome to preserve here. Nothing destructive hangs off it.
pub fn recorded_in(dir: &Path, project: &str) -> Option<u16> {
    let raw = std::fs::read_to_string(record_path(dir, project)).ok()?;
    // Revert-checked: letting `Ok(0)` through here (i.e. `Err(_) => None, Ok(p)
    // => Some(p)`) makes `port_zero_is_never_recorded_and_never_returned` fail
    // with assertion message: `assertion \`left == right\` failed
    //   left: Some(0)
    //  right: None`
    match raw.trim().parse::<u16>() {
        Ok(0) | Err(_) => None,
        Ok(p) => Some(p),
    }
}

/// Records `port`, atomically. Best-effort: a failure means the next start
/// falls back to an OS-assigned port, which is exactly today's behaviour.
///
/// Temp file with a **pid-unique** name, then `rename` — `registry::write_origin`'s
/// pattern and for its reason: two roosts sharing one `ROOST_STATE_DIR` is a
/// supported configuration, and a shared temp name would let one process's
/// `rename` publish the other's half-written file.
pub fn record_in(dir: &Path, project: &str, port: u16) {
    // 0 is "the OS chooses" and is never a hint worth keeping.
    if port == 0 {
        return;
    }
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = dir.join(format!(".{}.port.tmp.{}", crate::projects::storage_key(project), std::process::id()));
    if std::fs::write(&tmp, format!("{port}\n")).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if std::fs::rename(&tmp, record_path(dir, project)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_port_reads_back() {
        let d = tempfile::tempdir().unwrap();
        record_in(d.path(), "karpie", 45123);
        assert_eq!(recorded_in(d.path(), "karpie"), Some(45123));
    }

    #[test]
    fn no_record_is_none_and_creates_nothing() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(recorded_in(d.path(), "never-seen"), None);
        assert!(!d.path().join("never-seen.port").exists());
    }

    #[test]
    fn a_nested_project_key_is_percent_encoded_not_a_directory() {
        // `karpie/src` must not become a `karpie/` subdirectory, and must not
        // collide with a project literally named `karpie%2Fsrc`.
        let d = tempfile::tempdir().unwrap();
        record_in(d.path(), "karpie/src", 40001);
        assert_eq!(recorded_in(d.path(), "karpie/src"), Some(40001));
        assert!(d.path().join("karpie%2Fsrc.port").exists());
        assert!(!d.path().join("karpie").exists());
    }

    #[test]
    fn a_later_record_replaces_an_earlier_one() {
        let d = tempfile::tempdir().unwrap();
        record_in(d.path(), "karpie", 1111);
        record_in(d.path(), "karpie", 2222);
        assert_eq!(recorded_in(d.path(), "karpie"), Some(2222));
    }

    #[test]
    fn an_unparseable_record_is_none_not_a_panic() {
        // A hand-edited or truncated file must degrade to "no hint", never
        // abort a project from opening. Asserted on the value, not merely
        // that it did not panic.
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path()).unwrap();
        std::fs::write(d.path().join("karpie.port"), b"not a port\n").unwrap();
        assert_eq!(recorded_in(d.path(), "karpie"), None);
        std::fs::write(d.path().join("karpie.port"), b"99999999\n").unwrap();
        assert_eq!(recorded_in(d.path(), "karpie"), None, "out of u16 range is not a port");
    }

    #[test]
    fn port_zero_is_never_recorded_and_never_returned() {
        // 0 means "let the OS choose". Recording it would make the next start
        // "restore" a meaningless hint, and returning it would make the caller
        // bind ephemeral while believing it restored something.
        let d = tempfile::tempdir().unwrap();
        record_in(d.path(), "karpie", 0);
        assert_eq!(recorded_in(d.path(), "karpie"), None);
        std::fs::write(d.path().join("other.port"), b"0\n").unwrap();
        assert_eq!(recorded_in(d.path(), "other"), None);
    }

    #[test]
    fn writing_leaves_no_temp_file_behind() {
        let d = tempfile::tempdir().unwrap();
        record_in(d.path(), "karpie", 45123);
        let leftovers: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "karpie.port")
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
    }
}
