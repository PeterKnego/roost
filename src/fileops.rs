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
    let disk_meta = std::fs::metadata(&abs).map_err(|e| e.to_string())?;
    if disk_meta.len() > MAX_WRITE_BYTES as u64 {
        return Err(format!("file too large ({} bytes)", disk_meta.len()));
    }
    let disk = std::fs::read_to_string(&abs).map_err(|e| e.to_string())?;
    if !force && crate::workspace::hash_text(&disk) != base_hash {
        return Ok(SaveOutcome::Conflict { disk_text: disk });
    }
    atomic_write(&abs, text)?;
    Ok(SaveOutcome::Written)
}

/// Refuses unless the OS *positively reports* nothing at `abs`. Three outcomes,
/// not two: something is there (refuse), nothing is there (proceed), or the
/// question could not be answered (refuse).
///
/// `symlink_metadata` rather than `exists()`/`metadata()` on two counts. It does
/// not follow symlinks, so a dangling symlink inside the project counts as
/// "already there" instead of being followed and written through to wherever it
/// points — outside the project. And it surfaces the error kind, where
/// `exists()` folds every failure into `false`: an `EACCES` on a parent, or a
/// path on a filesystem that has gone away, read as "nothing there" and the
/// caller then created or replaced through it.
///
/// That second half is the rule `registry`'s reaping had to learn eleven times
/// over (CLAUDE.md, "Absence of evidence is not evidence of absence"), and
/// leaving these two modules disagreeing about what a failed stat means is how a
/// twelfth instance gets written.
fn must_not_exist(abs: &Path, rel: &str) -> Result<(), String> {
    match abs.symlink_metadata() {
        Ok(_) => Err(format!("already exists: {rel}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot check whether {rel} exists: {e}")),
    }
}

pub fn create_file(project_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let abs = safe_resolve_parent(project_dir, rel)?;
    must_not_exist(&abs, rel)?;
    std::fs::write(&abs, "").map_err(|e| e.to_string())?;
    Ok(abs)
}

pub fn create_dir(project_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let abs = safe_resolve_parent(project_dir, rel)?;
    // mkdir(2) doesn't follow symlinks, so the symlink half of `must_not_exist`
    // is belt-and-braces here; the "couldn't stat" half is not.
    must_not_exist(&abs, rel)?;
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
    // The most destructive of the three: `rename` replaces its destination
    // outright. `must_not_exist` is what stands between a dangling symlink (or
    // a destination that merely could not be stat'd) and silently overwriting
    // whatever is really there.
    must_not_exist(&dst, to)?;
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
        // Build our own layout (rather than reusing `proj()`, which is
        // rooted directly under the system temp dir) so the sibling
        // "outside" file is guaranteed to exist at a known path, and so
        // both it and the project dir are cleaned up together.
        let outer = tempfile::tempdir().unwrap();
        let project = outer.path().join("project");
        fs::create_dir(&project).unwrap();
        let outside = outer.path().join("outside.txt");
        fs::write(&outside, "safe\n").unwrap();

        let err = match save(&project, "../outside.txt", "x", 0, true) {
            Err(e) => e,
            Ok(_) => panic!("save through a `..` escape must not succeed"),
        };
        assert!(err.contains("outside project"), "unexpected error: {err}");
        assert_eq!(
            fs::read_to_string(&outside).unwrap(),
            "safe\n",
            "a save through a `..` escape must not touch the file it reached"
        );
    }

    #[cfg(unix)]
    #[test]
    fn create_file_refuses_a_symlink_that_escapes_the_project() {
        use std::os::unix::fs::symlink;

        let d = proj();
        let outside_dir = tempfile::tempdir().unwrap();
        let missing = outside_dir.path().join("nope.txt"); // never created
        let existing = outside_dir.path().join("real.txt");
        fs::write(&existing, "real\n").unwrap();

        // Case 1: dangling symlink. `symlink_metadata` (not `exists`, which
        // follows the link and sees nothing) must treat this as "already
        // there" and refuse, rather than letting `fs::write` follow the
        // link and create the file outside the project.
        symlink(&missing, d.path().join("dangling")).unwrap();
        assert!(create_file(d.path(), "dangling").is_err());
        assert!(!missing.exists(), "must not have created the file outside the project");

        // Case 2: symlink to a file that already exists outside the
        // project. Must also be refused, and the target left untouched.
        symlink(&existing, d.path().join("linked")).unwrap();
        assert!(create_file(d.path(), "linked").is_err());
        assert_eq!(
            fs::read_to_string(&existing).unwrap(),
            "real\n",
            "existing outside file must be untouched"
        );
    }

    // Same shape as `create_file`'s refusal above, on the destination of a
    // rename: `exists()` follows the link, sees nothing, and returns false —
    // so the old check read "nothing there" and `rename` then destroyed the
    // symlink. Destroying anything needs evidence it isn't there, and a
    // stat that answered nothing is not that evidence.
    #[cfg(unix)]
    #[test]
    fn rename_refuses_a_destination_that_is_a_dangling_symlink() {
        use std::os::unix::fs::symlink;

        let d = proj();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path().join("nope.txt"), d.path().join("dst")).unwrap();

        let err = rename(d.path(), "a.txt", "dst").expect_err("must refuse, not clobber the link");
        assert!(err.contains("already exists"), "unexpected reason: {err}");
        assert!(
            d.path().join("dst").symlink_metadata().unwrap().file_type().is_symlink(),
            "the destination link itself must survive"
        );
        assert!(d.path().join("a.txt").exists(), "and the source must not have moved");
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

    /// A stat that fails for a reason other than "not there" must refuse, not
    /// proceed. `exists()` — and `symlink_metadata().is_ok()` before this —
    /// folded `EACCES` into "nothing there", so `create_file` would write and
    /// `rename` would replace through a destination nobody could actually see.
    ///
    /// Made unstattable by removing search permission from the parent
    /// directory, which is what an `EACCES` on a real deployment looks like.
    /// Skipped when running as root, since root ignores the permission bits and
    /// the stat would succeed — a silent pass there would make this vacuous.
    #[cfg(unix)]
    #[test]
    fn an_unstattable_destination_is_refused_rather_than_written_through() {
        use std::os::unix::fs::PermissionsExt;
        let d = proj();
        let locked = d.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        let victim = locked.join("target.txt");
        std::fs::write(&victim, "precious\n").unwrap();

        // 0o000: cannot even resolve names inside it.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let stat_blocked = std::fs::symlink_metadata(&victim).is_err();
        if !stat_blocked {
            // Running as root (or a filesystem ignoring the mode) — the
            // premise doesn't hold, so asserting anything here would prove
            // nothing at all.
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skipping: this process can stat through a 0o000 directory");
            return;
        }

        let err = must_not_exist(&victim, "locked/target.txt")
            .expect_err("an unstattable path must not be reported as absent");
        assert!(
            err.contains("cannot check"),
            "the refusal must say the check failed, not claim the file exists: {err}"
        );

        // Restore so the TempDir can clean itself up.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "precious\n",
            "and nothing may have been written through it"
        );
    }
}
