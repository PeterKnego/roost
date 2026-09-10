//! Which Claude is in which terminal, recorded from the hook that already
//! fires.
//!
//! Step 1 of #18. A terminal tab survives a reboot; the Claude that was
//! running in it does not, and until now nothing recorded enough to bring it
//! back — even though roost is *handed* the identifier on every hook event and
//! drops it on the floor. `cli::hook_message` reads three fields out of the
//! payload and discards the rest, and two of the discarded ones are
//! `session_id` and `transcript_path`.
//!
//! So the expensive part of this — a hook installed, owned, and firing — was
//! already done. This is the place to put two strings.
//!
//! ## Why `SessionStart` had to be added
//!
//! `Stop` fires when a turn ends. A session that starts and never finishes a
//! turn never fires it — which is precisely the session a crash interrupted,
//! the one worth recovering. Learning the id only from `Stop` would record
//! every session except the ones that matter.
//!
//! Adding a third event to `claudehooks::EVENTS` is fine. Widening what roost
//! writes is not, and this changes nothing there: still only
//! `.claude/settings.local.json`, still only entries whose command is exactly
//! `roost claude-hook`.
//!
//! ## What this is not
//!
//! It is not a claim that a terminal has no Claude. Hooks are per-project and
//! opt-in via the bell, so a `claude` typed by hand into a plain terminal with
//! hooks off records nothing — and `claudes.rs` already documents that third
//! answer. "No recorded session" must never be presented as "there was no
//! session"; `recorded` returns `Option`, and the absence means *unknown*.
//!
//! It is also not a copy of the transcript. What is stored is the path Claude
//! Code reported, which belongs to Claude Code — its directory name encodes an
//! absolute cwd, so it does not survive being restored at a different path on
//! another machine. #18 leaves that decision open on purpose; recording where
//! the file is today costs nothing and settles nothing.

use std::path::{Path, PathBuf};

/// What roost knows about the Claude in one terminal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Recorded {
    /// Claude Code's own session id, as reported by the hook.
    pub session_id: String,
    /// Where Claude Code says it is writing the conversation. Optional
    /// because it is not roost's file and not roost's promise: an event
    /// without one is still worth recording for the id alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// The event this was learned from, so a later reader can tell a
    /// `SessionStart` (an id, and nothing has happened yet) from a `Stop` (a
    /// turn completed under it).
    pub event: String,
}

/// A session id roost is willing to hold.
///
/// It ends up on a command line — `claude --resume <id>` is the whole point of
/// recording it — and it arrives from another program's JSON. The shape Claude
/// Code emits is a UUID; this accepts that and a little either side rather
/// than pinning the format, but nothing that could be an option, a path or a
/// second word. Same reasoning as `session::valid_name`, same conclusion.
fn valid_session_id(s: &str) -> bool {
    // The leading-dash refusal is not tidiness. `-` is in the character class
    // because a UUID is full of them, and with only the class to go on
    // `--dangerously-skip-permissions` is a perfectly good "session id" — one
    // that stops being a value and becomes an option the moment it reaches
    // `claude --resume <id>`. The test that asserts this caught it; the class
    // alone had looked obviously sufficient.
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('-')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn dir_for(project: &str) -> PathBuf {
    crate::wsstate::state_dir().join("claude").join(crate::projects::storage_key(project))
}

fn path_for(project: &str, session: &str) -> PathBuf {
    dir_for(project).join(format!("{session}.json"))
}

/// Pulls the two fields out of a hook payload, if they are there and usable.
///
/// Separate from the writing so the parse can be tested against real captured
/// payloads without touching a state directory.
pub fn from_payload(v: &serde_json::Value) -> Option<Recorded> {
    let event = v.get("hook_event_name")?.as_str()?.to_string();
    let session_id = v.get("session_id")?.as_str()?.to_string();
    if !valid_session_id(&session_id) {
        return None;
    }
    // A path from another program, stored and never run. Rejected if it is
    // relative — the field is documented absolute, and a relative one would be
    // resolved against whatever cwd a later reader happens to have, which is a
    // different file every time.
    let transcript_path = v
        .get("transcript_path")
        .and_then(|p| p.as_str())
        .filter(|p| !p.is_empty() && Path::new(p).is_absolute())
        .map(|p| p.to_string());
    Some(Recorded { session_id, transcript_path, event })
}

/// Which terminal a hook process is running in, from its own environment.
///
/// `session_env` exports `ROOST_PROJECT`/`ROOST_SESSION` into every roost
/// shell, `claude` inherits them, and so does the hook `claude` spawns. That
/// inheritance is what `claudes.rs` already relies on to attribute a Claude to
/// a terminal; this is the same fact read from the other end.
pub fn terminal_from_env() -> Option<(String, String)> {
    let project = std::env::var("ROOST_PROJECT").ok()?;
    let session = std::env::var("ROOST_SESSION").ok()?;
    if !crate::session::valid_name(&session) || !crate::session::valid_project(&project) {
        return None;
    }
    Some((project, session))
}

/// Writes one terminal's record, atomically.
///
/// Write-then-rename with a pid-unique temp name: the same discipline as
/// `registry::write_origin`, and here because hooks from several terminals can
/// fire at once and a reader must never see half a JSON object.
pub fn record(project: &str, session: &str, rec: &Recorded) {
    if !crate::session::valid_name(session) || !crate::session::valid_project(project) {
        return;
    }
    let Ok(json) = serde_json::to_vec(rec) else { return };
    let marker = path_for(project, session);
    if std::fs::read(&marker).map(|cur| cur == json).unwrap_or(false) {
        return; // a `Stop` per turn would otherwise rewrite this all day
    }
    let dir = dir_for(project);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let tmp = dir.join(format!(".{session}.tmp.{}", std::process::id()));
    if std::fs::write(&tmp, &json).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // 0600 and meant: this names a transcript, which #18 calls the
        // highest-value file on the machine.
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if std::fs::rename(&tmp, &marker).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// What roost recorded for a terminal, or `None` for *nothing recorded* —
/// which is not the same as "no Claude ran here", and no caller may render it
/// that way.
pub fn recorded(project: &str, session: &str) -> Option<Recorded> {
    if !crate::session::valid_name(session) || !crate::session::valid_project(project) {
        return None;
    }
    let raw = std::fs::read(path_for(project, session)).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Drops a terminal's record, for when the session itself is ended.
pub fn forget(project: &str, session: &str) {
    if !crate::session::valid_name(session) || !crate::session::valid_project(project) {
        return;
    }
    let _ = std::fs::remove_file(path_for(project, session));
}

/// The whole hook-side path: read the payload, find the terminal, write it
/// down. Silent on every failure — this runs inside `roost claude-hook`, which
/// must always exit 0 and say nothing (see `cli::run_claude_hook`).
pub fn record_from_hook(v: &serde_json::Value) {
    let Some(rec) = from_payload(v) else { return };
    let Some((project, session)) = terminal_from_env() else { return };
    record(&project, &session, &rec);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `Stop` payload, captured from this host on 2026-09-10 and
    /// recorded verbatim in #18. Written out rather than trimmed to the two
    /// fields under test: the parse has to survive the payload Claude Code
    /// actually sends, including every key roost has no use for.
    fn stop_payload() -> serde_json::Value {
        serde_json::json!({
            "session_id": "864ee734-e3ab-434e-8278-745a850b16ad",
            "transcript_path": "/home/claude/.claude/projects/-tmp-hookprobe/864ee734-e3ab-434e-8278-745a850b16ad.jsonl",
            "cwd": "/tmp/hookprobe",
            "prompt_id": "e844385c-0000-0000-0000-000000000000",
            "permission_mode": "bypassPermissions",
            "hook_event_name": "Stop",
            "stop_hook_active": false,
            "last_assistant_message": "ok"
        })
    }

    #[test]
    fn the_two_fields_roost_used_to_discard_are_read_from_a_real_payload() {
        let got = from_payload(&stop_payload()).expect("a real Stop payload parses");
        assert_eq!(got.session_id, "864ee734-e3ab-434e-8278-745a850b16ad");
        assert_eq!(
            got.transcript_path.as_deref(),
            Some("/home/claude/.claude/projects/-tmp-hookprobe/864ee734-e3ab-434e-8278-745a850b16ad.jsonl")
        );
        assert_eq!(got.event, "Stop", "which event it came from is part of the record");
    }

    #[test]
    fn a_session_start_carries_an_id_and_no_transcript_yet() {
        // The event that had to be added, and the reason: `Stop` fires when a
        // turn *ends*, so a session interrupted before its first turn
        // completed — the one a crash leaves behind, the one worth recovering
        // — would announce itself nowhere else.
        let v = serde_json::json!({
            "session_id": "abc123",
            "hook_event_name": "SessionStart",
            "cwd": "/tmp/x"
        });
        let got = from_payload(&v).expect("no transcript_path is still worth recording");
        assert_eq!(got.session_id, "abc123");
        assert_eq!(got.transcript_path, None);
        assert_eq!(got.event, "SessionStart");
    }

    #[test]
    fn a_session_id_that_could_be_an_argument_is_refused() {
        // It exists to be spliced into `claude --resume <id>`. It arrives from
        // another program's JSON, so it is not roost's string by the time it
        // gets here — the same reasoning that gives session names a character
        // class, reached the same way.
        for bad in [
            "--dangerously-skip-permissions",
            "a b",
            "a;rm -rf /",
            "../../etc/passwd",
            "",
            &"x".repeat(65),
        ] {
            let v = serde_json::json!({ "session_id": bad, "hook_event_name": "Stop" });
            assert!(from_payload(&v).is_none(), "accepted {bad:?} as a session id");
        }
        // Asserts the state it negates: the shape Claude Code really sends is
        // accepted, so the refusals above are not a function that refuses
        // everything.
        let v = serde_json::json!({
            "session_id": "864ee734-e3ab-434e-8278-745a850b16ad",
            "hook_event_name": "Stop"
        });
        assert!(from_payload(&v).is_some(), "a real id must still be accepted");
    }

    #[test]
    fn a_relative_transcript_path_is_dropped_but_the_id_is_kept() {
        // The field is documented absolute. A relative one would resolve
        // against whatever cwd a later reader happens to have — a different
        // file each time — and the id is still worth having without it.
        let v = serde_json::json!({
            "session_id": "abc123",
            "transcript_path": "notes/session.jsonl",
            "hook_event_name": "Stop"
        });
        let got = from_payload(&v).expect("the id survives a bad path");
        assert_eq!(got.transcript_path, None, "a relative path is not recorded");
        assert_eq!(got.session_id, "abc123");
    }

    #[test]
    fn a_payload_with_no_id_records_nothing() {
        let v = serde_json::json!({ "hook_event_name": "Stop", "last_assistant_message": "ok" });
        assert!(from_payload(&v).is_none());
    }

    #[test]
    fn a_record_round_trips_and_ending_the_session_drops_it() {
        crate::wsstate::set_state_dir_for_test();
        let project = format!("recsess{}", std::process::id());
        let rec = from_payload(&stop_payload()).unwrap();
        assert_eq!(recorded(&project, "term"), None, "setup: nothing recorded yet");
        record(&project, "term", &rec);
        assert_eq!(recorded(&project, "term").as_ref(), Some(&rec), "what went in comes back");
        // `next_free_name` hands `term` straight back out after a close, so a
        // record left behind would attribute the closed session's Claude to
        // whatever opens next under that name.
        forget(&project, "term");
        assert_eq!(recorded(&project, "term"), None, "and ending the session drops it");
    }

    #[test]
    fn nothing_recorded_is_not_a_claim_that_no_claude_ran() {
        // Hooks are per-project and opt-in via the bell, so a `claude` typed
        // by hand into a plain terminal with hooks off records nothing.
        // `claudes.rs` already has this third answer; the point here is that
        // the type keeps it available — an `Option`, never a `Recorded` with
        // an empty id that a caller could render as "no session".
        crate::wsstate::set_state_dir_for_test();
        let project = format!("unknownsess{}", std::process::id());
        assert_eq!(recorded(&project, "term"), None);
    }

    #[test]
    fn a_terminal_name_from_the_environment_cannot_escape_the_record_directory() {
        // `ROOST_SESSION` is read from this process's own environment, which a
        // user can export by hand, and it becomes a filename.
        crate::wsstate::set_state_dir_for_test();
        let project = format!("envesc{}", std::process::id());
        let rec = from_payload(&stop_payload()).unwrap();
        record(&project, "term", &rec);
        assert!(recorded(&project, "term").is_some(), "setup: a good name records");
        for bad in ["../escape", "a/b", "", &"x".repeat(33)] {
            record(&project, bad, &rec);
            assert_eq!(recorded(&project, bad), None, "recorded under {bad:?}");
        }
        assert!(
            !crate::wsstate::state_dir().join("claude").join("escape.json").exists(),
            "and nothing was written outside the record directory"
        );
    }

    #[test]
    fn the_record_is_not_world_readable() {
        // It names a transcript, and #18 calls a transcript the
        // highest-value file on the machine: every prompt, every file read,
        // every command run and its output.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            crate::wsstate::set_state_dir_for_test();
            let project = format!("permsess{}", std::process::id());
            record(&project, "term", &from_payload(&stop_payload()).unwrap());
            let m = std::fs::metadata(path_for(&project, "term")).unwrap();
            assert_eq!(m.permissions().mode() & 0o077, 0, "no group or other bits");
        }
    }
}
