//! The lock file that tells Claude Code this project has an IDE socket.
//!
//! Claude Code discovers IDEs by scanning `~/.claude/ide/*.lock`; the filename
//! is the port and the contents carry the token that authenticates the
//! socket. Two properties are load-bearing and neither is obvious.
//!
//! The write is atomic because the CLI *deletes* any lock file it cannot
//! parse. A half-written file therefore does not degrade the integration, it
//! silently unregisters it — so a reader must never see a partial one.
//!
//! Removal only ever touches the path this process wrote. The directory is
//! shared with every other IDE on the host: a sweep of "stale-looking"
//! entries would unlink a live IntelliJ's registration the moment a check
//! failed, which is exactly the class of defect CLAUDE.md's table is about.
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};

/// `$CLAUDE_CONFIG_DIR/ide` when set — the CLI honours the same override, so
/// a user who relocated their Claude config still finds us.
pub fn ide_dir() -> PathBuf {
    if let Ok(d) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d).join("ide");
        }
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude/ide")
}

/// 128 bits from the OS CSPRNG, hex-encoded — the same shape the CLI's own
/// extensions use. `/dev/urandom` rather than a dependency: this is the only
/// randomness resh needs, and the deploy target is Linux.
pub fn new_token() -> Result<String, String> {
    let mut f = std::fs::File::open("/dev/urandom").map_err(|e| format!("no CSPRNG: {e}"))?;
    let mut buf = [0u8; 16];
    f.read_exact(&mut buf).map_err(|e| format!("short read from /dev/urandom: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn temp_name(port: u16) -> String {
    // Pid-unique: two resh instances share this directory, and a shared temp
    // name lets one truncate the other's in-flight write.
    format!(".{}.{}.resh.tmp", port, std::process::id())
}

pub struct Lock {
    path: PathBuf,
}

impl Lock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Exactly this path. Never a scan of the directory.
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn write_in(dir: &Path, port: u16, token: &str, workspace: &Path) -> Result<Lock, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let body = serde_json::json!({
        "pid": std::process::id(),
        "workspaceFolders": [workspace.to_string_lossy()],
        "ideName": "resh",
        "transport": "ws",
        "authToken": token,
    })
    .to_string();
    let tmp = dir.join(temp_name(port));
    let path = dir.join(format!("{port}.lock"));
    let mut f = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })?;
    Ok(Lock { path })
}

pub fn write(port: u16, token: &str, workspace: &Path) -> Result<Lock, String> {
    write_in(&ide_dir(), port, token, workspace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_thirty_two_hex_chars_and_not_a_constant() {
        let a = new_token().expect("/dev/urandom must be readable");
        let b = new_token().unwrap();
        assert_eq!(a.len(), 32, "128 bits, hex-encoded");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // Reverting to a fixed token passes every other test in this file;
        // only this assertion fails.
        assert_ne!(a, b, "two tokens must differ or the CSPRNG is not being read");
    }

    #[test]
    fn the_lock_file_carries_what_the_cli_reads_out_of_it() {
        let d = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let lock = write_in(d.path(), 5599, "cafe", ws.path()).unwrap();
        assert_eq!(lock.path().file_name().unwrap(), "5599.lock", "the CLI parses the port out of the filename");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(lock.path()).unwrap()).unwrap();
        assert_eq!(v["pid"], serde_json::json!(std::process::id()));
        assert_eq!(v["transport"], "ws");
        assert_eq!(v["authToken"], "cafe");
        assert_eq!(v["ideName"], "resh");
        assert_eq!(v["workspaceFolders"], serde_json::json!([ws.path().to_str().unwrap()]));
    }

    #[test]
    fn writing_leaves_no_temp_file_behind_and_replaces_an_existing_one() {
        let d = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("5599.lock"), "stale garbage").unwrap();
        let lock = write_in(d.path(), 5599, "cafe", ws.path()).unwrap();
        let names: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["5599.lock".to_string()], "a temp file must not survive the write");
        assert!(std::fs::read_to_string(lock.path()).unwrap().contains("cafe"));
    }

    #[test]
    fn the_temp_name_is_unique_per_process_so_two_resh_instances_cannot_collide() {
        // Two processes writing the same port is impossible (the OS assigned
        // it), but two processes writing *different* ports into one directory
        // is the normal case, and a shared temp name would let one truncate
        // the other's in-flight write.
        let d = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        assert!(temp_name(5599).contains(&std::process::id().to_string()));
        assert_ne!(temp_name(5599), temp_name(5600));
        let _ = write_in(d.path(), 5599, "cafe", ws.path()).unwrap();
    }

    #[test]
    fn dropping_removes_our_lock_and_leaves_a_strangers_alone() {
        let d = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        // A real IntelliJ's registration, in the directory we share with it.
        let foreign = d.path().join("4711.lock");
        std::fs::write(&foreign, "{}").unwrap();
        {
            let _lock = write_in(d.path(), 5599, "cafe", ws.path()).unwrap();
            assert!(d.path().join("5599.lock").exists());
        }
        assert!(!d.path().join("5599.lock").exists(), "our own lock must go on drop");
        // Reverting `Drop` to a directory sweep passes the line above and
        // fails this one. That sweep is the defect this test exists for.
        assert!(foreign.exists(), "resh must never delete a lock file it did not write");
    }

    #[test]
    fn a_missing_directory_is_created_rather_than_failing_the_project_open() {
        let d = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let nested = d.path().join("claude/ide");
        let lock = write_in(&nested, 5599, "cafe", ws.path()).unwrap();
        assert!(lock.path().exists());
    }
}
