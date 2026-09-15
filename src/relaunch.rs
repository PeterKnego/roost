//! Which terminals roost itself started an agent in, remembered across a
//! reboot.
//!
//! Step 3 of #17, and the one that issue is most careful about: relaunching a
//! Claude means starting an agent nobody is watching, in a checkout whose
//! state it does not know. Three decisions follow from that, and they are the
//! substance of this module rather than caveats on it.
//!
//! **Off unless asked for.** The `relaunch` setting defaults to false, so
//! nothing here happens on an install that has not opted in.
//!
//! **Global only.** `GLOBAL_ONLY_KEYS` exists for decisions a checkout must
//! not get a vote on, and "start an agent when this project is opened" is the
//! clearest case there has been: a cloned repository that could set this would
//! be arranging to run an agent on a machine it has just arrived on.
//!
//! **When the project is opened, not at boot.** #17 asks for "relaunch on
//! boot" and this deliberately does less. A boot-time relaunch is the version
//! with nobody in front of it; doing it when someone opens the project keeps a
//! person there while it happens, and costs almost nothing — the terminal they
//! are about to look at is the one being started.
//!
//! **The launch kind only, never `--resume`.** #17 is explicit that resuming
//! makes this worse rather than better: it would continue a conversation whose
//! last turn may have been mid-edit. What is recorded is that this terminal
//! was started to run `claude`, and that is all that comes back.

use std::path::PathBuf;

fn dir_for(project: &str) -> PathBuf {
    crate::wsstate::state_dir().join("launch").join(crate::projects::storage_key(project))
}

/// Writes down that `session` was started to run `launch`.
///
/// Write-then-rename with a pid-unique temp name, the discipline
/// `registry::write_origin` sets: a half-written marker read by the other
/// instance sharing this state directory would be a launch name that is not a
/// launch name, and the parse below would refuse it — recoverable, but the
/// atomic write costs nothing.
pub fn record(project: &str, session: &str, launch: crate::proto::Launch) {
    if !crate::session::valid_name(session) || !crate::session::valid_project(project) {
        return;
    }
    let bytes = crate::launch::wire_name(launch).as_bytes().to_vec();
    let dir = dir_for(project);
    let marker = dir.join(session);
    if std::fs::read(&marker).map(|cur| cur == bytes).unwrap_or(false) {
        return;
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let tmp = dir.join(format!(".{session}.tmp.{}", std::process::id()));
    if std::fs::write(&tmp, &bytes).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if std::fs::rename(&tmp, &marker).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// What roost started in this terminal, if it started anything.
///
/// `None` covers three different things — nothing recorded, unreadable, and a
/// name this build does not know — and they are folded on purpose: the action
/// for all three is the same, and it is the *safe* one. Nothing here decides
/// to launch on a guess.
pub fn recorded(project: &str, session: &str) -> Option<crate::proto::Launch> {
    if !crate::session::valid_name(session) || !crate::session::valid_project(project) {
        return None;
    }
    let raw = std::fs::read(dir_for(project).join(session)).ok()?;
    let name = String::from_utf8(raw).ok()?;
    crate::launch::from_wire(name.trim())
}

pub fn forget(project: &str, session: &str) {
    if !crate::session::valid_name(session) || !crate::session::valid_project(project) {
        return;
    }
    let _ = std::fs::remove_file(dir_for(project).join(session));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::Launch;

    #[test]
    fn a_launch_round_trips_and_ending_the_session_forgets_it() {
        crate::wsstate::set_state_dir_for_test();
        let project = format!("relaunch{}", std::process::id());
        assert_eq!(recorded(&project, "term"), None, "setup: nothing recorded");
        record(&project, "term", Launch::Claude);
        assert_eq!(recorded(&project, "term"), Some(Launch::Claude));
        forget(&project, "term");
        assert_eq!(recorded(&project, "term"), None, "and closing the tab drops it");
    }

    #[test]
    fn a_launch_name_this_build_does_not_know_starts_nothing() {
        // A marker written by a newer roost, read by an older one sharing the
        // same state directory — the pairing `roostedge` makes ordinary. The
        // safe answer is `None`, because the only thing a `Some` causes here
        // is an agent starting.
        crate::wsstate::set_state_dir_for_test();
        let project = format!("unknownlaunch{}", std::process::id());
        let dir = dir_for(&project);
        std::fs::create_dir_all(&dir).unwrap();
        // Asserts the state it then negates: a name this build *does* know is
        // read back, so `None` below is about the name and not about the file.
        std::fs::write(dir.join("term"), b"claude").unwrap();
        assert_eq!(recorded(&project, "term"), Some(Launch::Claude), "setup");
        std::fs::write(dir.join("term"), b"some-future-agent").unwrap();
        assert_eq!(recorded(&project, "term"), None, "an unknown launch is not a launch");
        std::fs::write(dir.join("term"), b"").unwrap();
        assert_eq!(recorded(&project, "term"), None, "and neither is an empty marker");
    }

    #[test]
    fn a_session_name_cannot_escape_the_marker_directory() {
        crate::wsstate::set_state_dir_for_test();
        let project = format!("relesc{}", std::process::id());
        record(&project, "term", Launch::Claude);
        assert!(recorded(&project, "term").is_some(), "setup: a good name records");
        for bad in ["../escape", "a/b", "", &"x".repeat(33)] {
            record(&project, bad, Launch::Claude);
            assert_eq!(recorded(&project, bad), None, "recorded under {bad:?}");
        }
        assert!(
            !crate::wsstate::state_dir().join("launch").join("escape").exists(),
            "and nothing was written outside the marker directory"
        );
    }

    #[test]
    fn the_setting_is_off_by_default_and_a_project_cannot_turn_it_on() {
        // The two properties this feature's safety rests on, asserted together
        // because either alone is worth little: a default of off with a
        // project-writable key is not off, and a global-only key that defaults
        // to on is not opt-in.
        assert!(
            !crate::config::relaunch(),
            "opening a project must not start an agent on an install that never asked"
        );
        assert!(
            crate::config::GLOBAL_ONLY_KEYS.contains(&"relaunch"),
            "a cloned repository must not be able to arrange to run an agent"
        );
    }
}
