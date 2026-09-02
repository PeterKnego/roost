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

/// Every `(project, terminal)` a running `claude` sits in, from ONE walk of
/// `proc_root`. The overview polls every few seconds and asks about every
/// project, so the walk is done once per request and shared — the same
/// hoisting `ages_snapshot`/`holders_snapshot` do for `ps` — rather than
/// once per project. An entry whose environment cannot be read is skipped,
/// never counted as "no Claude".
pub fn claude_terminals(proc_root: &std::path::Path) -> Vec<(String, String)> {
    // The three read-only callers (the overview's ✻, the worktree prompt)
    // have always folded an unreadable root into "found none": a stale glyph
    // is recoverable and nothing destructive hangs off it. `tick` may not —
    // see `try_claude_terminals`.
    try_claude_terminals(proc_root).unwrap_or_default()
}

/// `claude_terminals`, keeping the third outcome: `None` means the walk
/// itself failed, which is not "no Claude is running". Folding the two
/// together is the mistake this codebase made eleven times; here it would
/// empty the cache and tell every open workspace that every Claude exited.
pub fn try_claude_terminals(proc_root: &std::path::Path) -> Option<Vec<(String, String)>> {
    let Ok(rd) = std::fs::read_dir(proc_root) else { return None };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let Ok(_pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        let Ok(comm) = std::fs::read_to_string(e.path().join("comm")) else { continue };
        if comm.trim() != "claude" {
            continue;
        }
        let Ok(raw) = std::fs::read(e.path().join("environ")) else { continue };
        let (mut proj, mut sess) = (None, None);
        for entry in raw.split(|b| *b == 0) {
            let Ok(kv) = std::str::from_utf8(entry) else { continue };
            if let Some(v) = kv.strip_prefix("ROOST_PROJECT=") { proj = Some(v.to_string()); }
            else if let Some(v) = kv.strip_prefix("ROOST_SESSION=") { sess = Some(v.to_string()); }
        }
        if let (Some(p), Some(s)) = (proj, sess) {
            if crate::session::valid_name(&s) {
                out.push((p, s));
            }
        }
    }
    out.sort();
    out.dedup();
    Some(out)
}

fn names_for(project: &str, scan: &[(String, String)]) -> Vec<String> {
    scan.iter().filter(|(p, _)| p == project).map(|(_, s)| s.clone()).collect()
}

/// One project's evidence, walking `/proc` itself. For a single question —
/// a ✻ click — this is the right call; a loop over projects should scan
/// once with `claude_terminals` and use `claude_evidence_with_scan`.
pub fn claude_evidence(project: &str) -> ClaudeEvidence {
    claude_evidence_with_scan(project, &claude_terminals(std::path::Path::new("/proc")))
}

/// `claude_evidence` over an already-taken process scan.
pub fn claude_evidence_with_scan(project: &str, scan: &[(String, String)]) -> ClaudeEvidence {
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
static SCAN: OnceLock<Mutex<Vec<(String, String)>>> = OnceLock::new();

fn scan_cell() -> &'static Mutex<Vec<(String, String)>> {
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

/// Projects whose set of Claude terminals differs between two scans — the
/// only ones that need a fresh snapshot pushed. A project that gained *and*
/// lost nothing is not woken.
fn changed_projects(old: &[(String, String)], new: &[(String, String)]) -> Vec<String> {
    let a: HashSet<&(String, String)> = old.iter().collect();
    let b: HashSet<&(String, String)> = new.iter().collect();
    let mut out: Vec<String> = a.symmetric_difference(&b).map(|(p, _)| p.clone()).collect();
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
    /// Revert-checked: with the scan's result replaced by `Vec::new()` the
    /// first assertion failed with `left: [] right: ["term3"]`.
    #[test]
    fn a_running_claude_process_names_its_terminal() {
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
            vec![("karpie".to_string(), "term3".to_string()), ("other".to_string(), "term".to_string())]
        );
    }
}
