//! The worktrees that appeared, kept until someone deals with them (#188 §3).
//!
//! `Event::WorktreeAppeared` is one-shot: a browser that was not connected
//! when the hub's worktree confirm found a new worktree never hears of it,
//! and neither does any page after a restart. That is exactly the case the
//! banner exists for — a Claude ran `git worktree add` while nobody was
//! looking, quite possibly outside every root, where nothing else in roost
//! will ever show it. So the confirm step also appends `{path, branch}`
//! here, the hub's `State` snapshot carries the list, and an entry leaves
//! only on positive evidence that it has been seen: Dismiss, the worktree's
//! own page being opened, or the worktree dropping out of a `git worktree
//! list` that actually answered.
//!
//! Why roost's state and not the repository: like `outside.rs`'s records,
//! this is a fact about what *this roost* has shown its user, and a file a
//! clone could bring with it would let a repository plant banners — with
//! links — in whichever roost opens it.
//!
//! Every read doubt resolves to "nothing to show", never to a panic: a
//! damaged file is an empty list, logged once per file so a broken file
//! does not repeat on every snapshot. That direction is safe here and only
//! here — this list gates no destruction, so losing a banner is the whole
//! cost. The one emptiness this module refuses to act on is an empty
//! listing in `retain_listed_in`: that is what git answers when it could not
//! be asked, never evidence that every worktree is gone.
//!
//! Writes are atomic (a `tmp_suffix` temp file, `create_new`, 0600, then
//! `rename`) and serialised by `WRITE_LOCK`, a leaf lock held only across
//! one small read-modify-write of roost's own state — the same cost the hub
//! already accepts under its lock for `home::record`. Nothing is ever
//! acquired while holding it.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// At most this many entries per project. Over it the oldest is dropped,
/// and the log line names this constant, so a cap that fires says so.
pub const APPEARED_CAP: usize = 16;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Appeared {
    /// Canonical, as `hub::seen_worktrees` reports it.
    pub path: String,
    pub branch: String,
}

/// Serialises every read-modify-write, so two writers (a hub's confirm and
/// a page-open clear from another project's connection thread) cannot each
/// read the old list and have the later rename silently drop the other's
/// change.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// `state/appeared/<storage_key(project)>`, or `None` for a project that
/// cannot be one — the same refusal as `outside::dir_in`.
fn file_in(state: &Path, project: &str) -> Option<PathBuf> {
    if project.is_empty() || project.split('/').any(|seg| seg == "..") {
        return None;
    }
    Some(state.join("appeared").join(crate::projects::storage_key(project)))
}

/// Damaged files already logged, so a broken one is said once per process,
/// not on every load.
fn log_damaged_once(path: &Path, why: &str) {
    static LOGGED: Mutex<Option<std::collections::HashSet<PathBuf>>> = Mutex::new(None);
    let mut g = LOGGED.lock().unwrap_or_else(|e| e.into_inner());
    if g.get_or_insert_with(Default::default).insert(path.to_path_buf()) {
        eprintln!("roost: {} is damaged ({why}); showing no appeared worktrees from it", path.display());
    }
}

/// The list as stored: missing → empty, damaged or unreadable → empty and
/// logged once.
pub fn load_in(state: &Path, project: &str) -> Vec<Appeared> {
    let Some(path) = file_in(state, project) else { return Vec::new() };
    let raw = match std::fs::read(&path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            log_damaged_once(&path, &e.to_string());
            return Vec::new();
        }
    };
    match serde_json::from_slice::<Vec<Appeared>>(&raw) {
        Ok(v) => v,
        Err(e) => {
            log_damaged_once(&path, &e.to_string());
            Vec::new()
        }
    }
}

fn write_in(state: &Path, project: &str, list: &[Appeared]) -> Result<(), String> {
    let path = file_in(state, project).ok_or_else(|| format!("invalid project {project:?}"))?;
    let dir = path.parent().expect("file_in always has a parent");
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_vec(list).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".appeared.{}.tmp", crate::wsstate::tmp_suffix()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let written = opts.open(&tmp).and_then(|mut f| {
        use std::io::Write;
        f.write_all(&body)
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Load, change, and write back under `WRITE_LOCK`; writes only when the
/// list changed. A failed write is logged and the changed list still
/// returned — this gates nothing destructive, and the caller's in-memory
/// copy showing the banner this session is better than showing nothing.
fn modify_in(state: &Path, project: &str, f: impl FnOnce(&mut Vec<Appeared>)) -> Vec<Appeared> {
    let _g = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let before = load_in(state, project);
    let mut list = before.clone();
    f(&mut list);
    if list != before {
        if let Err(e) = write_in(state, project, &list) {
            eprintln!("roost: {project}: cannot save the appeared worktrees: {e}");
        }
    }
    list
}

/// Appends `a`, replacing any entry for the same path (it moves to the
/// newest end), and drops the oldest above `APPEARED_CAP`.
pub fn add_in(state: &Path, project: &str, a: Appeared) -> Vec<Appeared> {
    modify_in(state, project, |list| {
        list.retain(|x| x.path != a.path);
        list.push(a);
        while list.len() > APPEARED_CAP {
            let old = list.remove(0);
            eprintln!(
                "roost: {project}: more than APPEARED_CAP ({APPEARED_CAP}) appeared worktrees; dropped the oldest, {}",
                old.path
            );
        }
    })
}

pub fn remove_in(state: &Path, project: &str, path: &str) -> Vec<Appeared> {
    modify_in(state, project, |list| list.retain(|x| x.path != path))
}

/// Keeps only entries whose path is in `listed`. An empty `listed` changes
/// nothing: it is what git answers when it could not be asked (CLAUDE.md,
/// "absence of evidence"), and the caller holding that distinction already
/// does not call this then — this is the second line against the same
/// mistake, as `outside::prune_in` is.
pub fn retain_listed_in(state: &Path, project: &str, listed: &[PathBuf]) -> Vec<Appeared> {
    if listed.is_empty() {
        return load_in(state, project);
    }
    modify_in(state, project, |list| list.retain(|x| listed.iter().any(|p| p == Path::new(&x.path))))
}

pub fn load(project: &str) -> Vec<Appeared> {
    load_in(&crate::wsstate::state_dir(), project)
}
pub fn add(project: &str, a: Appeared) -> Vec<Appeared> {
    add_in(&crate::wsstate::state_dir(), project, a)
}
pub fn remove(project: &str, path: &str) -> Vec<Appeared> {
    remove_in(&crate::wsstate::state_dir(), project, path)
}
pub fn retain_listed(project: &str, listed: &[PathBuf]) -> Vec<Appeared> {
    retain_listed_in(&crate::wsstate::state_dir(), project, listed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(n: usize) -> Appeared {
        Appeared { path: format!("/w/wt{n}"), branch: format!("b{n}") }
    }

    fn paths(v: &[Appeared]) -> Vec<String> {
        v.iter().map(|x| x.path.clone()).collect()
    }

    /// Seventeen distinct adds leave sixteen, the first one gone; a re-add
    /// of a present path does not duplicate it. Read back through a fresh
    /// `load_in`, so this pins what is on disk and not only the return.
    ///
    /// Revert-checked: with the `while list.len() > APPEARED_CAP` trim
    /// removed, fails — observed: `left: 17, right: 16` on the first count.
    /// With the `retain(|x| x.path != a.path)` dedupe removed, the trim
    /// still holds the length at 16, so it is the last assertion that
    /// fails — observed: `left: 2, right: 1`. Both restored.
    #[test]
    fn add_dedupes_and_caps() {
        let d = tempfile::tempdir().unwrap();
        for n in 0..=APPEARED_CAP {
            add_in(d.path(), "p", a(n));
        }
        let got = load_in(d.path(), "p");
        assert_eq!(got.len(), APPEARED_CAP);
        assert!(!paths(&got).contains(&a(0).path), "the oldest is the one dropped: {got:?}");
        assert_eq!(got.last(), Some(&a(APPEARED_CAP)), "the newest is kept, at the end");
        add_in(d.path(), "p", a(5));
        let got = load_in(d.path(), "p");
        assert_eq!(got.len(), APPEARED_CAP, "a re-add is not a second entry: {got:?}");
        assert_eq!(got.iter().filter(|x| x.path == a(5).path).count(), 1);
    }

    /// Revert-checked: with `remove_in`'s retain made a no-op, fails —
    /// observed: `left: ["/w/wt0", "/w/wt1", "/w/wt2"]`. With
    /// `retain_listed_in`'s early return for an empty listing removed,
    /// fails on "an empty listing clears nothing" — observed: `left: 0,
    /// right: 2`. Both restored.
    #[test]
    fn remove_and_retain() {
        let d = tempfile::tempdir().unwrap();
        for n in 0..3 {
            add_in(d.path(), "p", a(n));
        }
        remove_in(d.path(), "p", &a(1).path);
        assert_eq!(paths(&load_in(d.path(), "p")), vec![a(0).path, a(2).path]);
        // A path never listed is a no-op, not an error.
        remove_in(d.path(), "p", "/nowhere");
        assert_eq!(load_in(d.path(), "p").len(), 2);

        retain_listed_in(d.path(), "p", &[]);
        assert_eq!(load_in(d.path(), "p").len(), 2, "an empty listing clears nothing");
        retain_listed_in(d.path(), "p", &[PathBuf::from(&a(2).path), PathBuf::from("/w/main")]);
        assert_eq!(paths(&load_in(d.path(), "p")), vec![a(2).path]);
    }

    /// Revert-checked: with `load_in`'s parse error `.unwrap()`ed instead
    /// of matched, panics — observed: `called Result::unwrap() on an Err
    /// value: Error("expected ident", line: 1, column: 2)`. Restored.
    #[test]
    fn a_damaged_file_is_empty_not_a_panic() {
        let d = tempfile::tempdir().unwrap();
        let f = file_in(d.path(), "p").unwrap();
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "not json{").unwrap();
        assert!(load_in(d.path(), "p").is_empty());
        // And adding over it still works: the list gates nothing, so a
        // banner beats a stuck one.
        assert_eq!(add_in(d.path(), "p", a(1)), vec![a(1)]);
        assert_eq!(load_in(d.path(), "p"), vec![a(1)]);
        assert!(load_in(d.path(), "..").is_empty(), "an impossible project reads nothing");
    }

    /// Revert-checked: with `opts.mode(0o600)` removed, fails — observed:
    /// `left: 436, right: 384` (0664 under this umask). Restored.
    #[cfg(unix)]
    #[test]
    fn the_file_is_written_0600() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        add_in(d.path(), "nested/p", a(1));
        let f = file_in(d.path(), "nested/p").unwrap();
        assert_eq!(f.parent().unwrap(), d.path().join("appeared"), "one file per project, key encoded");
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        let leftovers: Vec<_> = std::fs::read_dir(f.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "no temp file left behind: {leftovers:?}");
    }
}
