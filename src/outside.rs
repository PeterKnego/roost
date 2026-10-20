//! The one exception to "every filesystem path is confined to the roots."
//!
//! A git worktree's metadata can point it anywhere a clone's author chose,
//! and that is sometimes outside every configured root (#188). Confinement
//! means there is no path expression that reaches outside a root, and this
//! module does not weaken that rule — it is the one deliberate door next to
//! it. A user allows a single directory, by their own action, and roost
//! remembers *that specific directory* by identity, `(dev, ino)`, never by
//! path alone, in a record this process writes to its own state directory.
//!
//! The record is the only authority. `<parent>/.claude/outside/<name>` is a
//! virtual URL; `resolve_in` is the only thing that gives it a directory, and
//! it does so by reading the record and checking the identity — never by
//! trying the literal path on disk. A record is written only by `allow_in`,
//! which never consults git itself: that check (is `path` really a worktree
//! of a project under the roots, right now) belongs to the caller, so this
//! module stays a pure records store, testable without a repository.
//!
//! Why the record lives in roost's state, and nowhere else: a repository, or
//! a project's own `.claude/` config, is something a clone brings with it.
//! A record stored there would let a cloned repository allow itself into
//! whichever roost later opens it. `state/outside/<parent key>/<name>` is
//! reachable only by whoever already runs this roost.
//!
//! What must never be made "consistent" with the rest of the codebase — each
//! is the vulnerability, not the cleanup:
//! - resolving the virtual path by trying it against disk, with or without
//!   consulting the record first — a directory planted at that literal path
//!   must never be mistaken for the one the user allowed;
//! - accepting a record whose directory's `(dev, ino)` no longer matches —
//!   the identity check is what makes "delete and recreate" and "swap the
//!   final component for a symlink" fail closed instead of silently opening
//!   whatever is there now;
//! - letting anything but this module's own `allow_in` write a record —
//!   project config and repository contents are attacker-controlled, by the
//!   same reasoning as the confinement rule itself.
//!
//! Every doubt here resolves to `None`/`Err` (CLAUDE.md, "absence of
//! evidence is not evidence of absence"): a damaged, unreadable, or simply
//! absent record is withheld access, never granted access.

use std::path::{Path, PathBuf};

/// At most this many allow records per parent project. Named so it names
/// itself in the refusal message when it fires.
pub const OUTSIDE_CAP: usize = 32;

/// The path segment that marks a virtual "outside" URL under its parent.
pub const SEGMENT: &str = ".claude/outside";

/// A single allow record: the name it resolves under its parent, the real
/// directory it names, and the identity that directory had when allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub path: PathBuf,
    pub id: (u64, u64),
}

/// `<parent>/.claude/outside/<name>` -> `(parent, name)`, else `None`. Pure:
/// no filesystem access, no validation of `parent` beyond non-emptiness
/// (that it names a real, confined project is `resolve_project`'s job, in a
/// later task). `name` must be exactly one path segment and pass
/// `wtname::valid` — so `.../outside/wt/x`, a name with a `/` in it, and a
/// name that fails that shape check all return `None`, and a URL with no
/// non-empty parent before the segment (including one that starts with the
/// segment itself) returns `None`.
pub fn split_url(url: &str) -> Option<(&str, &str)> {
    const SEP: &str = "/.claude/outside/";
    let idx = url.find(SEP)?;
    let parent = &url[..idx];
    let name = &url[idx + SEP.len()..];
    if parent.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    if !crate::wtname::valid(name) {
        return None;
    }
    Some((parent, name))
}

/// The inverse of `split_url`, for a `parent`/`name` already known valid.
pub fn url_for(parent: &str, name: &str) -> String {
    format!("{parent}/{SEGMENT}/{name}")
}

fn dir_in(state: &Path, parent: &str) -> PathBuf {
    state.join("outside").join(crate::projects::storage_key(parent))
}

fn record_path_in(state: &Path, parent: &str, name: &str) -> PathBuf {
    dir_in(state, parent).join(name)
}

/// Reads and validates a record; `None` for missing, damaged, or unreadable
/// — never a reason to treat the name as resolvable. Fork-free: two
/// reads at most (name shape, then the file), nothing that can block for
/// long, so it is safe to call under the hub or `REGISTRY` lock.
///
/// The file holds exactly two non-empty lines: the recorded canonical path,
/// then `"<dev> <ino>"`. Anything else — one line, extra lines, a
/// non-numeric identity, an empty file — is damaged and reads as absent.
pub fn read_in(state: &Path, parent: &str, name: &str) -> Option<Record> {
    if !crate::wtname::valid(name) {
        return None;
    }
    let raw = std::fs::read_to_string(record_path_in(state, parent, name)).ok()?;
    let mut lines = raw.lines();
    let path_line = lines.next()?;
    let id_line = lines.next()?;
    if path_line.is_empty() || lines.next().is_some() {
        return None;
    }
    let mut parts = id_line.split_whitespace();
    let dev = parts.next()?.parse::<u64>().ok()?;
    let ino = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(Record { name: name.to_string(), path: PathBuf::from(path_line), id: (dev, ino) })
}

/// The record's directory, only when it still exists with the recorded
/// identity — else `None`. Deliberately `metadata`, not `symlink_metadata`:
/// if the final component was swapped for a symlink, following it lands on
/// whatever that symlink now points at, and that directory's `(dev, ino)`
/// will not be the recorded one, so the mismatch still refuses it. Every
/// stat error — gone, unreadable, permission denied — is `None`, same as a
/// damaged record.
#[cfg(unix)]
pub fn resolve_in(state: &Path, parent: &str, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let rec = read_in(state, parent, name)?;
    let md = std::fs::metadata(&rec.path).ok()?;
    if !md.is_dir() || (md.dev(), md.ino()) != rec.id {
        return None;
    }
    Some(rec.path)
}

#[cfg(not(unix))]
pub fn resolve_in(_state: &Path, _parent: &str, _name: &str) -> Option<PathBuf> {
    None
}

/// All records for a parent, sorted by name. A damaged record (see
/// `read_in`) is skipped rather than aborting the whole listing — one bad
/// file must not hide every other allow.
pub fn list_in(state: &Path, parent: &str) -> Vec<Record> {
    let dir = dir_in(state, parent);
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else { return out };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Dot-prefixed names are always our own in-flight temp files.
        if name.starts_with('.') {
            continue;
        }
        if let Some(rec) = read_in(state, parent, &name) {
            out.push(rec);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The record naming `path` for this parent, if any. `path` is compared
/// exactly, as a canonical path — callers (here, `allow_in`; later,
/// whatever re-checks a worktree listing) are expected to already hold a
/// canonicalized path, same as every record this module writes.
pub fn find_in(state: &Path, parent: &str, path: &Path) -> Option<Record> {
    list_in(state, parent).into_iter().find(|r| r.path == path)
}

fn write_record(dir: &Path, name: &str, path: &Path, id: (u64, u64)) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".{name}.{}.tmp", crate::wsstate::tmp_suffix()));
    let body = format!("{}\n{} {}\n", path.display(), id.0, id.1);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = opts.open(&tmp).and_then(|mut f| {
        use std::io::Write;
        f.write_all(body.as_bytes())
    });
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    std::fs::rename(&tmp, dir.join(name)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Allows `path` under `parent`: validates it, allocates a name, enforces
/// `OUTSIDE_CAP`, and writes the record atomically at mode 0600. Does not
/// consult git at all — that `path` is really a worktree of a project under
/// the roots is the caller's job, checked before this is reached.
///
/// `path` must be absolute and equal to its own canonicalization (so no
/// symlink anywhere in it — a symlinked path and the real path are not the
/// same allow), and the canonical target must be a directory. Allowing the
/// same directory twice returns the existing record rather than erroring or
/// writing a second one.
///
/// The name is the directory's basename, with `-2`, `-3`, … appended while
/// that name is already in use for this parent (scoped to `parent`'s own
/// directory, so the same basename is free again under a different parent).
/// Each candidate must itself pass `wtname::check` — a basename of the wrong
/// shape fails on the first try and no suffix will rescue it, since a
/// leading character or length problem persists through every suffix.
#[cfg(unix)]
pub fn allow_in(state: &Path, parent: &str, path: &Path) -> Result<Record, String> {
    use std::os::unix::fs::MetadataExt;

    if !path.is_absolute() {
        return Err(format!("path must be absolute: {}", path.display()));
    }
    let canon = path.canonicalize().map_err(|e| format!("cannot resolve {}: {e}", path.display()))?;
    if canon != path {
        return Err(format!("path is not its own canonicalization (a symlink?): {}", path.display()));
    }
    let md = std::fs::metadata(&canon).map_err(|e| e.to_string())?;
    if !md.is_dir() {
        return Err(format!("not a directory: {}", canon.display()));
    }
    let id = (md.dev(), md.ino());

    if let Some(existing) = find_in(state, parent, &canon) {
        return Ok(existing);
    }

    if list_in(state, parent).len() >= OUTSIDE_CAP {
        return Err(format!("at most {OUTSIDE_CAP} outside worktrees are allowed per project"));
    }

    let basename = canon
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("cannot derive a name from {}", canon.display()))?;
    let dir = dir_in(state, parent);
    let mut name = basename.to_string();
    let mut n: u32 = 1;
    loop {
        crate::wtname::check(&name)?;
        if dir.join(&name).symlink_metadata().is_err() {
            break;
        }
        n += 1;
        if n > 1000 {
            return Err(format!("could not allocate a name for {}", canon.display()));
        }
        name = format!("{basename}-{n}");
    }

    write_record(&dir, &name, &canon, id)?;
    Ok(Record { name, path: canon, id })
}

#[cfg(not(unix))]
pub fn allow_in(_state: &Path, _parent: &str, _path: &Path) -> Result<Record, String> {
    Err("outside allows require a unix filesystem (dev/ino identity)".into())
}

/// Deletes the record, if any. Not finding one is not an error — revoking
/// twice, or revoking a record that a prune already removed, is a no-op.
pub fn revoke_in(state: &Path, parent: &str, name: &str) -> Result<(), String> {
    if !crate::wtname::valid(name) {
        return Err(format!("invalid worktree name {name:?}"));
    }
    match std::fs::remove_file(record_path_in(state, parent, name)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Deletes every record of `parent` whose path is not in `listed`, and
/// returns the names removed. `listed` must be a non-empty listing — an
/// empty one is what git answers when it cannot be asked, never evidence
/// that nothing is a worktree any more, so this still refuses to touch
/// anything on one (CLAUDE.md: absence of evidence). The caller (the
/// websocket hub, a later task) is the one that already holds that
/// distinction; this is a second, defensive line against the same mistake.
pub fn prune_in(state: &Path, parent: &str, listed: &[PathBuf]) -> Vec<String> {
    if listed.is_empty() {
        return Vec::new();
    }
    let mut removed = Vec::new();
    for rec in list_in(state, parent) {
        if !listed.iter().any(|p| p == &rec.path) && revoke_in(state, parent, &rec.name).is_ok() {
            removed.push(rec.name);
        }
    }
    removed
}

/// The non-`_in` wrappers every other stateful module in this crate pairs
/// with its `_in` form, over the real `wsstate::state_dir()`.
pub fn read(parent: &str, name: &str) -> Option<Record> {
    read_in(&crate::wsstate::state_dir(), parent, name)
}
pub fn resolve(parent: &str, name: &str) -> Option<PathBuf> {
    resolve_in(&crate::wsstate::state_dir(), parent, name)
}
pub fn list(parent: &str) -> Vec<Record> {
    list_in(&crate::wsstate::state_dir(), parent)
}
pub fn find(parent: &str, path: &Path) -> Option<Record> {
    find_in(&crate::wsstate::state_dir(), parent, path)
}
pub fn allow(parent: &str, path: &Path) -> Result<Record, String> {
    allow_in(&crate::wsstate::state_dir(), parent, path)
}
pub fn revoke(parent: &str, name: &str) -> Result<(), String> {
    revoke_in(&crate::wsstate::state_dir(), parent, name)
}
pub fn prune(parent: &str, listed: &[PathBuf]) -> Vec<String> {
    prune_in(&crate::wsstate::state_dir(), parent, listed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_url_accepts_only_the_exact_shape() {
        assert_eq!(split_url("p/.claude/outside/wt"), Some(("p", "wt")));
        assert_eq!(split_url("karpie/src/.claude/outside/wt"), Some(("karpie/src", "wt")));

        for bad in [
            "p/.claude/outside",
            "p/.claude/outside/wt/x",
            ".claude/outside/wt",
            "p/.claude/outside/../x",
            "p/.claude/outside/--bad",
        ] {
            assert_eq!(split_url(bad), None, "{bad:?} must not split");
        }
    }

    #[test]
    fn allow_then_resolve() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let wt = outside.path().join("wt");
        std::fs::create_dir(&wt).unwrap();

        let rec = allow_in(state.path(), "p", &wt).unwrap();
        assert_eq!(rec.name, "wt");
        assert_eq!(rec.path, wt.canonicalize().unwrap());

        assert_eq!(read_in(state.path(), "p", "wt").as_ref(), Some(&rec));
        assert_eq!(resolve_in(state.path(), "p", "wt"), Some(wt.canonicalize().unwrap()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = dir_in(state.path(), "p").join("wt");
            let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the record must be private");
        }
    }

    /// Revert-checked (Step 5a): dropping the identity comparison in
    /// `resolve_in` (so it returns `Some(rec.path)` right after the `is_dir`
    /// check) makes this fail: both assertions below expect `None` and get
    /// `Some(..)` instead — "assertion failed: `(left == right)` ... left:
    /// `Some(..)`, right: `None`" for the recreated directory, and the same
    /// for the symlink swap.
    #[test]
    fn a_swapped_directory_does_not_resolve() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        // "Deleted and recreated" means: same path, a *different* inode.
        // Actually doing `rm -rf`/`mkdir` and hoping the OS hands back a
        // different inode is not reliable — observed on this host's temp
        // filesystem, which reuses the just-freed inode immediately, which
        // would make this assertion pass by accident even with the identity
        // check removed. So the mismatch is fabricated directly: corrupt the
        // record's stored identity, which is exactly the state a real
        // recreate-with-a-new-inode would leave behind.
        let d = outside.path().join("d");
        std::fs::create_dir(&d).unwrap();
        let wt = d.join("wt");
        std::fs::create_dir(&wt).unwrap();
        let rec = allow_in(state.path(), "p", &wt).unwrap();
        let record_file = dir_in(state.path(), "p").join(&rec.name);
        std::fs::write(&record_file, format!("{}\n{} {}\n", wt.display(), rec.id.0, rec.id.1 + 1)).unwrap();
        assert_eq!(resolve_in(state.path(), "p", "wt"), None, "a recreated directory must not resolve");

        let wt2 = d.join("wt2");
        std::fs::create_dir(&wt2).unwrap();
        allow_in(state.path(), "p", &wt2).unwrap();
        let elsewhere = outside.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::remove_dir(&wt2).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&elsewhere, &wt2).unwrap();
        assert_eq!(resolve_in(state.path(), "p", "wt2"), None, "a symlink swap must not resolve");
    }

    #[test]
    fn a_damaged_record_does_not_resolve() {
        let state = tempfile::tempdir().unwrap();
        let dir = dir_in(state.path(), "p");
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("a"), "/tmp/somewhere\n").unwrap(); // one line only
        std::fs::write(dir.join("b"), "/tmp/somewhere\nnot-numeric junk\n").unwrap();
        std::fs::write(dir.join("c"), "/tmp/somewhere\n1 2\nextra\n").unwrap();
        std::fs::write(dir.join("d"), "").unwrap();

        for name in ["a", "b", "c", "d"] {
            assert_eq!(read_in(state.path(), "p", name), None, "{name:?} must be damaged");
            assert_eq!(resolve_in(state.path(), "p", name), None, "{name:?} must not resolve");
        }

        #[cfg(unix)]
        {
            if unsafe { libc::geteuid() } != 0 {
                use std::os::unix::fs::PermissionsExt;
                std::fs::write(dir.join("e"), "/tmp/somewhere\n1 2\n").unwrap();
                std::fs::set_permissions(dir.join("e"), std::fs::Permissions::from_mode(0o000)).unwrap();
                assert_eq!(read_in(state.path(), "p", "e"), None, "unreadable must be None");
                // restore so the tempdir can be cleaned up
                std::fs::set_permissions(dir.join("e"), std::fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
    }

    /// Revert-checked (Step 5c): skipping the `canon != path` check makes
    /// this fail on the symlinked-parent case — `allow_in` succeeds (`Err`
    /// expected, got `Ok(..)`) and the record file for the swallowed name
    /// gets written, so the "no file is written" assertion also fails.
    #[test]
    fn allow_refuses_a_symlinked_or_relative_path() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        // Through a symlinked parent.
        let real = outside.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = outside.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(allow_in(state.path(), "p", &link).is_err());

        // Relative.
        assert!(allow_in(state.path(), "p", Path::new("relative/wt")).is_err());

        // A file, not a directory.
        let file = outside.path().join("f");
        std::fs::write(&file, b"x").unwrap();
        assert!(allow_in(state.path(), "p", &file).is_err());

        assert!(list_in(state.path(), "p").is_empty(), "nothing must have been written");
    }

    #[test]
    fn names_are_allocated_per_parent() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        let a = outside.path().join("a").join("wt");
        std::fs::create_dir_all(&a).unwrap();
        let b = outside.path().join("b").join("wt");
        std::fs::create_dir_all(&b).unwrap();

        let rec_a = allow_in(state.path(), "p", &a).unwrap();
        let rec_b = allow_in(state.path(), "p", &b).unwrap();
        assert_eq!(rec_a.name, "wt");
        assert_eq!(rec_b.name, "wt-2");

        let other_parent = allow_in(state.path(), "q", &b).unwrap();
        assert_eq!(other_parent.name, "wt", "a different parent starts fresh");
    }

    #[test]
    fn allowing_the_same_path_twice_returns_the_same_record() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let wt = outside.path().join("wt");
        std::fs::create_dir(&wt).unwrap();

        let first = allow_in(state.path(), "p", &wt).unwrap();
        let second = allow_in(state.path(), "p", &wt).unwrap();
        assert_eq!(first, second);
        assert_eq!(list_in(state.path(), "p").len(), 1);
    }

    #[test]
    fn the_cap_names_itself() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        for i in 0..OUTSIDE_CAP {
            let wt = outside.path().join(format!("wt{i}"));
            std::fs::create_dir(&wt).unwrap();
            allow_in(state.path(), "p", &wt).unwrap();
        }
        let over = outside.path().join("one-too-many");
        std::fs::create_dir(&over).unwrap();
        let err = allow_in(state.path(), "p", &over).unwrap_err();
        assert!(err.contains("32"), "the refusal must name the cap, got {err:?}");
    }

    #[test]
    fn revoke_and_prune() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let keep = outside.path().join("keep");
        std::fs::create_dir(&keep).unwrap();
        let drop_me = outside.path().join("drop");
        std::fs::create_dir(&drop_me).unwrap();

        let keep_rec = allow_in(state.path(), "p", &keep).unwrap();
        let drop_rec = allow_in(state.path(), "p", &drop_me).unwrap();

        revoke_in(state.path(), "p", &drop_rec.name).unwrap();
        assert_eq!(read_in(state.path(), "p", &drop_rec.name), None);
        assert_eq!(read_in(state.path(), "p", &keep_rec.name).map(|r| r.name), Some(keep_rec.name.clone()));

        // Re-allow to exercise prune_in with two records again.
        let drop_rec2 = allow_in(state.path(), "p", &drop_me).unwrap();
        let removed = prune_in(state.path(), "p", &[keep_rec.path.clone()]);
        assert_eq!(removed, vec![drop_rec2.name]);
        assert_eq!(read_in(state.path(), "p", &keep_rec.name).map(|r| r.name), Some(keep_rec.name));
    }
}
