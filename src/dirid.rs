//! One directory identity, shared by every rename-following check in #183.
//!
//! **Why birth time is part of the identity.** `(dev, ino)` alone is not
//! enough: `outside.rs`'s probe of this project's own production filesystem
//! (ext4) found that `rm` + `mkdir` at the same path reused the just-freed
//! inode number in 50 of 50 cycles, and a *different*, unrelated directory
//! created moments later reused it too. `Metadata::created()` — the
//! filesystem's birth time — was the one thing that changed on every single
//! recreation in that same probe, on both ext4 and tmpfs. So `DirId` is
//! `(dev, ino, bsec, bnsec)`, not two fields, and `of`/`of_entry` refuse to
//! produce an identity at all when the birth time cannot be read — never a
//! fallback back down to `(dev, ino)` alone, which is exactly the hole the
//! measurement describes.
//!
//! **Why a legacy identity may protect a directory but must never license a
//! follow.** #181's original `.ino` records predate birth time and carry
//! only `(dev, ino)` — `Stored::Legacy`. Two fields are weak evidence of
//! *which* directory this is (the reuse above is exactly why), but they are
//! not nothing: they still rule out almost everything else on the
//! filesystem, which is enough to justify the *keep* direction — refusing to
//! reap or forget a session just because an old-format record has not yet
//! been upgraded. They are not enough to justify the *follow* direction —
//! redirecting a terminal's ownership to a new place on the strength of a
//! match that ext4 can produce by coincidence is exactly the vulnerability
//! the birth-time field exists to close, and treating the two directions as
//! interchangeable — accepting a legacy match wherever a full one would be
//! accepted — is the same mistake with a different name. `matches` only
//! reports *whether* `stored` names `id`; deciding which direction that
//! answer is about to be used for, and whether a `Legacy` match is good
//! enough for it, is the caller's job, not this module's.
//!
//! Like every path in this codebase (CLAUDE.md, "absence of evidence is not
//! evidence of absence"), `of`/`of_entry` have three outcomes, not two:
//! `Ok(None)` for a path that genuinely is not there (`NotFound`), `Err` for
//! anything else this process could not determine (a permission error, an
//! unreadable birth time), and `Ok(Some(id))` only when every field was
//! actually read. Folding the middle case into either of the other two is
//! the "I could not determine X" defect this codebase has shipped a dozen
//! times already.

use std::path::Path;

/// One directory's identity: `(dev, ino, bsec, bnsec)`. `bsec` is signed
/// because a birth time before the epoch (a restored backup, a clock set
/// wrong once) is still a real, comparable time, not an error — only an
/// *unreadable* birth time is refused, by `of`/`of_entry` returning `Err`,
/// never by lying about the sign.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DirId {
    pub dev: u64,
    pub ino: u64,
    pub bsec: i64,
    pub bnsec: u32,
}

/// What a `.ino`-style record file actually holds: either a full identity,
/// or a pre-birth-time `(dev, ino)` pair from #181. `parse_line` is the only
/// thing that produces this; everything else that cares about which kind of
/// match is good enough goes through `matches`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stored {
    Full(DirId),
    Legacy { dev: u64, ino: u64 },
}

/// Birth time as `(bsec, bnsec)`, in the same encoding `format`/`parse_line`
/// use: nanoseconds since the epoch equal `bsec * 1_000_000_000 + bnsec`,
/// with `bnsec` always in `0..1_000_000_000` — so a time before the epoch
/// gets a negative `bsec` and a *positive* `bnsec` counting forward from it,
/// never a negative `bnsec`. An unreadable birth time is `Err`; there is no
/// fallback, per the module doc.
/// Converts `before` — how far before the epoch a time is, i.e.
/// `UNIX_EPOCH - that time`, always non-negative — to the signed `(bsec,
/// bnsec)` encoding `format`/`parse_line` use, where `bnsec` stays
/// non-negative by borrowing a second whenever there's a sub-second
/// remainder. Pulled out of `birth` so the conversion can be unit-tested
/// without needing a real pre-epoch file (nothing on this host can be
/// dated before 1970 to order).
fn pre_epoch(before: std::time::Duration) -> (i64, u32) {
    if before.subsec_nanos() == 0 {
        (-(before.as_secs() as i64), 0)
    } else {
        (-(before.as_secs() as i64) - 1, 1_000_000_000 - before.subsec_nanos())
    }
}

#[cfg(unix)]
fn birth(md: &std::fs::Metadata) -> Result<(i64, u32), String> {
    let created = md.created().map_err(|e| format!("birth time unreadable: {e}"))?;
    match created.duration_since(std::time::UNIX_EPOCH) {
        Ok(dur) => Ok((dur.as_secs() as i64, dur.subsec_nanos())),
        Err(e) => Ok(pre_epoch(e.duration())),
    }
}

#[cfg(unix)]
fn id_from(result: std::io::Result<std::fs::Metadata>) -> Result<Option<DirId>, String> {
    use std::os::unix::fs::MetadataExt;
    let md = match result {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let (bsec, bnsec) = birth(&md)?;
    Ok(Some(DirId { dev: md.dev(), ino: md.ino(), bsec, bnsec }))
}

#[cfg(not(unix))]
fn id_from(_result: std::io::Result<std::fs::Metadata>) -> Result<Option<DirId>, String> {
    Err("dirid requires unix metadata".to_string())
}

/// The directory's identity from its own `metadata` (follows a final
/// symlink, like `outside::resolve_in`). `Ok(None)` = `NotFound`; `Err` =
/// cannot tell (any other error, or an unreadable birth time).
pub fn of(path: &Path) -> Result<Option<DirId>, String> {
    id_from(std::fs::metadata(path))
}

/// Same, but `symlink_metadata` (for scanning directory entries without
/// resolving the entry itself).
pub fn of_entry(path: &Path) -> Result<Option<DirId>, String> {
    id_from(std::fs::symlink_metadata(path))
}

/// `"dev ino bsec bnsec"`, the on-disk line format for a full identity.
pub fn format(id: &DirId) -> String {
    format!("{} {} {} {}", id.dev, id.ino, id.bsec, id.bnsec)
}

/// Parses one line of a `.ino`-style record. Four whitespace-separated
/// fields is a full identity; exactly two is a legacy `(dev, ino)` pair;
/// anything else — three fields, five, a non-numeric field, an empty line —
/// is damaged, read as `None` rather than guessed at.
pub fn parse_line(line: &str) -> Option<Stored> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    match parts.as_slice() {
        [dev, ino, bsec, bnsec] => Some(Stored::Full(DirId {
            dev: dev.parse().ok()?,
            ino: ino.parse().ok()?,
            bsec: bsec.parse().ok()?,
            bnsec: bnsec.parse().ok()?,
        })),
        [dev, ino] => Some(Stored::Legacy { dev: dev.parse().ok()?, ino: ino.parse().ok()? }),
        _ => None,
    }
}

/// Does `stored` name the directory `id`? A `Full` identity compares all
/// four fields; a `Legacy` one compares only `(dev, ino)` — weaker evidence,
/// by construction (see the module doc). Callers decide whether that
/// weaker match is good enough for what they are about to do with it; this
/// function only answers "does it match," never "is that enough."
pub fn matches(stored: &Stored, id: &DirId) -> bool {
    match *stored {
        Stored::Full(sid) => sid == *id,
        Stored::Legacy { dev, ino } => dev == id.dev && ino == id.ino,
    }
}

/// Atomic 0600 write of one or more lines to `path`: a temp file in the same
/// directory (`wsstate::tmp_suffix` for the name, `create_new` so two
/// writers can never silently clobber each other's temp file, `mode(0o600)`
/// so the content is never briefly world- or group-readable), then
/// `rename`. The temp file is removed on any error along the way — this
/// mirrors `outside.rs`'s `write_body`/`replace_record` pair, collapsed into
/// one call because every caller here always wants the final name.
pub fn write_lines(path: &Path, lines: &[String]) -> Result<(), String> {
    let dir = path.parent().ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{} has no file name", path.display()))?;
    let tmp = dir.join(format!(".{file_name}.{}.tmp", crate::wsstate::tmp_suffix()));

    let mut body = String::new();
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }

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

    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// `$HOME`, not a hardcoded path or `std::env::tempdir()`: this host's
    /// `/tmp` is tmpfs, and the inode-reuse behavior this module exists for
    /// is an ext4 property, so a test that wants to actually exercise it has
    /// to land on ext4. `$HOME` sits on the ext4 root on this host and on
    /// GitHub's runners; falling back to `std::env::temp_dir()` only when
    /// `$HOME` is unset is a courtesy for some other environment, not a
    /// correctness requirement — `a_recreated_directory_differs`'s
    /// assertion (`!=`) holds on any filesystem, a fallback onto tmpfs just
    /// means that run didn't get to probe ext4's reuse.
    fn ext4_tempdir() -> tempfile::TempDir {
        let base = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        tempfile::Builder::new().prefix("dirid-ext4-").tempdir_in(base).unwrap()
    }

    /// Covers all three of `of`'s outcomes (module doc): an existing
    /// directory is `Ok(Some)`, a missing path is `Ok(None)`, and anything
    /// else this process cannot determine — here, a path whose parent
    /// cannot even be entered — is `Err`, never folded into either of the
    /// other two.
    ///
    /// Revert-checked: changed `id_from` so every error, not just
    /// `NotFound`, returns `Ok(None)` (`Err(_) => return Ok(None)` in place
    /// of the `NotFound`-only arm and the `Err(e) => return Err(..)` arm).
    /// The first two assertions were unaffected — this test's own first
    /// version only ever exercised `NotFound`, not a *different* error, so
    /// it could not have caught this; that gap is exactly why the
    /// permission-denied case below was added. With the break in place,
    /// observed failure (a plain `assert!`, so only the message prints):
    /// `panicked at src/dirid.rs:...: a permission error must not read as
    /// NotFound` — the permission error was silently read as "not there."
    /// Restored.
    #[test]
    fn of_reads_a_directory_and_says_notfound_for_none() {
        let dir = tempfile::tempdir().unwrap();
        let id = of(dir.path()).unwrap();
        assert!(id.is_some(), "an existing directory must produce an identity");

        let gone = dir.path().join("does-not-exist");
        assert_eq!(of(&gone).unwrap(), None, "a missing path is NotFound, not an error");

        #[cfg(unix)]
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt;
            let locked = dir.path().join("locked");
            std::fs::create_dir(&locked).unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
            let unreachable = locked.join("inner");
            assert!(of(&unreachable).is_err(), "a permission error must not read as NotFound");
            // Restore so the tempdir can be cleaned up on drop.
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    /// Real `rm` + `mkdir` at the same path, on ext4 (see `ext4_tempdir`).
    /// #191's probe found ext4 reuses a directory's just-freed inode in
    /// 50/50 such cycles — this is the exact case birth time exists to
    /// catch; without it, `first` and `second` would share `(dev, ino)`
    /// and this assertion would need to be `==`, not `!=`.
    ///
    /// Revert-checked: narrowed this test's own assertion to `assert_ne!(
    /// (first.dev, first.ino), (second.dev, second.ino), ...)` (dropped
    /// birth time from the check; `DirId`/`of` untouched). Observed failure
    /// on this host's `$HOME` (ext4): `assertion `left != right` failed: a
    /// recreated directory must get a new identity\n  left: (64512,
    /// 4332556)\n right: (64512, 4332556)` — the recreated directory reused
    /// the freed inode (and dev), so `(dev, ino)` alone matched. Restored
    /// to comparing the full `DirId`.
    #[test]
    fn a_recreated_directory_differs() {
        let outside = ext4_tempdir();
        let target = outside.path().join("wt");
        std::fs::create_dir(&target).unwrap();
        let first = of(&target).unwrap().unwrap();

        std::fs::remove_dir(&target).unwrap();
        std::fs::create_dir(&target).unwrap();
        let second = of(&target).unwrap().unwrap();

        assert_ne!(first, second, "a recreated directory must get a new identity");
    }

    /// Revert-checked: changed `parse_line`'s match arm for `[dev, ino,
    /// _extra]` to accept a 3-field line as `Legacy { dev, ino }` (ignoring
    /// the third field), leaving the 4- and 2-field arms alone. Observed
    /// failure: `assertion `left == right` failed: "1 2 3" must be
    /// damaged\n  left: Some(Legacy { dev: 1, ino: 2 })\n right: None`.
    /// Restored to refusing any length but 4 or 2.
    #[test]
    fn parse_line_full_legacy_and_damaged() {
        assert_eq!(parse_line("1 2 3 4"), Some(Stored::Full(DirId { dev: 1, ino: 2, bsec: 3, bnsec: 4 })));
        assert_eq!(parse_line("8 9"), Some(Stored::Legacy { dev: 8, ino: 9 }));

        // One field past u64::MAX: parses as a number, but not as a u64.
        let overflow = "18446744073709551616 2 3 4";
        for damaged in ["1 2 3", "1 2 3 4 5", "", "a b c d", "1 2 x y", overflow] {
            assert_eq!(parse_line(damaged), None, "{damaged:?} must be damaged");
        }
    }

    /// Revert-checked: changed `matches`'s `Legacy` arm to `Stored::Legacy
    /// { .. } => true` (ignoring `dev`/`ino` entirely; the `Full` arm
    /// untouched). Observed failure (a plain `assert!`, so only the message
    /// prints): `panicked at src/dirid.rs:...: legacy still requires dev
    /// and ino` — a `Legacy` matched a directory with a different inode.
    /// Restored to comparing `(dev, ino)`.
    #[test]
    fn matches_legacy_by_dev_ino_only() {
        let legacy = Stored::Legacy { dev: 1, ino: 2 };
        let same_dev_ino_different_birth = DirId { dev: 1, ino: 2, bsec: 999, bnsec: 0 };
        assert!(matches(&legacy, &same_dev_ino_different_birth), "legacy ignores birth time");

        let different_ino = DirId { dev: 1, ino: 3, bsec: 999, bnsec: 0 };
        assert!(!matches(&legacy, &different_ino), "legacy still requires dev and ino");

        let full = Stored::Full(DirId { dev: 1, ino: 2, bsec: 5, bnsec: 6 });
        assert!(matches(&full, &DirId { dev: 1, ino: 2, bsec: 5, bnsec: 6 }));
        assert!(!matches(&full, &DirId { dev: 1, ino: 2, bsec: 5, bnsec: 7 }), "full compares birth time too");
    }

    /// Revert-checked: commented out the `opts.mode(0o600)` call in
    /// `write_lines` (the `#[cfg(unix)] { use ...; opts.mode(0o600); }`
    /// block), leaving `create_new`/`rename` untouched. Observed failure:
    /// `assertion `left == right` failed: the record must be private\n
    /// left: 436\n right: 384` (436 = 0o664, the default-umask mode
    /// `create_new` writes without an explicit `mode`). Restored.
    #[test]
    fn write_lines_is_0600_and_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record");

        write_lines(&path, &["a".to_string(), "b".to_string()]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nb\n");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the record must be private");
        }

        // A second write replaces the file wholesale (the atomic part: a
        // reader never sees a half-written mix of old and new content),
        // and leaves no temp file behind either time.
        write_lines(&path, &["c".to_string()]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "c\n", "a second write replaces the file");

        let leftovers: Vec<_> =
            std::fs::read_dir(dir.path()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains(".tmp")).collect();
        assert!(leftovers.is_empty(), "a successful write must leave no temp file behind");
    }

    /// `DirId`'s `bsec` is signed (a birth time before the epoch is a real
    /// time, not an error — see `DirId`'s doc), so `format`/`parse_line`
    /// must round-trip a negative `bsec` exactly, not just the positive
    /// case every other test here happens to use.
    ///
    /// Revert-checked: changed `format` to write `id.bsec.unsigned_abs()`
    /// instead of `id.bsec` (dropping the sign; `parse_line` untouched).
    /// Observed failure: `assertion `left == right` failed: a negative
    /// bsec must round-trip through format/parse_line\n  left:
    /// Some(Full(DirId { dev: 7, ino: 11, bsec: 5, bnsec: 250 }))\n right:
    /// Some(Full(DirId { dev: 7, ino: 11, bsec: -5, bnsec: 250 }))` — the
    /// sign was lost on the way out, so the parsed-back identity no longer
    /// matched the original. Restored.
    #[test]
    fn a_birth_time_before_the_epoch_round_trips() {
        let id = DirId { dev: 7, ino: 11, bsec: -5, bnsec: 250 };
        let line = format(&id);
        assert_eq!(parse_line(&line), Some(Stored::Full(id)), "a negative bsec must round-trip through format/parse_line");
    }

    /// `pre_epoch` is the pure conversion `birth` applies to a pre-epoch
    /// `Duration`; tested directly because no file on this host can
    /// actually be dated before 1970 to exercise it through `of`.
    ///
    /// Revert-checked: dropped the "borrow a second" adjustment, changing
    /// the `else` branch to `(-(before.as_secs() as i64), 1_000_000_000 -
    /// before.subsec_nanos())` (no `- 1` on `bsec`). Observed failure:
    /// `assertion `left == right` failed\n  left: (-1, 750000000)\n right:
    /// (-2, 750000000)` — the flipped version reported 1.25s before the
    /// epoch as `bsec: -1` (i.e. only 0.25s before epoch, off by a whole
    /// second), instead of the correct `-2` with `bnsec` counting forward
    /// from it. Restored the `- 1` borrow.
    #[test]
    fn pre_epoch_conversion_borrows_a_second() {
        // 1.25s before the epoch: one whole second borrowed into bsec, the
        // remaining 0.75s carried forward as bnsec.
        assert_eq!(pre_epoch(std::time::Duration::new(1, 250_000_000)), (-2, 750_000_000));
        // Exactly N seconds before the epoch, no remainder: no borrow.
        assert_eq!(pre_epoch(std::time::Duration::new(3, 0)), (-3, 0));
    }
}
