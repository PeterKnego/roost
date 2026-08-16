//! File mutations: creation, deletion, rename, and the conflict-guarded
//! atomic save. Every path here is confined before use.
use crate::projects::{safe_resolve, safe_resolve_parent};
use std::path::{Path, PathBuf};

const MAX_WRITE_BYTES: usize = 2_000_000;

pub enum SaveOutcome {
    Written,
    Conflict { disk_text: String },
}

/// Write atomically: temp file in the same directory, copy the original's
/// mode, then rename. Never truncate in place — a crash mid-save must not
/// leave a half-written source file.
fn atomic_write(path: &Path, text: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent directory")?;
    let tmp = dir.join(format!(
        ".{}.deadlight.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("buf")
    ));
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode();
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
        }
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

pub fn save(
    project_dir: &Path,
    rel: &str,
    text: &str,
    base_hash: u64,
    force: bool,
) -> Result<SaveOutcome, String> {
    if text.len() > MAX_WRITE_BYTES {
        return Err(format!("file too large ({} bytes)", text.len()));
    }
    let abs = safe_resolve(project_dir, rel)?;
    let disk = std::fs::read_to_string(&abs).map_err(|e| e.to_string())?;
    if !force && crate::workspace::hash_text(&disk) != base_hash {
        return Ok(SaveOutcome::Conflict { disk_text: disk });
    }
    atomic_write(&abs, text)?;
    Ok(SaveOutcome::Written)
}

pub fn create_file(project_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let abs = safe_resolve_parent(project_dir, rel)?;
    if abs.exists() {
        return Err(format!("already exists: {rel}"));
    }
    std::fs::write(&abs, "").map_err(|e| e.to_string())?;
    Ok(abs)
}

pub fn create_dir(project_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let abs = safe_resolve_parent(project_dir, rel)?;
    if abs.exists() {
        return Err(format!("already exists: {rel}"));
    }
    std::fs::create_dir(&abs).map_err(|e| e.to_string())?;
    Ok(abs)
}

/// Non-recursive by design: files and empty directories only. Not because
/// recursive delete is an escalation — the terminal is right there — but so a
/// misclick in a tree cannot remove `target/` or `.git`.
pub fn delete(project_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let abs = safe_resolve(project_dir, rel)?;
    let meta = std::fs::metadata(&abs).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        std::fs::remove_dir(&abs).map_err(|_| format!("directory not empty: {rel}"))?;
    } else {
        std::fs::remove_file(&abs).map_err(|e| e.to_string())?;
    }
    Ok(abs)
}

pub fn rename(project_dir: &Path, from: &str, to: &str) -> Result<PathBuf, String> {
    let src = safe_resolve(project_dir, from)?;
    let dst = safe_resolve_parent(project_dir, to)?;
    if dst.exists() {
        return Err(format!("already exists: {to}"));
    }
    std::fs::rename(&src, &dst).map_err(|e| e.to_string())?;
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn proj() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.txt"), "one\n").unwrap();
        fs::create_dir(d.path().join("sub")).unwrap();
        d
    }

    #[test]
    fn save_writes_when_the_base_hash_matches() {
        let d = proj();
        let base = crate::workspace::hash_text("one\n");
        let out = save(d.path(), "a.txt", "two\n", base, false).unwrap();
        assert!(matches!(out, SaveOutcome::Written));
        assert_eq!(fs::read_to_string(d.path().join("a.txt")).unwrap(), "two\n");
    }

    #[test]
    fn save_refuses_when_disk_changed_underneath() {
        let d = proj();
        let stale = crate::workspace::hash_text("what the buffer was opened with\n");
        let out = save(d.path(), "a.txt", "mine\n", stale, false).unwrap();
        match out {
            SaveOutcome::Conflict { disk_text } => assert_eq!(disk_text, "one\n"),
            SaveOutcome::Written => panic!("stale save must not clobber"),
        }
        assert_eq!(
            fs::read_to_string(d.path().join("a.txt")).unwrap(),
            "one\n",
            "the file must be untouched after a refused save"
        );
    }

    #[test]
    fn force_overrides_the_conflict() {
        let d = proj();
        let stale = crate::workspace::hash_text("stale\n");
        let out = save(d.path(), "a.txt", "mine\n", stale, true).unwrap();
        assert!(matches!(out, SaveOutcome::Written));
        assert_eq!(fs::read_to_string(d.path().join("a.txt")).unwrap(), "mine\n");
    }

    #[test]
    fn save_preserves_file_mode() {
        let d = proj();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = d.path().join("a.txt");
            fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
            let base = crate::workspace::hash_text("one\n");
            save(d.path(), "a.txt", "two\n", base, false).unwrap();
            let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o640, "atomic rename must not reset permissions");
        }
    }

    #[test]
    fn save_is_confined() {
        let d = proj();
        assert!(save(d.path(), "../outside.txt", "x", 0, true).is_err());
    }

    #[test]
    fn create_delete_rename_round_trip() {
        let d = proj();
        create_file(d.path(), "new.txt").unwrap();
        assert!(d.path().join("new.txt").is_file());
        create_dir(d.path(), "sub/deeper").unwrap();
        assert!(d.path().join("sub/deeper").is_dir());
        rename(d.path(), "new.txt", "sub/moved.txt").unwrap();
        assert!(d.path().join("sub/moved.txt").is_file());
        delete(d.path(), "sub/moved.txt").unwrap();
        assert!(!d.path().join("sub/moved.txt").exists());
    }

    #[test]
    fn create_file_refuses_to_clobber() {
        let d = proj();
        assert!(create_file(d.path(), "a.txt").is_err(), "must not truncate an existing file");
    }

    #[test]
    fn delete_is_non_recursive() {
        let d = proj();
        std::fs::write(d.path().join("sub/inner.txt"), "x").unwrap();
        assert!(delete(d.path(), "sub").is_err(), "a misclick must not take out a tree");
        assert!(d.path().join("sub/inner.txt").exists());
        // an empty directory is fine
        std::fs::create_dir(d.path().join("empty")).unwrap();
        assert!(delete(d.path(), "empty").is_ok());
    }

    #[test]
    fn operations_are_confined() {
        let d = proj();
        assert!(create_file(d.path(), "../evil.txt").is_err());
        assert!(delete(d.path(), "../../etc/passwd").is_err());
        assert!(rename(d.path(), "a.txt", "../evil.txt").is_err());
    }
}
