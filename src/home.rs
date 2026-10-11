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

#[cfg(test)]
mod tests {
    use super::*;

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
}
