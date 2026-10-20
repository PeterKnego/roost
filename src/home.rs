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
    let tmp = dir.join(format!(".{name}.tmp.{}", crate::wsstate::tmp_suffix()));
    std::fs::write(&tmp, format!("{owner}\n")).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(name)).map_err(|e| { let _ = std::fs::remove_file(&tmp); e.to_string() })
}

pub fn forget_in(state: &Path, home: &str, name: &str) {
    if crate::session::valid_name(name) {
        let _ = std::fs::remove_file(dir_in(state, home).join(name));
        let _ = std::fs::remove_file(identity_path(state, home, name));
    }
}

/// `.<name>.ino` beside the record: the owner directory's `(device, inode)`
/// when the re-home was made (#158 §5). A worktree whose only terminals were
/// re-homed into it has no session of its own, so `registry::move_evidence`
/// has no shell to look through when it is renamed; this is what lets the
/// R18 sweep recognise the same directory under its new name, from the
/// resident's own Claude's cwd. A dot-file, so `all_in` and every other
/// enumeration of this directory skip it.
fn identity_path(state: &Path, home: &str, name: &str) -> PathBuf {
    dir_in(state, home).join(format!(".{name}.ino"))
}

/// Writes the owner directory's full four-field identity (`dirid::format`),
/// atomically and 0600 via `dirid::write_lines`. `dirid::of` already refuses
/// to produce an identity when the directory is gone or its birth time is
/// unreadable (`Ok(None)` / `Err`); both cases are errors here too — a
/// record whose `.ino` cannot be trusted is exactly the case that must never
/// license a follow (see the module doc on `identity_path` and #183 §1).
pub fn record_identity_in(state: &Path, home: &str, name: &str, owner_dir: &Path) -> Result<(), String> {
    if !crate::session::valid_name(name) {
        return Err(format!("bad session name {name:?}"));
    }
    let id = crate::dirid::of(owner_dir)?.ok_or_else(|| format!("{} does not exist", owner_dir.display()))?;
    let dir = dir_in(state, home);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    crate::dirid::write_lines(&identity_path(state, home, name), &[crate::dirid::format(&id)])
}

/// The recorded owner identity. Three outcomes: `Ok(None)` — none recorded
/// (a re-home older than this file, or one whose identity could not be read
/// at write time), which is no evidence; `Ok(Some(Full|Legacy))`; and
/// `Err` — unreadable, or damaged (`dirid::parse_line` gave `None`), which is
/// "cannot tell" and must suspend whatever would have acted on its absence.
/// Damaged must never read as `Ok(None)`: the difference between "nothing
/// was ever recorded" and "something was recorded but cannot be trusted" is
/// exactly the distinction CLAUDE.md's "absence of evidence" rule exists for.
pub fn identity_of_in(state: &Path, home: &str, name: &str) -> Result<Option<crate::dirid::Stored>, String> {
    if !crate::session::valid_name(name) {
        return Err(format!("bad session name {name:?}"));
    }
    match std::fs::read_to_string(identity_path(state, home, name)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
        Ok(s) => crate::dirid::parse_line(s.trim()).map(Some).ok_or_else(|| format!("damaged identity record {s:?}")),
    }
}

/// Upgrades a Legacy `.ino` to Full once `owner_dir`'s current `(dev, ino)`
/// still matches the legacy pair: the record was never wrong, only written
/// before birth time existed, so this is the one case where adding it is
/// safe. Every other case — no file, already Full, a legacy pair that no
/// longer matches — writes nothing and returns `false`; a stat error on
/// `owner_dir` (via `dirid::of`) is `Err`, same as everywhere else evidence
/// about a directory is read. `dirid::matches` is deliberately not used
/// here: that function answers "does this stored value name this directory
/// at all," which for a `Legacy` value is true for *any* birth time, whereas
/// upgrading requires the stronger "the (dev, ino) pair itself still holds
/// right now" — checked directly against `owner_dir`'s current identity.
pub fn upgrade_identity_in(state: &Path, home: &str, name: &str, owner_dir: &Path) -> Result<bool, String> {
    if !crate::session::valid_name(name) {
        return Err(format!("bad session name {name:?}"));
    }
    let Some(crate::dirid::Stored::Legacy { dev, ino }) = identity_of_in(state, home, name)? else {
        return Ok(false);
    };
    let Some(id) = crate::dirid::of(owner_dir)? else {
        return Ok(false);
    };
    if id.dev != dev || id.ino != ino {
        return Ok(false);
    }
    crate::dirid::write_lines(&identity_path(state, home, name), &[crate::dirid::format(&id)])?;
    Ok(true)
}

pub fn forget_identity_in(state: &Path, home: &str, name: &str) {
    if crate::session::valid_name(name) {
        let _ = std::fs::remove_file(identity_path(state, home, name));
    }
}
pub fn forget_identity(home: &str, name: &str) { forget_identity_in(&crate::wsstate::state_dir(), home, name) }
pub fn record_identity(home: &str, name: &str, owner_dir: &Path) -> Result<(), String> {
    record_identity_in(&crate::wsstate::state_dir(), home, name, owner_dir)
}
pub fn identity_of(home: &str, name: &str) -> Result<Option<crate::dirid::Stored>, String> {
    identity_of_in(&crate::wsstate::state_dir(), home, name)
}
pub fn upgrade_identity(home: &str, name: &str, owner_dir: &Path) -> Result<bool, String> {
    upgrade_identity_in(&crate::wsstate::state_dir(), home, name, owner_dir)
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
/// one of `home`'s own `.claude/worktrees/<dir>`, still a directory — or the
/// one owner `home`'s `.moved-to` names (see [`resolves_with`]).
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
    resolves_with(&crate::wsstate::state_dir(), roots, home, owner)
}

/// [`resolves_in`] with the state dir injected. The one widening (#158 §5):
/// an owner equal to `home`'s `.moved-to`, while that owner still resolves.
/// Nothing else widens — a record naming any other owner outside the home's
/// `.claude/worktrees/` still does not resolve.
///
/// Still fork-free: `resolve_recorded_project`, not `resolve_project`, since
/// the owner of a renamed worktree has a dot segment and `resolve_project`
/// would ask git about it under the hub registry lock. The `.moved-to` is
/// the evidence git vouched; it was written only once `resolve_project`
/// had, and is rewritten or removed by every `reconcile`.
pub fn resolves_with(state: &Path, roots: &[PathBuf], home: &str, owner: &str) -> bool {
    owner_dir_with(state, roots, home, owner).is_some()
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
    owner_dir_with(&crate::wsstate::state_dir(), roots, home, owner)
}

/// [`owner_dir_in`] with the state dir injected, and the one place the
/// rename widenings (#158 §5, #183 §3) live — so every caller, `resolves`
/// and `ide.rs`'s delivery alike, sees a followed session's owner the same
/// way. The `.claude/worktrees` shape is tried first; failing that, an
/// owner equal to `home`'s `.moved-to` is its directory when that still
/// resolves, through the fork-free `resolve_recorded_project` (see
/// [`resolves_with`]); and failing that, an owner whose directory carries
/// one of `home`'s key identities in full. That last is where a session
/// that followed its own shell went when it is not the key's one target:
/// an occupant renamed to D while B, the `.moved-to`, still stands. Without
/// it, D's sessions were followed on paper and shown nowhere — the old URL
/// no longer resolves, and D would not list them.
pub fn owner_dir_with(state: &Path, roots: &[PathBuf], home: &str, owner: &str) -> Option<PathBuf> {
    shaped_owner_dir(roots, home, owner)
        .or_else(|| {
            (moved_to_in(state, home).as_deref() == Some(owner))
                .then(|| crate::projects::resolve_recorded_project(roots, owner))
                .flatten()
        })
        .or_else(|| {
            let key_dir = state.join("sock").join(crate::projects::storage_key(home));
            crate::projects::resolve_recorded_project(roots, owner).filter(|d| crate::registry::carries_identity(&key_dir, d))
        })
}

fn shaped_owner_dir(roots: &[PathBuf], home: &str, owner: &str) -> Option<PathBuf> {
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
/// boundary: absent, unreadable, empty and malformed all read `None`, which
/// means "do not follow, do not widen". That is the destroy-nothing
/// direction for the widenings (the sessions stay where #178 left them), but
/// not everywhere — so the R18 sweep does not read this file through here:
/// `registry::find_by_identity` reads it three-valued, trusts its target
/// only when that directory carries the record's own identity, and treats a
/// "cannot tell" as a reason to keep the record, not to forget it.
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
    let tmp = dir.join(format!(".{leaf}.copy.tmp.{}", crate::wsstate::tmp_suffix()));
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

/// One of a renamed key's own sessions, and the directory its shell is
/// working in: an ancestor of its session leader's cwd carrying one of the
/// key's full identities (`registry::identities`), confirmed by the resolver
/// to be project `url` — and not the key's own current directory. The
/// evidence #183 §3 lets a native session follow on: about *that* terminal,
/// never about the key as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellAt {
    pub name: String,
    pub url: String,
    pub dir: PathBuf,
}

/// What part (a) of [`follow_move_in`] would do for one of the key's own
/// sessions.
#[derive(Debug, PartialEq, Eq)]
enum OwnFollow {
    /// Re-homed by its own Claude's move, or a record that cannot be read:
    /// not this rename's to touch.
    NotOurs,
    /// Already owned by the directory its shell is in.
    There,
    /// No evidence about this terminal: its shell is outside every one of
    /// the key's directories. It stays where it is.
    Stays,
    /// Follows `url`, whose directory is `dir`.
    To(String, PathBuf),
}

/// Part (a)'s decision for session `n` of `old_url` (#183 §3). A session
/// that is `Here`, or that an earlier follow sent to `prev`, follows the
/// directory its own shell is in (`shells`). Failing that, a session sent
/// to `prev` follows `new_dir` when its record's own full identity is the
/// one found there — #182's A→B, `cd` out, B→C, where the shell gives no
/// evidence but the record still does. Any other `Elsewhere` was re-homed
/// by its own Claude's move and stays where that put it.
///
/// Never "the key moved, so all of its sessions did": a key whose old name
/// has held an occupant has sessions working in two different directories,
/// and only each one's own shell says which.
fn own_follow(state: &Path, old_url: &str, n: &str, new_url: &str, new_dir: Option<&Path>, shells: &[ShellAt], prev: Option<&str>) -> OwnFollow {
    let owner = owner_of_in(state, old_url, n);
    let shell = shells.iter().find(|s| s.name == n);
    if let (Owner::Elsewhere(o), Some(s)) = (&owner, shell) {
        if *o == s.url {
            return OwnFollow::There;
        }
    }
    let from_prev = match &owner {
        Owner::Here => false,
        Owner::Elsewhere(o) if prev == Some(o.as_str()) => true,
        _ => return OwnFollow::NotOurs,
    };
    if let Some(s) = shell {
        return OwnFollow::To(s.url.clone(), s.dir.clone());
    }
    if from_prev {
        if let Some(dir) = new_dir {
            let here = crate::dirid::of(dir).ok().flatten();
            if matches!((identity_of_in(state, old_url, n), here), (Ok(Some(crate::dirid::Stored::Full(id))), Some(h)) if id == h) {
                return OwnFollow::To(new_url.to_string(), dir.to_path_buf());
            }
        }
    }
    log_stays_once(old_url, n);
    OwnFollow::Stays
}

/// Once per session for the life of the process: a stranded session is
/// re-examined every pass, and its reason does not change.
fn log_stays_once(old_url: &str, n: &str) {
    static SAID: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    let first = SAID.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).insert(format!("{old_url}/{n}"));
    if first {
        eprintln!("roost: {old_url}/{n} did not follow {old_url}'s rename: its shell is outside every directory the key has had, so nothing says where it belongs");
    }
}

/// Spec §5 steps a–d: `old_url`'s directory is now `new_url`'s, so its live
/// sessions become residents of the project their own shell is in, records
/// naming the old URL as owner name the new one, and the new key inherits
/// the old key's `.base` and layout when it has none. `live_in(url)` is
/// `session::live_names(url)` — the R16 gate, asked of each session's own
/// target. Idempotent; returns whether anything changed.
///
/// `prev` is the target an earlier follow re-pointed things at, when that
/// target has since been renamed on (A→B→C, `prev` = B): what that follow
/// wrote is this follow's to move on, in both (a) and (b).
///
/// `new_dir` is `new_url`'s directory, as its caller resolved it: (b)
/// re-points a record only when that record's own identity is the one
/// found there (#183 §1), and `None` re-points none.
///
/// `shells` is (a)'s evidence, one entry per session whose shell is in one
/// of the key's directories (#183 §3): each session follows its own, which
/// need not be `new_url` — an occupant renamed to D while B still stands
/// takes its sessions to D, and the earlier-followed ones stay with B.
///
/// Records and copies only: no hub, no lock, no git. Its caller
/// (`registry::follow_moves`) resolves `shells` and refreshes hubs.
#[allow(clippy::too_many_arguments)]
pub fn follow_move_in(
    state: &Path,
    old_url: &str,
    new_url: &str,
    new_dir: Option<&Path>,
    live: &[String],
    shells: &[ShellAt],
    live_in: &dyn Fn(&str) -> Vec<String>,
    prev: Option<&str>,
) -> bool {
    let mut changed = false;
    let prev = prev.filter(|p| *p != new_url);
    let followed_before = |o: &str| prev == Some(o);
    // (a) The key's own sessions, each on its own shell's evidence. Every
    // writer of a record writes its target's identity, so a later rename of
    // that target is found by the record itself (R18, or the fallback in
    // `own_follow`) once the shell has wandered off.
    for n in live {
        let OwnFollow::To(url, dir) = own_follow(state, old_url, n, new_url, new_dir, shells, prev) else { continue };
        // R16: a resident never shadows a native session of the same name.
        if live_in(&url).contains(n) {
            eprintln!("roost: {old_url}/{n} did not follow the rename to {url}: {n} already exists there");
            continue;
        }
        match record_in(state, old_url, n, &url) {
            Ok(()) => {
                changed = true;
                if let Err(e) = record_identity_in(state, old_url, n, &dir) {
                    forget_identity_in(state, old_url, n);
                    eprintln!("roost: {old_url}/{n} followed the rename to {url}, but its identity was not recorded: {e}");
                }
            }
            Err(e) => eprintln!("roost: {old_url}/{n} did not follow the rename to {url}: {e}"),
        }
    }
    // (b) Residents of the old URL (a main checkout's terminals re-homed
    // into the moved worktree). Re-read right before each write, like the
    // R18 sweep's compare-before-forget: a record re-pointed since the scan
    // names somewhere else and is not this move's to rewrite.
    //
    // And each one only on evidence about *its* directory (#183 §1): its
    // own `.ino`, as a full identity, equal to the new directory's. That the
    // old key's directory moved here says nothing about a record written
    // while an occupant held the old name (chain a) — that one's directory
    // is wherever its identity is, which R18 looks for record by record.
    let ours = |o: &Owner| matches!(o, Owner::Elsewhere(o) if o == old_url || followed_before(o));
    let new_id = new_dir.and_then(|d| crate::dirid::of(d).ok().flatten());
    for (home, name, owner) in all_in(state) {
        if !ours(&owner) {
            continue;
        }
        // The old key's own sessions are (a)'s, with its R16 gate.
        if home == old_url {
            continue;
        }
        // Only to an owner a record of this home may name at all: anything
        // else could never resolve, so it is left for R18 to forget rather
        // than rewritten into a record that points nowhere.
        if !owner_shaped(&home, new_url) {
            eprintln!("roost: {home}/{name}'s owner {old_url} was renamed to {new_url}, outside {home}/.claude/worktrees; not following");
            continue;
        }
        let (Some(new_id), Some(new_dir)) = (new_id, new_dir) else { continue };
        if !matches!(identity_of_in(state, &home, &name), Ok(Some(crate::dirid::Stored::Full(id))) if id == new_id) {
            continue;
        }
        if !ours(&owner_of_in(state, &home, &name)) {
            continue;
        }
        match record_in(state, &home, &name, new_url) {
            Ok(()) => {
                changed = true;
                // Every writer of a record writes its target's identity; one
                // that cannot be read leaves none rather than a stale one.
                if let Err(e) = record_identity_in(state, &home, &name, new_dir) {
                    forget_identity_in(state, &home, &name);
                    eprintln!("roost: {home}/{name} followed {old_url} to {new_url}, but its identity was not recorded: {e}");
                }
            }
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
/// from each target's own live sessions. Never call under a lock:
/// `session::live_names` takes `SESSIONS`.
pub fn follow_move(old_url: &str, new_url: &str, new_dir: Option<&Path>, live: &[String], shells: &[ShellAt], prev: Option<&str>) -> bool {
    follow_move_in(&crate::wsstate::state_dir(), old_url, new_url, new_dir, live, shells, &crate::session::live_names, prev)
}

/// Whether part (a) of a non-fresh follow has anything left to do: one of
/// the key's own sessions whose evidence names somewhere it is not yet
/// owned by, and that R16 would not hold back (that one would only log its
/// refusal every pass). Never call under a lock, as [`follow_move`].
pub fn own_follow_pending(old_url: &str, new_url: &str, new_dir: Option<&Path>, live: &[String], shells: &[ShellAt], prev: Option<&str>) -> bool {
    let state = crate::wsstate::state_dir();
    let prev = prev.filter(|p| *p != new_url);
    live.iter().any(|n| match own_follow(&state, old_url, n, new_url, new_dir, shells, prev) {
        OwnFollow::To(url, _) => !crate::session::live_names(&url).contains(n),
        _ => false,
    })
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

    /// What `ide.rs`'s `delivery_target` and `hub::rehome_to` ask: a
    /// followed session's owner directory is the new project's, so a
    /// proposal from its Claude reaches the new project's hub rather than
    /// falling back to the old key's, which never exists. The home here is
    /// dotted — a renamed worktree — which the shape rule refuses outright.
    ///
    /// Revert-checked, run and restored: with `owner_dir_with`'s `.moved-to`
    /// arm removed, fails "a followed session's owner is the new project's
    /// directory" — left `None`, which `delivery_target` answers with the
    /// old key.
    #[test]
    fn a_followed_sessions_owner_dir_is_the_new_projects_directory() {
        let st = tempfile::tempdir().unwrap();
        let s = st.path();
        let root = tempfile::tempdir().unwrap();
        let roots = vec![root.path().to_path_buf()];
        let old = "repo/.claude/worktrees/worktree-a";
        let new = "repo/.claude/worktrees/worktree-b";
        std::fs::create_dir_all(root.path().join(new)).unwrap();
        std::fs::create_dir_all(root.path().join("unrelated")).unwrap();
        assert_eq!(owner_dir_with(s, &roots, old, new), None, "setup: no .moved-to, no owner directory");

        let p = moved_to_path(s, old);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, format!("{new}\n")).unwrap();
        assert_eq!(
            owner_dir_with(s, &roots, old, new),
            Some(root.path().join(new).canonicalize().unwrap()),
            "a followed session's owner is the new project's directory"
        );
        assert_eq!(owner_dir_with(s, &roots, old, "unrelated"), None, "and only that owner's");
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
    /// key's own layout is never overwritten" — left `"{\"old\":1}"`; the
    /// `owner_shaped` gate in (b) removed fails "(b) only to an owner the
    /// record's home may name" — left `Elsewhere(".../worktree-b")`.
    ///
    /// #183, revert-checked, each run and restored: (b)'s identity check
    /// removed fails "(b) not a record whose directory is another one" —
    /// left `Elsewhere(".../worktree-b")`; the `record_identity_in` after
    /// (b)'s rewrite removed fails "(b) and that .ino was written by the
    /// follow, not left over" (the content alone cannot tell: a rename keeps
    /// the identity, so only the file's own inode shows a write).
    ///
    /// #183 §3 (part (a) on each session's own shell), revert-checked, each
    /// run and restored: `own_follow` taking `new_url` whenever a shell was
    /// found fails "(a) a session follows its own shell, not the key's
    /// move" — left `Elsewhere(".../worktree-b")`; its `Stays` arm answering
    /// `new_url` fails "(a) a shell outside every directory is no evidence:
    /// it stays" — left the same; (a)'s R16 check removed fails "R16: the
    /// new key's native busy is not shadowed"; (a)'s `record_identity_in`
    /// removed fails "(a) a followed session's record carries its
    /// directory's identity" — left `Ok(None)`.
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
        // `new`'s directory, and a different one: (b) follows a record only
        // when its own identity is the one found at `new_dir` (#183 §1).
        let dirs = tempfile::tempdir().unwrap();
        let new_dir = dirs.path().join("b");
        let elsewhere = dirs.path().join("d");
        std::fs::create_dir_all(&new_dir).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        record_in(s, "repo", "term3", old).unwrap();
        record_identity_in(s, "repo", "term3", &new_dir).unwrap();
        record_in(s, "repo", "term4", other).unwrap();
        record_identity_in(s, "repo", "term4", &new_dir).unwrap();
        // A record no home may name `new` for: left for R18, never rewritten.
        record_in(s, "zed", "term5", old).unwrap();
        record_identity_in(s, "zed", "term5", &new_dir).unwrap();
        // Owned by the old URL, but its directory is another one (an
        // occupant's resident), or unknown: neither is this move's.
        record_in(s, "repo", "term6", old).unwrap();
        record_identity_in(s, "repo", "term6", &elsewhere).unwrap();
        record_in(s, "repo", "term7", old).unwrap();
        let ino_file = |n: &str| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(dir_in(s, "repo").join(format!(".{n}.ino"))).unwrap().ino()
        };
        let term3_ino_before = ino_file("term3");
        let (ok, nk) = (crate::projects::storage_key(old), crate::projects::storage_key(new));
        crate::worktree::write_base(s, &ok, "main").unwrap();
        std::fs::write(s.join(format!("{ok}.json")), r#"{"old":1}"#).unwrap();
        std::fs::write(s.join(format!("{nk}.json")), r#"{"new":1}"#).unwrap();

        // (a)'s evidence: each session's own shell. `todee`'s is in another
        // of the key's directories (an occupant renamed to D), `left`'s is
        // in none; `away`'s is in B, but its own Claude re-homed it.
        let d_url = "repo/.claude/worktrees/worktree-d";
        let at = |n: &str, url: &str, dir: &Path| ShellAt { name: n.into(), url: url.into(), dir: dir.to_path_buf() };
        let shells = vec![at("shell", new, &new_dir), at("busy", new, &new_dir), at("away", new, &new_dir), at("todee", d_url, &elsewhere)];
        let busy_in_new = |u: &str| if u == new { vec!["busy".to_string()] } else { vec![] };
        let live: Vec<String> = ["shell", "busy", "away", "unsure", "todee", "left"].map(String::from).to_vec();
        assert!(follow_move_in(s, old, new, Some(&new_dir), &live, &shells, &busy_in_new, None));

        assert_eq!(owner_of_in(s, old, "shell"), Owner::Elsewhere(new.into()), "(a) a live session follows");
        assert_eq!(owner_of_in(s, old, "busy"), Owner::Here, "R16: the new key's native busy is not shadowed");
        assert_eq!(owner_of_in(s, old, "away"), Owner::Elsewhere(other.into()), "a session already re-homed elsewhere stays there");
        assert!(matches!(owner_of_in(s, old, "unsure"), Owner::Unknown(_)), "cannot tell writes nothing");
        assert_eq!(owner_of_in(s, old, "todee"), Owner::Elsewhere(d_url.into()), "(a) a session follows its own shell, not the key's move");
        assert_eq!(owner_of_in(s, old, "left"), Owner::Here, "(a) a shell outside every directory is no evidence: it stays");
        let id_of = |d: &Path| Ok(Some(crate::dirid::Stored::Full(crate::dirid::of(d).unwrap().unwrap())));
        assert_eq!(identity_of_in(s, old, "shell"), id_of(&new_dir), "(a) a followed session's record carries its directory's identity");
        assert_eq!(identity_of_in(s, old, "todee"), id_of(&elsewhere), "(a) its own directory's, not the key's new one");
        assert_eq!(owner_of_in(s, "repo", "term3"), Owner::Elsewhere(new.into()), "(b) a resident of the old URL follows");
        assert_eq!(owner_of_in(s, "repo", "term4"), Owner::Elsewhere(other.into()), "(b) another owner's resident is untouched");
        assert_eq!(owner_of_in(s, "zed", "term5"), Owner::Elsewhere(old.into()), "(b) only to an owner the record's home may name");
        assert_eq!(owner_of_in(s, "repo", "term6"), Owner::Elsewhere(old.into()), "(b) not a record whose directory is another one");
        assert_eq!(owner_of_in(s, "repo", "term7"), Owner::Elsewhere(old.into()), "(b) not a record with no identity");
        assert_eq!(
            identity_of_in(s, "repo", "term3"),
            Ok(Some(crate::dirid::Stored::Full(crate::dirid::of(&new_dir).unwrap().unwrap()))),
            "(b) the followed record carries the new directory's identity"
        );
        assert_ne!(ino_file("term3"), term3_ino_before, "(b) and that .ino was written by the follow, not left over");
        assert_eq!(crate::worktree::read_base(s, &nk).as_deref(), Some("main"), "(c) the base is copied to a key that had none");
        assert_eq!(std::fs::read_to_string(s.join(format!("{nk}.json"))).unwrap(), r#"{"new":1}"#, "the new key's own layout is never overwritten");

        assert!(!follow_move_in(s, old, new, Some(&new_dir), &live, &shells, &busy_in_new, None), "a second pass changes nothing");

        std::fs::remove_file(s.join(format!("{nk}.json"))).unwrap();
        assert!(follow_move_in(s, old, new, Some(&new_dir), &live, &shells, &|_| vec![], None));
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

    /// The owner identity beside a record: three outcomes on read, never
    /// listed as a record itself, and gone with the record it belongs to —
    /// a stale one would let a later owner's rename be "recognised" by a
    /// directory the terminal never lived in.
    ///
    /// Revert-checked, run and restored: without `forget_in`'s identity
    /// removal, fails "forgetting the record forgets its identity" — left
    /// `Ok(Some(..))`.
    #[test]
    fn an_identity_lives_and_dies_with_its_record() {
        let st = tempfile::tempdir().unwrap();
        let s = st.path();
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(identity_of_in(s, "foo", "term3"), Ok(None), "none recorded is no evidence");
        record_in(s, "foo", "term3", "foo/.claude/worktrees/w").unwrap();
        record_identity_in(s, "foo", "term3", dir.path()).unwrap();
        assert!(matches!(identity_of_in(s, "foo", "term3"), Ok(Some(_))));
        assert_eq!(all_in(s).len(), 1, "the identity file is not a record");
        std::fs::write(identity_path(s, "foo", "term3"), "garbage").unwrap();
        assert!(identity_of_in(s, "foo", "term3").is_err(), "damaged is cannot-tell, not none");
        record_identity_in(s, "foo", "term3", dir.path()).unwrap();
        forget_in(s, "foo", "term3");
        assert_eq!(identity_of_in(s, "foo", "term3"), Ok(None), "forgetting the record forgets its identity");
    }

    /// `record_identity_in` writes `dirid::format`'s four whitespace-
    /// separated fields, through `dirid::write_lines` (atomic, 0600) — not
    /// the old two-field `dev ino` line.
    ///
    /// Revert-checked, run and restored: reverting the write to the old
    /// `format!("{} {}", m.dev(), m.ino())` line (bypassing `dirid`
    /// entirely) fails the first assertion — observed: `assertion `left ==
    /// right` failed: the written line is dirid::format's\n  left:
    /// "64512 9730054\n"\n right: "64512 9730054 1792502522
    /// 575264209\n"` (values from this run).
    #[test]
    fn a_record_identity_is_four_fields_and_0600() {
        let st = tempfile::tempdir().unwrap();
        let s = st.path();
        let owner_dir = tempfile::tempdir().unwrap();
        record_in(s, "foo", "term3", "foo/.claude/worktrees/w").unwrap();
        record_identity_in(s, "foo", "term3", owner_dir.path()).unwrap();

        let id = crate::dirid::of(owner_dir.path()).unwrap().unwrap();
        let path = identity_path(s, "foo", "term3");
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body, format!("{}\n", crate::dirid::format(&id)), "the written line is dirid::format's");
        assert_eq!(body.split_whitespace().count(), 4, "a full identity is four fields");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "dirid::write_lines' 0600, same as every other identity file");
        }
    }

    /// A pre-#183 two-field `.ino` reads as `Stored::Legacy`, not rejected
    /// and not silently treated as a full match.
    #[test]
    fn a_legacy_ino_reads_as_legacy() {
        let st = tempfile::tempdir().unwrap();
        let s = st.path();
        record_in(s, "foo", "term3", "foo/.claude/worktrees/w").unwrap();
        std::fs::create_dir_all(dir_in(s, "foo")).unwrap();
        std::fs::write(identity_path(s, "foo", "term3"), "64512 123\n").unwrap();
        assert_eq!(identity_of_in(s, "foo", "term3"), Ok(Some(crate::dirid::Stored::Legacy { dev: 64512, ino: 123 })));
    }

    /// Review Focus 3: a Legacy `.ino` whose `(dev, ino)` still matches its
    /// owner directory's identity right now is upgraded to Full in place —
    /// the record was never wrong, it just predates birth time.
    ///
    /// Revert-checked, run and restored: see
    /// `a_legacy_ino_is_not_upgraded_onto_a_different_directory`'s comment
    /// for the shared revert (ignoring the `(dev, ino)` comparison) — that
    /// check is run against *that* test since this one would pass either
    /// way (a real match upgrades under both the fixed and broken code).
    #[test]
    fn a_legacy_ino_is_upgraded_when_its_owner_matches() {
        let st = tempfile::tempdir().unwrap();
        let s = st.path();
        let owner_dir = tempfile::tempdir().unwrap();
        let id = crate::dirid::of(owner_dir.path()).unwrap().unwrap();
        record_in(s, "foo", "term3", "foo/.claude/worktrees/w").unwrap();
        std::fs::create_dir_all(dir_in(s, "foo")).unwrap();
        std::fs::write(identity_path(s, "foo", "term3"), format!("{} {}\n", id.dev, id.ino)).unwrap();

        assert_eq!(upgrade_identity_in(s, "foo", "term3", owner_dir.path()), Ok(true));
        assert_eq!(identity_of_in(s, "foo", "term3"), Ok(Some(crate::dirid::Stored::Full(id))), "upgraded in place to Full");
    }

    /// A Legacy pair that does not match `owner_dir`'s current `(dev, ino)`
    /// must not be upgraded: that would stamp a birth time onto evidence
    /// that was never confirmed to be this directory in the first place.
    ///
    /// Revert-checked, run and restored: changing `upgrade_identity_in`'s
    /// `if id.dev != dev || id.ino != ino { return Ok(false); }` guard to
    /// always continue (upgrading on any Legacy record regardless of match)
    /// fails here — observed: `assertion `left == right` failed\n  left:
    /// Ok(true)\n  right: Ok(false)` — and the stored record would have
    /// become `Full` with a birth time belonging to `owner_dir`, not the
    /// directory the Legacy pair actually named. Restored.
    #[test]
    fn a_legacy_ino_is_not_upgraded_onto_a_different_directory() {
        let st = tempfile::tempdir().unwrap();
        let s = st.path();
        let owner_dir = tempfile::tempdir().unwrap();
        let other_dir = tempfile::tempdir().unwrap();
        let other_id = crate::dirid::of(other_dir.path()).unwrap().unwrap();
        record_in(s, "foo", "term3", "foo/.claude/worktrees/w").unwrap();
        std::fs::create_dir_all(dir_in(s, "foo")).unwrap();
        // The stored Legacy pair names `other_dir`'s identity, not
        // `owner_dir`'s — two distinct existing directories can never share
        // a (dev, ino), so this is a genuine mismatch, not a coincidence.
        std::fs::write(identity_path(s, "foo", "term3"), format!("{} {}\n", other_id.dev, other_id.ino)).unwrap();

        assert_eq!(upgrade_identity_in(s, "foo", "term3", owner_dir.path()), Ok(false));
        assert_eq!(
            identity_of_in(s, "foo", "term3"),
            Ok(Some(crate::dirid::Stored::Legacy { dev: other_id.dev, ino: other_id.ino })),
            "left untouched, not upgraded onto the wrong directory"
        );
    }

    /// A damaged `.ino` (`dirid::parse_line` returns `None`) must be `Err`,
    /// never `Ok(None)` — the difference between "no identity was ever
    /// recorded" and "something was recorded but cannot be trusted" is
    /// exactly what licenses a follow versus what must suspend one.
    ///
    /// Revert-checked, run and restored: changing the `Ok(s) => ...`
    /// arm to `Ok(s) => Ok(crate::dirid::parse_line(s.trim()))` (folding a
    /// damaged line into `Ok(None)` instead of `Err`) fails here — observed
    /// (a plain `assert!`, so only the message prints): `panicked at
    /// src/home.rs:...: damaged must not read as no evidence`. Restored.
    #[test]
    fn a_damaged_ino_is_err_not_none() {
        let st = tempfile::tempdir().unwrap();
        let s = st.path();
        record_in(s, "foo", "term3", "foo/.claude/worktrees/w").unwrap();
        std::fs::create_dir_all(dir_in(s, "foo")).unwrap();
        std::fs::write(identity_path(s, "foo", "term3"), "garbage\n").unwrap();
        assert!(identity_of_in(s, "foo", "term3").is_err(), "damaged must not read as no evidence");
    }

    /// #158 §5's widening is gated: an owner outside `.claude/worktrees`
    /// resolves only when `sock/<home key>/.moved-to` names exactly that
    /// owner, and that owner is still a directory under the roots.
    ///
    /// Revert-checked, run and restored (re-run once the arm moved into
    /// `owner_dir_with`): accepting any owner once a `.moved-to` exists
    /// (`is_some()` for the equality) fails "a .moved-to
    /// vouches for its one owner, not any"; dropping the resolve check fails
    /// "a named owner that no longer resolves does not"; `resolve_project`
    /// in place of `resolve_recorded_project` fails "the record is git's
    /// vouching".
    #[test]
    fn a_record_naming_an_unrelated_owner_still_does_not_resolve() {
        let st = tempfile::tempdir().unwrap();
        let s = st.path();
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("root");
        for d in ["proj", "renamed", "unrelated"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::create_dir_all(outer.path().join("escape")).unwrap();
        let roots = vec![root.clone()];

        assert!(!resolves_with(s, &roots, "proj", "renamed"), "no .moved-to: an arbitrary owner does not resolve");
        assert!(!resolves_with(s, &roots, "proj", "unrelated"));

        let p = moved_to_path(s, "proj");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "renamed\n").unwrap();
        assert!(resolves_with(s, &roots, "proj", "renamed"), "the owner .moved-to names resolves");
        assert!(!resolves_with(s, &roots, "proj", "unrelated"), "a .moved-to vouches for its one owner, not any");
        assert!(!resolves_with(s, &roots, "other", "renamed"), "nor for another home's records");

        std::fs::remove_dir_all(root.join("renamed")).unwrap();
        assert!(!resolves_with(s, &roots, "proj", "renamed"), "a named owner that no longer resolves does not");

        // A dotted owner goes through the fork-free resolver: this is a
        // plain directory git never heard of, so `resolve_project` (which
        // asks git about any dot segment) would refuse it — and run git
        // under the hub registry lock to do so.
        std::fs::create_dir_all(root.join("proj2/.claude/worktrees/worktree-b")).unwrap();
        std::fs::write(&p, "proj2/.claude/worktrees/worktree-b\n").unwrap();
        assert!(resolves_with(s, &roots, "proj", "proj2/.claude/worktrees/worktree-b"), "the record is git's vouching");
        // `escape` exists, beside the root rather than under it, so only
        // confinement can refuse it.
        std::fs::write(&p, "../escape\n").unwrap();
        assert!(!resolves_with(s, &roots, "proj", "../escape"), "confinement still applies");
    }
}
