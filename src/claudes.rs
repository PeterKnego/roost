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
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
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
    /// `/proc/<pid>/cwd`, read fresh on every walk via `read_link`. `None`
    /// means the link could not be read — a process gone between the
    /// `environ` read and this one, or a permission the walking user lacks
    /// — and every reader must treat that as "unknown", never as "still at
    /// home": see `detect_moves`. Deliberately excluded from
    /// `changed_projects`' comparison key, the same way `sse_port` already
    /// is and for the same reason stated there — a plain `cd` changes this
    /// on every single poll for a Claude sitting in a shell someone is
    /// actively using, and including it would rebroadcast every such
    /// project every three seconds for a change nothing renders from this
    /// struct directly (`detect_moves`/`detect_gone_owners` decide what, if
    /// anything, is worth telling a browser about, and de-dup their own
    /// announcements separately — see `tick`).
    pub cwd: Option<std::path::PathBuf>,
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
                // A `read_link` failure (the process exited between the
                // `environ` read above and this one, or an unreadable
                // `/proc` entry) folds to `None` — "unknown", never "at
                // home". See the field's own doc comment.
                let cwd = std::fs::read_link(e.path().join("cwd")).ok();
                out.push(ClaudeProc { project: p, session: s, owner, sse_port: port, cwd });
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

/// A move roost was not asked about (spec §7). `Into` is a native Claude
/// whose cwd has landed inside one of its home's own worktrees; `Back` is a
/// resident's Claude that has cd'd back out to its home's main checkout.
/// Both are offers a browser accepts or ignores, with one exception: an
/// `Into` that is exactly the move an approved `WorktreeCreate` expected is
/// taken by `offer_moves_at`, through `hub::rehome_to` — the same writer
/// `hub::do_rehome` uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Move {
    Into { home: String, session: String, worktree: String },
    Back { home: String, session: String },
}

/// `detect_moves`, over an already-taken scan — what `tick` calls, so a
/// move check never doubles the `/proc` walk `try_claude_terminals` just
/// did. `detect_moves` itself (used directly by its own tests, which have
/// no scan to hand in) is this plus that walk.
fn detect_moves_in(
    procs: &[ClaudeProc],
    home_dir_of: &dyn Fn(&str) -> Option<PathBuf>,
    owner_of: &dyn Fn(&str, &str) -> crate::home::Owner,
) -> Vec<Move> {
    let mut out = Vec::new();
    // Both caches are keyed by `c.project` and filled at most once per
    // distinct project per call. R8's "N Claudes cost one git call per
    // home" already covered `worktree::list`; `home_dir_of` needs the
    // same treatment (fix round 1, finding/R25c) — it stands in for
    // `projects::resolve_project`, which itself runs git for a worktree
    // URL (`is_vouched_worktree`), so this was a second, uncached git cost
    // per Claude rather than per project.
    let mut home_dirs: HashMap<String, Option<PathBuf>> = HashMap::new();
    let mut lists: HashMap<String, Vec<crate::worktree::Worktree>> = HashMap::new();
    for c in procs {
        // Unreadable cwd is "unknown", never "still at home" — report
        // nothing rather than guess (CLAUDE.md's evidence rule).
        let Some(cwd) = c.cwd.as_ref() else { continue };
        let Ok(cwd_canon) = cwd.canonicalize() else { continue };
        let home_dir = home_dirs.entry(c.project.clone()).or_insert_with(|| home_dir_of(&c.project)).clone();
        let Some(home_dir) = home_dir else { continue };
        let Ok(home_canon) = home_dir.canonicalize() else { continue };
        let list = lists.entry(c.project.clone()).or_insert_with(|| crate::worktree::list(&home_dir));
        if list.is_empty() {
            continue; // git did not answer: no evidence either way
        }

        // Finding 3: `c.project` is only a genuine *home* — the main
        // checkout re-homing is defined relative to — when its own
        // resolved directory IS the main entry of its own
        // `worktree::list`. A terminal native to a worktree *project*
        // (`ROOST_PROJECT` itself names a `.claude/worktrees/<x>` url)
        // resolves to that worktree's own directory instead, which can
        // never be a home: there is no re-homing relationship from a
        // worktree to itself. Without this guard, such a Claude's own
        // worktree matched its own non-main entry below and built the
        // self-referential url `repo/.claude/worktrees/w1/.claude/worktrees/w1`.
        let is_real_home = list
            .iter()
            .find(|w| w.is_main)
            .and_then(|m| m.path.canonicalize().ok())
            .map(|p| p == home_canon)
            .unwrap_or(false);
        if !is_real_home {
            continue;
        }

        // Finding 2: containment, not equality, and the LONGEST matching
        // root wins. A linked worktree lives *under* the main checkout's
        // own directory (`home/.claude/worktrees/<name>`), so the main
        // checkout's own path is always a prefix of every one of its
        // worktrees' paths too — checking containment without picking the
        // most specific match would read `main/.claude/worktrees/wt/src`
        // as "still in main" (it matches `main` as a prefix) instead of
        // "in wt" (the correct, more specific answer). Resolved through
        // git's own list, never a string prefix on top of that: a plain
        // directory that merely looks like `.claude/worktrees/<x>` is not
        // a worktree, and only git can tell the two apart.
        let mut best: Option<(&crate::worktree::Worktree, PathBuf)> = None;
        for w in list.iter() {
            let Ok(wcanon) = w.path.canonicalize() else { continue };
            if cwd_canon.starts_with(&wcanon) {
                let longer = best.as_ref().map(|(_, b)| wcanon.as_os_str().len() > b.as_os_str().len()).unwrap_or(true);
                if longer {
                    best = Some((w, wcanon));
                }
            }
        }
        let Some((w, wcanon)) = best else { continue }; // cwd is nowhere under this home at all

        if w.is_main {
            if matches!(owner_of(&c.project, &c.session), crate::home::Owner::Elsewhere(_)) {
                out.push(Move::Back { home: c.project.clone(), session: c.session.clone() });
            }
            continue;
        }

        // Finding 3's other half: the worktree url is built exactly as
        // `Hub::do_rehome` validates one — a canonical `strip_prefix` of
        // the home's own directory — never a dirname guess, so the two
        // can never disagree about what counts as "a worktree of this
        // home".
        let Ok(rel) = wcanon.strip_prefix(&home_canon) else { continue };
        let worktree = format!("{}/{}", c.project, rel.display());
        // Only a worktree a record can name (final review d): one outside
        // `.claude/worktrees/` never `resolves`, so the move would be a
        // record the session ignores and an offer that returns every tick.
        if !crate::home::owner_shaped(&c.project, &worktree) {
            continue;
        }
        let already_recorded = matches!(
            owner_of(&c.project, &c.session),
            crate::home::Owner::Elsewhere(ref o) if *o == worktree
        );
        if !already_recorded {
            out.push(Move::Into { home: c.project.clone(), session: c.session.clone(), worktree });
        }
    }
    out
}

/// Whether any Claude's cwd disagrees with what its home's re-home record
/// currently says — `home_dir_of` stands in for `projects::resolve_project`
/// and `owner_of` for `home::owner_of`, both injected so this needs no
/// project roots or state directory to test directly.
pub fn detect_moves(
    proc_root: &std::path::Path,
    home_dir_of: &dyn Fn(&str) -> Option<PathBuf>,
    owner_of: &dyn Fn(&str, &str) -> crate::home::Owner,
) -> Vec<Move> {
    // No Claudes at all costs no git call — try_claude_terminals returning
    // `None` (walk failed) or an empty `Vec` both short-circuit the loop in
    // detect_moves_in before any `worktree::list` runs.
    let Some(procs) = try_claude_terminals(proc_root) else { return Vec::new() };
    detect_moves_in(&procs, home_dir_of, owner_of)
}

/// Positive evidence (R18) that a resident's worktree is gone: `home`'s
/// `git worktree list` answered — non-empty, since the main checkout is
/// always listed — and does not contain the owner's directory, canonically
/// compared, *and* `home::resolves` (a bare stat) independently agrees.
/// Both are required: a stat alone is "cannot tell" territory everywhere
/// else `resolves` is used (display-only, record kept), and this function
/// exists specifically to license the stronger act of forgetting the
/// record — see `sweep_gone_owners`.
///
/// A home with no re-home record at all costs zero git calls, and so does
/// one whose records' owners still resolve.
pub fn detect_gone_owners(home_dir_of: &dyn Fn(&str) -> Option<PathBuf>) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for home in crate::home::all_homes() {
        let elsewhere: Vec<(String, String)> = crate::home::away_from(&home)
            .into_iter()
            .filter_map(|(n, o)| match o {
                crate::home::Owner::Elsewhere(u) => Some((n, u)),
                _ => None,
            })
            .collect();
        if elsewhere.is_empty() {
            continue;
        }
        let Some(home_dir) = home_dir_of(&home) else { continue };
        let list = crate::worktree::list(&home_dir);
        if list.is_empty() {
            continue; // git did not answer: no evidence either way
        }
        let listed: Vec<PathBuf> = list.iter().filter_map(|w| w.path.canonicalize().ok()).collect();
        for (name, owner) in elsewhere {
            if crate::home::resolves(&home, &owner) {
                continue; // still there
            }
            // The leaf no longer exists (that is the whole point), so only
            // its parent — `.claude/worktrees`, which exists as long as the
            // home repo does — can be canonicalised; the leaf name is
            // appended back on for the comparison against `listed`.
            let Some(expected) = crate::home::owner_path(&home, &owner) else { continue };
            let (Some(parent), Some(leaf)) = (expected.parent(), expected.file_name()) else { continue };
            let Ok(parent_canon) = parent.canonicalize() else { continue };
            let expected_canon = parent_canon.join(leaf);
            if !listed.contains(&expected_canon) {
                out.push((home.clone(), name, owner));
            }
        }
    }
    out
}

/// Acts on `detect_gone_owners`' evidence: ALWAYS attempts the forget
/// off-lock (fix round 1, finding 1 — see below for why "always"), tells
/// the home's browsers once per *triple* (`Event::WorktreeGone`, gated by
/// `announced`), and — only once the forget has actually taken — gives the
/// home its tab back (R25a, `hub::retab_native_if_missing`, not a plain
/// refresh: see that function's own doc comment for why this must be the
/// one that calls it). `home::forget` and
/// `hub::broadcast_to_project`/`retab_native_if_missing` each take their
/// own lock briefly; none is held across the git calls
/// `detect_gone_owners` already finished before this runs.
///
/// `announced` is keyed on `(home, session, owner)`, not `(home, session)`
/// — finding 1's bug: term3 re-homed to worktree-x, removed, forgotten;
/// later re-homed to worktree-y, removed too. A two-field key would see
/// `(home, term3)` already in the set from the first removal and refuse
/// to even *attempt* the second forget, leaving the record stuck
/// `Elsewhere(y)` — not merely unannounced, genuinely never cleaned up,
/// since nothing else in this module revisits a pair once it is in
/// `announced`. The fix is two-fold: the forget is attempted
/// unconditionally on every sweep that finds the evidence (gating it was
/// the actual defect), and the three-field key means a *different* owner
/// for the same `(home, session)` is simply a different key, never
/// shadowed by the first.
///
/// The key is removed again the moment the record actually reads `Here`
/// — bounding the set to only the pairs genuinely stuck (an unexpected
/// I/O error on `forget`, which is silent — see its own doc comment — so
/// there is no success/failure signal to gate on beyond re-reading the
/// record). That removal is also what lets a *subsequent* removal of the
/// *same* worktree (re-created and re-removed) announce again rather than
/// staying silently suppressed forever.
pub fn sweep_gone_owners(
    home_dir_of: &dyn Fn(&str) -> Option<PathBuf>,
    announced: &Mutex<HashSet<(String, String, String)>>,
) {
    sweep_gone_owners_with(home_dir_of, announced, &|_, _| {})
}

/// [`sweep_gone_owners`] with `before_forget(home, name)` called between the
/// evidence and the forget, so a test can land a re-point in exactly that
/// window instead of racing for it.
pub(crate) fn sweep_gone_owners_with(
    home_dir_of: &dyn Fn(&str) -> Option<PathBuf>,
    announced: &Mutex<HashSet<(String, String, String)>>,
    before_forget: &dyn Fn(&str, &str),
) {
    for (home, name, owner) in detect_gone_owners(home_dir_of) {
        before_forget(&home, &name);
        // Compare, then forget (final review c, amending R18). The evidence
        // is about `owner`; a record re-pointed since — an approval moving
        // this terminal into a new worktree mid-sweep — names another, and
        // says nothing about that one. Not atomic: a write landing between
        // this read and the unlink still loses, but the window shrinks from
        // a `git worktree list` per home to two syscalls.
        match crate::home::owner_of(&home, &name) {
            crate::home::Owner::Elsewhere(ref o) if *o == owner => {
                // Always attempted, never gated on `announced` — the bug
                // this fixes is exactly a forget that was skipped because
                // an *earlier* owner for this same (home, session) had
                // already been announced.
                crate::home::forget(&home, &name);
            }
            // Already gone: nothing to forget, and the tab still wants
            // giving back below.
            crate::home::Owner::Here => {}
            other => {
                eprintln!("roost: kept {home}/{name}'s re-home record: it no longer names the removed {owner} ({other:?})");
                continue;
            }
        }

        let key = (home.clone(), name.clone(), owner.clone());
        let first_time = {
            let mut g = announced.lock().unwrap_or_else(|e| e.into_inner());
            g.insert(key.clone())
        };
        if first_time {
            crate::hub::broadcast_to_project(
                &home,
                &crate::proto::Event::WorktreeGone { worktree: owner, session: name.clone() },
            );
        }

        if matches!(crate::home::owner_of(&home, &name), crate::home::Owner::Here) {
            {
                let mut g = announced.lock().unwrap_or_else(|e| e.into_inner());
                g.remove(&key);
            }
            // The forget actually took, whether on this attempt or (after
            // an earlier failure) a later one: give the home its tab back.
            crate::hub::retab_native_if_missing(&home, &name);
        }
    }
}

/// Process-level "last offered" state for `Event::ClaudeElsewhere`: a
/// banner is sent only when a session's into/back state actually changes
/// between ticks, never every three seconds for a Claude simply sitting
/// still in the worktree (or main checkout) it was last offered for.
/// `None` means "last seen back in the main checkout"; `Some(url)` means
/// "last seen in that worktree". Never on a `Hub` — see the module's own
/// `SCAN`, which this mirrors for the same reason.
static LAST_OFFERED: OnceLock<Mutex<HashMap<(String, String), Option<String>>>> = OnceLock::new();

fn offered_cell() -> &'static Mutex<HashMap<(String, String), Option<String>>> {
    LAST_OFFERED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Process-level "already told" set for `sweep_gone_owners`'s
/// `Event::WorktreeGone`, keyed `(home, session, owner)` — see that
/// function's own doc comment (fix round 1, finding 1) for why the owner
/// is part of the key and why an entry is removed again once the record
/// actually reads `Here`.
static GONE_ANNOUNCED: OnceLock<Mutex<HashSet<(String, String, String)>>> = OnceLock::new();

fn gone_announced_cell() -> &'static Mutex<HashSet<(String, String, String)>> {
    GONE_ANNOUNCED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// How long an approved `WorktreeCreate` stays the explanation for its
/// Claude's cwd landing in that worktree (spec §1). Long enough for a
/// Claude to act on the `Path` reply and for one `POLL` to see it; short
/// enough that a subagent's approval, which never moves the parent, is
/// forgotten before anything else can be mistaken for it.
pub const EXPECT_MOVE_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);

/// One approved move not yet seen. `refused` is set the first time
/// `rehome` refuses it (R16): from then on it is an ordinary offer and
/// never retried (spec §1, "the existing offer path takes over"). It still
/// stays until it expires, so a re-approval replaces it as usual.
#[derive(Debug, Clone)]
pub(crate) struct Expectation {
    url: String,
    at: std::time::Instant,
    refused: bool,
}

type Expectations = Mutex<HashMap<(String, String), Expectation>>;

/// Approved-but-not-yet-seen moves, keyed `(home, session)`. In memory
/// only: a restart loses an entry, which only turns the automatic move into
/// the ordinary offer.
static EXPECTED: OnceLock<Expectations> = OnceLock::new();

fn expected_cell() -> &'static Expectations {
    EXPECTED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Records that `(home, session)`'s Claude was just told to work in
/// `worktree_url`. Approval is what Claude *intends*; the hook cannot tell
/// the Claude's own `EnterWorktree` from a subagent's `isolation:
/// "worktree"`, so nothing moves until `/proc` shows that Claude's cwd
/// actually inside the worktree — see `offer_moves_at`.
pub fn expect_move(home: &str, session: &str, worktree_url: &str) {
    expect_move_in(expected_cell(), home, session, worktree_url, std::time::Instant::now());
}

/// `expect_move` against an explicit map and clock. One entry per
/// `(home, session)`: a newer approval replaces the older outright.
pub(crate) fn expect_move_in(
    expect: &Expectations,
    home: &str,
    session: &str,
    worktree_url: &str,
    now: std::time::Instant,
) {
    let mut g = expect.lock().unwrap_or_else(|e| e.into_inner());
    g.insert(
        (home.to_string(), session.to_string()),
        Expectation { url: worktree_url.to_string(), at: now, refused: false },
    );
}

/// The worktree `(home, session)` is currently expected to move into.
#[cfg(test)]
pub(crate) fn expectation_for(home: &str, session: &str) -> Option<String> {
    let g = expected_cell().lock().unwrap_or_else(|e| e.into_inner());
    g.get(&(home.to_string(), session.to_string())).map(|e| e.url.clone())
}

/// Delivers `Event::ClaudeElsewhere` for every `Move` whose (home, session)
/// state changed since the last tick that saw one. `Into` goes to the home
/// — the page still showing the session natively, with the offer to move
/// it elsewhere; `Back` goes to whichever project the record currently
/// names as owner — the page showing the resident, with the offer to move
/// it back. A `Back` whose record already changed out from under it between
/// `detect_moves` and here (`home::owner_of` re-read fresh) sends nothing,
/// the same "cannot tell, do nothing" direction as everywhere else in this
/// module.
fn offer_moves(moves: &[Move], last: &Mutex<HashMap<(String, String), Option<String>>>) {
    offer_moves_at(
        moves,
        last,
        expected_cell(),
        std::time::Instant::now(),
        &crate::hub::rehome_to,
        &crate::hub::broadcast_to_project,
    );
}

/// `offer_moves` with the expectation map, the clock, the re-home and the
/// broadcast injected.
///
/// The one exception to "a move is only ever offered": an `Into` that is
/// exactly the worktree an approval named for this `(home, session)`, seen
/// within `EXPECT_MOVE_WINDOW`, is the approved move landing, so it is
/// taken without asking again. The evidence is the Claude's own cwd, which
/// a subagent's worktree never changes. Every other `Into` — an expired
/// expectation, a different worktree, an expectation a restart lost, or
/// one `rehome` refused (R16) — gets the ordinary offer. A refused one
/// keeps its expectation to expire on its own (spec §1), marked so it is
/// never re-homed automatically again.
///
/// `rehome` is called with no lock held: it takes hub locks one at a time
/// and broadcasts to every hub.
pub(crate) fn offer_moves_at(
    moves: &[Move],
    last: &Mutex<HashMap<(String, String), Option<String>>>,
    expect: &Expectations,
    now: std::time::Instant,
    rehome: &dyn Fn(&str, &str, &str) -> Result<(), String>,
    broadcast: &dyn Fn(&str, &crate::proto::Event),
) {
    // The window is enforced here and only here: whatever survives this
    // sweep is fresh.
    expect
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|_, e| now.saturating_duration_since(e.at) <= EXPECT_MOVE_WINDOW);
    for mv in moves {
        match mv {
            Move::Into { home, session, worktree } => {
                let key = (home.clone(), session.clone());
                let expected = {
                    let g = expect.lock().unwrap_or_else(|e| e.into_inner());
                    g.get(&key).is_some_and(|e| e.url == *worktree && !e.refused)
                };
                if expected {
                    match rehome(home, session, worktree) {
                        Ok(()) => {
                            {
                                let mut g = expect.lock().unwrap_or_else(|e| e.into_inner());
                                // Only the entry that was acted on: an
                                // approval that landed during `rehome` names
                                // something else and stays.
                                if g.get(&key).is_some_and(|e| e.url == *worktree) {
                                    g.remove(&key);
                                }
                            }
                            last.lock().unwrap_or_else(|e| e.into_inner()).insert(key, Some(worktree.clone()));
                            continue;
                        }
                        Err(msg) => {
                            // Logged once: a refused entry never reaches
                            // `rehome` again.
                            eprintln!("roost: {session} entered {worktree} as approved, but was not moved: {msg}");
                            let mut g = expect.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(e) = g.get_mut(&key).filter(|e| e.url == *worktree) {
                                e.refused = true;
                            }
                        }
                    }
                }
                let fresh = Some(worktree.clone());
                let changed = {
                    let mut g = last.lock().unwrap_or_else(|e| e.into_inner());
                    g.insert(key, fresh.clone()) != Some(fresh)
                };
                if changed {
                    broadcast(
                        home,
                        &crate::proto::Event::ClaudeElsewhere { session: session.clone(), worktree: Some(worktree.clone()) },
                    );
                }
            }
            Move::Back { home, session } => {
                let key = (home.clone(), session.clone());
                let changed = {
                    let mut g = last.lock().unwrap_or_else(|e| e.into_inner());
                    g.insert(key, None) != Some(None)
                };
                if changed {
                    if let crate::home::Owner::Elsewhere(owner) = crate::home::owner_of(home, session) {
                        broadcast(&owner, &crate::proto::Event::ClaudeElsewhere { session: session.clone(), worktree: None });
                    }
                }
            }
        }
    }
}

/// One poll: walk, store, and push a snapshot to each project that changed.
/// Returns the projects it pushed to, which is what the test asserts on.
///
/// The walk happens before any lock is taken, and the scan lock is dropped
/// before the broadcast — `hub::broadcast_state_for` locks a hub, whose
/// snapshot calls `cached_sessions`, which locks the scan again. Holding it
/// across the broadcast would deadlock every project.
///
/// The two git-based checks below (moves, and gone owners — R8) run here,
/// off every lock: not the scan lock, released above; not a hub lock,
/// none of which is held on this thread; and not `SESSIONS` or the
/// claudes scan-cache lock either. Each is wrapped in `catch_unwind` so a
/// panic deep in git parsing cannot take down the watcher thread that also
/// owns `SCAN` and every project's state broadcast.
pub fn tick(proc_root: &std::path::Path) -> Vec<String> {
    // A walk that failed tells us nothing, so the cache — and every client
    // reading it — is left exactly as it was.
    let Some(fresh) = try_claude_terminals(proc_root) else { return Vec::new() };
    let changed = {
        let mut cell = scan_cell().lock().unwrap_or_else(|e| e.into_inner());
        let changed = changed_projects(&cell, &fresh);
        *cell = fresh.clone();
        changed
    };
    for p in &changed {
        crate::hub::broadcast_state_for(p);
    }

    let home_dir_of = |home: &str| crate::projects::resolve_project(&crate::projects::roots(), home);

    let moved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        detect_moves_in(&fresh, &home_dir_of, &crate::home::owner_of)
    }))
    .unwrap_or_default();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| offer_moves(&moved, offered_cell())));

    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sweep_gone_owners(&home_dir_of, gone_announced_cell())
    }));

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
                ClaudeProc { project: "karpie".into(), session: "term3".into(), owner: "karpie".into(), sse_port: None, cwd: None },
                ClaudeProc { project: "other".into(), session: "term".into(), owner: "other".into(), sse_port: None, cwd: None },
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
            cwd: None,
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
            cwd: None,
        }];
        let after = vec![ClaudeProc {
            project: "karpie".into(),
            session: "term3".into(),
            owner: "karpie/.claude/worktrees/worktree-x".into(),
            sse_port: None,
            cwd: None,
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
        let before = vec![ClaudeProc { project: "karpie".into(), session: "term".into(), owner: "karpie".into(), sse_port: Some(1111), cwd: None }];
        let after = vec![ClaudeProc { project: "karpie".into(), session: "term".into(), owner: "karpie".into(), sse_port: Some(2222), cwd: None }];
        assert!(changed_projects(&before, &after).is_empty());
        // …but a terminal appearing or leaving still does.
        let gone: Vec<ClaudeProc> = vec![];
        assert_eq!(changed_projects(&before, &gone), vec!["karpie".to_string()]);
    }

    // --- Task 12: moves roost was not asked about -------------------------

    /// A real git repo plus one real worktree (`.claude/worktrees/feat`,
    /// branch `feat`), nested inside the same `TempDir` the fake `/proc`
    /// entries live under — one `TempDir` cleans up both, and the repo and
    /// worktree outlive the function that creates them. The project name
    /// every caller passes is "karpie"; nothing here reads the directory
    /// name back, so that is convention only.
    fn fake_proc_with_repo_and_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("_repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        std::fs::write(repo.join("a.txt"), "x").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "init"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        let wt = repo.join(".claude/worktrees/feat");
        std::fs::create_dir_all(wt.parent().unwrap()).unwrap();
        assert!(std::process::Command::new("git")
            .arg("-C").arg(&repo)
            .args(["worktree", "add", "-b", "feat", wt.to_str().unwrap()])
            .status().unwrap().success());
        (root, repo, wt)
    }

    /// A fake `/proc/<pid>` entry: `comm` + `environ` only, deliberately no
    /// `cwd` symlink — the "unreadable cwd" shape
    /// `an_unreadable_cwd_reports_nothing` needs.
    fn mk(root: &std::path::Path, pid: u32, comm: &str, env: &str) {
        let p = root.join(pid.to_string());
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
        std::fs::write(p.join("environ"), env.replace('\n', "\0")).unwrap();
    }

    /// `mk`, plus a real `cwd` symlink to `target` — always `comm: claude`,
    /// since every test that needs a `cwd` is asking about a Claude.
    fn mk_with_cwd(root: &std::path::Path, pid: u32, env: &str, target: &std::path::Path) {
        mk(root, pid, "claude", env);
        std::os::unix::fs::symlink(target, root.join(pid.to_string()).join("cwd")).unwrap();
    }

    #[test]
    fn a_claude_in_a_worktree_of_its_home_is_reported_moved() {
        let (root, repo, wt) = fake_proc_with_repo_and_worktree();
        mk_with_cwd(root.path(), 100, "ROOST_PROJECT=karpie\nROOST_SESSION=term3\n", &wt);
        let moves = detect_moves(root.path(), &|p| (p == "karpie").then(|| repo.clone()), &|_, _| crate::home::Owner::Here);
        assert_eq!(
            moves,
            vec![Move::Into { home: "karpie".into(), session: "term3".into(), worktree: "karpie/.claude/worktrees/feat".into() }]
        );
    }

    /// Final-review d: a worktree of the home that is not under its
    /// `.claude/worktrees/` is never offered. A record naming one never
    /// `resolves` (`home::owner_path` admits only that shape), so accepting
    /// the offer writes a record the session ignores — the tab stays at
    /// home and the banner comes back next tick, forever.
    ///
    /// Revert-checked (final fix wave): with the `home::owner_shaped`
    /// filter removed from `detect_moves_in`, fails with `left: [Into {
    /// .., worktree: "karpie/elsewhere" }], right: []`.
    #[test]
    fn a_worktree_outside_claude_worktrees_is_not_offered() {
        let (root, repo, _wt) = fake_proc_with_repo_and_worktree();
        let outside = repo.join("elsewhere");
        assert!(std::process::Command::new("git")
            .arg("-C").arg(&repo)
            .args(["worktree", "add", "-q", "-b", "elsewhere", outside.to_str().unwrap()])
            .status().unwrap().success());
        mk_with_cwd(root.path(), 100, "ROOST_PROJECT=karpie\nROOST_SESSION=term3\n", &outside);
        let moves = detect_moves(root.path(), &|p| (p == "karpie").then(|| repo.clone()), &|_, _| crate::home::Owner::Here);
        assert_eq!(moves, vec![]);
    }

    /// An unreadable `cwd` is "unknown", never "still at home" — reporting
    /// anything here would be exactly the evidence mistake CLAUDE.md's
    /// table is about, just in the "says something untrue" direction
    /// rather than the destructive one.
    #[test]
    fn an_unreadable_cwd_reports_nothing() {
        let (root, repo, _wt) = fake_proc_with_repo_and_worktree();
        mk(root.path(), 100, "claude", "ROOST_PROJECT=karpie\nROOST_SESSION=term3\n"); // no cwd link
        assert!(detect_moves(root.path(), &|_| Some(repo.clone()), &|_, _| crate::home::Owner::Here).is_empty());
    }

    /// Revert check: replacing the `worktree::list`-based match in
    /// `detect_moves_in` with `cwd.starts_with(repo.join(".claude/worktrees"))`
    /// makes this fail — observed: `left: [Into { .. }], right: []` — a
    /// plain directory that merely sits where a worktree could is not one,
    /// and only asking git can tell the two apart. Restored.
    #[test]
    fn a_sibling_directory_with_a_matching_prefix_is_not_a_worktree() {
        let (root, repo, _wt) = fake_proc_with_repo_and_worktree();
        let impostor = repo.join(".claude/worktrees/feat-old");
        std::fs::create_dir_all(&impostor).unwrap();
        mk_with_cwd(root.path(), 100, "ROOST_PROJECT=karpie\nROOST_SESSION=term3\n", &impostor);
        assert!(detect_moves(root.path(), &|_| Some(repo.clone()), &|_, _| crate::home::Owner::Here).is_empty());
    }

    /// A Claude back in the main checkout, for a session the record already
    /// shows as `Elsewhere`, is reported as `Move::Back` — the "Move back"
    /// offer's trigger.
    #[test]
    fn a_resident_claude_back_in_the_main_checkout_is_reported_back() {
        let (root, repo, _wt) = fake_proc_with_repo_and_worktree();
        mk_with_cwd(root.path(), 100, "ROOST_PROJECT=karpie\nROOST_SESSION=term3\n", &repo);
        let moves = detect_moves(root.path(), &|_| Some(repo.clone()), &|_, _| {
            crate::home::Owner::Elsewhere("karpie/.claude/worktrees/feat".into())
        });
        assert_eq!(moves, vec![Move::Back { home: "karpie".into(), session: "term3".into() }]);
    }

    /// A Claude already correctly recorded at the worktree it is sitting in
    /// is not reported again — `detect_moves` only ever names a
    /// *disagreement* between `cwd` and the record, never a steady state.
    #[test]
    fn an_already_recorded_move_is_not_reported_again() {
        let (root, repo, wt) = fake_proc_with_repo_and_worktree();
        mk_with_cwd(root.path(), 100, "ROOST_PROJECT=karpie\nROOST_SESSION=term3\n", &wt);
        let moves = detect_moves(root.path(), &|_| Some(repo.clone()), &|_, _| {
            crate::home::Owner::Elsewhere("karpie/.claude/worktrees/feat".into())
        });
        assert!(moves.is_empty());
    }

    // --- Fix round 1 -------------------------------------------------------

    /// Finding 2: a Claude in a *subdirectory* of a worktree (`wt/src`,
    /// not `wt` itself) must still be reported `Into` — the pre-fix
    /// equality check reported nothing for it at all.
    ///
    /// Revert check: replacing the `starts_with`/longest-match loop with
    /// the old `wt_canon != cwd_canon` equality fails this — observed:
    /// `assertion 'left == right' failed: left: [], right: [Into { .. }]`.
    /// Restored.
    #[test]
    fn a_claude_in_a_subdirectory_of_a_worktree_is_still_reported_into() {
        let (root, repo, wt) = fake_proc_with_repo_and_worktree();
        std::fs::create_dir_all(wt.join("src")).unwrap();
        mk_with_cwd(root.path(), 100, "ROOST_PROJECT=karpie\nROOST_SESSION=term3\n", &wt.join("src"));
        let moves = detect_moves(root.path(), &|_| Some(repo.clone()), &|_, _| crate::home::Owner::Here);
        assert_eq!(
            moves,
            vec![Move::Into { home: "karpie".into(), session: "term3".into(), worktree: "karpie/.claude/worktrees/feat".into() }]
        );
    }

    /// Finding 2's mirror: a re-homed Claude in a *subdirectory* of the
    /// main checkout (`main/src`, not `main` itself) must still be
    /// reported `Back` — the pre-fix equality check reported nothing.
    #[test]
    fn a_rehomed_claude_in_a_subdirectory_of_main_is_still_reported_back() {
        let (root, repo, _wt) = fake_proc_with_repo_and_worktree();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        mk_with_cwd(root.path(), 100, "ROOST_PROJECT=karpie\nROOST_SESSION=term3\n", &repo.join("src"));
        let moves = detect_moves(root.path(), &|_| Some(repo.clone()), &|_, _| {
            crate::home::Owner::Elsewhere("karpie/.claude/worktrees/feat".into())
        });
        assert_eq!(moves, vec![Move::Back { home: "karpie".into(), session: "term3".into() }]);
    }

    /// Finding 2's longest-match requirement, directly: `main` is always
    /// a prefix of its own `.claude/worktrees/<x>`, so a cwd reached
    /// *through* main's own path at `wt`'s location must still resolve to
    /// `wt`, never be mistaken for "in main" just because main's shorter
    /// canonical path also matches as a prefix.
    ///
    /// Revert check: picking the FIRST matching root instead of the
    /// LONGEST (iterating `list` with `is_main` first, as `detect_moves_in`
    /// used to before this fix, and returning on the first `starts_with`
    /// hit) fails this — observed: `assertion 'left == right' failed:
    /// left: [], right: [Into { .. }]` (main matched first, found no
    /// `Elsewhere` to report `Back` for, and never looked further for
    /// `wt`). Restored.
    #[test]
    fn a_cwd_reached_through_mains_own_path_still_resolves_to_the_worktree() {
        let (root, repo, wt) = fake_proc_with_repo_and_worktree();
        let via_main = repo.join(".claude/worktrees/feat/src");
        std::fs::create_dir_all(&via_main).unwrap();
        assert_eq!(via_main.canonicalize().unwrap(), wt.join("src").canonicalize().unwrap(), "setup: same real directory");
        mk_with_cwd(root.path(), 100, "ROOST_PROJECT=karpie\nROOST_SESSION=term3\n", &via_main);
        let moves = detect_moves(root.path(), &|_| Some(repo.clone()), &|_, _| crate::home::Owner::Here);
        assert_eq!(
            moves,
            vec![Move::Into { home: "karpie".into(), session: "term3".into(), worktree: "karpie/.claude/worktrees/feat".into() }]
        );
    }

    /// Finding 3: a terminal native to a worktree *project* — its own
    /// `ROOST_PROJECT` already names the worktree url — must never be
    /// offered a move at all. Before the fix, `home_dir_of("karpie/.claude/worktrees/feat")`
    /// resolved to the worktree's own directory, which is listed as a
    /// non-main entry of its OWN `worktree::list`, so the Claude's cwd
    /// (sitting right there) matched it and built the self-referential
    /// url `karpie/.claude/worktrees/feat/.claude/worktrees/feat`.
    ///
    /// Revert check: removing the `is_real_home` guard reproduces exactly
    /// that — observed: `assertion 'left == right' failed: left:
    /// [Into { worktree: "karpie/.claude/worktrees/feat/.claude/worktrees/feat", .. }],
    /// right: []`. Restored.
    #[test]
    fn a_terminal_native_to_a_worktree_project_is_never_offered_a_move() {
        let (root, repo, wt) = fake_proc_with_repo_and_worktree();
        mk_with_cwd(root.path(), 100, "ROOST_PROJECT=karpie/.claude/worktrees/feat\nROOST_SESSION=term3\n", &wt);
        let home_dir_of = |p: &str| match p {
            "karpie" => Some(repo.clone()),
            "karpie/.claude/worktrees/feat" => Some(wt.clone()),
            _ => None,
        };
        assert!(detect_moves(root.path(), &home_dir_of, &|_, _| crate::home::Owner::Here).is_empty());
    }

    /// R18's positive evidence: a real `.base`-less worktree removed with
    /// `git worktree remove` behind roost's back, and a stale re-home
    /// record still naming it. `detect_gone_owners` must find it only once
    /// both `resolves` (a bare stat) and git's own list agree it is gone.
    #[test]
    fn detect_gone_owners_finds_positive_evidence() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_ROOTS", root.path());
        let home = "r18-gone-home";
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(args).status().unwrap().success());
        }
        std::fs::write(home_dir.join("a.txt"), "x").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "init"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(args).status().unwrap().success());
        }
        let wt = home_dir.join(".claude/worktrees/w1");
        assert!(std::process::Command::new("git")
            .arg("-C").arg(&home_dir)
            .args(["worktree", "add", "-b", "w1", wt.to_str().unwrap()])
            .status().unwrap().success());
        let owner = format!("{home}/.claude/worktrees/w1");
        crate::home::record(home, "term3", &owner).unwrap();

        // Claude Code's own cleanup, done here by hand: the worktree is
        // gone, but roost's record does not know that yet.
        assert!(std::process::Command::new("git")
            .arg("-C").arg(&home_dir)
            .args(["worktree", "remove", wt.to_str().unwrap()])
            .status().unwrap().success());

        let home_dir_of = |p: &str| (p == home).then(|| home_dir.clone());
        assert_eq!(detect_gone_owners(&home_dir_of), vec![(home.to_string(), "term3".to_string(), owner)]);

        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// `git worktree list` not answering (a non-repository directory, here)
    /// is "cannot tell", never "gone" — the opposite of the destructive
    /// default CLAUDE.md's evidence table is about.
    #[test]
    fn detect_gone_owners_finds_nothing_when_git_cannot_answer() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let home = "r18-gitfail-home";
        let not_a_repo = tempfile::tempdir().unwrap(); // no .git at all
        crate::home::record(home, "term3", &format!("{home}/.claude/worktrees/w1")).unwrap();
        let home_dir_of = |p: &str| (p == home).then(|| not_a_repo.path().to_path_buf());
        assert!(detect_gone_owners(&home_dir_of).is_empty(), "git did not answer: no evidence either way");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// No Claudes and no records must spawn no git at all — a `home_dir_of`
    /// that panics if ever called is the proof: if either sweep spent even
    /// one git call here, the panic would fail the test.
    #[test]
    fn an_idle_host_spawns_no_git() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path()); // empty: no records anywhere
        let never_called = |_: &str| -> Option<PathBuf> { panic!("no record exists; git must never be asked") };
        assert!(detect_gone_owners(&never_called).is_empty());
        let proc_root = tempfile::tempdir().unwrap(); // empty: no Claudes
        assert!(detect_moves(proc_root.path(), &never_called, &crate::home::owner_of).is_empty());
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// `sweep_gone_owners` acts on the evidence: the record is forgotten
    /// (so the session is `Here` again, not merely displayed at home) and
    /// the pair is marked `announced`. A second sweep with the same
    /// evidence finds nothing to do — the record is already gone, so
    /// `detect_gone_owners` itself no longer names the pair, which is R18's
    /// "once per (owner, session)" guarantee holding by construction.
    ///
    /// Revert check: skipping the `home::forget` call (commenting it out)
    /// makes the second-sweep assertion fail — `owner_of` still answers
    /// `Elsewhere`, observed as `assertion failed: matches!(...,
    /// crate::home::Owner::Here)`. Restored.
    #[test]
    fn sweep_gone_owners_forgets_the_record_exactly_once() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_ROOTS", root.path());
        let home = "r18-sweep-home";
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(args).status().unwrap().success());
        }
        std::fs::write(home_dir.join("a.txt"), "x").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "init"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(args).status().unwrap().success());
        }
        let wt = home_dir.join(".claude/worktrees/w1");
        assert!(std::process::Command::new("git")
            .arg("-C").arg(&home_dir)
            .args(["worktree", "add", "-b", "w1", wt.to_str().unwrap()])
            .status().unwrap().success());
        let owner = format!("{home}/.claude/worktrees/w1");
        crate::home::record(home, "term3", &owner).unwrap();
        assert!(std::process::Command::new("git")
            .arg("-C").arg(&home_dir)
            .args(["worktree", "remove", wt.to_str().unwrap()])
            .status().unwrap().success());
        assert!(matches!(crate::home::owner_of(home, "term3"), crate::home::Owner::Elsewhere(_)), "setup");

        let home_dir_of = |p: &str| (p == home).then(|| home_dir.clone());
        let announced = Mutex::new(HashSet::new());
        sweep_gone_owners(&home_dir_of, &announced);
        assert!(matches!(crate::home::owner_of(home, "term3"), crate::home::Owner::Here), "the record must be forgotten");
        // Fix round 1, finding 1: the entry is removed again the moment
        // the record actually reads `Here` — bounding the set to only the
        // pairs genuinely stuck — so it must NOT still be in `announced`
        // after a successful forget. (Before the fix this asserted the
        // opposite; see `two_sequential_removals_with_different_owners_both_forget_and_announce`
        // for the scenario that two-field key broke.)
        assert!(
            !announced.lock().unwrap().contains(&(home.to_string(), "term3".to_string(), owner.clone())),
            "a successfully forgotten pair must not linger in the set"
        );

        // Second sweep: nothing left to find, nothing left to do.
        sweep_gone_owners(&home_dir_of, &announced);
        assert!(matches!(crate::home::owner_of(home, "term3"), crate::home::Owner::Here));

        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Fix round 1, finding 1's exact bug: term3 re-homed to worktree-x,
    /// removed, forgotten; later re-homed to worktree-y, removed too. The
    /// pre-fix two-field `(home, session)` key already had `(home, term3)`
    /// in `announced` from the first removal and skipped even *attempting*
    /// the second forget — the record stayed stuck `Elsewhere(y)` forever
    /// (not merely unannounced: nothing else revisits a pair once it is in
    /// the set). The fix makes `announced` three-field and makes the
    /// forget itself unconditional.
    ///
    /// Revert check: reverting the key back to `(home, session)` and
    /// re-introducing the `if !g.insert(key) { continue }` gate *before*
    /// the forget reproduces this exactly — see the comment at the
    /// revert site below.
    #[test]
    fn two_sequential_removals_with_different_owners_both_forget_and_announce() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_ROOTS", root.path());
        let home = "r25-tworemovals-home";
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(args).status().unwrap().success());
        }
        std::fs::write(home_dir.join("a.txt"), "x").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "init"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(args).status().unwrap().success());
        }
        let home_dir_of = |p: &str| (p == home).then(|| home_dir.clone());
        let announced = Mutex::new(HashSet::new());

        // First worktree, recorded, removed, swept: forgotten and announced.
        let wx = home_dir.join(".claude/worktrees/worktree-x");
        assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(["worktree", "add", "-b", "worktree-x", wx.to_str().unwrap()]).status().unwrap().success());
        let owner_x = format!("{home}/.claude/worktrees/worktree-x");
        crate::home::record(home, "term3", &owner_x).unwrap();
        assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(["worktree", "remove", wx.to_str().unwrap()]).status().unwrap().success());
        sweep_gone_owners(&home_dir_of, &announced);
        assert!(matches!(crate::home::owner_of(home, "term3"), crate::home::Owner::Here), "first removal must forget the record");

        // Re-homed again, to a *different* worktree, removed too.
        let wy = home_dir.join(".claude/worktrees/worktree-y");
        assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(["worktree", "add", "-b", "worktree-y", wy.to_str().unwrap()]).status().unwrap().success());
        let owner_y = format!("{home}/.claude/worktrees/worktree-y");
        crate::home::record(home, "term3", &owner_y).unwrap();
        assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(["worktree", "remove", wy.to_str().unwrap()]).status().unwrap().success());
        sweep_gone_owners(&home_dir_of, &announced);
        assert!(
            matches!(crate::home::owner_of(home, "term3"), crate::home::Owner::Here),
            "the second removal, for a DIFFERENT owner, must also be forgotten — not silently stuck Elsewhere(y)"
        );

        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// Final-review c (amends R18): the evidence names the *gone* owner,
    /// and a record re-pointed between that evidence and the forget — an
    /// approval re-homing the same terminal into a new worktree, landing
    /// mid-sweep — names a different one. Forgetting it anyway would drop a
    /// live re-home on evidence about a worktree it no longer names. Driven
    /// through `sweep_gone_owners_with`'s hook, not a timing race.
    ///
    /// Revert-checked (final fix wave): with the re-read before `forget`
    /// removed (the unconditional `home::forget` restored), fails on "a
    /// record re-pointed after the evidence was forgotten on evidence about
    /// its old owner" — the record reads `Here`.
    #[test]
    fn a_record_repointed_between_evidence_and_forget_survives() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_ROOTS", root.path());
        let home = "c-repointed-home";
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(args).status().unwrap().success());
        }
        std::fs::write(home_dir.join("a.txt"), "x").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "init"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(args).status().unwrap().success());
        }
        let git_wt = |args: &[&str]| {
            assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).arg("worktree").args(args).status().unwrap().success());
        };
        let wx = home_dir.join(".claude/worktrees/worktree-x");
        let wy = home_dir.join(".claude/worktrees/worktree-y");
        git_wt(&["add", "-q", "-b", "worktree-x", wx.to_str().unwrap()]);
        git_wt(&["add", "-q", "-b", "worktree-y", wy.to_str().unwrap()]);
        let owner_x = format!("{home}/.claude/worktrees/worktree-x");
        let owner_y = format!("{home}/.claude/worktrees/worktree-y");
        crate::home::record(home, "term3", &owner_x).unwrap();
        git_wt(&["remove", wx.to_str().unwrap()]);

        let home_dir_of = |p: &str| (p == home).then(|| home_dir.clone());
        let announced = Mutex::new(HashSet::new());
        let ran = std::cell::Cell::new(false);
        sweep_gone_owners_with(&home_dir_of, &announced, &|h, n| {
            assert_eq!((h, n), (home, "term3"));
            crate::home::record(h, n, &owner_y).unwrap();
            ran.set(true);
        });

        assert!(ran.get(), "setup: the sweep found the gone owner and reached the forget");
        assert_eq!(
            crate::home::owner_of(home, "term3"),
            crate::home::Owner::Elsewhere(owner_y.clone()),
            "a record re-pointed after the evidence was forgotten on evidence about its old owner"
        );
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// The other half of finding 1: a forget that FAILS once (the record's
    /// directory made unwritable, a real permission failure rather than an
    /// injected one) must still send the event exactly once, while the
    /// forget itself is attempted again on the very next sweep — and
    /// succeeds once the directory is writable again.
    #[test]
    fn a_failed_forget_announces_once_but_is_retried_until_it_succeeds() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", state.path());
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_ROOTS", root.path());
        let home = "r25-failedforget-home";
        let home_dir = root.path().join(home);
        std::fs::create_dir_all(&home_dir).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(args).status().unwrap().success());
        }
        std::fs::write(home_dir.join("a.txt"), "x").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "init"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(args).status().unwrap().success());
        }
        let wt = home_dir.join(".claude/worktrees/w1");
        assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(["worktree", "add", "-b", "w1", wt.to_str().unwrap()]).status().unwrap().success());
        let owner = format!("{home}/.claude/worktrees/w1");
        crate::home::record(home, "term3", &owner).unwrap();
        assert!(std::process::Command::new("git").arg("-C").arg(&home_dir).args(["worktree", "remove", wt.to_str().unwrap()]).status().unwrap().success());

        // Lock the record's own directory so `std::fs::remove_file` fails
        // with EACCES — a real forget failure, not an injected one. Read
        // access (needed by `owner_of`'s own `read_to_string`, and by
        // `all_homes`/`away_from`'s directory listing) survives; only the
        // write needed to unlink the file is denied.
        let rec_dir = crate::wsstate::state_dir().join("home").join(crate::projects::storage_key(home));
        std::fs::set_permissions(&rec_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Confirm the lock actually bites before trusting it: a write
        // that succeeds despite 0o555 means this user can bypass
        // directory permissions (root), and the rest of the test would
        // not mean what it claims to.
        let probe = rec_dir.join(".probe-write");
        let blind = std::fs::write(&probe, "x").is_err();
        if !blind {
            let _ = std::fs::remove_file(&probe);
            std::fs::set_permissions(&rec_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skipped: this user can write a 0o555 directory (likely root)");
            return;
        }

        let home_dir_of = |p: &str| (p == home).then(|| home_dir.clone());
        let announced = Mutex::new(HashSet::new());
        sweep_gone_owners(&home_dir_of, &announced);
        assert!(
            matches!(crate::home::owner_of(home, "term3"), crate::home::Owner::Elsewhere(_)),
            "the forget must have failed (directory unwritable)"
        );
        assert!(
            announced.lock().unwrap().contains(&(home.to_string(), "term3".to_string(), owner.clone())),
            "the event must be remembered as sent, so a retry does not resend it"
        );

        // Second sweep, still locked: same evidence, forget attempted
        // again (and fails again), but the event must not fire twice.
        sweep_gone_owners(&home_dir_of, &announced);
        assert!(matches!(crate::home::owner_of(home, "term3"), crate::home::Owner::Elsewhere(_)));

        // Unlock, sweep a third time: the retried forget now succeeds.
        std::fs::set_permissions(&rec_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        sweep_gone_owners(&home_dir_of, &announced);
        assert!(
            matches!(crate::home::owner_of(home, "term3"), crate::home::Owner::Here),
            "the forget must be retried, and must succeed once the directory is writable again"
        );

        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    // --- §1: a terminal moves only when its Claude does (#158) ------------

    type Rehomes = std::cell::RefCell<Vec<(String, String, String)>>;
    type Broadcasts = std::cell::RefCell<Vec<(String, crate::proto::Event)>>;

    const WX: &str = "h/.claude/worktrees/worktree-x";
    const WY: &str = "h/.claude/worktrees/worktree-y";

    fn into(worktree: &str) -> Move {
        Move::Into { home: "h".into(), session: "term3".into(), worktree: worktree.into() }
    }

    fn expecting(url: &str, at: std::time::Instant) -> Expectations {
        let m = Mutex::new(HashMap::new());
        expect_move_in(&m, "h", "term3", url, at);
        m
    }

    /// Runs one `offer_moves_at` with a rehome that records and answers
    /// `answer`, and a broadcaster that records.
    fn run(
        moves: &[Move],
        last: &Mutex<HashMap<(String, String), Option<String>>>,
        expect: &Expectations,
        now: std::time::Instant,
        answer: Result<(), String>,
    ) -> (Vec<(String, String, String)>, Vec<(String, crate::proto::Event)>) {
        let rehomes: Rehomes = Default::default();
        let sent: Broadcasts = Default::default();
        offer_moves_at(
            moves,
            last,
            expect,
            now,
            &|h, s, o| {
                rehomes.borrow_mut().push((h.to_string(), s.to_string(), o.to_string()));
                answer.clone()
            },
            &|p, ev| sent.borrow_mut().push((p.to_string(), ev.clone())),
        );
        (rehomes.into_inner(), sent.into_inner())
    }

    fn is_offer_of(sent: &[(String, crate::proto::Event)], worktree: &str) -> bool {
        sent.iter().any(|(p, ev)| {
            p == "h"
                && matches!(ev, crate::proto::Event::ClaudeElsewhere { session, worktree: Some(w) }
                    if session == "term3" && w == worktree)
        })
    }

    /// The approved move landing: the watcher re-homes on its own, offers
    /// nothing, and the expectation is spent.
    ///
    /// Revert-checked: dropping the `last` insert on `Ok` fails the last
    /// assertion — observed: `left: None, right: Some(Some("h/.claude/
    /// worktrees/worktree-x"))`. Restored.
    #[test]
    fn an_expected_move_rehomes_without_an_offer() {
        let t0 = std::time::Instant::now();
        let last = Mutex::new(HashMap::new());
        let expect = expecting(WX, t0);
        let (rehomes, sent) = run(&[into(WX)], &last, &expect, t0 + std::time::Duration::from_secs(3), Ok(()));
        assert_eq!(rehomes, vec![("h".to_string(), "term3".to_string(), WX.to_string())], "re-homed once, to x");
        assert!(sent.is_empty(), "no ClaudeElsewhere offer for an approved move: {sent:?}");
        assert!(expect.lock().unwrap().is_empty(), "the expectation is consumed");
        assert_eq!(
            last.lock().unwrap().get(&("h".to_string(), "term3".to_string())),
            Some(&Some(WX.to_string())),
            "recorded as seen, so a later tick does not offer it either"
        );
    }

    /// The tick after a successful re-home. `detect_moves_in` normally
    /// yields nothing then (the record reads `Elsewhere(x)` —
    /// `an_already_recorded_move_is_not_reported_again`), but should the
    /// same `Into` be seen once more (a record write `owner_of` cannot read
    /// back yet), it must be neither re-homed nor offered.
    ///
    /// Revert-checked with the `last` insert on `Ok` dropped — observed:
    /// `no offer for a move already taken: [("h", ClaudeElsewhere {
    /// session: "term3", worktree: Some(".../worktree-x") })]`. Restored.
    #[test]
    fn a_rehomed_move_seen_again_is_neither_offered_nor_moved_again() {
        let t0 = std::time::Instant::now();
        let last = Mutex::new(HashMap::new());
        let expect = expecting(WX, t0);
        let _ = run(&[into(WX)], &last, &expect, t0 + std::time::Duration::from_secs(3), Ok(()));
        let (rehomes, sent) = run(&[into(WX)], &last, &expect, t0 + std::time::Duration::from_secs(6), Ok(()));
        assert!(rehomes.is_empty(), "the expectation was spent: {rehomes:?}");
        assert!(sent.is_empty(), "no offer for a move already taken: {sent:?}");
    }

    /// The subagent case: approval left an expectation, but the parent
    /// Claude never moved. Past the window it is gone and nothing happened.
    ///
    /// Revert-checked with the window `retain` made to keep everything —
    /// observed: `the expired expectation is removed`. Restored.
    #[test]
    fn an_expectation_without_a_move_expires_and_moves_nothing() {
        let t0 = std::time::Instant::now();
        let last = Mutex::new(HashMap::new());
        let expect = expecting(WX, t0);
        // Inside the window, with no move: kept.
        let (rehomes, sent) = run(&[], &last, &expect, t0 + std::time::Duration::from_secs(29), Ok(()));
        assert!(rehomes.is_empty() && sent.is_empty());
        assert_eq!(expect.lock().unwrap().len(), 1, "still fresh at 29 s");
        // Past it: swept.
        let (rehomes, sent) = run(&[], &last, &expect, t0 + std::time::Duration::from_secs(31), Ok(()));
        assert!(rehomes.is_empty(), "nothing re-homed: {rehomes:?}");
        assert!(sent.is_empty(), "nothing offered: {sent:?}");
        assert!(expect.lock().unwrap().is_empty(), "the expired expectation is removed");
        assert!(last.lock().unwrap().is_empty(), "and nothing recorded as offered");
    }

    /// Revert-checked (brief step 5b): with the window `retain` made to keep
    /// everything, fails — observed: `too late to be the approved move:
    /// [("h", "term3", "h/.claude/worktrees/worktree-x")]`. Restored.
    #[test]
    fn a_late_move_gets_the_offer_not_an_automatic_move() {
        let t0 = std::time::Instant::now();
        let last = Mutex::new(HashMap::new());
        let expect = expecting(WX, t0);
        let (rehomes, sent) = run(&[into(WX)], &last, &expect, t0 + std::time::Duration::from_secs(31), Ok(()));
        assert!(rehomes.is_empty(), "too late to be the approved move: {rehomes:?}");
        assert!(is_offer_of(&sent, WX), "the existing offer instead: {sent:?}");
        assert!(expect.lock().unwrap().is_empty(), "expired");
    }

    /// Revert-checked (brief step 5c): with the URL equality dropped (any
    /// expectation for the key qualifies), fails — observed: `y was never
    /// approved: [("h", "term3", "h/.claude/worktrees/worktree-y")]`; so
    /// does `a_newer_expectation_replaces_the_older` (`x was superseded`).
    /// Restored.
    #[test]
    fn an_expected_move_into_a_different_worktree_is_offered_not_taken() {
        let t0 = std::time::Instant::now();
        let last = Mutex::new(HashMap::new());
        let expect = expecting(WX, t0);
        let (rehomes, sent) = run(&[into(WY)], &last, &expect, t0 + std::time::Duration::from_secs(3), Ok(()));
        assert!(rehomes.is_empty(), "y was never approved: {rehomes:?}");
        assert!(is_offer_of(&sent, WY), "offered y: {sent:?}");
        assert_eq!(
            expect.lock().unwrap().get(&("h".to_string(), "term3".to_string())).map(|e| e.url.as_str()),
            Some(WX),
            "the expectation for x is untouched"
        );
    }

    /// One entry per `(home, session)`, and the newest approval wins: a
    /// Claude asked twice is expected in the second worktree, not the first.
    ///
    /// Revert-checked: `expect_move_in` keeping the older entry
    /// (`entry(..).or_insert(..)` instead of `insert`) fails — observed:
    /// `x was superseded: [("h", "term3", "h/.claude/worktrees/worktree-x")]`.
    /// Restored.
    #[test]
    fn a_newer_expectation_replaces_the_older() {
        let t0 = std::time::Instant::now();
        let last = Mutex::new(HashMap::new());
        let expect = Mutex::new(HashMap::new());
        expect_move_in(&expect, "h", "term3", WX, t0);
        expect_move_in(&expect, "h", "term3", WY, t0 + std::time::Duration::from_secs(1));
        assert_eq!(expect.lock().unwrap().len(), 1, "one entry per (home, session)");
        // The older approval's worktree is no longer the expected one.
        let (rehomes, sent) = run(&[into(WX)], &last, &expect, t0 + std::time::Duration::from_secs(2), Ok(()));
        assert!(rehomes.is_empty(), "x was superseded: {rehomes:?}");
        assert!(is_offer_of(&sent, WX), "{sent:?}");
        let (rehomes, sent) = run(&[into(WY)], &last, &expect, t0 + std::time::Duration::from_secs(3), Ok(()));
        assert_eq!(rehomes, vec![("h".to_string(), "term3".to_string(), WY.to_string())], "re-homed to y");
        assert!(sent.is_empty(), "{sent:?}");
        assert!(expect.lock().unwrap().is_empty());
    }

    /// Pins *existing* behaviour: with no expectation at all (a restart
    /// emptied the in-memory map), an `Into` is the ordinary offer it was
    /// before #158. No revert of the new expectation code can fail this —
    /// with no entry, that branch never runs. What it guards is the offer
    /// path itself surviving the refactor into `offer_moves_at`.
    ///
    /// Revert-checked against that existing code: disabling the `Into` arm's
    /// `broadcast(home, &ClaudeElsewhere { .. })` (`if changed && false`)
    /// fails the `is_offer_of` assertion — observed: panicked with the
    /// recorded broadcasts `[]`. Restored.
    #[test]
    fn an_expectation_lost_to_a_restart_falls_back_to_the_offer() {
        let t0 = std::time::Instant::now();
        let last = Mutex::new(HashMap::new());
        let expect = Mutex::new(HashMap::new());
        let (rehomes, sent) = run(&[into(WX)], &last, &expect, t0, Ok(()));
        assert!(rehomes.is_empty(), "{rehomes:?}");
        assert!(is_offer_of(&sent, WX), "{sent:?}");
    }

    /// Spec §1, strictly: once R16 refuses, the automatic path for that
    /// expectation is over. The same move seen again — even with the
    /// collision gone, so `rehome` would now succeed — is never re-homed;
    /// it stays an offer, sent once (the `last` dedupe), and the entry
    /// lingers until the sweep expires it.
    ///
    /// Revert-checked: dropping `!e.refused` from the expected check fails
    /// — observed: `retried after R16 refused: [("h", "term3",
    /// "h/.claude/worktrees/worktree-x"), (same again)]; left: 2, right: 1`.
    /// Restored.
    #[test]
    fn a_refused_expectation_is_not_retried() {
        let t0 = std::time::Instant::now();
        let last = Mutex::new(HashMap::new());
        let expect = expecting(WX, t0);
        let secs = std::time::Duration::from_secs;
        let (r1, s1) = run(&[into(WX)], &last, &expect, t0 + secs(3), Err(format!("term3 already exists in {WX}")));
        let (r2, s2) = run(&[into(WX)], &last, &expect, t0 + secs(6), Ok(()));
        let calls: Vec<_> = r1.iter().chain(r2.iter()).cloned().collect();
        assert_eq!(calls.len(), 1, "retried after R16 refused: {calls:?}");
        let offers = s1.iter().chain(s2.iter()).filter(|(p, _)| p == "h").count();
        assert!(is_offer_of(&s1, WX), "offered on the refusal: {s1:?}");
        assert_eq!(offers, 1, "offered once, not every tick: {s1:?} {s2:?}");
        assert_eq!(expect.lock().unwrap().len(), 1, "kept, refused, until it expires");
        let _ = run(&[], &last, &expect, t0 + secs(30), Ok(()));
        assert_eq!(expect.lock().unwrap().len(), 1, "still there at 30 s");
        let _ = run(&[], &last, &expect, t0 + secs(31), Ok(()));
        assert!(expect.lock().unwrap().is_empty(), "swept at 31 s");
    }

    /// Spec §1: R16 refusing leaves the expectation to expire on its own,
    /// and the existing offer takes over.
    ///
    /// Revert-checked: an `Err` arm that drops the expectation and skips
    /// the offer fails — observed: `refused, so the offer: []`. Restored.
    #[test]
    fn a_refused_expected_move_keeps_the_expectation_and_offers() {
        let t0 = std::time::Instant::now();
        let last = Mutex::new(HashMap::new());
        let expect = expecting(WX, t0);
        let (rehomes, sent) = run(
            &[into(WX)],
            &last,
            &expect,
            t0 + std::time::Duration::from_secs(3),
            Err(format!("term3 already exists in {WX}")),
        );
        assert_eq!(rehomes.len(), 1, "the re-home was attempted: {rehomes:?}");
        assert!(is_offer_of(&sent, WX), "refused, so the offer: {sent:?}");
        assert_eq!(expect.lock().unwrap().len(), 1, "the expectation is kept until it expires");
    }
}
