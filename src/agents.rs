//! What the other agents in this project are doing, for an agent to ask.
//!
//! #118. Compared against herdr, whose agents "wait until another agent is
//! genuinely blocked", roost had one instance of that idea — the hard-coded
//! PR-loop button — and no general answer.
//!
//! **The sensing already existed.** `claudehooks::EVENTS` installs three
//! events and `claudesess::Recorded` stores which one it last saw, per
//! terminal, so `SessionStart`/`Notification`/`Stop` has been sitting on disk
//! as working/blocked/idle since #18 step 1. This module is the rendering, and
//! deliberately reads nothing else: herdr classifies a pane by reading its
//! scrollback, and roost does not have to, because the agent says so through a
//! hook. Guessing from scrollback would be inventing evidence where a fact
//! already exists.
//!
//! ## `Unknown` is the whole safety argument
//!
//! Hooks are per-project and opt-in via the bell, so a Claude typed by hand
//! into a plain terminal records nothing. `claudesess::recorded` returns
//! `Option` precisely so that absence means *unknown*, and its doc says the
//! rule out loud: "No recorded session must never be presented as there was no
//! session."
//!
//! An agent told `idle` because roost could not look would start editing on
//! top of another agent's half-finished work. So `Unknown` is its own state,
//! and **`wait` is never satisfied by it** — see `Outcome`.

use std::time::{Duration, Instant};

/// What one terminal's agent is doing, as far as roost can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// A session started and no turn has finished under it yet.
    Working,
    /// It asked for permission, or is waiting for input.
    Blocked,
    /// A turn completed.
    Idle,
    /// Nothing recorded. **Not** "no agent" and **not** "idle" — hooks may
    /// simply be off for this project.
    Unknown,
}

impl State {
    pub fn wire(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Blocked => "blocked",
            State::Idle => "idle",
            State::Unknown => "unknown",
        }
    }

    /// The states a `--for` may name. `unknown` is deliberately not one: it is
    /// an answer roost gives, never a condition anything may wait for.
    pub fn from_wanted(s: &str) -> Option<State> {
        match s {
            "idle" => Some(State::Idle),
            "blocked" => Some(State::Blocked),
            "working" => Some(State::Working),
            _ => None,
        }
    }
}

/// The mapping the whole feature rests on, and the reason it is a function
/// rather than three inline comparisons: it is the one place the hook
/// vocabulary becomes roost's, and it is tested event by event.
///
/// An event roost does not know is `Unknown`, never a guess. A future Claude
/// Code release adding a fourth event must make roost say "I do not know"
/// rather than quietly classify it as idle and release a waiter.
pub fn state_of(event: &str) -> State {
    match event {
        "SessionStart" => State::Working,
        "Notification" => State::Blocked,
        "Stop" => State::Idle,
        _ => State::Unknown,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub session: String,
    /// A socket is on disk for it. Positive evidence, the same the registry
    /// uses.
    pub live: bool,
    pub state: State,
    /// Claude Code's own session id, where one was recorded. Carried so a
    /// caller can correlate with a transcript; never used to decide a state.
    pub session_id: Option<String>,
}

/// Every terminal of `project`, with what roost knows about each.
///
/// `Err` for an unreadable socket directory rather than an empty list:
/// "no terminals" and "I could not look" would print identically, and a script
/// reading the empty list would conclude it is alone. `socket_names_checked`
/// exists to preserve that distinction and this is a second caller that needs
/// it.
pub fn sessions(project: &str) -> Result<Vec<Row>, String> {
    if !crate::session::valid_project(project) {
        return Err(format!("{project:?} is not a project name roost will use"));
    }
    let live = crate::session::socket_names_checked(project).ok_or_else(|| {
        format!("cannot read the session directory for {project} — roost cannot tell what is running")
    })?;
    let mut names: std::collections::BTreeSet<String> =
        live.iter().filter(|n| crate::session::valid_name(n)).cloned().collect();
    // A terminal roost has a record for but no socket is worth listing too: it
    // is the shell a reboot took, and #18's whole point is that its
    // conversation is still recoverable.
    names.extend(recorded_names(project));
    Ok(names
        .into_iter()
        .map(|session| {
            let rec = crate::claudesess::recorded(project, &session);
            Row {
                live: live.contains(&session),
                state: rec.as_ref().map(|r| state_of(&r.event)).unwrap_or(State::Unknown),
                session_id: rec.map(|r| r.session_id),
                session,
            }
        })
        .collect())
}

/// Terminals with a `claudesess` record, whether or not a socket survives.
///
/// A failure to read is an empty list here and not an error, which is the
/// opposite of the socket directory above — deliberately. A missing record
/// directory is the *normal* state for a project whose hooks are off, and
/// every row it would have added already degrades to `Unknown`, which is the
/// same answer. Nothing is hidden by folding it.
fn recorded_names(project: &str) -> Vec<String> {
    let dir = crate::wsstate::state_dir().join("claude").join(crate::projects::storage_key(project));
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.strip_suffix(".json").map(str::to_string)
        })
        .filter(|n| crate::session::valid_name(n))
        .collect()
}

/// How a wait ended. Three outcomes, and the third is why this is an enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The state was reached.
    Reached,
    /// The deadline passed while roost *could* see the terminal's state — it
    /// simply never became what was asked for.
    TimedOut { last: State },
    /// The deadline passed and roost never had a record at all. A different
    /// problem with a different fix — hooks are off — and it must not share a
    /// message with the case above, or the reader looks in the wrong place.
    NeverKnew,
}

/// Blocks until `session` reaches `want`, or the deadline passes.
///
/// **`Unknown` never satisfies a wait**, whatever was asked for. That is the
/// entire safety property of this module: a caller running
/// `roost wait other --for idle && edit_the_files` must not proceed because
/// roost could not look.
pub fn wait(
    project: &str,
    session: &str,
    want: State,
    timeout: Duration,
    poll: Duration,
    sleep: &dyn Fn(Duration),
    now: &dyn Fn() -> Instant,
) -> Outcome {
    let deadline = now() + timeout;
    let mut ever_knew = false;
    loop {
        let state = crate::claudesess::recorded(project, session)
            .map(|r| state_of(&r.event))
            .unwrap_or(State::Unknown);
        if state != State::Unknown {
            ever_knew = true;
            if state == want {
                return Outcome::Reached;
            }
        }
        if now() >= deadline {
            return if ever_knew {
                Outcome::TimedOut { last: state }
            } else {
                Outcome::NeverKnew
            };
        }
        sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(event: &str) -> serde_json::Value {
        serde_json::json!({
            "session_id": "864ee734-e3ab-434e-8278-745a850b16ad",
            "hook_event_name": event,
        })
    }

    /// Event by event, not a table of one. A mapping asserted only on `Stop`
    /// passes against a function that returns `Idle` for everything — and
    /// `Idle` is the state that releases a waiter.
    #[test]
    fn each_hook_event_maps_to_its_own_state() {
        assert_eq!(state_of("SessionStart"), State::Working);
        assert_eq!(state_of("Notification"), State::Blocked);
        assert_eq!(state_of("Stop"), State::Idle);
        // A fourth event from a future Claude Code release must read as "I do
        // not know", never as a guess — a wrong guess of `Stop` would release
        // a waiter onto another agent's half-finished work.
        assert_eq!(state_of("Compact"), State::Unknown);
        assert_eq!(state_of(""), State::Unknown);
    }

    /// `unknown` is never a thing to wait *for*. It is an answer roost gives.
    #[test]
    fn a_wait_cannot_be_asked_for_unknown() {
        assert_eq!(State::from_wanted("idle"), Some(State::Idle));
        assert_eq!(State::from_wanted("blocked"), Some(State::Blocked));
        assert_eq!(State::from_wanted("working"), Some(State::Working));
        assert_eq!(State::from_wanted("unknown"), None, "unknown must not be waitable");
        assert_eq!(State::from_wanted("done"), None);
    }

    /// **The safety property of the whole module.**
    ///
    /// A caller writes `roost wait other --for idle && edit_the_files`. If
    /// roost cannot see the terminal — hooks off, which is the default — it
    /// must not answer "idle". It waits, and then says it never knew.
    ///
    /// Revert-checked: dropping the `state != State::Unknown` guard so an
    /// unknown state is compared against `want` makes this fail immediately —
    /// `wait` returns `Reached` on the first poll, having seen nothing at all.
    #[test]
    fn an_unrecorded_terminal_never_satisfies_a_wait() {
        crate::wsstate::set_state_dir_for_test();
        let project = format!("agentsunknown{}", std::process::id());
        // A clock that advances only when asked, so the test neither sleeps
        // nor races a real deadline. `Cell`, because `wait` takes `&dyn Fn`
        // and a `Fn` may not mutate what it captures.
        let ticks = std::cell::Cell::new(0u32);
        let start = std::time::Instant::now();
        let clock = || {
            ticks.set(ticks.get() + 1);
            start + std::time::Duration::from_millis(u64::from(ticks.get()) * 20)
        };
        let out = wait(
            &project,
            "term",
            State::Idle,
            std::time::Duration::from_millis(30),
            std::time::Duration::from_millis(1),
            &|_| {},
            &clock,
        );
        assert_eq!(out, Outcome::NeverKnew, "an unknown state released a waiter");
    }

    /// And the other half: once roost *does* know, the wait is satisfied. A
    /// test with only the refusal above passes against a `wait` that never
    /// returns `Reached` at all.
    #[test]
    fn a_recorded_state_satisfies_the_wait_it_names() {
        crate::wsstate::set_state_dir_for_test();
        let project = format!("agentsreach{}", std::process::id());
        let r = crate::claudesess::from_payload(&rec("Stop")).unwrap();
        crate::claudesess::record(&project, "term", &r);
        let start = std::time::Instant::now();
        let out = wait(
            &project,
            "term",
            State::Idle,
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(1),
            &|_| {},
            &|| start,
        );
        assert_eq!(out, Outcome::Reached);
        // And a wait for a *different* state times out rather than being
        // satisfied by whatever happens to be recorded.
        let ticks = std::cell::Cell::new(0u32);
        let clock = || {
            ticks.set(ticks.get() + 1);
            start + std::time::Duration::from_millis(u64::from(ticks.get()) * 20)
        };
        let out = wait(
            &project,
            "term",
            State::Blocked,
            std::time::Duration::from_millis(30),
            std::time::Duration::from_millis(1),
            &|_| {},
            &clock,
        );
        assert_eq!(
            out,
            Outcome::TimedOut { last: State::Idle },
            "the timeout must carry what it did see, not merely that it failed"
        );
        crate::claudesess::forget(&project, "term");
    }

    /// `TimedOut` and `NeverKnew` are different problems with different fixes
    /// — "it did not happen" versus "hooks are off" — and the CLI words them
    /// apart. Folding them would send the reader to the wrong place.
    #[test]
    fn the_two_failures_are_distinguishable_by_the_caller() {
        let a = Outcome::TimedOut { last: State::Working };
        let b = Outcome::NeverKnew;
        assert_ne!(a, b);
    }

    /// An unreadable session directory is an error, not an empty list. "No
    /// terminals" and "I could not look" print identically otherwise, and a
    /// script reading the empty list concludes it is alone.
    #[test]
    fn an_unreadable_session_directory_is_an_error_not_an_empty_list() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            crate::wsstate::set_state_dir_for_test();
            let project = format!("agentsblind{}", std::process::id());
            let dir = crate::wsstate::state_dir()
                .join("sock")
                .join(crate::projects::storage_key(&project));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
            let got = sessions(&project);
            let readable = std::fs::read_dir(&dir).is_ok();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            // Skipped rather than inverted as root, which reads a 0000
            // directory regardless — a test that quietly passes as root is the
            // "passes for the wrong reason" class by another name.
            if !readable {
                let e = got.expect_err("an unreadable session directory must be an error");
                assert!(e.contains("cannot tell"), "{e}");
            }
        }
    }

    #[test]
    fn a_terminal_with_a_socket_and_no_record_is_live_and_unknown() {
        // The honest rendering of "a shell is there and no hook has spoken":
        // not idle, not absent.
        crate::wsstate::set_state_dir_for_test();
        let project = format!("agentslive{}", std::process::id());
        let dir = crate::wsstate::state_dir()
            .join("sock")
            .join(crate::projects::storage_key(&project));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("term"), b"").unwrap();
        let rows = sessions(&project).expect("a readable directory");
        let row = rows.iter().find(|r| r.session == "term").expect("the terminal");
        assert!(row.live);
        assert_eq!(row.state, State::Unknown);
        assert_eq!(row.session_id, None);
        let _ = std::fs::remove_file(dir.join("term"));
    }

    #[test]
    fn a_recorded_terminal_whose_shell_is_gone_is_still_listed() {
        // #18's whole point: the shell a reboot took still has a conversation
        // worth recovering, so it must not vanish from the list.
        crate::wsstate::set_state_dir_for_test();
        let project = format!("agentsgone{}", std::process::id());
        let r = crate::claudesess::from_payload(&rec("Notification")).unwrap();
        crate::claudesess::record(&project, "term9", &r);
        let rows = sessions(&project).expect("readable");
        let row = rows.iter().find(|r| r.session == "term9").expect("the recorded terminal");
        assert!(!row.live, "it has no socket");
        assert_eq!(row.state, State::Blocked);
        assert_eq!(row.session_id.as_deref(), Some("864ee734-e3ab-434e-8278-745a850b16ad"));
        crate::claudesess::forget(&project, "term9");
    }
}
