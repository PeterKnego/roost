//! The project registry: what deadlight knows about, and what is running.
//!
//! Rebuilt at startup rather than accumulated in memory, because dtach
//! sessions deliberately outlive deadlight. An in-memory-only registry would
//! forget every running shell on restart — which is exactly how nine
//! orphaned sessions for deleted directories accumulated unnoticed in
//! production on 2026-08-17.
use std::path::PathBuf;

pub struct ProjectStatus {
    /// Storage key, percent-encoded (`karpie%2Fsrc`).
    pub key: String,
    /// URL form, readable slashes (`karpie/src`).
    pub url: String,
    pub live: usize,
    pub oldest_age_secs: u64,
    pub has_layout: bool,
    /// Branch name: the repo's own current branch for a plain/main-worktree
    /// entry, or a linked worktree's branch. Empty when nothing has ever
    /// asked git (a project entry that wasn't found to be a git repo).
    pub branch: String,
    /// The parent project's storage key, for a worktree grouped under the
    /// repository it belongs to. `None` for a main worktree or a plain
    /// project — the two render identically at the top level.
    pub parent: Option<String>,
    /// False only for a worktree git reports that does not canonicalise
    /// under any of ROOTS — real (git vouches for it existing), but outside
    /// the confinement boundary, so it is rendered dimmed and unclickable
    /// rather than silently omitted. Always true for entries not discovered
    /// via worktree grouping.
    pub reachable: bool,
}

pub struct ReapReport {
    pub dead_sockets: usize,
    pub gone_projects: usize,
}

/// Inverse of the storage-key encoding used by `wsstate` and `session`. Not
/// `http::percent_decode` — see `projects::decode_storage_key`'s doc comment
/// for why that general form-decoder is the wrong inverse for this specific
/// encoding (it would mangle a project literally named `gtk+`).
pub fn decode_key(key: &str) -> String {
    crate::projects::decode_storage_key(key)
}

fn sock_root() -> PathBuf {
    crate::wsstate::state_dir().join("sock")
}

/// One snapshot of every process's pid and its raw command-line text, taken
/// with a single `ps` call. Passed around so that sweeping every socket
/// under `sock/` to see which are still held costs one subprocess total, not
/// one per socket. Deliberately not `pgrep -f`: that treats its pattern as
/// an extended regex matched anywhere in the command line, which is wrong
/// two different ways for a filesystem path — (1) a project name may contain
/// regex metacharacters (`valid_project` allows them; only *session* names
/// are restricted to `[A-Za-z0-9_-]`), which can make the "pattern" an
/// invalid ERE and silently match nothing, so a live socket's process looks
/// dead; and (2) unanchored substring matching means session `claude`'s
/// socket path matches inside session `claude-2`'s command line too, so an
/// actually-dead `claude` socket is never reaped once `claude-2` exists.
/// `pids_holding` matches each raw line as plain text (no regex ever built
/// from untrusted input) and requires the match be a whole argument, which
/// avoids both hazards without needing to split the line into words first —
/// splitting on whitespace would break on a socket path that itself contains
/// a space, which a project directory name may (`list_projects`,
/// `resolve_project`, and `valid_project` all permit spaces; only *session*
/// names are restricted).
fn process_snapshot() -> Vec<(u32, String)> {
    let Ok(out) = std::process::Command::new("ps").args(["-Ao", "pid=,args="]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let (pid_str, args) = trimmed.split_once(char::is_whitespace)?;
            let pid: u32 = pid_str.parse().ok()?;
            Some((pid, args.trim_start().to_string()))
        })
        .collect()
}

/// True when `target` appears in `line` as one whole command-line argument:
/// bounded by whitespace, or the line's own start/end, on both sides — never
/// as a fragment of a longer word, but with any internal spaces `target`
/// itself contains left intact. Plain substring matching alone (a bare
/// `line.contains(target)`) would have the same prefix-collision hazard as
/// `pgrep -f` (`claude` inside `claude-2`); splitting the line into words
/// first (`==` against each) would instead break on a `target` that itself
/// contains a space, silently treating a live socket as unheld — see
/// `process_snapshot`'s doc comment for why that matters here. This is the
/// middle path: a bounded substring search.
fn line_has_whole_arg(line: &str, target: &str) -> bool {
    if target.is_empty() {
        return false;
    }
    line.match_indices(target).any(|(start, matched)| {
        let end = start + matched.len();
        let before_ok = start == 0 || line.as_bytes()[start - 1].is_ascii_whitespace();
        let after_ok = end == line.len() || line.as_bytes()[end].is_ascii_whitespace();
        before_ok && after_ok
    })
}

/// Pids in `snapshot` whose command line has this socket path as one whole
/// argument (see `line_has_whole_arg`). `dtach` always carries the socket
/// path as a single, whole argument (see `session::default_command`) —
/// never embedded in a larger word.
fn pids_holding(snapshot: &[(u32, String)], path: &std::path::Path) -> Vec<u32> {
    let target = path.to_string_lossy();
    snapshot.iter().filter(|(_, line)| line_has_whole_arg(line, &target)).map(|(pid, _)| *pid).collect()
}

/// True when some live process holds this socket path right now. Takes its
/// own fresh snapshot — used for one-off checks (tests, and re-polling after
/// a kill to see whether it has taken effect yet), where the whole point is
/// to observe current state rather than a sweep's frozen-in-time one.
fn socket_has_process(path: &std::path::Path) -> bool {
    !pids_holding(&process_snapshot(), path).is_empty()
}

/// Kills every process currently holding `sock_path`, waits (bounded) for
/// them to die, and removes the socket file only once none remain. Leaves
/// the file in place if a kill fails or a process survives the wait, so the
/// session stays discoverable and reapable by a later pass rather than
/// becoming an unreachable orphan (its socket gone, so nothing — not a
/// browser, not `reconcile` itself — can ever find or attach to it again).
///
/// Shared by `reconcile`'s project-gone branch and `session::kill_project`
/// (an explicit "Close Project" from the UI): both need to end whatever
/// `dtach` *master* — and, through it, the user's shell — is still holding
/// a socket, not merely the in-process *client* deadlight itself spawned.
/// Killing only that client is just a detach: in `-A` mode `dtach` forks a
/// master that immediately detaches and reparents to init, so the master
/// and shell survive a client kill with a live socket, unreachable and
/// (without this) invisibly still running. Both call sites need the
/// identical confirm-before-unlink safety property, so they share this one
/// implementation rather than two that can drift.
///
/// Returns `true` once the socket ends up fully cleaned up — no process
/// left, file removed — including the vacuous case where nothing held it to
/// begin with. Returns `false` if a kill failed or a process survived the
/// wait.
pub fn kill_and_unlink(sock_path: &std::path::Path) -> bool {
    let pids = pids_holding(&process_snapshot(), sock_path);
    for pid in &pids {
        let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
    }
    // Bounded wait for the kill(s) to take effect rather than an instant
    // recheck: SIGKILL is not synchronous. Bounded so a process that
    // refuses to die can't hang the caller forever. When `pids` was empty
    // this loop runs zero iterations, and `still_alive`'s first read is
    // itself the fresh, immediately-before-delete recheck (see R8 in this
    // module's history: a stale "nothing held it" from an earlier snapshot
    // must never be trusted at delete time without re-verifying).
    let mut still_alive = socket_has_process(sock_path);
    for _ in 0..20 {
        if !still_alive {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
        still_alive = socket_has_process(sock_path);
    }
    if still_alive {
        return false;
    }
    let _ = std::fs::remove_file(sock_path);
    true
}

/// What to do with one socket file, given what's known about it. Kept as a
/// pure function, separate from the `kill`/`pgrep`/`remove_file` calls that
/// gather its inputs, so every combination — including "a kill attempt
/// failed and the process survived" — can be exercised in a fast, ordinary
/// unit test rather than only via a real process that may or may not
/// actually die on schedule.
#[derive(Debug, PartialEq, Eq)]
enum ReapAction {
    /// Nothing holds the socket; always safe to remove.
    RemoveDeadSocket,
    /// The project is gone, a kill was attempted, and no process still
    /// holds the socket: safe to remove.
    RemoveKilled,
    /// A live process, and either the project still exists, or (project
    /// gone) a kill attempt failed and something still holds the socket.
    /// Leave the file in place: a socket that still exists remains
    /// discoverable by the next `reconcile`, so leaving it is strictly
    /// safer than deleting the only path back to an orphaned process.
    Leave,
}

fn reap_decision(project_gone: bool, held_before: bool, held_after_kill_attempt: bool) -> ReapAction {
    if !held_before {
        return ReapAction::RemoveDeadSocket;
    }
    if !project_gone {
        return ReapAction::Leave;
    }
    if held_after_kill_attempt {
        ReapAction::Leave
    } else {
        ReapAction::RemoveKilled
    }
}

/// Removes sockets whose process is gone and sessions whose project directory
/// no longer exists. Runs at startup and on every enumeration, so orphans
/// cannot accumulate silently the way they did before this existed.
///
/// Deliberately narrow about what it deletes: only a dead socket file (no
/// process holds it — dtach itself died and left it behind) or a live
/// session's socket file when its project directory is gone *and* the
/// process(es) holding that socket were actually killed and confirmed gone.
/// It never touches a saved-layout state file (`wsstate`'s `<key>.json`): a
/// layout for a project that has moved is still the user's data, and losing
/// unsaved buffer text embedded in it would be unrecoverable. A moved-back or
/// re-created project directory simply picks its layout back up.
///
/// The kill-then-confirm step for the project-gone case matters because this
/// function's primary caller is startup: `session::kill_project` only ends
/// sessions present in *this* process's in-memory map, which is empty right
/// after a restart. A `dtach` session that outlived the previous deadlight
/// process has no map entry, so without directly signalling its pid, the
/// socket file would be deleted while the process kept running — a shell
/// that is now both unreachable (no socket to attach through) and
/// unreapable (no socket for the next `reconcile` to find it by). That is
/// worse than the orphan accumulation this function exists to fix, so the
/// socket is removed only once the pid(s) are confirmed dead; if a kill
/// fails or a process survives, the socket is left in place on purpose.
///
/// "Cannot verify whether a project exists" must never be treated as
/// "the project is gone": `resolve_project` returns `None` for every
/// project alike when a root fails to canonicalize (a mount not up yet, a
/// permissions problem, a `DEADLIGHT_ROOTS` typo, or just running on a host
/// where a configured root doesn't exist there). Without a guard, that
/// transient condition would make this function SIGKILL every live session
/// on the machine — strictly worse than doing nothing, for a tool whose
/// whole premise is that sessions survive a restart. So the project-gone
/// branch only ever runs once *every* configured root is confirmed
/// reachable — not merely one of them (the default deploy config ships two
/// roots; if only one of them is a mount that isn't up, every session under
/// the down root must not be declared gone just because the other root
/// resolved fine) — otherwise every socket is left alone (dead-socket
/// reaping, which needs no project information, still runs).
pub fn reconcile(roots: &[PathBuf]) -> ReapReport {
    let mut report = ReapReport { dead_sockets: 0, gone_projects: 0 };
    let roots_ok = !roots.is_empty() && roots.iter().all(|r| r.canonicalize().is_ok());
    if !roots_ok {
        eprintln!(
            "deadlight: none of the configured roots could be read ({roots:?}) — \
             reaping sessions for missing projects is suspended this pass"
        );
    }
    let Ok(rd) = std::fs::read_dir(sock_root()) else { return report };
    // One process listing for the whole sweep (see process_snapshot's doc
    // comment) rather than one `ps` per socket file.
    let snapshot = process_snapshot();
    for entry in rd.flatten() {
        // Directory entry names under sock/ are storage keys (percent-encoded:
        // `karpie%2Fsrc`), not the raw project form. `session`'s functions
        // take the raw form and encode internally via storage_key to build
        // their map key, so passing the encoded `key` straight through would
        // double-encode a nested project's `%` and match nothing — the exact
        // invisibility bug this registry exists to prevent. `url` (the
        // decoded, raw form) is what every `session::*` call below must use.
        let key = entry.file_name().to_string_lossy().into_owned();
        let url = decode_key(&key);
        let project_gone = roots_ok && crate::projects::resolve_project(roots, &url).is_none();
        // In-process sessions first, once per project rather than once per
        // socket file below: `session::kill_project` now ends each session's
        // dtach master too (via `kill_and_unlink`, the same helper the loop
        // below uses), not just deadlight's own client, so any socket it
        // fully cleans up is already gone by the time `read_dir` runs and
        // simply won't appear in `inner` — only a genuine failure (or a
        // session this process never knew about, e.g. one that outlived a
        // restart) is left for the per-socket loop to handle. `ended` is
        // only ever incremented for a session actually confirmed dead, so
        // it's safe to fold straight into the report here.
        if project_gone {
            let in_process_ended = crate::session::kill_project(&url);
            report.gone_projects += in_process_ended;
            if in_process_ended > 0 {
                eprintln!(
                    "deadlight: reaped {in_process_ended} in-process session(s) for {key} — project directory is gone"
                );
            }
        }
        let Ok(inner) = std::fs::read_dir(entry.path()) else { continue };
        for sock in inner.flatten() {
            let pids = pids_holding(&snapshot, &sock.path());
            let held_before = !pids.is_empty();
            let name = sock.file_name().to_string_lossy().into_owned();

            if held_before && project_gone {
                // Covers a session this process never had in memory (one
                // that outlived a restart) as well as a retry for one
                // `kill_project` above tried and failed to fully end.
                if kill_and_unlink(&sock.path()) {
                    report.gone_projects += 1;
                    eprintln!(
                        "deadlight: reaped session {key}/{name} — project directory is gone (pids {pids:?})"
                    );
                } else {
                    eprintln!(
                        "deadlight: could not end session {key}/{name} for a missing project — pids {pids:?} did not fully die; socket left in place so it stays discoverable"
                    );
                }
                continue;
            }

            match reap_decision(project_gone, held_before, held_before) {
                ReapAction::RemoveDeadSocket => {
                    // `held_before` came from `snapshot`, taken once before
                    // this whole sweep for efficiency — by the time this
                    // particular socket is reached (especially after another
                    // socket's kill-confirmation sleep earlier in the same
                    // pass) it can be stale, and a new process may have
                    // attached since. Re-check right now, immediately before
                    // deleting, rather than trust the sweep's frozen-in-time
                    // view: a socket that survives one sweep costs nothing,
                    // but a live one deleted out from under a fresh session
                    // is unrecoverable. This matters once this runs on more
                    // than just startup — `known_projects` (below) already
                    // calls `reconcile` on every enumeration, by design.
                    if !socket_has_process(&sock.path()) {
                        let _ = std::fs::remove_file(sock.path());
                        report.dead_sockets += 1;
                        eprintln!("deadlight: reaped dead socket {}", sock.path().display());
                    }
                }
                ReapAction::Leave => {} // live process, project still exists: untouched
                ReapAction::RemoveKilled => unreachable!("only reached via the project_gone branch above"),
            }
        }
        // An emptied directory is noise; ignore failure when it is not empty
        // (a live session for a project that still exists must survive).
        let _ = std::fs::remove_dir(entry.path());
    }
    report
}

// `known_projects` runs `reconcile` on every call, and it is itself called on
// every picker load, every `?at=` browse, every workspace load, and every
// header-strip refresh (routes.rs) — while lib.rs spawns one thread per
// connection, so two of those can land concurrently. Unguarded, that means:
// (a) two sweeps of sock/ racing their own kill/remove decisions against each
// other, and (b) a project whose process ignores SIGKILL (`ReapAction::Leave`
// — the socket is deliberately left in place) paying reconcile's ~500ms
// bounded confirmation poll again on every single subsequent load, forever,
// since nothing remembers that this pass already tried and failed. The mutex
// below serializes sweeps; the minimum interval, checked and updated while
// still holding that same lock, skips a sweep entirely when the last one
// finished recently enough that its result is still trustworthy — which
// also means a second thread that loses the race to the first simply finds
// "too soon" once it gets the lock, rather than duplicating the work.
//
// This throttling lives in a wrapper around `reconcile`, not inside
// `reconcile` itself: startup (lib.rs) and the test suite both call
// `reconcile` directly and need every call to actually run, unthrottled.
const RECONCILE_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

fn reconcile_gate() -> &'static std::sync::Mutex<Option<std::time::Instant>> {
    static GATE: std::sync::OnceLock<std::sync::Mutex<Option<std::time::Instant>>> =
        std::sync::OnceLock::new();
    GATE.get_or_init(|| std::sync::Mutex::new(None))
}

/// Pure timing decision, factored out so it can be unit-tested with
/// constructed `Instant`s rather than real sleeps.
fn should_reconcile(last: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    match last {
        None => true,
        Some(t) => now.duration_since(t) >= RECONCILE_MIN_INTERVAL,
    }
}

fn reconcile_throttled(roots: &[PathBuf]) {
    // A poisoned lock (a prior sweep panicked mid-sweep while holding it)
    // must not take every future request down with it — recovering and
    // proceeding is strictly safer than propagating the panic onto this
    // socket thread.
    let mut last = reconcile_gate().lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    if should_reconcile(*last, now) {
        let _ = reconcile(roots);
        *last = Some(now);
    }
}

/// Every project deadlight knows about: those with a saved layout, those with
/// live sessions, and those with both.
pub fn known_projects(roots: &[PathBuf]) -> Vec<ProjectStatus> {
    reconcile_throttled(roots);
    let mut by_key: std::collections::BTreeMap<String, ProjectStatus> = Default::default();

    if let Ok(rd) = std::fs::read_dir(crate::wsstate::state_dir()) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(key) = name.strip_suffix(".json") else { continue };
            by_key.insert(
                key.to_string(),
                ProjectStatus {
                    key: key.to_string(),
                    url: decode_key(key),
                    live: 0,
                    oldest_age_secs: 0,
                    has_layout: true,
                    branch: String::new(),
                    parent: None,
                    reachable: true,
                },
            );
        }
    }

    if let Ok(rd) = std::fs::read_dir(sock_root()) {
        for e in rd.flatten() {
            // `key` is the encoded directory name; `session::list_sessions`
            // takes the raw form and encodes internally, so it must be
            // decoded here too, or a nested project's sessions never match
            // and the project shows idle while shells are actually running.
            let key = e.file_name().to_string_lossy().into_owned();
            let sessions = crate::session::list_sessions(&decode_key(&key));
            // `reconcile` (just run, above) guarantees every socket file
            // still here is process-backed: a socket with no process is
            // reaped outright, and a live one for a since-deleted project is
            // killed before its socket is removed. So right after a
            // restart — when this process's in-memory session map is empty
            // and `list_sessions` can only ever report 0 — the number of
            // socket files under this key is a truthful floor for "how many
            // are live", even though it can't yet supply real ages or
            // attachment counts. Once something in this process actually
            // attaches, the in-memory list becomes authoritative again.
            let (live, oldest) = if !sessions.is_empty() {
                (sessions.len(), sessions.iter().map(|s| s.age_secs).max().unwrap_or(0))
            } else {
                let floor = std::fs::read_dir(e.path()).map(|rd| rd.flatten().count()).unwrap_or(0);
                (floor, 0)
            };
            let slot = by_key.entry(key.clone()).or_insert(ProjectStatus {
                key: key.clone(),
                url: decode_key(&key),
                live: 0,
                oldest_age_secs: 0,
                has_layout: false,
                branch: String::new(),
                parent: None,
                reachable: true,
            });
            slot.live = live;
            slot.oldest_age_secs = oldest;
        }
    }

    group_worktrees(roots, &mut by_key);
    order_with_children(by_key.into_values().collect())
}

/// For every already-known entry that is a git repository, asks git for its
/// worktrees and folds the answer in: the entry's own `branch` (from the main
/// worktree record) and, for each linked worktree, either an existing or a
/// freshly-created child entry carrying `branch` and `parent`.
///
/// Deliberately narrow about when it pays the `git worktree list` subprocess
/// cost: only entries already in `by_key` (i.e. already being displayed —
/// has a saved layout or a live session) and only once `.git` confirms the
/// directory is actually a repository. That check is `.is_dir()`, not merely
/// `.exists()`: a *linked* worktree's `.git` is a file (a pointer back to the
/// real repo's gitdir), not a directory, so `.exists()` alone would treat an
/// already-known worktree as a repository in its own right, run `git
/// worktree list` from inside it (reporting the same records, main first),
/// and parent it to itself — `order_with_children` then can't find a
/// matching parent row and strands it at the end of the strip. A plain
/// project, or one that hasn't resolved, is skipped outright.
fn group_worktrees(roots: &[PathBuf], by_key: &mut std::collections::BTreeMap<String, ProjectStatus>) {
    let parent_keys: Vec<String> = by_key.keys().cloned().collect();
    for key in parent_keys {
        let url = by_key[&key].url.clone();
        let Some(dir) = crate::projects::resolve_project(roots, &url) else { continue };
        if !dir.join(".git").is_dir() {
            continue;
        }
        for w in crate::worktree::list(&dir) {
            if w.is_main {
                // The repo's own entry is the main worktree; its branch is
                // git's branch, not a separate row.
                if let Some(slot) = by_key.get_mut(&key) {
                    slot.branch = w.branch;
                }
                continue;
            }
            match rel_under_roots(roots, &w.path) {
                Some(child_url) => {
                    let child_key = crate::projects::storage_key(&child_url);
                    // Belt-and-braces: the `.is_dir()` gate above should
                    // already make a repo's own key impossible to reach here
                    // as `child_key`, but a worktree can never sanely be its
                    // own parent, so refuse it outright rather than trust
                    // that gate alone.
                    if child_key == key {
                        continue;
                    }
                    let slot = by_key.entry(child_key.clone()).or_insert(ProjectStatus {
                        key: child_key,
                        url: child_url,
                        live: 0,
                        oldest_age_secs: 0,
                        has_layout: false,
                        branch: String::new(),
                        parent: None,
                        reachable: true,
                    });
                    slot.branch = w.branch;
                    slot.parent = Some(key.clone());
                }
                None => {
                    // git reports this worktree, but its path canonicalises
                    // under no ROOT — a repository's worktree metadata lives
                    // *inside* the repo, so a cloned repo could point one
                    // anywhere. Confinement (not the dot-segment allowlist)
                    // is what keeps it from ever being opened; it is still
                    // shown, dimmed and unclickable, rather than dropped, so
                    // the user isn't left wondering where it went.
                    let raw = w.path.to_string_lossy().into_owned();
                    let child_key = crate::projects::storage_key(&raw);
                    by_key.entry(child_key.clone()).or_insert(ProjectStatus {
                        key: child_key,
                        url: raw,
                        live: 0,
                        oldest_age_secs: 0,
                        has_layout: false,
                        branch: w.branch,
                        parent: Some(key.clone()),
                        reachable: false,
                    });
                }
            }
        }
    }
}

/// `w.path` (git's own, absolute report) rewritten as a rel path from one of
/// `roots`, the same form every other `ProjectStatus.url` uses — or `None`
/// when it canonicalises under none of them. Not `projects::resolve_project`:
/// that takes the rel *string* and re-derives the path; here it's the reverse
/// direction, an already-known absolute path being turned back into a rel
/// string, and reusing resolve_project would mean re-parsing a path we
/// already have just to throw the parsed form away.
fn rel_under_roots(roots: &[PathBuf], path: &std::path::Path) -> Option<String> {
    let canon = path.canonicalize().ok()?;
    for root in roots {
        // One root failing to canonicalize (unreadable, doesn't exist here)
        // must not abandon the whole search — `?` would bail out of this
        // function on the first bad root even if a later one matches, the
        // same hazard `confined_path` avoids with `continue`.
        let Ok(base) = root.canonicalize() else { continue };
        if let Ok(rel) = canon.strip_prefix(&base) {
            if !rel.as_os_str().is_empty() {
                return Some(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    None
}

/// Orders entries so a parent (`parent: None`) is immediately followed by its
/// children (`parent: Some(parent_key)`), which plain key order can't
/// guarantee — a child worktree's storage key has no relationship to its
/// parent's. Top-level entries keep key order; children of one parent sort
/// by branch.
fn order_with_children(mut items: Vec<ProjectStatus>) -> Vec<ProjectStatus> {
    items.sort_by(|a, b| a.key.cmp(&b.key));
    let mut children: std::collections::HashMap<String, Vec<ProjectStatus>> = Default::default();
    let mut parents = Vec::new();
    for item in items {
        match &item.parent {
            Some(pk) => children.entry(pk.clone()).or_default().push(item),
            None => parents.push(item),
        }
    }
    let mut out = Vec::with_capacity(parents.len());
    for parent in parents {
        let key = parent.key.clone();
        out.push(parent);
        if let Some(mut kids) = children.remove(&key) {
            kids.sort_by(|a, b| a.branch.cmp(&b.branch));
            out.extend(kids);
        }
    }
    // A child whose parent key isn't itself a top-level entry can't happen
    // through group_worktrees (a child is only ever created alongside its
    // parent's own key), but stay defensive rather than silently drop rows.
    let mut leftover: Vec<ProjectStatus> = children.into_values().flatten().collect();
    leftover.sort_by(|a, b| a.key.cmp(&b.key));
    out.extend(leftover);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn with_state<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path());
        let out = f(d.path());
        std::env::remove_var("DEADLIGHT_STATE_DIR");
        out
    }

    #[test]
    fn decode_key_reverses_the_storage_encoding() {
        assert_eq!(decode_key("karpie"), "karpie");
        assert_eq!(decode_key("karpie%2Fsrc"), "karpie/src");
        assert_eq!(decode_key("a%2Fb%2Fc"), "a/b/c");
        // Regression: http::percent_decode (a form decoder) would turn `+`
        // into a space; decode_key must not, or a project literally named
        // `gtk+` would look gone and have its live session killed.
        assert_eq!(decode_key("gtk+"), "gtk+");
    }

    // Pure arithmetic over constructed `Instant`s rather than real sleeping,
    // so this stays fast and non-flaky while still pinning the exact
    // boundary `reconcile_throttled` relies on to skip redundant sweeps.
    #[test]
    fn reconcile_throttle_skips_within_the_interval_and_resumes_after() {
        let t0 = std::time::Instant::now();
        assert!(should_reconcile(None, t0), "first call always runs");
        assert!(!should_reconcile(Some(t0), t0), "immediately after: still within interval");
        let just_under = t0 + RECONCILE_MIN_INTERVAL - std::time::Duration::from_millis(1);
        assert!(!should_reconcile(Some(t0), just_under), "just under the interval: still skipped");
        let at_interval = t0 + RECONCILE_MIN_INTERVAL;
        assert!(should_reconcile(Some(t0), at_interval), "interval elapsed: runs again");
    }

    #[test]
    fn a_saved_layout_alone_makes_a_project_known_but_idle() {
        with_state(|state| {
            fs::create_dir_all(state).unwrap();
            fs::write(state.join("karpie.json"), "{}").unwrap();
            let roots = vec![PathBuf::from("/nonexistent-root")];
            let ps = known_projects(&roots);
            let p = ps.iter().find(|p| p.key == "karpie").expect("saved layout must be listed");
            assert!(p.has_layout);
            assert_eq!(p.live, 0, "no sessions means idle, not live");
            assert_eq!(p.url, "karpie");
        });
    }

    /// Real `git worktree add`, same reasoning as `worktree`'s own test: the
    /// porcelain format and the confinement check are the things under test
    /// here (end to end, through `known_projects` itself), so a hand-built
    /// `ProjectStatus` fixture would only prove the render layer, not that
    /// registry actually wires worktree discovery in.
    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn known_projects_groups_a_repos_worktrees_under_it_labelled_by_branch() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            let repo = root.path().join("repo");
            fs::create_dir_all(&repo).unwrap();
            run_git(&repo, &["init", "-q", "-b", "main"]);
            run_git(&repo, &["config", "user.email", "t@t"]);
            run_git(&repo, &["config", "user.name", "t"]);
            fs::write(repo.join("a.txt"), "x").unwrap();
            run_git(&repo, &["add", "."]);
            run_git(&repo, &["commit", "-qm", "init"]);
            run_git(&repo, &["worktree", "add", "-q", "-b", "feat", ".claude/worktrees/feat"]);

            // The repo must already be "known" (a saved layout, here) for
            // its worktrees to even be looked up — see group_worktrees's
            // cost-control comment. Without this, the assertions below would
            // pass trivially because nothing was ever grouped.
            fs::create_dir_all(state).unwrap();
            fs::write(state.join("repo.json"), "{}").unwrap();

            // The worktree ALSO already has its own saved layout — the
            // normal state after someone opens it once. A linked worktree's
            // `.git` is a *file* (a pointer back to the real repo's gitdir),
            // so `.exists()` is true for it too; without gating on
            // `.is_dir()`, `group_worktrees` treats the worktree itself as a
            // second "repository", runs `git worktree list` from inside it
            // (which reports the same two records, in the same order), and
            // parents it to itself. Without this second state file the child
            // never enters `group_worktrees`'s `parent_keys` snapshot at
            // all, and the self-parenting bug never executes — this is
            // exactly the "test passes for the wrong reason" trap.
            let child_key = crate::projects::storage_key("repo/.claude/worktrees/feat");
            fs::write(state.join(format!("{child_key}.json")), "{}").unwrap();

            let ps = known_projects(&[root.path().to_path_buf()]);

            let idx_parent = ps.iter().position(|p| p.key == "repo").expect("repo must be known");
            assert_eq!(ps[idx_parent].branch, "main", "the repo's own branch comes from the main worktree record");
            assert!(ps[idx_parent].parent.is_none());

            let idx_child = ps.iter().position(|p| p.key == child_key).expect(
                "the worktree must appear even though nobody ever opened it or saved a layout for it",
            );
            assert_eq!(ps[idx_child].branch, "feat");
            assert_eq!(
                ps[idx_child].parent.as_deref(),
                Some("repo"),
                "the worktree's own saved layout must not make it parent itself"
            );
            assert!(ps[idx_child].reachable);
            assert_eq!(idx_child, idx_parent + 1, "a child must be sorted immediately after its parent");
        });
    }

    // Confinement, not the dot-segment allowlist, is what forbids opening a
    // worktree outside ROOTS — but git still reports it exists (a repository
    // clone can name a worktree anywhere), so `known_projects` must still
    // surface it, just as unreachable, rather than silently drop the row.
    #[test]
    fn known_projects_marks_a_worktree_outside_roots_unreachable_but_still_lists_it() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            let repo = root.path().join("repo");
            fs::create_dir_all(&repo).unwrap();
            run_git(&repo, &["init", "-q", "-b", "main"]);
            run_git(&repo, &["config", "user.email", "t@t"]);
            run_git(&repo, &["config", "user.name", "t"]);
            fs::write(repo.join("a.txt"), "x").unwrap();
            run_git(&repo, &["add", "."]);
            run_git(&repo, &["commit", "-qm", "init"]);

            let outside = tempfile::tempdir().unwrap(); // deliberately not under `root`
            let stray_path = outside.path().join("stray");
            run_git(
                &repo,
                &["worktree", "add", "-q", "-b", "stray", stray_path.to_str().unwrap()],
            );

            fs::create_dir_all(state).unwrap();
            fs::write(state.join("repo.json"), "{}").unwrap();

            let ps = known_projects(&[root.path().to_path_buf()]);
            let child = ps
                .iter()
                .find(|p| p.branch == "stray")
                .expect("a worktree outside ROOTS must still be listed, not omitted");
            assert!(!child.reachable, "a worktree outside ROOTS must never be marked reachable");
            assert_eq!(child.parent.as_deref(), Some("repo"));
        });
    }

    // The default deploy config ships two ROOTS. A worktree confined under
    // the *second* one must still resolve even when the first is bogus —
    // `rel_under_roots` must try every root, not bail out via `?` the moment
    // the first one fails to canonicalize (that was a real bug caught here:
    // `Option::ok()?` inside the loop returned `None` from the whole
    // function on the first bad root, never trying the one that actually
    // matched).
    #[test]
    fn rel_under_roots_tries_every_root_not_just_the_first() {
        let good = tempfile::tempdir().unwrap();
        let nested = good.path().join("repo/.claude/worktrees/feat");
        fs::create_dir_all(&nested).unwrap();
        let roots = vec![PathBuf::from("/definitely-not-a-real-root-xyz"), good.path().to_path_buf()];
        let rel = rel_under_roots(&roots, &nested).expect("must still resolve via the second, good root");
        assert_eq!(rel, "repo/.claude/worktrees/feat");
    }

    #[test]
    fn a_socket_with_no_live_process_is_reaped() {
        with_state(|state| {
            let sock = state.join("sock/ghost");
            fs::create_dir_all(&sock).unwrap();
            // A plain file stands in for a stale socket: no dtach process holds it.
            fs::write(sock.join("shell"), "").unwrap();
            let report = reconcile(&[PathBuf::from("/nonexistent-root")]);
            assert!(report.dead_sockets >= 1, "a socket with no process must be removed");
            assert!(!sock.join("shell").exists(), "the stale socket file must be gone");
        });
    }

    #[test]
    fn nested_project_keys_produce_slashed_urls() {
        with_state(|state| {
            fs::create_dir_all(state).unwrap();
            fs::write(state.join("karpie%2Fsrc.json"), "{}").unwrap();
            let ps = known_projects(&[PathBuf::from("/nonexistent-root")]);
            let p = ps.iter().find(|p| p.key == "karpie%2Fsrc").expect("nested project must list");
            assert_eq!(p.url, "karpie/src", "the URL keeps readable slashes");
        });
    }

    // Exhaustive over every (project_gone, held_before, held_after_kill)
    // combination reap_decision can actually be called with by `reconcile`
    // (held_after_kill is only meaningful when held_before is true), so the
    // "kill failed, keep the socket" path is proven without needing a
    // process that refuses to die on cue.
    #[test]
    fn reap_decision_never_deletes_a_socket_whose_process_survived() {
        // No process holds it: always safe to remove, regardless of project.
        assert_eq!(reap_decision(true, false, false), ReapAction::RemoveDeadSocket);
        assert_eq!(reap_decision(false, false, false), ReapAction::RemoveDeadSocket);
        // Live process, project still exists: never touched.
        assert_eq!(reap_decision(false, true, true), ReapAction::Leave);
        // Live process, project gone, kill confirmed successful: safe to remove.
        assert_eq!(reap_decision(true, true, false), ReapAction::RemoveKilled);
        // Live process, project gone, but something still holds the socket
        // after the kill attempt (kill failed, or it hasn't died yet): the
        // socket must survive so the next reconcile can still find it.
        assert_eq!(
            reap_decision(true, true, true),
            ReapAction::Leave,
            "a failed kill must never lose the only path back to an orphaned process"
        );
    }

    /// Spawns `dtach -n <sock_path> sleep 30`: it creates the socket, forks a
    /// detached session, and its own foreground process exits once that
    /// setup is done — so `Command::status()` returning is itself the
    /// synchronization point (no arbitrary sleep needed to wait for the
    /// socket to appear), and no in-memory `session::SESSIONS` entry is ever
    /// created, mirroring exactly what a session that outlived a previous
    /// deadlight process leaves behind. Returns `None` (rather than
    /// panicking) if `dtach` genuinely isn't installed, since its own setup
    /// step could not have succeeded either. `dtach` is a hard runtime
    /// prerequisite of this project (see README.md, docs/deploy.md), so this
    /// isn't introducing a new kind of test dependency.
    fn spawn_detached_dtach(sock_path: &std::path::Path) -> Option<()> {
        let status = std::process::Command::new("dtach")
            .args(["-n", sock_path.to_str().unwrap(), "sleep", "30"])
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        Some(())
    }

    // Regression for the startup case: a session that outlived a previous
    // deadlight process has no entry in `session`'s in-memory map, so
    // `session::kill_project` alone is a no-op. Uses a real `dtach` process
    // so this actually proves the OS-level pid is killed, not just forgotten
    // about by having its socket file deleted out from under it. Roots point
    // at a real, existing directory that genuinely does not contain
    // "ghost-proj" — not a nonexistent path — so `project_gone` becomes true
    // for the right reason (the project really isn't there) rather than by
    // accident of the C3 "roots unverifiable" guard also forcing it false.
    #[test]
    fn a_live_session_for_a_deleted_project_is_actually_killed_not_just_forgotten() {
        with_state(|state| {
            let sock_dir = state.join("sock/ghost-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            let root = tempfile::tempdir().unwrap(); // exists, but has no "ghost-proj" child

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }
            assert!(
                socket_has_process(&sock_path),
                "test setup: the detached process must be observable before reconcile runs"
            );

            let report = reconcile(&[root.path().to_path_buf()]);

            assert_eq!(report.gone_projects, 1, "the gone-project session must be counted as reaped");
            assert!(
                !socket_has_process(&sock_path),
                "the dtach process itself must be killed, not merely forgotten by deleting its socket"
            );
            assert!(!sock_path.exists(), "the socket must be removed only once the process is confirmed gone");
        });
    }

    // I5: the end-to-end guard the review asked for. Without this, a future
    // change to the project_gone condition could start deleting live
    // sockets for projects that still exist with every other test green.
    #[test]
    fn a_live_session_for_a_project_that_still_exists_is_never_touched() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("still-here")).unwrap();
            let sock_dir = state.join("sock/still-here");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }
            assert!(socket_has_process(&sock_path), "test setup: process must be observable");

            let report = reconcile(&[root.path().to_path_buf()]);

            assert_eq!(report.gone_projects, 0, "the project still exists; nothing should be reaped");
            assert!(sock_path.exists(), "the socket of a live session for an existing project must survive");
            assert!(
                socket_has_process(&sock_path),
                "the process itself must survive — the project was never gone"
            );

            // Clean up: this process was never in any in-memory map, so
            // nothing else will ever end it.
            for pid in pids_holding(&process_snapshot(), &sock_path) {
                let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        });
    }

    // C3: a root that cannot be verified must never be mistaken for "the
    // project is gone". Without this guard, a transiently unreadable mount
    // or a DEADLIGHT_ROOTS typo would make startup reconcile SIGKILL every
    // live session on the machine.
    #[test]
    fn unreadable_roots_suspend_gone_project_reaping_but_dead_sockets_still_reap() {
        with_state(|state| {
            let sock_dir = state.join("sock/whatever-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }

            // Every root fails to canonicalize: roots_ok must be false.
            let report = reconcile(&[PathBuf::from("/nonexistent-root-a"), PathBuf::from("/nonexistent-root-b")]);

            assert_eq!(report.gone_projects, 0, "must not reap anything when roots can't be verified");
            assert!(sock_path.exists(), "a live session must survive when its project can't be checked");
            assert!(socket_has_process(&sock_path), "the process itself must survive");

            for pid in pids_holding(&process_snapshot(), &sock_path) {
                let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }

            // Dead-socket reaping needs no project information at all, and
            // must still work even while gone-project reaping is suspended.
            let dead = state.join("sock/other-proj");
            fs::create_dir_all(&dead).unwrap();
            fs::write(dead.join("shell"), "").unwrap();
            let report2 = reconcile(&[PathBuf::from("/nonexistent-root-a")]);
            assert!(report2.dead_sockets >= 1, "a socket with no process must still be reaped");
        });
    }

    // I4: after a restart, `session::list_sessions` can only ever report 0
    // (its map is in-memory and starts empty), but `reconcile` guarantees
    // every socket file left standing is process-backed — so `live` must
    // reflect that floor rather than falsely reporting an active project as
    // idle, which is exactly the invisibility this whole module exists to
    // remove.
    #[test]
    fn known_projects_reports_a_live_floor_from_socket_files_when_the_in_memory_map_is_empty() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("survivor")).unwrap();
            let sock_dir = state.join("sock/survivor");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }

            let ps = known_projects(&[root.path().to_path_buf()]);
            let p = ps.iter().find(|p| p.key == "survivor").expect("survivor must be listed");
            assert_eq!(
                p.live, 1,
                "a process-backed socket with no in-memory record must still count as live, not idle"
            );

            for pid in pids_holding(&process_snapshot(), &sock_path) {
                let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        });
    }

    // C4 regression: a project directory name may contain a space
    // (list_projects/resolve_project/valid_project all permit it; only
    // *session* names are restricted). storage_key doesn't escape spaces,
    // so the socket directory name is the literal "my project". A
    // word-split match can never equal a target containing a space, so this
    // is the direct proof that matching must not first split the `ps` line
    // on whitespace.
    #[test]
    fn a_socket_path_containing_a_space_is_still_correctly_matched() {
        with_state(|state| {
            let sock_dir = state.join("sock/my project");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }

            assert!(
                socket_has_process(&sock_path),
                "a socket path containing a space must still be recognized as held"
            );

            for pid in pids_holding(&process_snapshot(), &sock_path) {
                let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        });
    }

    // Same regression, exercised end to end through `reconcile`: before the
    // fix, a spaced socket path's `held_before` came back false, so it fell
    // into the *unconditional* dead-socket branch and was deleted without
    // ever attempting a kill first — worse than a plain missed reap, since
    // it happened even though the process was alive and the project existed.
    #[test]
    fn a_live_session_for_a_spaced_project_name_survives_reconcile() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("my project")).unwrap();
            let sock_dir = state.join("sock/my project");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }

            let report = reconcile(&[root.path().to_path_buf()]);

            assert_eq!(report.dead_sockets, 0, "a live, spaced-path socket must not look dead");
            assert_eq!(report.gone_projects, 0, "the project exists; nothing should be reaped");
            assert!(sock_path.exists(), "the socket of a live session must survive");
            assert!(socket_has_process(&sock_path), "the process itself must survive");

            for pid in pids_holding(&process_snapshot(), &sock_path) {
                let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        });
    }

    // I6 regression: the default deploy config ships two roots. `.any()`
    // let one resolving root paper over a second, genuinely-down one, so
    // every session under the down root looked gone and got SIGKILLed —
    // C3's failure mode, just narrowed to a partial outage instead of a
    // total one. `.all()` must refuse to treat any project as gone unless
    // every configured root is verifiable.
    #[test]
    fn one_unreadable_root_among_several_still_suspends_gone_project_reaping() {
        with_state(|state| {
            let good_root = tempfile::tempdir().unwrap(); // resolves fine
            let sock_dir = state.join("sock/partial-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }

            let roots =
                vec![good_root.path().to_path_buf(), PathBuf::from("/nonexistent-root-partial")];
            let report = reconcile(&roots);

            assert_eq!(
                report.gone_projects, 0,
                "one unreadable root among several must still suspend gone-project reaping"
            );
            assert!(sock_path.exists());
            assert!(socket_has_process(&sock_path));

            for pid in pids_holding(&process_snapshot(), &sock_path) {
                let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        });
    }

    // R8: the primitive `reconcile`'s dead-socket branch now rechecks with,
    // immediately before every delete, must reflect state *at call time* —
    // not some memoized value from an earlier snapshot in the same sweep —
    // or the recheck would be no fix at all. A true reproduction of the
    // race itself (a process attaching to a specific socket in the exact
    // window between the sweep's one-time snapshot and reconcile reaching
    // that socket) isn't practical to make deterministic from a black-box
    // test without internal hooks into reconcile's loop; this instead pins
    // down the property the fix depends on: two calls bracketing a process
    // actually attaching must disagree.
    #[test]
    fn socket_has_process_reflects_state_at_call_time_not_a_stale_snapshot() {
        with_state(|state| {
            let sock_dir = state.join("sock/timing-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            assert!(!socket_has_process(&sock_path), "nothing holds this path yet");

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }

            assert!(
                socket_has_process(&sock_path),
                "a fresh check must observe a process that just attached, not a cached earlier answer"
            );

            for pid in pids_holding(&process_snapshot(), &sock_path) {
                let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        });
    }
}
