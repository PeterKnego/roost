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
        ".{}.resh.tmp",
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

/// Rejects what `safe_resolve_parent` does not. That function validates the
/// final component for traversal, but a part's filename arrives from the
/// browser's `DataTransfer` and is attacker-influenced even though the endpoint
/// checks `Origin`. Separators are refused rather than flattened, which is how
/// directory upload stays a non-goal instead of arriving by accident.
fn valid_upload_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(format!("invalid filename: {name:?}"));
    }
    if name.len() > 255 {
        return Err(format!("invalid filename: {} bytes is too long", name.len()));
    }
    if name.contains('/') || name.contains('\\') || name.chars().any(|c| c.is_control()) {
        return Err(format!("invalid filename: {name:?}"));
    }
    Ok(())
}

/// Refuses destinations the file tree never renders.
///
/// Not a path-safety rule — these paths are legal and inside the project, which
/// is exactly why nothing else refuses them — but a *visibility* one. A file
/// written into `.git` or `.claude` cannot be seen, opened, or deleted from the
/// UI that wrote it, and the next upload of the same name is refused as
/// "already exists" against a file the user has no way to find. Inside `.git`
/// it is worse than confusing: a write into an object or ref directory can
/// corrupt the repository.
///
/// Keyed on `SKIP_DIRS`, deliberately not on a leading dot. The tree hides a
/// fixed list of directories, so `.gitignore` is visible and uploading one is
/// honest.
fn visible_in_tree(rel: &str) -> Result<(), String> {
    for segment in rel.split('/').filter(|s| !s.is_empty()) {
        if crate::projects::SKIP_DIRS.contains(&segment) {
            return Err(format!(
                "{segment} is not visible in the tree; refusing to upload into it"
            ));
        }
    }
    Ok(())
}

/// Distinguishes concurrent temp files within one process. The pid alone is not
/// enough: one request may carry two parts with the same filename, and they are
/// open at overlapping times, so a pid-only name would have the second part
/// writing through the first's temp before either committed.
static TEMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A part being streamed to disk.
///
/// Writes to a temp file in the *destination* directory so the final step is a
/// rename on the same filesystem — atomic, so a watcher never sees a partial
/// file under the real name — and removes that temp on drop, so an abandoned
/// upload leaves nothing behind.
#[derive(Debug)]
pub struct UploadTemp {
    /// `None` once committed, which is also what stops `Drop` deleting the
    /// file we just renamed into place.
    tmp: Option<PathBuf>,
    dest: PathBuf,
    rel: String,
    file: std::fs::File,
}

impl UploadTemp {
    pub fn create(project_dir: &Path, dir_rel: &str, name: &str) -> Result<Self, String> {
        valid_upload_name(name)?;
        let rel =
            if dir_rel.is_empty() { name.to_string() } else { format!("{dir_rel}/{name}") };
        visible_in_tree(&rel)?;
        let dest = safe_resolve_parent(project_dir, &rel)?;
        // Checked here so an upload that cannot land is refused before its bytes
        // are accepted, and again in `commit` because a streamed part takes long
        // enough for the answer to change under it.
        must_not_exist(&dest, &rel)?;
        let parent = dest.parent().ok_or("no parent directory")?;
        let seq = TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = parent.join(format!(".{name}.{}-{seq}.resh.tmp", std::process::id()));
        let file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        Ok(UploadTemp { tmp: Some(tmp), dest, rel, file })
    }

    pub fn write(&mut self, chunk: &[u8]) -> Result<(), String> {
        use std::io::Write;
        self.file.write_all(chunk).map_err(|e| e.to_string())
    }

    pub fn commit(mut self) -> Result<PathBuf, String> {
        use std::io::Write;
        self.file.flush().map_err(|e| e.to_string())?;
        must_not_exist(&self.dest, &self.rel)?;
        let tmp = self.tmp.take().ok_or("upload already committed")?;
        std::fs::rename(&tmp, &self.dest).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })?;
        Ok(self.dest.clone())
    }
}

impl Drop for UploadTemp {
    fn drop(&mut self) {
        if let Some(tmp) = self.tmp.take() {
            let _ = std::fs::remove_file(tmp);
        }
    }
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

    fn put(project: &Path, dir: &str, name: &str, data: &[u8]) -> Result<PathBuf, String> {
        let mut t = UploadTemp::create(project, dir, name)?;
        t.write(data)?;
        t.commit()
    }

    fn leftover_temps(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("resh.tmp"))
            .collect()
    }

    fn running_as_root() -> bool {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim() == "0")
            .unwrap_or(false)
    }

    /// Reaches the confinement check rather than failing earlier for an
    /// unrelated reason: `..` exists, so this cannot pass on ENOENT. That hole
    /// is why a symlink escape once survived review here.
    #[test]
    fn upload_refuses_a_traversal_and_says_why() {
        let d = tempfile::tempdir().unwrap();
        let project = d.path().join("proj");
        fs::create_dir(&project).unwrap();
        let e = put(&project, "..", "escape.png", b"x").unwrap_err();
        assert!(e.contains("path outside project"), "unexpected message: {e}");
        assert!(!d.path().join("escape.png").exists(), "a refused upload escaped the project");
    }

    /// The test that keeps directory upload from arriving by accident: a
    /// separator in a part's filename is an error, never silently flattened.
    #[test]
    fn a_separator_in_the_filename_is_refused_not_flattened() {
        let d = tempfile::tempdir().unwrap();
        for name in ["sub/a.png", "sub\\a.png"] {
            let e = put(d.path(), "", name, b"x").unwrap_err();
            assert!(e.contains("invalid filename"), "unexpected message for {name}: {e}");
        }
        assert!(!d.path().join("a.png").exists(), "a flattened file was written");
    }

    /// Pins the *early* check specifically. `commit` re-checks, so the
    /// collision test below still passes with this one deleted — which is how
    /// the early check came to have no coverage at all. What it buys is that a
    /// doomed part is refused before a single byte is accepted, rather than
    /// after a caller has streamed 100 MB into a temp file for nothing.
    #[test]
    fn a_colliding_part_is_refused_before_any_bytes_are_accepted() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.png"), b"original").unwrap();
        let e = UploadTemp::create(d.path(), "", "a.png").unwrap_err();
        assert!(e.contains("already exists"), "unexpected message: {e}");
        assert!(
            leftover_temps(d.path()).is_empty(),
            "a refused part still opened a temp file to stream into"
        );
    }

    /// Asserting an error alone would also pass against an implementation that
    /// truncated the file and *then* failed — the outcome this forbids.
    #[test]
    fn upload_refuses_a_collision_and_leaves_the_original_intact() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.png"), b"original").unwrap();
        let e = put(d.path(), "", "a.png", b"replacement").unwrap_err();
        assert!(e.contains("already exists"), "unexpected message: {e}");
        assert_eq!(fs::read(d.path().join("a.png")).unwrap(), b"original");
    }

    /// A skipped directory is never rendered in the tree, so a file written
    /// there is invisible in the UI that wrote it — and inside `.git` it can
    /// corrupt the repository. The path is legal and inside the project, which
    /// is exactly why nothing else refuses it.
    #[test]
    fn upload_refuses_a_destination_the_tree_never_shows() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join(".git")).unwrap();
        let e = put(d.path(), ".git", "config", b"x").unwrap_err();
        assert!(e.contains("not visible in the tree"), "unexpected message: {e}");
        assert!(!d.path().join(".git/config").exists());
    }

    /// The complement, and what stops the rule being written as "refuse a
    /// leading dot": the tree hides a fixed list of *directories*, not
    /// dotfiles, so `.gitignore` is visible and uploading it is honest.
    #[test]
    fn upload_allows_an_ordinary_dotfile() {
        let d = tempfile::tempdir().unwrap();
        assert!(put(d.path(), "", ".gitignore", b"target\n").is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn upload_refuses_when_it_cannot_tell_whether_the_target_exists() {
        use std::os::unix::fs::PermissionsExt;
        if running_as_root() {
            return; // mode bits do not bind root; the fixture cannot enter its own precondition
        }
        let d = tempfile::tempdir().unwrap();
        let locked = d.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let r = put(d.path(), "locked", "a.png", b"x");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let e = r.unwrap_err();
        assert!(
            e.contains("no such directory") || e.contains("cannot check"),
            "an unreadable parent must be refused as unknown, not read as absent: {e}"
        );
    }

    #[test]
    fn a_part_written_in_chunks_lands_whole_and_leaves_no_temp() {
        let d = tempfile::tempdir().unwrap();
        let mut t = UploadTemp::create(d.path(), "", "a.bin").unwrap();
        t.write(&[0x89, 0x50]).unwrap();
        t.write(&[0x4e, 0x47]).unwrap();
        let p = t.commit().unwrap();
        assert_eq!(fs::read(&p).unwrap(), [0x89, 0x50, 0x4e, 0x47]);
        assert!(leftover_temps(d.path()).is_empty());
    }

    /// An abandoned upload — a cap breach, a dropped connection — must leave
    /// nothing behind. Dropping without committing is the common path here, not
    /// an edge case.
    #[test]
    fn an_abandoned_part_removes_its_temp_file() {
        let d = tempfile::tempdir().unwrap();
        {
            let mut t = UploadTemp::create(d.path(), "", "a.bin").unwrap();
            t.write(b"partial").unwrap();
        }
        assert!(leftover_temps(d.path()).is_empty(), "a dropped upload left its temp behind");
        assert!(!d.path().join("a.bin").exists(), "an uncommitted upload became visible");
    }

    /// Two uploads of the same name in one request must not share a temp path,
    /// or the second clobbers the first's bytes before either commits.
    #[test]
    fn two_concurrent_parts_do_not_share_a_temp_file() {
        let d = tempfile::tempdir().unwrap();
        let mut a = UploadTemp::create(d.path(), "", "same.bin").unwrap();
        let mut b = UploadTemp::create(d.path(), "", "same.bin").unwrap();
        a.write(b"aaaa").unwrap();
        b.write(b"bbbb").unwrap();
        let pa = a.commit().unwrap();
        assert_eq!(fs::read(&pa).unwrap(), b"aaaa", "the second part overwrote the first's temp");
        // The loser is refused rather than silently replacing what just landed.
        assert!(b.commit().unwrap_err().contains("already exists"));
    }
}
