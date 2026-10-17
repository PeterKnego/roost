//! Which project a terminal belongs to, when that is not the project it was
//! started in.
//!
//! A session cannot move. Its dtach socket path is baked into the master's
//! argv and every liveness check matches on it (`registry::pids_holding`), its
//! `SESSIONS` key is captured by the pump thread, and its environment is the
//! process's own. So a terminal re-homed into a worktree keeps all of those
//! naming its *home*, and this one record names its *owner*: the project it is
//! shown in, counted in, and closed with. `registry::reconcile` never reads
//! it, which is the point — the physical layout it sweeps does not change.
//!
//! Three outcomes on read, never two. An empty or unreadable record is
//! `Unknown`: it may be half-written, and treating it as `Here` would let
//! Close Project on the home end a Claude that belongs to a worktree.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    Here,
    Elsewhere(String),
    Unknown(String),
}

fn dir_in(state: &Path, home: &str) -> PathBuf {
    state.join("home").join(crate::projects::storage_key(home))
}

pub fn owner_of_in(state: &Path, home: &str, name: &str) -> Owner {
    // Validate the session name before building any path; Unknown is the
    // destroy-nothing direction, whereas Here would let Close Project act.
    if !crate::session::valid_name(name) {
        return Owner::Unknown(format!("invalid session name {name:?}"));
    }
    match std::fs::read_to_string(dir_in(state, home).join(name)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Owner::Here,
        Err(e) => Owner::Unknown(format!("cannot read re-home record: {e}")),
        Ok(s) => {
            let s = s.trim_end_matches('\n');
            if s.is_empty() { Owner::Unknown("empty re-home record".into()) } else { Owner::Elsewhere(s.to_string()) }
        }
    }
}

pub fn record_in(state: &Path, home: &str, name: &str, owner: &str) -> Result<(), String> {
    if !crate::session::valid_name(name) {
        return Err(format!("bad session name {name:?}"));
    }
    let dir = dir_in(state, home);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    std::fs::write(&tmp, format!("{owner}\n")).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(name)).map_err(|e| { let _ = std::fs::remove_file(&tmp); e.to_string() })
}

pub fn forget_in(state: &Path, home: &str, name: &str) {
    if crate::session::valid_name(name) {
        let _ = std::fs::remove_file(dir_in(state, home).join(name));
    }
}

/// Every record, as (home, name, owner). Dot-files (temps) skipped.
fn all_in(state: &Path) -> Vec<(String, String, Owner)> {
    let mut out = Vec::new();
    let Ok(homes) = std::fs::read_dir(state.join("home")) else { return out };
    for h in homes.flatten() {
        let home = crate::registry::decode_key(&h.file_name().to_string_lossy());
        let Ok(names) = std::fs::read_dir(h.path()) else { continue };
        for n in names.flatten() {
            let name = n.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !crate::session::valid_name(&name) { continue; }
            let owner = owner_of_in(state, &home, &name);
            out.push((home.clone(), name, owner));
        }
    }
    out.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    out
}

/// Every home that has at least one re-home record under it — what Task
/// 12's gone-worktree sweep (`claudes::detect_gone_owners`) needs to know
/// which repositories to even ask git about: a record is the only reason to
/// spend a `git worktree list` on a home at all, and a home with none must
/// cost zero git calls (CLAUDE.md's "if the scan finds no Claudes and no
/// records, it must spawn no git at all").
pub fn all_homes_in(state: &Path) -> Vec<String> {
    let mut out: Vec<String> = all_in(state).into_iter().map(|(h, _, _)| h).collect();
    out.sort();
    out.dedup();
    out
}

pub fn all_homes() -> Vec<String> { all_homes_in(&crate::wsstate::state_dir()) }

pub fn residents_of_in(state: &Path, owner: &str) -> Vec<(String, String)> {
    all_in(state).into_iter()
        .filter_map(|(h, n, o)| matches!(&o, Owner::Elsewhere(x) if x == owner).then_some((h, n)))
        .collect()
}

pub fn away_from_in(state: &Path, home: &str) -> Vec<(String, Owner)> {
    all_in(state).into_iter().filter(|(h, _, _)| h == home).map(|(_, n, o)| (n, o)).collect()
}

pub fn sweep_in(state: &Path, sock_state: &dyn Fn(&str, &str) -> Option<bool>) {
    for (home, name, _) in all_in(state) {
        if sock_state(&home, &name) == Some(false) {
            forget_in(state, &home, &name);
        }
    }
}

pub fn owner_of(home: &str, name: &str) -> Owner { owner_of_in(&crate::wsstate::state_dir(), home, name) }
pub fn record(home: &str, name: &str, owner: &str) -> Result<(), String> { record_in(&crate::wsstate::state_dir(), home, name, owner) }
pub fn forget(home: &str, name: &str) { forget_in(&crate::wsstate::state_dir(), home, name) }
pub fn residents_of(owner: &str) -> Vec<(String, String)> { residents_of_in(&crate::wsstate::state_dir(), owner) }
pub fn away_from(home: &str) -> Vec<(String, Owner)> { away_from_in(&crate::wsstate::state_dir(), home) }
pub fn sweep(sock_state: &dyn Fn(&str, &str) -> Option<bool>) { sweep_in(&crate::wsstate::state_dir(), sock_state) }

/// Whether `owner` is still a place `home`'s re-homed session can be shown:
/// one of `home`'s own `.claude/worktrees/<dir>`, still a directory.
///
/// Fork-free on purpose. `projects::resolve_project` on the owner URL would
/// run `git worktree list` (its dot segment needs git to vouch for it), and
/// this runs from `Hub::new` under the process-global hub-registry lock. So
/// the owner is checked by shape against the home, and only the home — which
/// has no dot segment, so resolves without git — goes through the resolver.
///
/// A `false` here destroys nothing: the session is shown in its home, and
/// the record is kept (see the module doc and `session::owned_live_names`).
pub fn resolves_in(roots: &[PathBuf], home: &str, owner: &str) -> bool {
    owner_dir_in(roots, home, owner).is_some()
}

pub fn resolves(home: &str, owner: &str) -> bool {
    resolves_in(&crate::projects::roots(), home, owner)
}

/// Where `owner` would live on disk, shape-validated against `home`,
/// whether or not anything is there right now. `owner_dir_in` is this plus
/// an existence check; Task 12's git-based gone-worktree detection
/// (`claudes::detect_gone_owners`) needs the path itself even once the
/// worktree is gone and nothing answers `symlink_metadata` any more — it
/// compares this against `git worktree list`'s own (still-existing) entries
/// rather than trusting a stat that a removed directory can never pass.
fn owner_path_in(roots: &[PathBuf], home: &str, owner: &str) -> Option<PathBuf> {
    let dir = owner_leaf(home, owner)?;
    let home_dir = crate::projects::resolve_project(roots, home)?;
    Some(home_dir.join(".claude/worktrees").join(dir))
}

/// The `<dir>` of `home/.claude/worktrees/<dir>`, when `owner` has the one
/// shape a re-home record may name — [`owner_path`] without the disk. Pure,
/// so a caller that has already found the home's directory another way
/// (`claudes::detect_moves_in`) can ask "could a record naming this ever
/// resolve" without a second resolve.
fn owner_leaf<'a>(home: &str, owner: &'a str) -> Option<&'a str> {
    let dir = owner.strip_prefix(home).and_then(|r| r.strip_prefix("/.claude/worktrees/"))?;
    // roost's own `worktree-<name>` and the older `claude-N` both fit the
    // session-name alphabet, except a `worktree-` name longer than 23. No
    // shape admits `/`, `.` or `..`, which is what keeps `dir` one segment.
    let shaped = crate::session::valid_name(dir)
        || dir.strip_prefix("worktree-").is_some_and(crate::wtname::valid);
    if !shaped {
        return None;
    }
    // A home with a dot segment would send `resolve_project` to git. Every
    // home is a main checkout, which has none; refusing one keeps this
    // fork-free by construction rather than by that assumption.
    if home.split('/').any(|s| s.starts_with('.')) {
        return None;
    }
    Some(dir)
}

/// Whether a record naming `owner` for a session of `home` could ever
/// resolve: the shape half of [`owner_path`]. A worktree outside the home's
/// `.claude/worktrees/` cannot, so it is never offered as a move target.
pub fn owner_shaped(home: &str, owner: &str) -> bool {
    owner_leaf(home, owner).is_some()
}

pub fn owner_path(home: &str, owner: &str) -> Option<PathBuf> {
    owner_path_in(&crate::projects::roots(), home, owner)
}

/// The owner's real directory when it `resolves`, rather than just a bool —
/// what `ide.rs`'s `open_diff` needs to re-relativise a confined path
/// against before delivering a proposal to the owner's hub instead of the
/// home's. `resolves_in` is this with the directory discarded; see it for
/// why this is fork-free and for the "cannot tell" direction (display-only,
/// so it leans toward keeping the session at the owner here exactly as
/// `resolves_in` leans toward keeping it shown there).
pub fn owner_dir_in(roots: &[PathBuf], home: &str, owner: &str) -> Option<PathBuf> {
    let wt_dir = owner_path_in(roots, home, owner)?;
    match std::fs::symlink_metadata(&wt_dir) {
        Ok(m) if m.is_dir() => Some(wt_dir),
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        // Cannot tell: keep showing the session where its record says. This
        // is display-only and destroys nothing either way, so it does not
        // need positive evidence — but a worktree we merely could not stat
        // is not one that was removed, and moving the tab back to the home
        // on an EACCES would be the same misreading in a milder form.
        Err(_) => Some(wt_dir),
    }
}

pub fn owner_dir(home: &str, owner: &str) -> Option<PathBuf> {
    owner_dir_in(&crate::projects::roots(), home, owner)
}

/// The project a session actually shows in: `home` itself, unless the
/// record names an owner that still `resolves`. Folds the two destructive-
/// default traps `owner_of` already guards (`Unknown`, an owner that no
/// longer resolves) into one answer every consumer can use without
/// re-deriving the fallback: a session must always be shown *somewhere*,
/// and "somewhere" is always its home when the owner is in any doubt.
pub fn effective_owner(home: &str, name: &str) -> String {
    effective_owner_with(&crate::wsstate::state_dir(), &resolves, home, name)
}

/// [`effective_owner`] with both the state dir and the `resolves` check
/// injected, for tests that must stay fork-free (no project roots) or must
/// not touch the real state dir.
pub fn effective_owner_with(
    state: &Path,
    resolves: &dyn Fn(&str, &str) -> bool,
    home: &str,
    name: &str,
) -> String {
    match owner_of_in(state, home, name) {
        Owner::Elsewhere(o) if resolves(home, &o) => o,
        _ => home.to_string(),
    }
}

/// `sock/<home key>/.moved-to`: where the sweep found `home`'s directory
/// now lives (#158 §5), as the project URL it resolves as. Written only by
/// `registry::reconcile`, only on #178's inode evidence, and only when that
/// URL resolves under the roots.
fn moved_to_path(state: &Path, home: &str) -> PathBuf {
    state.join("sock").join(crate::projects::storage_key(home)).join(".moved-to")
}

/// The URL `home` was renamed to, or `None`. Not three-valued at this
/// boundary on purpose: absent, unreadable, empty and malformed all read
/// `None`, and `None` only ever means "do not follow, do not widen" — the
/// direction that destroys nothing (the sessions stay where #178 left them).
pub fn moved_to_in(state: &Path, home: &str) -> Option<String> {
    let s = std::fs::read_to_string(moved_to_path(state, home)).ok()?;
    let s = s.strip_suffix('\n').unwrap_or(&s);
    // One line naming a URL: anything else is a damaged or foreign file.
    if s.is_empty() || s.bytes().any(|b| b.is_ascii_control()) || s.starts_with('/') {
        return None;
    }
    Some(s.to_string())
}

pub fn moved_to(home: &str) -> Option<String> { moved_to_in(&crate::wsstate::state_dir(), home) }

/// A no-clobber copy: `dst` is written only when it is positively absent,
/// through a pid-unique temp file and a `hard_link` (which, unlike `rename`,
/// refuses an existing target — so a file the new key gained meanwhile is
/// never overwritten). `Ok(false)` when there was nothing to do; any "cannot
/// tell" about `dst` is nothing to do.
fn copy_if_absent(src: &Path, dst: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(dst) {
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Ok(false),
    }
    let bytes = match std::fs::read(src) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("cannot read {}: {e}", src.display())),
    };
    let dir = dst.parent().ok_or("no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let leaf = dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{leaf}.copy.tmp.{}", std::process::id()));
    let out = (|| {
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        match std::fs::hard_link(&tmp, dst) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    })();
    let _ = std::fs::remove_file(&tmp);
    out
}

/// Spec §5 steps a–d: `old_url`'s directory is now `new_url`'s, so its live
/// sessions become residents of the new project, records naming the old
/// URL as owner name the new one, and the new key inherits the old key's
/// `.base` and layout when it has none. `new_live` is `session::live_names
/// (new_url)` — the R16 gate. Idempotent; returns whether anything changed.
///
/// Records and copies only: no hub, no lock, no git. Its caller
/// (`registry::follow_moves`) refreshes hubs afterwards, one at a time.
pub fn follow_move_in(state: &Path, old_url: &str, new_url: &str, live: &[String], new_live: &[String]) -> bool {
    let mut changed = false;
    // (a) Only a session that is `Here` follows. `Elsewhere(other)` was
    // re-homed by its own Claude's move and stays where that put it;
    // `Unknown` is "cannot tell", and cannot-tell writes nothing.
    for n in live {
        if !matches!(owner_of_in(state, old_url, n), Owner::Here) {
            continue;
        }
        // R16: a resident never shadows a native session of the same name.
        if new_live.contains(n) {
            eprintln!("roost: {old_url}/{n} did not follow the rename to {new_url}: {n} already exists there");
            continue;
        }
        match record_in(state, old_url, n, new_url) {
            Ok(()) => changed = true,
            Err(e) => eprintln!("roost: {old_url}/{n} did not follow the rename to {new_url}: {e}"),
        }
    }
    // (b) Residents of the old URL (a main checkout's terminals re-homed
    // into the moved worktree). Re-read right before each write, like the
    // R18 sweep's compare-before-forget: a record re-pointed since the scan
    // names somewhere else and is not this move's to rewrite.
    for (home, name, owner) in all_in(state) {
        if !matches!(&owner, Owner::Elsewhere(o) if o == old_url) {
            continue;
        }
        if !matches!(owner_of_in(state, &home, &name), Owner::Elsewhere(ref o) if o == old_url) {
            continue;
        }
        match record_in(state, &home, &name, new_url) {
            Ok(()) => changed = true,
            Err(e) => eprintln!("roost: {home}/{name}'s owner {old_url} was renamed to {new_url}, but its record was not rewritten: {e}"),
        }
    }
    // (c) and (d): only when the new key has none. A rename keeps every
    // path relative to the project root, so the old layout's file tabs and
    // the old base both still mean the same thing.
    let (ok, nk) = (crate::projects::storage_key(old_url), crate::projects::storage_key(new_url));
    for (src, dst) in [
        (crate::worktree::base_file(state, &ok), crate::worktree::base_file(state, &nk)),
        (state.join(format!("{ok}.json")), state.join(format!("{nk}.json"))),
    ] {
        match copy_if_absent(&src, &dst) {
            Ok(c) => changed |= c,
            Err(e) => eprintln!("roost: not copied after the rename to {new_url}: {e}"),
        }
    }
    changed
}

/// [`follow_move_in`] against the real state dir, with the R16 gate read
/// from the new project's own live sessions. Never call under a lock:
/// `session::live_names` takes `SESSIONS`.
pub fn follow_move(old_url: &str, new_url: &str, live: &[String]) -> bool {
    let new_live = crate::session::live_names(new_url);
    follow_move_in(&crate::wsstate::state_dir(), old_url, new_url, live, &new_live)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `resolves_in` reads the disk with a plain directory standing in for a
    /// worktree: it must not run git, so it cannot tell a real worktree from
    /// a directory, and that is deliberate (it runs under the hub registry
    /// lock).
    #[test]
    fn resolves_only_an_existing_dir_under_the_homes_own_claude_worktrees() {
        let root = tempfile::tempdir().unwrap();
        let wts = root.path().join("proj/.claude/worktrees");
        std::fs::create_dir_all(wts.join("worktree-x")).unwrap();
        std::fs::create_dir_all(wts.join("claude-3")).unwrap();
        std::fs::write(wts.join("worktree-file"), "").unwrap();
        std::fs::create_dir_all(root.path().join("other/.claude/worktrees/worktree-x")).unwrap();
        let roots = vec![root.path().to_path_buf()];

        assert!(resolves_in(&roots, "proj", "proj/.claude/worktrees/worktree-x"));
        assert!(resolves_in(&roots, "proj", "proj/.claude/worktrees/claude-3"), "an existing claude-N worktree keeps resolving");
        assert!(!resolves_in(&roots, "proj", "proj/.claude/worktrees/worktree-file"), "a file is not a worktree");
        assert!(!resolves_in(&roots, "proj", "other/.claude/worktrees/worktree-x"), "an owner under another project's worktrees");
        assert!(!resolves_in(&roots, "proj", "other"), "an owner that is not a worktree at all");
        assert!(!resolves_in(&roots, "proj", "proj"), "the home itself");
        // Each of these names a directory that exists, so only the shape
        // check can refuse them.
        assert!(!resolves_in(&roots, "proj", "proj/.claude/worktrees/.."), "a dot-dot segment");
        assert!(!resolves_in(&roots, "proj", "proj/.claude/worktrees/worktree-x/.."));
        assert!(!resolves_in(&roots, "proj", "proj/.claude/worktrees/../worktrees/worktree-x"));
        assert!(!resolves_in(&roots, "proj", "proj/.claude/worktrees/worktree-x/"), "a trailing slash");
        assert!(!resolves_in(&roots, "nosuch", "nosuch/.claude/worktrees/worktree-x"), "a home that does not resolve");
        std::fs::create_dir_all(wts.join("worktree-x/.claude/worktrees/worktree-y")).unwrap();
        assert!(
            !resolves_in(&roots, "proj/.claude/worktrees/worktree-x", "proj/.claude/worktrees/worktree-x/.claude/worktrees/worktree-y"),
            "a dotted home is refused before it can reach git"
        );

        std::fs::remove_dir_all(wts.join("worktree-x")).unwrap();
        assert!(!resolves_in(&roots, "proj", "proj/.claude/worktrees/worktree-x"), "a removed worktree no longer resolves");
    }

    /// `owner_dir_in` is `resolves_in` with the directory kept rather than
    /// discarded — `ide.rs`'s `open_diff` needs the real path to
    /// re-relativise a confined file against, not just a yes/no.
    ///
    /// Revert-checked: changing the `Ok(m) if m.is_dir()` arm to return
    /// `home_dir` instead of `wt_dir` (still `Some`, so `resolves_in` — and
    /// every one of its own boolean-only tests — would not notice at all)
    /// fails here — observed: `assertion 'left == right' failed\n  left:
    /// ".../proj"\n right: ".../proj/.claude/worktrees/worktree-x"`. This is
    /// the test `resolves_in`'s suite structurally cannot write, since it
    /// only ever asserts true/false.
    #[test]
    fn owner_dir_in_returns_the_real_path_resolves_in_only_reports_as_true() {
        let root = tempfile::tempdir().unwrap();
        let wts = root.path().join("proj/.claude/worktrees");
        std::fs::create_dir_all(wts.join("worktree-x")).unwrap();
        let roots = vec![root.path().to_path_buf()];

        let dir = owner_dir_in(&roots, "proj", "proj/.claude/worktrees/worktree-x")
            .expect("an existing worktree resolves");
        assert_eq!(dir, wts.join("worktree-x"));

        assert!(owner_dir_in(&roots, "proj", "proj/.claude/worktrees/worktree-gone").is_none());
    }

    /// Display-only, so "cannot tell" keeps the session where its record
    /// says — the opposite of the destructive default, on purpose.
    #[test]
    fn an_unreadable_worktrees_dir_resolves_rather_than_guessing_gone() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let wts = root.path().join("proj/.claude/worktrees");
        std::fs::create_dir_all(wts.join("worktree-x")).unwrap();
        std::fs::set_permissions(&wts, std::fs::Permissions::from_mode(0o000)).unwrap();
        let blind = matches!(std::fs::symlink_metadata(wts.join("worktree-x")), Err(ref e) if e.kind() != std::io::ErrorKind::NotFound);
        let got = resolves_in(&[root.path().to_path_buf()], "proj", "proj/.claude/worktrees/worktree-x");
        std::fs::set_permissions(&wts, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !blind {
            eprintln!("skipped: this user can search a 0o000 directory");
            return;
        }
        assert!(got, "an error other than NotFound must not read as a removed worktree");
    }

    #[test]
    fn absent_record_is_here() {
        let st = tempfile::tempdir().unwrap();
        assert!(matches!(owner_of_in(st.path(), "foo", "term3"), Owner::Here));
    }

    #[test]
    fn recorded_owner_round_trips_and_lists_as_resident() {
        let st = tempfile::tempdir().unwrap();
        let wt = "foo/.claude/worktrees/worktree-x";
        record_in(st.path(), "foo", "term3", wt).unwrap();
        assert!(matches!(owner_of_in(st.path(), "foo", "term3"), Owner::Elsewhere(o) if o == wt));
        assert_eq!(residents_of_in(st.path(), wt), vec![("foo".to_string(), "term3".to_string())]);
        assert!(residents_of_in(st.path(), "foo").is_empty(), "the home is not the owner");
    }

    #[test]
    fn empty_or_unreadable_record_is_unknown_not_here() {
        // Reverting the empty-file arm to `Here` must fail this: an empty
        // file is a half-written record, and `Here` would let Close Project
        // on the home kill a session that belongs to a worktree.
        // REVERT CHECK: changing `Owner::Unknown("empty re-home record".into())`
        // to `Owner::Here` causes this test to fail at the first assertion:
        // `assertion failed: matches!(owner_of_in(st.path(), "foo", "term3"), Owner::Unknown(_))`
        let st = tempfile::tempdir().unwrap();
        let d = st.path().join("home").join(crate::projects::storage_key("foo"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("term3"), "").unwrap();
        assert!(matches!(owner_of_in(st.path(), "foo", "term3"), Owner::Unknown(_)));
        std::fs::create_dir(d.join("term4")).unwrap(); // read_to_string on a dir errors
        assert!(matches!(owner_of_in(st.path(), "foo", "term4"), Owner::Unknown(_)));
        let away: Vec<_> = away_from_in(st.path(), "foo").into_iter().map(|(n, _)| n).collect();
        assert_eq!(away, vec!["term3".to_string(), "term4".to_string()]);
    }

    #[test]
    fn sweep_removes_only_on_positive_absence() {
        let st = tempfile::tempdir().unwrap();
        for n in ["gone", "alive", "unsure"] {
            record_in(st.path(), "foo", n, "foo/.claude/worktrees/worktree-x").unwrap();
        }
        sweep_in(st.path(), &|_, n| match n { "gone" => Some(false), "alive" => Some(true), _ => None });
        assert!(matches!(owner_of_in(st.path(), "foo", "gone"), Owner::Here));
        assert!(matches!(owner_of_in(st.path(), "foo", "alive"), Owner::Elsewhere(_)));
        // REVERT CHECK: changing `== Some(false)` to `!= Some(true)` in sweep_in causes
        // this assertion to fail with: `assertion failed: matches!(..., Owner::Elsewhere(_))`
        // on the "unsure" record, because `None` becomes treated as absent.
        assert!(matches!(owner_of_in(st.path(), "foo", "unsure"), Owner::Elsewhere(_)), "cannot tell → keep");
    }

    /// What `claudes::detect_gone_owners` iterates over: every distinct
    /// home that has at least one record, deduplicated even when it has
    /// several — the "ask git once per home, not once per record" cost
    /// bound depends on this not repeating a home.
    ///
    /// Revert check: dropping the `dedup()` (keeping only `sort()`) makes
    /// the assertion fail — observed: `left: ["foo", "foo", "zed"], right:
    /// ["foo", "zed"]`. Restored.
    #[test]
    fn all_homes_in_lists_each_home_with_a_record_once() {
        let st = tempfile::tempdir().unwrap();
        record_in(st.path(), "foo", "term1", "foo/.claude/worktrees/w1").unwrap();
        record_in(st.path(), "foo", "term2", "foo/.claude/worktrees/w2").unwrap();
        record_in(st.path(), "zed", "term1", "zed/.claude/worktrees/w1").unwrap();
        let mut got = all_homes_in(st.path());
        got.sort();
        assert_eq!(got, vec!["foo".to_string(), "zed".to_string()]);
        // A home with no record at all costs zero git calls elsewhere
        // precisely because it is absent here.
        assert!(all_homes_in(&tempfile::tempdir().unwrap().path().to_path_buf()).is_empty());
    }

    /// `owner_path_in` (via `owner_dir_in`) is what makes R18's git-based
    /// comparison possible even once the worktree is gone: it must still
    /// return the path the record names, with no existence check at all.
    #[test]
    fn owner_path_names_the_expected_location_whether_or_not_it_exists() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("proj")).unwrap();
        let roots = vec![root.path().to_path_buf()];
        let got = owner_path_in(&roots, "proj", "proj/.claude/worktrees/worktree-gone").unwrap();
        assert_eq!(got, root.path().join("proj/.claude/worktrees/worktree-gone"));
        assert!(!got.exists(), "the whole point: nothing needs to be there");
        assert!(owner_path_in(&roots, "proj", "other/.claude/worktrees/x").is_none(), "wrong home");
    }

    #[test]
    fn record_refuses_a_bad_session_name() {
        let st = tempfile::tempdir().unwrap();
        let e = record_in(st.path(), "foo", "../x", "foo/w").unwrap_err();
        assert!(e.contains("session name"), "{e}");
    }

    #[test]
    fn owner_gone_falls_back_to_home_and_keeps_record() {
        let st = tempfile::tempdir().unwrap();
        record_in(st.path(), "foo", "term3", "foo/.claude/worktrees/worktree-gone").unwrap();
        sweep_in(st.path(), &|_, _| Some(true)); // socket still there
        assert!(matches!(owner_of_in(st.path(), "foo", "term3"), Owner::Elsewhere(_)),
            "a vanished owner is not evidence the session ended");
    }

    #[test]
    fn an_invalid_name_is_unknown_and_reads_nothing_outside_the_record_dir() {
        let st = tempfile::tempdir().unwrap();
        // A file the traversal would reach: state/home/<key(foo)>/../outside
        std::fs::create_dir_all(st.path().join("home")).unwrap();
        std::fs::write(st.path().join("home").join("outside"), "evil/owner\n").unwrap();
        record_in(st.path(), "foo", "term", "x/w").unwrap(); // make the home dir exist
        // Without the validation guard, this reads outside the record dir and returns
        // Elsewhere("evil/owner"), which fails the assertion.
        assert!(matches!(owner_of_in(st.path(), "foo", "../outside"), Owner::Unknown(_)));
    }

    /// `effective_owner` is the one answer every consumer (removal gate,
    /// Claude evidence, notices, IDE, overview) should use instead of
    /// re-deriving the `Elsewhere`-and-resolves fold themselves: `Here` and
    /// `Unknown` both mean "show it at home", and so does an owner that no
    /// longer resolves.
    ///
    /// Revert-checked: changing the `if resolves(home, &o)` guard to always
    /// take the `Elsewhere` branch (i.e. trusting the record regardless of
    /// whether the owner still resolves) fails at the *second* assertion —
    /// observed: `assertion 'left == right' failed: left: "foo/.claude/worktrees/w",
    /// right: "foo"` — because that's the `no`-resolver case, reached before
    /// the removed-worktree case ever runs.
    #[test]
    fn effective_owner_falls_back_to_home_unless_the_record_resolves() {
        let st = tempfile::tempdir().unwrap();
        let yes = |_: &str, _: &str| true;
        let no = |_: &str, _: &str| false;

        // No record at all: Here.
        assert_eq!(effective_owner_with(st.path(), &yes, "foo", "term1"), "foo");

        // A resolving Elsewhere: the owner.
        record_in(st.path(), "foo", "term2", "foo/.claude/worktrees/w").unwrap();
        assert_eq!(
            effective_owner_with(st.path(), &yes, "foo", "term2"),
            "foo/.claude/worktrees/w"
        );

        // An Elsewhere whose owner no longer resolves: home, record kept.
        assert_eq!(effective_owner_with(st.path(), &no, "foo", "term2"), "foo");

        // Unknown (empty record): home, never the raw record text.
        let d = st.path().join("home").join(crate::projects::storage_key("foo"));
        std::fs::write(d.join("term3"), "").unwrap();
        assert_eq!(effective_owner_with(st.path(), &yes, "foo", "term3"), "foo");
    }

    /// Spec §5 (a)–(d) in one pass, with every branch that must write
    /// nothing given a fixture that would expose it: a native session of the
    /// same name at the new key (R16), a session already re-homed elsewhere,
    /// an `Unknown` record, a resident of some other owner, and a layout the
    /// new key already has.
    ///
    /// Revert-checked, each run and restored: (a)'s `Here` filter removed
    /// (every live name recorded) fails "a session already re-homed
    /// elsewhere stays there" — left `Elsewhere(".../worktree-b")`; (b)'s
    /// owner filter made to skip everything fails "(b) a resident of the old
    /// URL follows" — left `Elsewhere(".../worktree-a")`. `copy_if_absent`'s
    /// present-`dst` early return removed alone still passes (`hard_link`
    /// refuses the existing target too, so no-clobber is held twice); with
    /// that removed *and* `hard_link` swapped for `rename`, fails "the new
    /// key's own layout is never overwritten" — left `"{\"old\":1}"`.
    #[test]
    fn follow_move_rewrites_owner_records_and_copies_state_only_when_absent() {
        let st = tempfile::tempdir().unwrap();
        let s = st.path();
        let old = "repo/.claude/worktrees/worktree-a";
        let new = "repo/.claude/worktrees/worktree-b";
        let other = "repo/.claude/worktrees/other";
        record_in(s, old, "away", other).unwrap();
        let d = s.join("home").join(crate::projects::storage_key(old));
        std::fs::write(d.join("unsure"), "").unwrap();
        record_in(s, "repo", "term3", old).unwrap();
        record_in(s, "repo", "term4", other).unwrap();
        let (ok, nk) = (crate::projects::storage_key(old), crate::projects::storage_key(new));
        crate::worktree::write_base(s, &ok, "main").unwrap();
        std::fs::write(s.join(format!("{ok}.json")), r#"{"old":1}"#).unwrap();
        std::fs::write(s.join(format!("{nk}.json")), r#"{"new":1}"#).unwrap();

        let live: Vec<String> = ["shell", "busy", "away", "unsure"].map(String::from).to_vec();
        assert!(follow_move_in(s, old, new, &live, &["busy".to_string()]));

        assert_eq!(owner_of_in(s, old, "shell"), Owner::Elsewhere(new.into()), "(a) a live session follows");
        assert_eq!(owner_of_in(s, old, "busy"), Owner::Here, "R16: the new key's native busy is not shadowed");
        assert_eq!(owner_of_in(s, old, "away"), Owner::Elsewhere(other.into()), "a session already re-homed elsewhere stays there");
        assert!(matches!(owner_of_in(s, old, "unsure"), Owner::Unknown(_)), "cannot tell writes nothing");
        assert_eq!(owner_of_in(s, "repo", "term3"), Owner::Elsewhere(new.into()), "(b) a resident of the old URL follows");
        assert_eq!(owner_of_in(s, "repo", "term4"), Owner::Elsewhere(other.into()), "(b) another owner's resident is untouched");
        assert_eq!(crate::worktree::read_base(s, &nk).as_deref(), Some("main"), "(c) the base is copied to a key that had none");
        assert_eq!(std::fs::read_to_string(s.join(format!("{nk}.json"))).unwrap(), r#"{"new":1}"#, "the new key's own layout is never overwritten");

        assert!(!follow_move_in(s, old, new, &live, &["busy".to_string()]), "a second pass changes nothing");

        std::fs::remove_file(s.join(format!("{nk}.json"))).unwrap();
        assert!(follow_move_in(s, old, new, &live, &[]));
        assert_eq!(std::fs::read_to_string(s.join(format!("{nk}.json"))).unwrap(), r#"{"old":1}"#, "(d) the layout is copied to a key that had none");
        assert_eq!(owner_of_in(s, old, "busy"), Owner::Elsewhere(new.into()), "once the name is free there, it follows too");
        assert!(std::fs::read_dir(s).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().contains(".tmp.")), "no temp file left behind");
    }

    /// `None` for everything but one line naming a URL. Pins the parse,
    /// which the widenings and the gone-owner sweep all trust: a damaged
    /// file must never name an owner.
    ///
    /// Revert-checked: without the control-byte refusal, fails on
    /// `"proj2\nproj3\n"` — left `Some("proj2\nproj3")`. Restored.
    #[test]
    fn moved_to_reads_only_one_line_naming_a_url() {
        let st = tempfile::tempdir().unwrap();
        let s = st.path();
        let p = moved_to_path(s, "proj");
        assert_eq!(moved_to_in(s, "proj"), None, "absent");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        for (body, want) in [
            ("proj2\n", Some("proj2")),
            ("proj2", Some("proj2")),
            ("", None),
            ("\n", None),
            ("proj2\nproj3\n", None),
            ("/abs/path\n", None),
        ] {
            std::fs::write(&p, body).unwrap();
            assert_eq!(moved_to_in(s, "proj").as_deref(), want, "{body:?}");
        }
    }
}
