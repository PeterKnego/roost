//! The one exception to "every filesystem path is confined to the roots."
//!
//! A git worktree's metadata can point it anywhere a clone's author chose,
//! and that is sometimes outside every configured root (#188). Confinement
//! means there is no path expression that reaches outside a root, and this
//! module does not weaken that rule — it is the one deliberate door next to
//! it. A user allows a single directory, by their own action, and roost
//! remembers *that specific directory* by identity — `(dev, ino, birth
//! time)`, never by path alone — in a record this process writes to its own
//! state directory.
//!
//! Identity is three-part, not two, because two was measured and found
//! wanting. `dev`/`ino` alone is the obvious identity to reach for, and it is
//! not enough: a probe of this project's own production filesystem (ext4)
//! found that deleting a directory and recreating it at the same path reused
//! the just-freed inode number every single time (50/50), and a *different*,
//! unrelated directory created moments later reused it too (5/5) — inode
//! reuse on ext4 is immediate and is not even confined to the same path.
//! `Metadata::created()` (the filesystem's birth time, read via `statx`) was
//! the one thing in that same probe that changed on every single recreation,
//! on both ext4 and tmpfs. So the identity this module pins is `(dev, ino,
//! btime_secs, btime_nsec)`, and an unreadable birth time is refused, not
//! shrugged off back to `(dev, ino)` — see below.
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
//! - comparing only `(dev, ino)`, or falling back to it when the birth time
//!   can't be read — the measurement above is exactly why: on this project's
//!   own filesystem, "delete and recreate," even at a *different* path, can
//!   and does produce the same `(dev, ino)` as the directory that used to be
//!   there. An identity check that can't be completed is not evidence that
//!   the old one still matches;
//! - letting anything but this module's own `allow_in` write a record —
//!   project config and repository contents are attacker-controlled, by the
//!   same reasoning as the confinement rule itself.
//!
//! Every doubt here resolves to `None`/`Err` (CLAUDE.md, "absence of
//! evidence is not evidence of absence"): a damaged, unreadable, or simply
//! absent record is withheld access, never granted access. The one place
//! this module is *not* allowed to resolve a doubt to "nothing happened" is
//! the allow cap and the re-allow dedupe (`allow_in`): a directory listing
//! that could not actually be read must refuse the allow outright, not be
//! read as "zero existing records," which would silently bypass both.

use std::path::{Path, PathBuf};

/// At most this many allow records per parent project. Named so it names
/// itself in the refusal message when it fires.
pub const OUTSIDE_CAP: usize = 32;

/// The path segment that marks a virtual "outside" URL under its parent.
pub const SEGMENT: &str = ".claude/outside";

/// A single allow record: the name it resolves under its parent, the real
/// directory it names, and the identity — `(dev, ino, btime_secs,
/// btime_nsec)` — that directory had when allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub path: PathBuf,
    pub id: (u64, u64, u64, u64),
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

/// `state/outside/<storage_key(parent)>`, or `None` for a `parent` that
/// cannot name a real project: empty, or carrying a `..` segment. `parent`
/// is otherwise percent-encoded by `storage_key` same as every other
/// per-project state path, but `storage_key` encodes `/` rather than
/// rejecting it (nested projects are legitimate), so a `..` segment would
/// otherwise encode to a harmless-looking `%2E%2E` — safe on disk, but a
/// `parent` that can never be a real project is worth refusing up front
/// rather than silently keying a directory nothing will ever read back.
fn dir_in(state: &Path, parent: &str) -> Option<PathBuf> {
    if parent.is_empty() || parent.split('/').any(|seg| seg == "..") {
        return None;
    }
    Some(state.join("outside").join(crate::projects::storage_key(parent)))
}

fn record_path_in(state: &Path, parent: &str, name: &str) -> Option<PathBuf> {
    Some(dir_in(state, parent)?.join(name))
}

#[cfg(unix)]
fn birth(md: &std::fs::Metadata) -> Option<(u64, u64)> {
    let created = md.created().ok()?;
    let dur = created.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some((dur.as_secs(), dur.subsec_nanos() as u64))
}

/// Reads and validates a record; `None` for missing, damaged, or unreadable
/// — never a reason to treat the name as resolvable. Fork-free: a couple of
/// reads at most, nothing that can block for long, so it is safe to call
/// under the hub or `REGISTRY` lock.
///
/// The file holds exactly two non-empty lines: the recorded canonical path,
/// then `"<dev> <ino> <btime_secs> <btime_nsec>"`. Anything else — one line,
/// extra lines, a non-numeric field, a line with only two fields (the
/// pre-birth-time format this module briefly had) — is damaged and reads as
/// absent. No production record was ever written in that older format, so
/// there is nothing to migrate; refusing it is simply "damaged."
pub fn read_in(state: &Path, parent: &str, name: &str) -> Option<Record> {
    if !crate::wtname::valid(name) {
        return None;
    }
    let path = record_path_in(state, parent, name)?;
    let raw = std::fs::read_to_string(path).ok()?;
    let mut lines = raw.lines();
    let path_line = lines.next()?;
    let id_line = lines.next()?;
    if path_line.is_empty() || lines.next().is_some() {
        return None;
    }
    let mut parts = id_line.split_whitespace();
    let dev = parts.next()?.parse::<u64>().ok()?;
    let ino = parts.next()?.parse::<u64>().ok()?;
    let bsec = parts.next()?.parse::<u64>().ok()?;
    let bnsec = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(Record { name: name.to_string(), path: PathBuf::from(path_line), id: (dev, ino, bsec, bnsec) })
}

/// The record's directory, only when it still exists with the recorded
/// identity — else `None`. Deliberately `metadata`, not `symlink_metadata`:
/// if the final component was swapped for a symlink, following it lands on
/// whatever that symlink now points at, and that directory's identity will
/// not be the recorded one, so the mismatch still refuses it.
///
/// An unreadable birth time is `None`, the same as a mismatch — never a
/// reason to fall back to comparing `(dev, ino)` alone. That fallback is
/// exactly the hole the module doc's measurement describes: on this
/// project's own filesystem, `(dev, ino)` alone can match a directory that
/// is not the one that was allowed.
#[cfg(unix)]
pub fn resolve_in(state: &Path, parent: &str, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let rec = read_in(state, parent, name)?;
    let md = std::fs::metadata(&rec.path).ok()?;
    if !md.is_dir() {
        return None;
    }
    let (bsec, bnsec) = birth(&md)?;
    if (md.dev(), md.ino(), bsec, bnsec) != rec.id {
        return None;
    }
    Some(rec.path)
}

#[cfg(not(unix))]
pub fn resolve_in(_state: &Path, _parent: &str, _name: &str) -> Option<PathBuf> {
    None
}

/// All records for a parent, sorted by name. A read failure of any kind —
/// including one existing directory this process simply cannot read right
/// now — comes back as an empty listing: this function is for display and
/// enumeration, where "I don't know" and "there is nothing" are both
/// reasonably shown as nothing. `allow_in` cannot afford that conflation
/// (see `list_result_in`) and does not use this.
pub fn list_in(state: &Path, parent: &str) -> Vec<Record> {
    list_result_in(state, parent).unwrap_or_default()
}

/// `list_in`'s real outcome, with the read failure kept distinct from "zero
/// records": `Ok(vec![])` only for a parent directory that does not exist
/// yet (the ordinary case, before any allow), never for one that exists but
/// could not be read. `allow_in` uses this specifically so an unreadable
/// records directory refuses the allow instead of silently bypassing the
/// cap and the re-allow dedupe, both of which are counting on this listing
/// being complete.
fn list_result_in(state: &Path, parent: &str) -> Result<Vec<Record>, String> {
    let Some(dir) = dir_in(state, parent) else { return Ok(Vec::new()) };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot list outside records for {parent:?}: {e}")),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot list outside records for {parent:?}: {e}"))?;
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
    Ok(out)
}

/// The record naming `path` for this parent, if any. `path` is compared
/// exactly, as a canonical path — callers (here, `allow_in`; later,
/// whatever re-checks a worktree listing) are expected to already hold a
/// canonicalized path, same as every record this module writes.
pub fn find_in(state: &Path, parent: &str, path: &Path) -> Option<Record> {
    list_in(state, parent).into_iter().find(|r| r.path == path)
}

/// `state/outside` itself, created 0700. This is the shared container for
/// every parent's records, so unlike the 0600 record files inside it (each
/// already private on their own), the directory listing itself — which
/// parents have outside allows at all — is worth keeping to this user alone.
/// Idempotent: re-asserts the mode even if the directory already existed
/// from an older roost.
fn ensure_outside_root(state: &Path) -> Result<PathBuf, String> {
    let root = state.join("outside");
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(root)
}

fn write_body(tmp: &Path, path: &Path, id: (u64, u64, u64, u64)) -> Result<(), String> {
    let body = format!("{}\n{} {} {} {}\n", path.display(), id.0, id.1, id.2, id.3);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = opts.open(tmp).and_then(|mut f| {
        use std::io::Write;
        f.write_all(body.as_bytes())
    });
    if let Err(e) = result {
        let _ = std::fs::remove_file(tmp);
        return Err(e.to_string());
    }
    Ok(())
}

/// Atomically replaces whatever is at `dir/name`. Used only by `allow_in`'s
/// "rewrite a stale record" path — refreshing a record that already names
/// this exact path, under its own existing name, after the directory there
/// was recreated with a new identity. `rename` is correct here specifically
/// because the intent is to replace that one name, not to claim a fresh one;
/// `allocate_and_link` below is the one that must never silently replace.
fn replace_record(dir: &Path, name: &str, path: &Path, id: (u64, u64, u64, u64)) -> Result<(), String> {
    let tmp = dir.join(format!(".{name}.{}.tmp", crate::wsstate::tmp_suffix()));
    write_body(&tmp, path, id)?;
    std::fs::rename(&tmp, dir.join(name)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Claims a *fresh* name under `dir` for `path`/`id`, starting at `basename`
/// and trying `-2`, `-3`, … up to a thousand attempts.
///
/// The claim itself is `std::fs::hard_link` from the written temp file, not
/// `rename`: two concurrent allows racing for the same candidate name cannot
/// make one overwrite the other's record, because `hard_link` refuses
/// (`AlreadyExists`) when the destination already exists, where `rename`
/// would have silently replaced it. `AlreadyExists` is read only as "taken,
/// try the next name" — any other error refuses the whole allocation
/// outright; a name this process cannot even check must never be guessed
/// free.
fn allocate_and_link(dir: &Path, basename: &str, path: &Path, id: (u64, u64, u64, u64)) -> Result<String, String> {
    let tmp = dir.join(format!(".outside.{}.tmp", crate::wsstate::tmp_suffix()));
    write_body(&tmp, path, id)?;
    let mut name = basename.to_string();
    let mut n: u32 = 1;
    let result = loop {
        if let Err(e) = crate::wtname::check(&name) {
            break Err(e);
        }
        match std::fs::hard_link(&tmp, dir.join(&name)) {
            Ok(()) => break Ok(name.clone()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                n += 1;
                if n > 1000 {
                    break Err(format!("could not allocate a name for {}", path.display()));
                }
                name = format!("{basename}-{n}");
            }
            Err(e) => break Err(e.to_string()),
        }
    };
    let _ = std::fs::remove_file(&tmp);
    result
}

/// Allows `path` under `parent`: validates it, allocates a name, enforces
/// `OUTSIDE_CAP`, and writes the record. Does not consult git at all — that
/// `path` is really a worktree of a project under the roots is the caller's
/// job, checked before this is reached.
///
/// `path` must be plain UTF-8 with no embedded newline (the record format is
/// line-based, so a path that can't round-trip through one line would write
/// a record that can never resolve, or worse — once the non-UTF-8 bytes are
/// lossily re-encoded through `Display` — one that resolves to the wrong
/// path), absolute, and equal to its own canonicalization (so no symlink
/// anywhere in it). The canonical target must be a directory with a
/// readable birth time — if `created()` errors,
/// this refuses outright rather than pinning a weaker identity.
///
/// Re-allowing the same directory, unchanged, returns the existing record.
/// Re-allowing the same *path* after the directory there was recreated (same
/// path, new identity) rewrites that record under its existing name, rather
/// than returning the stale one or refusing — a user re-allowing a path they
/// already chose once must not be blocked by a record of a directory that
/// no longer exists.
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

    let s = path.to_str().ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))?;
    if s.contains('\n') {
        return Err(format!("path contains a newline: {s:?}"));
    }
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
    let (bsec, bnsec) =
        birth(&md).ok_or_else(|| "roost cannot read this directory's birth time, so it cannot pin it".to_string())?;
    let id = (md.dev(), md.ino(), bsec, bnsec);

    let dir = dir_in(state, parent).ok_or_else(|| format!("invalid parent {parent:?}"))?;
    let records = list_result_in(state, parent)?;

    if let Some(existing) = records.iter().find(|r| r.path == canon) {
        if existing.id == id {
            return Ok(existing.clone());
        }
        ensure_outside_root(state)?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        replace_record(&dir, &existing.name, &canon, id)?;
        return Ok(Record { name: existing.name.clone(), path: canon, id });
    }

    if records.len() >= OUTSIDE_CAP {
        return Err(format!("at most {OUTSIDE_CAP} outside worktrees are allowed per project"));
    }

    let basename = canon
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("cannot derive a name from {}", canon.display()))?;

    ensure_outside_root(state)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = allocate_and_link(&dir, basename, &canon, id)?;
    Ok(Record { name, path: canon, id })
}

#[cfg(not(unix))]
pub fn allow_in(_state: &Path, _parent: &str, _path: &Path) -> Result<Record, String> {
    Err("outside allows require a unix filesystem (dev/ino/btime identity)".into())
}

/// Deletes the record, if any. Not finding one is not an error — revoking
/// twice, or revoking a record that a prune already removed, is a no-op.
pub fn revoke_in(state: &Path, parent: &str, name: &str) -> Result<(), String> {
    if !crate::wtname::valid(name) {
        return Err(format!("invalid worktree name {name:?}"));
    }
    let path = record_path_in(state, parent, name).ok_or_else(|| format!("invalid parent {parent:?}"))?;
    match std::fs::remove_file(path) {
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

    /// This project's own production filesystem, ext4 — not
    /// `std::env::temp_dir()` (`/tmp` here is tmpfs). `a_recreated_directory_does_not_resolve`
    /// and `a_symlink_to_a_fresh_directory_does_not_resolve` need a real
    /// recreate on the real filesystem roost actually runs on, because the
    /// whole point of this fix round is that tmpfs and ext4 disagree about
    /// inode reuse.
    fn ext4_tempdir() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("outside-ext4-").tempdir_in("/home/claude/scratch/tmp").unwrap()
    }

    /// Revert-checked: deleting the `if !crate::wtname::valid(name) { return
    /// None; }` guard in `split_url` made `split_url("p/.claude/outside/--bad")`
    /// return `Some(("p", "--bad"))` instead of `None`, failing this test's
    /// loop at that element — `assertion `left == right` failed: "p/.claude/outside/--bad"
    /// must not split\n  left: Some(("p", "--bad"))\n right: None`. Restored.
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

    /// Revert-checked, two separate runs: dropping `opts.mode(0o600)` in
    /// `write_body` made the 0600 assertion below fail — `assertion `left
    /// == right` failed: the record must be private\n  left: 436\n right:
    /// 384` (436 = 0o664, the default-umask mode written by `create_new`
    /// alone); separately, dropping the `set_permissions(.., 0o700)` call
    /// in `ensure_outside_root` made the 0700 assertion fail — `assertion
    /// `left == right` failed: state/outside itself must be private\n
    /// left: 509\n right: 448` (509 = 0o775, the default-umask mode
    /// `create_dir_all` alone leaves a directory at). Both restored.
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
            let file = dir_in(state.path(), "p").unwrap().join("wt");
            let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the record must be private");

            let root_mode = std::fs::metadata(state.path().join("outside")).unwrap().permissions().mode() & 0o777;
            assert_eq!(root_mode, 0o700, "state/outside itself must be private");
        }
    }

    /// Pins the identity *comparison* directly, independent of how any one
    /// filesystem happens to behave on a real recreate — that is what
    /// `a_recreated_directory_does_not_resolve` and
    /// `a_symlink_to_a_fresh_directory_does_not_resolve` are for, against
    /// real ext4. This fabricates two mismatches instead: a record whose
    /// stored identity is nudged by one in its last field, and a path whose
    /// final component was swapped for a symlink to an unrelated directory
    /// (so unrelated that every one of its four identity fields differs,
    /// regardless of any filesystem's inode-reuse behavior).
    ///
    /// Revert-checked, run together with the two tests below in one
    /// `cargo test --lib outside`: narrowing `resolve_in`'s comparison from
    /// `(md.dev(), md.ino(), bsec, bnsec) != rec.id` to `(md.dev(),
    /// md.ino()) != (rec.id.0, rec.id.1)` (dropping only the two birth-time
    /// fields, keeping dev/ino) made the *first* assertion below fail —
    /// `assertion `left == right` failed: a record with a nudged identity
    /// must not resolve\n  left: Some(..)\n right: None` — because this
    /// test's fabricated mismatch deliberately nudges only the last field
    /// (`rec.id.3 + 1`, a birth-time nanosecond), leaving dev/ino untouched.
    /// The test panicked there, so this run did not separately exercise the
    /// second assertion (the symlink-to-an-unrelated-directory case); that
    /// one differs in every field regardless, so it is pinned by the test
    /// passing forward, not by an isolated revert. Restored.
    #[test]
    fn a_record_whose_identity_differs_does_not_resolve() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        let wt = outside.path().join("wt");
        std::fs::create_dir(&wt).unwrap();
        let rec = allow_in(state.path(), "p", &wt).unwrap();
        let record_file = dir_in(state.path(), "p").unwrap().join(&rec.name);
        std::fs::write(&record_file, format!("{}\n{} {} {} {}\n", wt.display(), rec.id.0, rec.id.1, rec.id.2, rec.id.3 + 1))
            .unwrap();
        assert_eq!(resolve_in(state.path(), "p", "wt"), None, "a record with a nudged identity must not resolve");

        let wt2 = outside.path().join("wt2");
        std::fs::create_dir(&wt2).unwrap();
        allow_in(state.path(), "p", &wt2).unwrap();
        let elsewhere = outside.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::remove_dir(&wt2).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &wt2).unwrap();
        assert_eq!(resolve_in(state.path(), "p", "wt2"), None, "a symlink to an unrelated directory must not resolve");
    }

    /// Real `rm -rf` + `mkdir` at the *same path*, on ext4 — not fabricated.
    /// The reviewer's probe found this reuses the just-freed inode 50/50 on
    /// this host's production filesystem, so without birth time in the
    /// comparison this would (almost always) wrongly resolve.
    ///
    /// Revert-checked (same narrowing as the test above, same run): this
    /// failed too — `assertion `left == right` failed: a recreated
    /// directory must not resolve\n  left: Some(..)\n right: None` —
    /// confirming the inode really was reused on this run, on real ext4.
    /// Restored.
    #[test]
    fn a_recreated_directory_does_not_resolve() {
        let state = tempfile::tempdir().unwrap();
        let outside = ext4_tempdir();

        let wt = outside.path().join("wt");
        std::fs::create_dir(&wt).unwrap();
        allow_in(state.path(), "p", &wt).unwrap();

        std::fs::remove_dir(&wt).unwrap();
        std::fs::create_dir(&wt).unwrap();

        assert_eq!(resolve_in(state.path(), "p", "wt"), None, "a recreated directory must not resolve");
    }

    /// Real `rm -rf wt; mkdir other; ln -s other wt`, on ext4. The
    /// reviewer's probe found `other` reused `wt`'s just-freed inode 5/5 —
    /// so this is not merely "symlink elsewhere," it is the specific case
    /// where `(dev, ino)` alone would have matched by coincidence.
    ///
    /// Revert-checked (same narrowing, same run, as the two tests above):
    /// this failed too — `assertion `left == right` failed: a symlink to a
    /// freshly made directory must not resolve\n  left: Some(..)\n right:
    /// None` — confirming `other` really did reuse `wt`'s inode on this
    /// run. Restored.
    #[test]
    fn a_symlink_to_a_fresh_directory_does_not_resolve() {
        let state = tempfile::tempdir().unwrap();
        let outside = ext4_tempdir();

        let wt = outside.path().join("wt");
        std::fs::create_dir(&wt).unwrap();
        allow_in(state.path(), "p", &wt).unwrap();

        std::fs::remove_dir(&wt).unwrap();
        let other = outside.path().join("other");
        std::fs::create_dir(&other).unwrap();
        std::os::unix::fs::symlink(&other, &wt).unwrap();

        assert_eq!(resolve_in(state.path(), "p", "wt"), None, "a symlink to a freshly made directory must not resolve");
    }

    /// Revert-checked: dropping the `if parts.next().is_some() { return
    /// None; }` guard right after the 4th identity field is parsed made the
    /// `extra_id_fields` case (one line short of `extra_lines`'s 3 lines —
    /// `"1 2 3 4 5"` all on line 2 — specifically chosen so the *line*-count
    /// guard above it can't be what catches it) keep resolving the first 4
    /// whitespace-separated tokens and ignore the 5th: `read_in` returned
    /// `Some(..)` instead of `None`, and this test's loop failed on it.
    /// Restored. The other five cases (one line, non-numeric, extra lines,
    /// a legacy two-field identity line, an empty file) are pinned by
    /// construction — each is exactly the shape its own `?`/field-count
    /// check exists to reject — and were not separately re-broken and
    /// re-run.
    #[test]
    fn a_damaged_record_does_not_resolve() {
        let state = tempfile::tempdir().unwrap();
        let dir = dir_in(state.path(), "p").unwrap();
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("one_line"), "/tmp/somewhere\n").unwrap();
        std::fs::write(dir.join("non_numeric"), "/tmp/somewhere\nnot-numeric junk\n").unwrap();
        std::fs::write(dir.join("extra_lines"), "/tmp/somewhere\n1 2 3 4\nextra\n").unwrap();
        std::fs::write(dir.join("extra_id_fields"), "/tmp/somewhere\n1 2 3 4 5\n").unwrap();
        std::fs::write(dir.join("empty"), "").unwrap();
        // The format this module briefly had before birth time joined the
        // identity. No production record was ever written this way, but the
        // parser must still treat it as damaged, not as a weaker-but-valid
        // record.
        std::fs::write(dir.join("legacy_two_field"), "/tmp/somewhere\n1 2\n").unwrap();

        for name in ["one_line", "non_numeric", "extra_lines", "extra_id_fields", "empty", "legacy_two_field"] {
            assert_eq!(read_in(state.path(), "p", name), None, "{name:?} must be damaged");
            assert_eq!(resolve_in(state.path(), "p", name), None, "{name:?} must not resolve");
        }

        #[cfg(unix)]
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(dir.join("unreadable"), "/tmp/somewhere\n1 2 3 4\n").unwrap();
            std::fs::set_permissions(dir.join("unreadable"), std::fs::Permissions::from_mode(0o000)).unwrap();
            assert_eq!(read_in(state.path(), "p", "unreadable"), None, "unreadable must be None");
            // restore so the tempdir can be cleaned up
            std::fs::set_permissions(dir.join("unreadable"), std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    /// The relative-path case this test used to carry (`allow_in(...,
    /// Path::new("relative/wt"))`) was removed: it was not discriminating.
    /// `"relative/wt"` does not exist relative to `cargo test`'s working
    /// directory, so even with the `is_absolute()` guard deleted entirely,
    /// the very next step (`canonicalize`) would fail with `NotFound` and
    /// `allow_in` would still return `Err` — for the wrong reason. A test
    /// that passes the same way whether or not the code it names is there
    /// is the exact defect CLAUDE.md's testing section calls out ("would
    /// this fail if I deleted the code it covers?"). Making it genuinely
    /// discriminating needs a relative path that actually exists from the
    /// test binary's cwd, which means creating something under the crate's
    /// own working directory — not worth the risk of leaving stray
    /// directories behind in this repo if a run panics before cleanup.
    ///
    /// Revert-checked: skipping the `canon != path` check in `allow_in` made
    /// the symlinked-parent case below fail — `assertion failed:
    /// allow_in(state.path(), "p", &link).is_err()` — and the test panicked
    /// there, before reaching the "file, not a directory" case or the final
    /// "nothing written" assertion, so this run observed only that one
    /// failure directly. Restored.
    #[test]
    fn allow_refuses_a_symlinked_path_or_a_file() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        // Through a symlinked parent.
        let real = outside.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = outside.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(allow_in(state.path(), "p", &link).is_err());

        // A file, not a directory.
        let file = outside.path().join("f");
        std::fs::write(&file, b"x").unwrap();
        assert!(allow_in(state.path(), "p", &file).is_err());

        assert!(list_in(state.path(), "p").is_empty(), "nothing must have been written");
    }

    /// A newline anywhere in the path — not just the final component, which
    /// `wtname::check` already rejects as a basename — would otherwise be
    /// written verbatim (well, `Display`-formatted) into line 1 of the
    /// record. A directory one level up from the worktree, so the broken
    /// basename check can't be what is pinning this.
    ///
    /// Revert-checked: removing the `s.contains('\n')` guard made this
    /// fail — `unwrap_err()` panicked with `called Result::unwrap_err() on
    /// an Ok value: Record { name: "wt", path: "…/mid\nline/wt", id: (..) }`
    /// — the panic message itself shows the record `allow_in` would have
    /// written, newline and all. Restored.
    #[test]
    fn allow_refuses_a_path_containing_a_newline() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mid = outside.path().join("mid\nline");
        std::fs::create_dir_all(&mid).unwrap();
        let wt = mid.join("wt");
        std::fs::create_dir(&wt).unwrap();

        let err = allow_in(state.path(), "p", &wt).unwrap_err();
        assert!(err.contains("newline"), "got {err:?}");
        assert!(list_in(state.path(), "p").is_empty(), "nothing must have been written");
    }

    /// A non-UTF-8 byte in a *middle* path component — the basename itself
    /// is plain ASCII, so the pre-existing `canon.file_name().and_then(|n|
    /// n.to_str())` check (needed anyway, to allocate a name) would not have
    /// caught this on its own; only an explicit check of the whole path
    /// does.
    ///
    /// Revert-checked (swapping `path.to_str().ok_or_else(..)?` for
    /// `path.to_string_lossy()`, which never fails): `unwrap_err()`
    /// panicked with `called Result::unwrap_err() on an Ok value: Record {
    /// name: "wt", path: "…/bad-\xFFdir/wt", id: (..) }` — the lossy
    /// conversion let the whole allow through, writing a record for a path
    /// that no longer byte-matches what is really on disk. Restored.
    #[test]
    fn allow_refuses_a_non_utf8_path() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mut mid_bytes = outside.path().as_os_str().as_bytes().to_vec();
        mid_bytes.extend_from_slice(b"/bad-\xffdir");
        let mid = PathBuf::from(OsStr::from_bytes(&mid_bytes));
        let wt = mid.join("wt");
        std::fs::create_dir_all(&wt).unwrap();

        let err = allow_in(state.path(), "p", &wt).unwrap_err();
        assert!(err.contains("UTF-8"), "got {err:?}");
        assert!(list_in(state.path(), "p").is_empty(), "nothing must have been written");
    }

    /// Revert-checked: replacing `std::fs::hard_link` with `std::fs::rename`
    /// in `allocate_and_link` made `rec_b`'s second `allow_in` call silently
    /// overwrite `rec_a`'s record file at the name `"wt"` instead of falling
    /// through to `"wt-2"` — this is exactly the collision `hard_link`'s
    /// `AlreadyExists` is supposed to catch. Failed with: `assertion `left
    /// == right` failed\n  left: "wt"\n right: "wt-2"`. Restored.
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

    /// Revert-checked: removing the whole `records.iter().find(|r| r.path
    /// == canon)` dedup-by-path block (not just its inner `existing.id ==
    /// id` shortcut — that shortcut alone turned out not to be what this
    /// test pins, since the fall-through "stale rewrite" path reuses the
    /// *same* name and writes the *same* identity when nothing actually
    /// changed, so `first == second` still held; the real guard is the
    /// lookup itself) made every call allocate a fresh name — `second`
    /// came back named `"wt-2"` instead of `"wt"`. Failed with: `assertion
    /// `left == right` failed\n  left: Record { name: "wt", .. }\n right:
    /// Record { name: "wt-2", .. }`. Restored.
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

    /// Re-allowing a path whose directory was recreated (same path, new
    /// identity, same name on disk already) must not be blocked by its own
    /// stale record.
    ///
    /// Revert-checked: replacing the stale-rewrite branch with `return
    /// Ok(existing.clone())` (returning the old record unchanged instead of
    /// rewriting it) made `second` literally equal `first` — identity
    /// included — so this test's own setup assertion caught it before
    /// reaching the "must resolve" one: `assertion `left != right` failed:
    /// setup: the recreated directory must actually have a new identity\n
    /// left: (64512, 9733058, ..)\n right: (64512, 9733058, ..)` (both
    /// sides identical). That is still the right failure for this revert —
    /// it shows directly that `second` was never recomputed from the
    /// recreated directory at all. Restored.
    #[test]
    fn re_allowing_a_recreated_path_resolves() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let wt = outside.path().join("wt");

        std::fs::create_dir(&wt).unwrap();
        let first = allow_in(state.path(), "p", &wt).unwrap();

        std::fs::remove_dir(&wt).unwrap();
        std::fs::create_dir(&wt).unwrap();
        let second = allow_in(state.path(), "p", &wt).unwrap();

        assert_eq!(second.name, first.name, "the same name is reused, not a fresh -2");
        assert_ne!(second.id, first.id, "setup: the recreated directory must actually have a new identity");
        assert_eq!(resolve_in(state.path(), "p", &second.name), Some(wt.canonicalize().unwrap()), "the second allow must resolve");
    }

    /// Revert-checked: changing the cap comparison from `>=` to `>` let the
    /// 33rd allow through — `unwrap_err()` panicked with `called
    /// Result::unwrap_err() on an Ok value: Record { .. }`. Restored.
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

    /// Revert-checked (skipped under root, where chmod 0300 has no effect):
    /// with `allow_in`'s `list_result_in(state, parent)?` swapped for the
    /// error-swallowing `list_in(state, parent)`, this failed — `allow_in`
    /// returned `Ok(Record { name: "wt2", .. })` instead of `Err`: `an
    /// unlistable records directory must refuse the allow, got
    /// Ok(Record { name: "wt2", path: "…/wt2", id: (..) })`. With the real
    /// code (`list_result_in`) and the same 0300 permissions, the test
    /// passes — confirmed by running it once before this revert, too.
    /// Restored.
    #[test]
    fn allow_refuses_when_existing_records_cannot_be_listed() {
        if unsafe { libc::geteuid() } == 0 {
            return; // root reads through 0o000; nothing to assert
        }
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let wt = outside.path().join("wt");
        std::fs::create_dir(&wt).unwrap();
        allow_in(state.path(), "p", &wt).unwrap(); // creates the per-parent dir

        let dir = dir_in(state.path(), "p").unwrap();
        use std::os::unix::fs::PermissionsExt;
        // 0300 (write+execute, no read), not 0000: `read_dir` needs the read
        // bit and fails, but creating a *new* name only needs write+execute
        // on the directory, not read, so `hard_link` would still succeed.
        // That asymmetry is the point — it isolates "the listing failed"
        // from "the write would also have failed," which 0000 would not:
        // under 0000 even the correct code's write attempt fails too, so a
        // test built on 0000 can't tell this refusal apart from one that
        // just happens to fail later for an unrelated reason.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o300)).unwrap();

        let wt2 = outside.path().join("wt2");
        std::fs::create_dir(&wt2).unwrap();
        let result = allow_in(state.path(), "p", &wt2);

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err(), "an unlistable records directory must refuse the allow, got {result:?}");
    }

    /// Revert-checked: dropping the `..`-segment check in `dir_in` (keeping
    /// only the emptiness check) made `dir_in(state.path(), "a/../b")` come
    /// back `Some(..)` instead of `None`, failing this test's loop —
    /// `assertion `left == right` failed: "a/../b" must be rejected\n  left:
    /// Some(..)\n right: None`. Restored.
    #[test]
    fn an_invalid_parent_is_refused() {
        let state = tempfile::tempdir().unwrap();
        for bad in ["", "a/../b", ".."] {
            assert_eq!(dir_in(state.path(), bad), None, "{bad:?} must be rejected");
            assert_eq!(read_in(state.path(), bad, "wt"), None);
            assert_eq!(resolve_in(state.path(), bad, "wt"), None);
            assert!(list_in(state.path(), bad).is_empty());
        }

        let outside = tempfile::tempdir().unwrap();
        let wt = outside.path().join("wt");
        std::fs::create_dir(&wt).unwrap();
        assert!(allow_in(state.path(), "a/../b", &wt).is_err());
    }

    /// Revert-checked: dropping the `if listed.is_empty() { return
    /// Vec::new(); }` guard in `prune_in` made the "empty listing keeps it"
    /// step below delete both records instead of none — panicked with "an
    /// empty listing must remove nothing, got [\"drop\", \"keep\"]".
    /// Restored.
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

        let empty_removed = prune_in(state.path(), "p", &[]);
        assert!(empty_removed.is_empty(), "an empty listing must remove nothing, got {empty_removed:?}");
        assert_eq!(read_in(state.path(), "p", &keep_rec.name).map(|r| r.name), Some(keep_rec.name.clone()));
        assert_eq!(read_in(state.path(), "p", &drop_rec2.name).map(|r| r.name), Some(drop_rec2.name.clone()));

        let removed = prune_in(state.path(), "p", &[keep_rec.path.clone()]);
        assert_eq!(removed, vec![drop_rec2.name]);
        assert_eq!(read_in(state.path(), "p", &keep_rec.name).map(|r| r.name), Some(keep_rec.name));
    }
}
