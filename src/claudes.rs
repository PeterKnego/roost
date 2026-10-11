//! What roost can say about Claudes running in a project, from what roost
//! itself observed — never from Claude's own session files.
//!
//! Two signals: a terminal roost typed `claude` into (`session::launched_names`)
//! and a connection on the project's IDE socket (`ide::connected_sessions`).
//! Three answers, not two: with the IDE integration switched off, a `claude`
//! typed by hand into a plain terminal is invisible, so "found nothing" is
//! not "nothing there". Only `Present` may change what a button does.
//!
//! A third signal, `claude_terminals`, walks `/proc`. That walk is too heavy
//! to run per question — a workspace snapshot goes out on every debounced
//! keystroke — so `watch` runs it on a timer and every reader takes the
//! cached result.

use crate::idesess::Sess;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeEvidence {
    /// Terminal names roost could attribute, sorted, deduplicated. May be
    /// empty when the only evidence is a connection it could not place.
    Present(Vec<String>),
    Absent,
    Unknown,
}

pub fn evidence_from(launched: &[String], connected: &[Sess], ide_on: bool) -> ClaudeEvidence {
    let mut names: Vec<String> = launched.to_vec();
    let mut any = !launched.is_empty();
    for s in connected {
        match s {
            Sess::In(n) => { names.push(n.clone()); any = true; }
            Sess::Unknown => any = true,
            // Positively in another project's terminal: not evidence here.
            Sess::Outside => {}
        }
    }
    if any {
        names.sort();
        names.dedup();
        return ClaudeEvidence::Present(names);
    }
    if ide_on { ClaudeEvidence::Absent } else { ClaudeEvidence::Unknown }
}

/// Terminals of `project` that a running `claude` process sits in, read from
/// the process table. `session_env` exports `ROOST_PROJECT`/`ROOST_SESSION`
/// into every roost shell and a `claude` started there inherits them, so a
/// `claude` process's environment names its terminal — `idesess.rs` reads
/// exactly this for one pid; this walks every pid whose `comm` is `claude`.
///
/// This is the restart-proof signal. The launch record and the IDE
/// connection map are in-process memory: a roost restart (every deploy)
/// empties both, and the overview then showed every Claude as a plain
/// shell until each one was restarted. A process that exists is evidence
/// regardless of who remembers starting it.
///
/// `proc_root` is injectable so a fake `/proc` can drive the test; an
/// unreadable entry is skipped, never treated as "no Claude here".
pub fn claudes_in_proc(proc_root: &std::path::Path, project: &str) -> Vec<String> {
    names_for(project, &claude_terminals(proc_root))
}

/// One terminal a running `claude` sits in: which project and session
/// (`ROOST_PROJECT`/`ROOST_SESSION`), the project it actually counts under
/// (`owner`), plus `CLAUDE_CODE_SSE_PORT` read from the same `environ`.
/// Ordered on the full tuple so a scan sorts and dedups deterministically;
/// `changed_projects` deliberately projects the port back out before
/// comparing — see its doc comment.
///
/// `owner` is `home::effective_owner(project, session)`, resolved *here*,
/// during the scan — a watcher-thread or HTTP-thread walk, never under a
/// hub lock. `names_for` and everything built on it (`claude_evidence_with_scan`,
/// `cached_sessions`, which `Hub::snapshot_event` calls on every debounced
/// keystroke) then does a plain field comparison and no I/O at all. Moving
/// the lookup here instead of leaving it in `names_for` is the fix for a
/// real violation: `effective_owner` reads a re-home record and stats a
/// worktree directory, and `Hub::snapshot_event` → `cached_sessions` →
/// `claude_evidence_with_scan` → `names_for` used to run that under the
/// hub lock on every snapshot.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClaudeProc {
    pub project: String,
    pub session: String,
    pub owner: String,
    pub sse_port: Option<u16>,
}

/// Every terminal a running `claude` sits in, from ONE walk of `proc_root`.
/// The overview polls every few seconds and asks about every project, so the
/// walk is done once per request and shared — the same hoisting
/// `ages_snapshot`/`holders_snapshot` do for `ps` — rather than once per
/// project. An entry whose environment cannot be read is skipped, never
/// counted as "no Claude".
pub fn claude_terminals(proc_root: &std::path::Path) -> Vec<ClaudeProc> {
    // The four read-only callers (the overview's ✻, the worktree prompt,
    // and Alt+K's no-connection message) have always folded an unreadable
    // root into "found none": a stale glyph is recoverable and nothing
    // destructive hangs off it. `tick` may not — see `try_claude_terminals`.
    try_claude_terminals(proc_root).unwrap_or_default()
}

/// `claude_terminals`, keeping the third outcome: `None` means the walk
/// itself failed, which is not "no Claude is running". Folding the two
/// together is the mistake this codebase made eleven times; here it would
/// empty the cache and tell every open workspace that every Claude exited.
pub fn try_claude_terminals(proc_root: &std::path::Path) -> Option<Vec<ClaudeProc>> {
    let Ok(rd) = std::fs::read_dir(proc_root) else { return None };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let Ok(_pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        let Ok(comm) = std::fs::read_to_string(e.path().join("comm")) else { continue };
        if comm.trim() != "claude" {
            continue;
        }
        let Ok(raw) = std::fs::read(e.path().join("environ")) else { continue };
        let (mut proj, mut sess, mut port) = (None, None, None);
        for entry in raw.split(|b| *b == 0) {
            let Ok(kv) = std::str::from_utf8(entry) else { continue };
            if let Some(v) = kv.strip_prefix("ROOST_PROJECT=") { proj = Some(v.to_string()); }
            else if let Some(v) = kv.strip_prefix("ROOST_SESSION=") { sess = Some(v.to_string()); }
            // Absent and unparseable are different questions than "found no
            // Claude here" — `sse_port_in`'s doc comment explains why both
            // fold to the same `Some(None)` for a caller.
            else if let Some(v) = kv.strip_prefix("CLAUDE_CODE_SSE_PORT=") { port = v.parse::<u16>().ok(); }
        }
        if let (Some(p), Some(s)) = (proj, sess) {
            if crate::session::valid_name(&s) {
                // Off any lock, here on the scanning thread: see the
                // struct's doc comment.
                let owner = crate::home::effective_owner(&p, &s);
                out.push(ClaudeProc { project: p, session: s, owner, sse_port: port });
            }
        }
    }
    out.sort();
    out.dedup();
    Some(out)
}

/// A Claude's terminal counts for its *effective owner* (`c.owner`), not the
/// raw `ROOST_PROJECT` its environment carries (`c.project`) — so a
/// re-homed Claude shows up (and the removal gate sees it) under the
/// worktree it was moved into rather than the main checkout it started in.
///
/// Pure field comparison, deliberately: `c.owner` was already resolved
/// during the scan (see `ClaudeProc`'s doc comment), so this does no I/O at
/// all and is safe to call under a hub lock.
fn names_for(project: &str, scan: &[ClaudeProc]) -> Vec<String> {
    scan.iter().filter(|c| c.owner == project).map(|c| c.session.clone()).collect()
}

/// This terminal's Claude's `CLAUDE_CODE_SSE_PORT`, if there is a Claude
/// there at all.
///
/// Two `Option`s, and they answer different questions. `None` means no Claude
/// in that terminal. `Some(None)` means a Claude is there but roost could not
/// read a port from it — an environment without the variable (the integration
/// was off when it started), or a value that is not a `u16`. `Some(Some(p))`
/// is a Claude whose port roost knows. `ide::notify_selected` says something
/// different for each, which is the whole reason this is not flattened.
pub fn sse_port_in(scan: &[ClaudeProc], project: &str, session: &str) -> Option<Option<u16>> {
    scan.iter().find(|c| c.project == project && c.session == session).map(|c| c.sse_port)
}

/// One project's evidence, walking `/proc` itself. For a single question —
/// a ✻ click — this is the right call; a loop over projects should scan
/// once with `claude_terminals` and use `claude_evidence_with_scan`.
pub fn claude_evidence(project: &str) -> ClaudeEvidence {
    claude_evidence_with_scan(project, &claude_terminals(std::path::Path::new("/proc")))
}

/// `claude_evidence` over an already-taken process scan.
pub fn claude_evidence_with_scan(project: &str, scan: &[ClaudeProc]) -> ClaudeEvidence {
    let mut launched: Vec<String> =
        crate::session::launched_names(project).into_iter().map(|(n, _)| n).collect();
    launched.extend(names_for(project, scan));
    evidence_from(&launched, &crate::ide::connected_sessions(project), crate::config::ide_enabled())
}

/// How often `watch` re-walks `/proc`. A Claude appearing in a terminal is
/// not urgent enough to poll harder, and the walk touches every pid.
pub const POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// The most recent `/proc` walk. One walk feeds every project's snapshot,
/// the same hoisting `claude_evidence_with_scan` exists for.
static SCAN: OnceLock<Mutex<Vec<ClaudeProc>>> = OnceLock::new();

fn scan_cell() -> &'static Mutex<Vec<ClaudeProc>> {
    SCAN.get_or_init(|| Mutex::new(Vec::new()))
}

/// Terminals of `project` running a Claude, for the tab-icon marking.
///
/// Reads the watcher's cached walk rather than taking one: this is called
/// from `hub::snapshot_event`, which runs on every debounced keystroke.
/// `Absent` and `Unknown` both yield an empty list — an unmarked tab is the
/// honest rendering of "no evidence" *and* of "cannot tell", because the tab
/// bar has no third glyph to say them apart. The ✻/—/? on the overview does.
pub fn cached_sessions(project: &str) -> Vec<String> {
    // Cloned out and the lock released before `claude_evidence_with_scan`,
    // which takes the session and IDE locks of its own. Holding this one
    // across those would be a lock-ordering hazard for no gain; the scan is
    // one entry per running Claude, so the clone is a handful of strings.
    let scan = scan_cell().lock().unwrap_or_else(|e| e.into_inner()).clone();
    match claude_evidence_with_scan(project, &scan) {
        ClaudeEvidence::Present(names) => names,
        ClaudeEvidence::Absent | ClaudeEvidence::Unknown => Vec::new(),
    }
}

/// The watcher's most recent walk, cloned out.
///
/// For callers that need the whole `ClaudeProc` rather than just the session
/// names `cached_sessions` returns — `ide::mention_to`, which wants the SSE
/// port to explain *why* a mention could not be delivered.
///
/// Up to `POLL` stale, which is the right trade for an explanation: the
/// alternative is a fresh `/proc` walk on a path that runs under the hub
/// lock, and CLAUDE.md forbids blocking I/O there. `Search` was diverted to a
/// worker thread for exactly that reason.
pub fn cached_scan() -> Vec<ClaudeProc> {
    scan_cell().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Projects to wake: every `owner` whose `(project, session, owner)` tuple
/// differs between the two scans. Never the port, for the reason below; but
/// `owner` IS in the key, on purpose — it is re-resolved fresh on every walk
/// (see `ClaudeProc`'s doc comment), so a re-home with no accompanying
/// process change (the Claude just `cd`s, nothing exits or restarts) still
/// changes this tuple on the very next poll, even though `project` and
/// `session` are identical. Reporting the *owner* of each differing tuple
/// — not the raw `project` — is what wakes both pages in one tick: the old
/// tuple's owner is the home (losing the session), the new tuple's owner is
/// the worktree (gaining it).
///
/// Compares `(project, session, owner)`, never the port. `tick` broadcasts
/// to every project this names, and a roost restart changes the port of
/// every surviving Claude relative to the new listener — so including it
/// here would wake every open workspace on the first tick after every
/// restart, for a change no client renders. The port is carried on
/// `ClaudeProc` for `ide::notify_selected` to read on an Alt+K, not for
/// this.
fn changed_projects(old: &[ClaudeProc], new: &[ClaudeProc]) -> Vec<String> {
    let key = |c: &ClaudeProc| (c.project.clone(), c.session.clone(), c.owner.clone());
    let a: HashSet<(String, String, String)> = old.iter().map(key).collect();
    let b: HashSet<(String, String, String)> = new.iter().map(key).collect();
    let mut out: Vec<String> = a.symmetric_difference(&b).map(|(_, _, owner)| owner.clone()).collect();
    out.sort();
    out.dedup();
    out
}

/// One poll: walk, store, and push a snapshot to each project that changed.
/// Returns the projects it pushed to, which is what the test asserts on.
///
/// The walk happens before any lock is taken, and the scan lock is dropped
/// before the broadcast — `hub::broadcast_state_for` locks a hub, whose
/// snapshot calls `cached_sessions`, which locks the scan again. Holding it
/// across the broadcast would deadlock every project.
pub fn tick(proc_root: &std::path::Path) -> Vec<String> {
    // A walk that failed tells us nothing, so the cache — and every client
    // reading it — is left exactly as it was.
    let Some(fresh) = try_claude_terminals(proc_root) else { return Vec::new() };
    let changed = {
        let mut cell = scan_cell().lock().unwrap_or_else(|e| e.into_inner());
        let changed = changed_projects(&cell, &fresh);
        *cell = fresh;
        changed
    };
    for p in &changed {
        crate::hub::broadcast_state_for(p);
    }
    changed
}

/// Poll `/proc` forever, pushing a snapshot whenever a project's Claudes
/// change. Started from `main`, not `serve`, so the test servers do not each
/// grow a thread walking the host's real process table.
///
/// Walks once before its first sleep: a tab open at startup should be marked
/// immediately, not after one `POLL`.
/// Serialises the tests that mutate `SCAN`, which is process-global. Without
/// it, this module's test and `hub`'s snapshot test race over one cache — the
/// "~1-in-8 flake" failure mode CLAUDE.md describes.
/// Builds a fake `/proc` in `dir`: one `claude` per (project, session).
/// Shared with `hub`'s snapshot test, which needs the cache seeded.
#[cfg(test)]
pub(crate) fn fake_proc(dir: &std::path::Path, procs: &[(u32, &str, &str)]) {
    for (pid, proj, sess) in procs {
        let p = dir.join(pid.to_string());
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("comm"), "claude\n").unwrap();
        std::fs::write(p.join("environ"), format!("ROOST_PROJECT={proj}\0ROOST_SESSION={sess}\0")).unwrap();
    }
}

#[cfg(test)]
pub(crate) static SCAN_TEST_LOCK: Mutex<()> = Mutex::new(());

pub fn watch() {
    std::thread::spawn(|| loop {
        tick(std::path::Path::new("/proc"));
        std::thread::sleep(POLL);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idesess::Sess;

    #[test]
    fn an_ide_connection_alone_is_present_and_names_its_terminal() {
        // Revert-checked: not including Sess::In names fails here — test panicked with `assertion 'left == right' failed: left: Present([]), right: Present(["term2"])`.
        assert_eq!(evidence_from(&[], &[Sess::In("term2".into())], true), ClaudeEvidence::Present(vec!["term2".into()]));
    }

    #[test]
    fn a_launched_terminal_alone_is_present_even_with_ide_off() {
        // Revert-checked: returning `Unknown` whenever `!ide_on` fails here — test panicked with `assertion 'left == right' failed: left: Unknown, right: Present(["term"])`.
        assert_eq!(evidence_from(&["term".into()], &[], false), ClaudeEvidence::Present(vec!["term".into()]));
    }

    #[test]
    fn a_connection_roost_cannot_place_is_still_present_but_unnamed() {
        // Revert-checked: not treating Sess::Unknown as evidence fails here — test panicked with `assertion 'left == right' failed: left: Absent, right: Present([])`.
        assert_eq!(evidence_from(&[], &[Sess::Unknown], true), ClaudeEvidence::Present(vec![]));
    }

    #[test]
    fn nothing_with_ide_on_is_absent() {
        // Asserted on the variant: `!= Present` would also pass for Unknown.
        // Revert-checked: always returning Unknown fails here — test panicked with `assertion 'left == right' failed: left: Unknown, right: Absent`.
        assert_eq!(evidence_from(&[], &[], true), ClaudeEvidence::Absent);
    }

    #[test]
    fn nothing_with_ide_off_is_unknown() {
        // Revert-checked: dropping the `ide_on` branch yields Absent here — test panicked with `assertion 'left == right' failed: left: Absent, right: Unknown`.
        assert_eq!(evidence_from(&[], &[], false), ClaudeEvidence::Unknown);
    }

    #[test]
    fn a_terminal_seen_both_ways_is_named_once() {
        // Revert-checked: skipping dedup fails here — test panicked with `assertion 'left == right' failed: left: Present(["term", "term"]), right: Present(["term"])`.
        assert_eq!(
            evidence_from(&["term".into()], &[Sess::In("term".into()), Sess::Outside], true),
            ClaudeEvidence::Present(vec!["term".into()])
        );
    }

    /// The watcher's two jobs: keep a cache readers can take cheaply, and wake
    /// only the projects whose Claudes actually changed.
    ///
    /// Revert-checked three ways. (a) Dropping the `changed_projects` filter
    /// so `tick` always returns every project in the scan: the second
    /// assertion failed with `left: ["watch-fixture"] right: []`. (b) Not
    /// writing `fresh` into the cell: the `cached_sessions` assertion failed
    /// with `left: [] right: ["term3"]`. (c) Diffing on project alone rather
    /// than on (project, session): the term3→term4 tick reported no change
    /// and the fourth assertion failed with `left: [] right:
    /// ["watch-fixture"]`.
    #[test]
    fn the_watcher_caches_a_walk_and_wakes_only_what_changed() {
        let _g = SCAN_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let root = d.path();

        fake_proc(root, &[(100, "watch-fixture", "term3")]);
        assert_eq!(tick(root), vec!["watch-fixture".to_string()], "a new Claude must wake its project");
        assert_eq!(cached_sessions("watch-fixture"), vec!["term3".to_string()]);

        // Nothing moved: no project is woken, so an idle host broadcasts
        // nothing every three seconds.
        assert_eq!(tick(root), Vec::<String>::new(), "an unchanged scan must wake nobody");

        // Same project, different terminal — a change the diff must see.
        std::fs::remove_dir_all(root.join("100")).unwrap();
        fake_proc(root, &[(101, "watch-fixture", "term4")]);
        assert_eq!(tick(root), vec!["watch-fixture".to_string()]);
        assert_eq!(cached_sessions("watch-fixture"), vec!["term4".to_string()]);

        // Claude exits: the project is woken again and the cache empties.
        std::fs::remove_dir_all(root.join("101")).unwrap();
        assert_eq!(tick(root), vec!["watch-fixture".to_string()], "a Claude going away must wake its project");
        assert!(cached_sessions("watch-fixture").is_empty());
    }

    /// An unreadable `/proc` is "cannot tell", not "every Claude exited" —
    /// the rule this codebase broke eleven times. A walk that returns nothing
    /// because the walk failed must not empty a populated cache and tell
    /// every workspace its Claudes are gone.
    ///
    /// Revert-checked, and it caught the bug for real: `tick` was written
    /// against `claude_terminals`, whose empty `Vec` on an unreadable root is
    /// indistinguishable from "no Claude anywhere". It failed at the wake
    /// assertion with `left: ["unreadable-fixture"] right: []` — i.e. it had
    /// already broadcast "your Claudes are gone" to every open workspace.
    #[test]
    fn an_unreadable_proc_leaves_the_cache_alone() {
        let _g = SCAN_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        fake_proc(d.path(), &[(100, "unreadable-fixture", "term")]);
        tick(d.path());
        assert_eq!(cached_sessions("unreadable-fixture"), vec!["term".to_string()]);

        assert_eq!(tick(&d.path().join("no-such-dir")), Vec::<String>::new(), "a failed walk must wake nobody");
        assert_eq!(
            cached_sessions("unreadable-fixture"),
            vec!["term".to_string()],
            "a walk that could not read /proc must not be read as 'no Claude anywhere'"
        );

        std::fs::remove_dir_all(d.path().join("100")).unwrap();
        tick(d.path()); // leave the shared cache empty for other tests
    }

    /// A `claude` in this project's terminal is evidence even when roost
    /// remembers launching nothing (it just restarted). Built on a fake
    /// `/proc`: pid 100 is a claude in term3 of this project, pid 200 a
    /// claude in another project, pid 300 a bash in this project.
    ///
    /// `ROOST_STATE_DIR` is set to an empty tempdir (rather than left at
    /// whatever `state_dir()` defaults to) so the `owner` field this scan
    /// now resolves is deterministic: no re-home record exists for either
    /// terminal, so `owner` must equal `project` for both.
    ///
    /// Revert-checked: with the scan's result replaced by `Vec::new()` the
    /// first assertion failed with `left: [] right: ["term3"]`.
    #[test]
    fn a_running_claude_process_names_its_terminal() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let d = tempfile::tempdir().unwrap();
        let mk = |pid: u32, comm: &str, env: &str| {
            let p = d.path().join(pid.to_string());
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
            std::fs::write(p.join("environ"), env.replace('\n', "\0")).unwrap();
        };
        mk(100, "claude", "ROOST_PROJECT=karpie\nROOST_SESSION=term3\n");
        mk(200, "claude", "ROOST_PROJECT=other\nROOST_SESSION=term\n");
        mk(300, "bash", "ROOST_PROJECT=karpie\nROOST_SESSION=term1\n");
        std::fs::write(d.path().join("self"), b"").unwrap(); // a non-pid entry, skipped
        assert_eq!(claudes_in_proc(d.path(), "karpie"), vec!["term3".to_string()]);
        assert!(claudes_in_proc(d.path(), "nowhere").is_empty());
        // The one-walk scan every loop shares: both claudes, neither the bash.
        assert_eq!(
            claude_terminals(d.path()),
            vec![
                ClaudeProc { project: "karpie".into(), session: "term3".into(), owner: "karpie".into(), sse_port: None },
                ClaudeProc { project: "other".into(), session: "term".into(), owner: "other".into(), sse_port: None },
            ]
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// A Claude whose terminal was re-homed into a worktree must count for
    /// that worktree, not for the main checkout its `ROOST_PROJECT`
    /// environment still names. Unlike the pre-fix design, the mapping now
    /// happens *during the scan* (`try_claude_terminals`, called here with
    /// no hub lock held) rather than inside `names_for` — this proves the
    /// owner really is resolved there: a real `.claude/worktrees/worktree-x`
    /// directory under a real `ROOST_ROOTS` root is what lets the
    /// production, fork-free `home::resolves` confirm it.
    ///
    /// Revert-checked: dropping the `effective_owner` call in
    /// `try_claude_terminals` (pushing `owner: p.clone()` unconditionally)
    /// fails the first assertion — observed: `assertion 'left == right'
    /// failed: left: [], right: ["term3"]`.
    #[test]
    fn a_rehomed_claude_counts_for_its_owner_not_its_home() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        std::env::set_var("ROOST_ROOTS", root.path());
        let home_dir = root.path().join("karpie");
        std::fs::create_dir_all(home_dir.join(".claude/worktrees/worktree-x")).unwrap();
        crate::home::record("karpie", "term3", "karpie/.claude/worktrees/worktree-x").unwrap();

        let d = tempfile::tempdir().unwrap();
        let mk = |pid: u32, comm: &str, env: &str| {
            let p = d.path().join(pid.to_string());
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
            std::fs::write(p.join("environ"), env.replace('\n', "\0")).unwrap();
        };
        mk(100, "claude", "ROOST_PROJECT=karpie\nROOST_SESSION=term3\n");
        let procs = claude_terminals(d.path());
        assert_eq!(names_for("karpie/.claude/worktrees/worktree-x", &procs), vec!["term3".to_string()]);
        assert!(names_for("karpie", &procs).is_empty());

        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// `names_for` must do no I/O of its own — the owner it compares against
    /// was already resolved when the scan was taken (see `ClaudeProc`'s doc
    /// comment) — which is what makes it safe to call from
    /// `cached_sessions`, under the hub lock, on every debounced keystroke.
    ///
    /// Proven by construction rather than by inspection: `owner` is set to
    /// a value that does *not* match `project` and that no real lookup
    /// against `ROOST_STATE_DIR` (pointed at an empty, otherwise-untouched
    /// tempdir) could ever produce — there is no re-home record anywhere
    /// under it. If `names_for` still called `effective_owner` itself, it
    /// would find `Owner::Here` and fall back to `project`, not the scan's
    /// `owner` field.
    ///
    /// Revert-checked: reverting `names_for` to filter on `c.project ==
    /// project` (ignoring the precomputed `owner`) fails the first
    /// assertion — observed: `assertion 'left == right' failed: left: [],
    /// right: ["term3"]` — because nothing in the fake scan has
    /// `project == "owner-that-cannot-be-derived"`.
    #[test]
    fn names_for_does_no_io_of_its_own() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap(); // empty: no re-home record anywhere under it
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let scan = vec![ClaudeProc {
            project: "karpie".into(),
            session: "term3".into(),
            owner: "owner-that-cannot-be-derived".into(),
            sse_port: None,
        }];
        assert_eq!(names_for("owner-that-cannot-be-derived", &scan), vec!["term3".to_string()]);
        assert!(names_for("karpie", &scan).is_empty(), "the raw ROOST_PROJECT must not win over the scanned owner");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// `changed_projects`'s whole point for re-homing: a Claude that just
    /// `cd`s into a worktree starts, exits and restarts no process at all,
    /// so the *only* thing that differs between two scans is the `owner`
    /// field — re-resolved fresh on every walk. Reporting the owner of each
    /// differing tuple (not the raw `project`) is what wakes both the home
    /// (losing the session) and the worktree (gaining it) from one tick.
    ///
    /// Revert-checked: reverting the key back to `(project, session)` (the
    /// pre-fix body) fails the assertion — observed: `assertion 'left ==
    /// right' failed\n  left: []\n right: ["karpie",
    /// "karpie/.claude/worktrees/worktree-x"]` — because an owner-only
    /// change is invisible to that key, which is exactly finding
    /// 1's bug: a re-home with no process change wakes nobody.
    #[test]
    fn an_owner_only_change_wakes_both_the_home_and_the_new_owner() {
        let before = vec![ClaudeProc {
            project: "karpie".into(),
            session: "term3".into(),
            owner: "karpie".into(),
            sse_port: None,
        }];
        let after = vec![ClaudeProc {
            project: "karpie".into(),
            session: "term3".into(),
            owner: "karpie/.claude/worktrees/worktree-x".into(),
            sse_port: None,
        }];
        let mut changed = changed_projects(&before, &after);
        changed.sort();
        assert_eq!(changed, vec!["karpie".to_string(), "karpie/.claude/worktrees/worktree-x".to_string()]);
    }

    /// End to end through `tick`: the same `claude` process survives
    /// between two polls (same pid, same environment — nothing exits or
    /// restarts), and only a re-home record written in between changes
    /// anything. `tick` must still notice on its very next poll and wake
    /// both projects, because `try_claude_terminals` re-resolves `owner`
    /// on every walk.
    ///
    /// Revert-checked: reverting `try_claude_terminals` to push
    /// `owner: p.clone()` unconditionally (so it never calls
    /// `effective_owner` at all) fails the second `tick` assertion —
    /// observed: `assertion 'left == right' failed: left: [], right:
    /// ["rehome-tick-fixture", "rehome-tick-fixture/.claude/worktrees/worktree-x"]`
    /// — i.e. the re-home is silently invisible to the watcher.
    #[test]
    fn tick_wakes_both_projects_on_a_rehome_with_no_process_change() {
        let _lock = SCAN_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        std::env::set_var("ROOST_ROOTS", root.path());
        let home = "rehome-tick-fixture";
        let owner = format!("{home}/.claude/worktrees/worktree-x");
        std::fs::create_dir_all(root.path().join(home).join(".claude/worktrees/worktree-x")).unwrap();

        let proc_root = tempfile::tempdir().unwrap();
        fake_proc(proc_root.path(), &[(100, home, "term3")]);

        assert_eq!(tick(proc_root.path()), vec![home.to_string()], "first sighting wakes the home");
        assert_eq!(cached_sessions(home), vec!["term3".to_string()]);

        // Nothing about the process changes — same pid, same environ —
        // only the re-home record appears in between the two polls.
        crate::home::record(home, "term3", &owner).unwrap();
        let mut changed = tick(proc_root.path());
        changed.sort();
        assert_eq!(
            changed,
            vec![home.to_string(), owner.clone()],
            "a re-home with no process change must still wake both pages on the very next poll"
        );
        assert_eq!(cached_sessions(&owner), vec!["term3".to_string()]);
        assert!(cached_sessions(home).is_empty(), "the home no longer shows it");

        // Leave the shared cache empty for other tests — the same
        // discipline `the_watcher_caches_a_walk_and_wakes_only_what_changed`
        // and `an_unreadable_proc_leaves_the_cache_alone` follow (ruling
        // R13b). Without this, `rehome-tick-fixture`'s owner row stays in
        // the process-global `SCAN` indefinitely, which is invisible here
        // only because this test happens to run alphabetically last among
        // the ones that touch `SCAN`.
        //
        // Revert-checked: dropping the final `tick(proc_root.path())`
        // (keeping only the `remove_dir_all` and the assertion) fails —
        // observed: `panicked ... cleanup: the owner's cache entry must be
        // gone too` — the directory is gone but the cache still holds the
        // last walk's result until another `tick` runs.
        std::fs::remove_dir_all(proc_root.path().join("100")).unwrap();
        tick(proc_root.path());
        assert!(cached_sessions(&owner).is_empty(), "cleanup: the owner's cache entry must be gone too");

        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// `sse_port_in`'s outer `Option` answers "is a Claude here at all";
    /// the inner answers "could roost read its port". A missing variable
    /// and an unparseable one both fold to `Some(None)` — deliberately, since
    /// Task 4 renders one sentence for both, not two.
    #[test]
    fn the_walk_reads_the_sse_port_from_the_same_environ() {
        let d = tempfile::tempdir().unwrap();
        let mk = |pid: u32, comm: &str, env: &str| {
            let p = d.path().join(pid.to_string());
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
            std::fs::write(p.join("environ"), env.replace('\n', "\0")).unwrap();
        };
        mk(100, "claude", "ROOST_PROJECT=karpie\nROOST_SESSION=term3\nCLAUDE_CODE_SSE_PORT=41011\n");
        // No port at all: a claude started before the integration existed, or with
        // the kill switch off. Distinct from a port roost could not parse.
        mk(200, "claude", "ROOST_PROJECT=karpie\nROOST_SESSION=term4\n");
        mk(300, "claude", "ROOST_PROJECT=karpie\nROOST_SESSION=term5\nCLAUDE_CODE_SSE_PORT=notanumber\n");
        let scan = claude_terminals(d.path());
        assert_eq!(sse_port_in(&scan, "karpie", "term3"), Some(Some(41011)));
        assert_eq!(sse_port_in(&scan, "karpie", "term4"), Some(None), "present, port unknown");
        assert_eq!(sse_port_in(&scan, "karpie", "term5"), Some(None), "unparseable is unknown, not absent");
        assert_eq!(sse_port_in(&scan, "karpie", "term9"), None, "no Claude in that terminal at all");
        assert_eq!(sse_port_in(&scan, "other", "term3"), None, "another project's terminal");
    }

    /// `tick` broadcasts to every project `changed_projects` names, and a
    /// roost restart changes every surviving Claude's port relative to the
    /// live listener. If the port entered the comparison, the first tick
    /// after every restart would wake every open workspace for no visible
    /// change.
    ///
    /// Revert-checked: comparing whole `ClaudeProc`s (port included) fails
    /// here with `assertion failed: changed_projects(&before, &after).is_empty()`.
    #[test]
    fn a_port_change_alone_does_not_wake_a_project() {
        let before = vec![ClaudeProc { project: "karpie".into(), session: "term".into(), owner: "karpie".into(), sse_port: Some(1111) }];
        let after = vec![ClaudeProc { project: "karpie".into(), session: "term".into(), owner: "karpie".into(), sse_port: Some(2222) }];
        assert!(changed_projects(&before, &after).is_empty());
        // …but a terminal appearing or leaving still does.
        let gone: Vec<ClaudeProc> = vec![];
        assert_eq!(changed_projects(&before, &gone), vec!["karpie".to_string()]);
    }
}
