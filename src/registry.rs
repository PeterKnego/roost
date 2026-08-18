//! The project registry: what resh knows about, and what is running.
//!
//! Rebuilt at startup rather than accumulated in memory, because dtach
//! sessions deliberately outlive resh. An in-memory-only registry would
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
    /// Age of the longest-running session, or `None` when it is genuinely not
    /// known — which is the normal case immediately after a restart, when this
    /// process's in-memory session map is empty and only the socket-file floor
    /// below is available. Rendering an unknown age as `0` claimed every
    /// project's oldest shell had just started, at exactly the moment "what did
    /// I leave running for days?" is the question the strip exists to answer.
    pub oldest_age_secs: Option<u64>,
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

/// Turns raw `ps -Ao pid=,args=` output into a snapshot, or `None` when the
/// listing itself is not trustworthy: the process exited non-zero, or
/// produced no output at all (a live host always has at least one process —
/// pid 1, if nothing else — so empty stdout means the listing failed
/// silently, not that the machine has no processes). Factored out from the
/// subprocess spawn so this decision is directly unit-testable against real
/// `Output` values without needing to actually break `ps` on the test host.
///
/// This distinction matters: before it existed, a failed or empty listing
/// was indistinguishable from "nothing holds this socket", so a transient
/// `ps` failure (fork/`EAGAIN` under memory or process-table pressure, a
/// stripped container, a broken `PATH`) would make `kill_and_unlink` unlink
/// a live session's socket — the seventh instance in this feature of one
/// shape: a false "gone" verdict destroying a live session. "Could not
/// determine" must be its own outcome, never silently folded into "empty".
fn parse_ps_snapshot(out: &std::process::Output) -> Option<Vec<(u32, String)>> {
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim_start();
                let (pid_str, args) = trimmed.split_once(char::is_whitespace)?;
                let pid: u32 = pid_str.parse().ok()?;
                Some((pid, args.trim_start().to_string()))
            })
            .collect(),
    )
}

/// One snapshot of every process's pid and its raw command-line text, taken
/// with a single `ps` call — or `None` when that listing could not be
/// trusted (see `parse_ps_snapshot`). Passed around so that sweeping every
/// socket under `sock/` to see which are still held costs one subprocess
/// total, not one per socket. Deliberately not `pgrep -f`: that treats its
/// pattern as an extended regex matched anywhere in the command line, which
/// is wrong two different ways for a filesystem path — (1) a project name
/// may contain regex metacharacters (`valid_project` allows them; only
/// *session* names are restricted to `[A-Za-z0-9_-]`), which can make the
/// "pattern" an invalid ERE and silently match nothing, so a live socket's
/// process looks dead; and (2) unanchored substring matching means session
/// `claude`'s socket path matches inside session `claude-2`'s command line
/// too, so an actually-dead `claude` socket is never reaped once `claude-2`
/// exists. `pids_holding` matches each raw line as plain text (no regex
/// ever built from untrusted input) and requires the match be a whole
/// argument, which avoids both hazards without needing to split the line
/// into words first — splitting on whitespace would break on a socket path
/// that itself contains a space, which a project directory name may
/// (`list_projects`, `resolve_project`, and `valid_project` all permit
/// spaces; only *session* names are restricted).
/// `-ww` and a cleared `COLUMNS` both defend the same thing: a **truncated**
/// args column is indistinguishable from a socket path that isn't there, and
/// the consequence of "isn't there" here is unlinking a live session's socket.
/// procps honours `COLUMNS` when deciding how wide to print, and a long socket
/// path under a deeply nested project is exactly the line that would get cut.
/// macOS was measured not to truncate when piped, but the deploy host is Linux
/// and could not be tested (unreachable by ssh), and `-ww` is accepted on both
/// platforms at no cost — so take it rather than rely on the untested half.
fn process_snapshot() -> Option<Vec<(u32, String)>> {
    let out = std::process::Command::new("ps")
        .args(["-Aww", "-o", "pid=,args="])
        .env_remove("COLUMNS")
        .output()
        .ok()?;
    parse_ps_snapshot(&out)
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

/// A source of process snapshots, injected rather than called directly, so
/// the "the listing is unverifiable" path through `kill_and_unlink` and
/// `reconcile` can be exercised deterministically in a test — no need to
/// actually break `ps` on the test host, and no shared global state (an env
/// var, say) to race other tests over.
type SnapshotFn<'a> = &'a dyn Fn() -> Option<Vec<(u32, String)>>;

/// True when some live process holds this socket path right now, or `None`
/// could not be determined — treated conservatively as "yes, assume held":
/// both of this function's uses (the post-kill confirmation poll, and the
/// immediately-before-delete recheck in `reconcile`'s dead-socket branch)
/// must never unlink a socket on a shrug. Takes its own fresh snapshot on
/// every call — used for one-off checks (tests, and re-polling after a kill
/// to see whether it has taken effect yet), where the whole point is to
/// observe current state rather than a sweep's frozen-in-time one.
fn socket_has_process_with(path: &std::path::Path, snapshot_fn: SnapshotFn) -> bool {
    match snapshot_fn() {
        None => true,
        Some(s) => !pids_holding(&s, path).is_empty(),
    }
}

// Production code now goes through `socket_has_process_with` directly (see
// `kill_and_unlink_with`/`reconcile_with`) so the injected snapshot source
// is threaded all the way through; this plain wrapper survives only as a
// convenience for tests that don't need to inject anything.
#[cfg(test)]
fn socket_has_process(path: &std::path::Path) -> bool {
    socket_has_process_with(path, &process_snapshot)
}

/// Kills every process currently holding `sock_path`, waits (bounded) for
/// them to die, and removes the socket file only once none remain. Leaves
/// the file in place if a kill fails, a process survives the wait, **or the
/// process listing itself could not be trusted** — so the session stays
/// discoverable and reapable by a later pass rather than becoming an
/// unreachable orphan (its socket gone, so nothing — not a browser, not
/// `reconcile` itself — can ever find or attach to it again).
///
/// That last case is deliberate, not an oversight: a `None` snapshot means
/// "we don't know what's out there", and proceeding as if nothing were —
/// the bug this function's own history already shows up twice over (a
/// `pgrep -f` regex hazard, a word-split that broke on a space) — must never
/// happen a third time just because the underlying listing failed instead
/// of merely mismatching.
///
/// Shared by `reconcile`'s project-gone branch and `session::kill_project`
/// (an explicit "Close Project" from the UI): both need to end whatever
/// `dtach` *master* — and, through it, the user's shell — is still holding
/// a socket, not merely the in-process *client* resh itself spawned.
/// Killing only that client is just a detach: in `-A` mode `dtach` forks a
/// master that immediately detaches and reparents to init, so the master
/// and shell survive a client kill with a live socket, unreachable and
/// (without this) invisibly still running. Both call sites need the
/// identical confirm-before-unlink safety property, so they share this one
/// implementation rather than two that can drift.
///
/// Returns `true` once the socket ends up fully cleaned up — no process
/// left, file removed — including the vacuous case where nothing held it to
/// begin with. Returns `false` if a kill failed, a process survived the
/// wait, or the process listing was unverifiable.
fn kill_and_unlink_with(sock_path: &std::path::Path, snapshot_fn: SnapshotFn) -> bool {
    let Some(snapshot) = snapshot_fn() else {
        eprintln!(
            "resh: could not verify what holds {} (process listing unavailable) — leaving it in place",
            sock_path.display()
        );
        return false;
    };
    let pids = pids_holding(&snapshot, sock_path);
    for pid in &pids {
        // M6: pid 0 should be unreachable here in practice (pid 0 is the
        // kernel/swapper, whose command line can never contain a socket
        // path), but `kill -9 0` signals this whole process group — i.e.
        // resh itself — so refuse it outright rather than trust that
        // impossibility.
        if *pid == 0 {
            continue;
        }
        // stderr silenced: `snapshot` is a moment old, so a process that
        // exited on its own in between makes `kill(1)` print
        // "kill: <pid>: No such process" — noise about something that has
        // already achieved exactly what was wanted, and noise that masks
        // real output in the journal and the test suite alike.
        let _ = std::process::Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .stderr(std::process::Stdio::null())
            .status();
    }
    // Bounded wait for the kill(s) to take effect rather than an instant
    // recheck: SIGKILL is not synchronous. Bounded so a process that
    // refuses to die can't hang the caller forever. When `pids` was empty
    // this loop runs zero iterations, and `still_alive`'s first read is
    // itself the fresh, immediately-before-delete recheck (see R8 in this
    // module's history: a stale "nothing held it" from an earlier snapshot
    // must never be trusted at delete time without re-verifying).
    let mut still_alive = socket_has_process_with(sock_path, snapshot_fn);
    for _ in 0..20 {
        if !still_alive {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
        still_alive = socket_has_process_with(sock_path, snapshot_fn);
    }
    if still_alive {
        return false;
    }
    let _ = std::fs::remove_file(sock_path);
    true
}

/// The only public entry point for `kill_and_unlink_with`'s logic — see its
/// doc comment for the full contract. Used by `reconcile`'s project-gone
/// branch and by `session::kill_project` (an explicit "Close Project" from
/// the UI), so both end a session's `dtach` master, not merely the
/// in-process client, through one shared, confirm-before-unlink
/// implementation.
pub fn kill_and_unlink(sock_path: &std::path::Path) -> bool {
    kill_and_unlink_with(sock_path, &process_snapshot)
}

fn origin_marker_path(key_dir: &std::path::Path) -> PathBuf {
    key_dir.join(".origin")
}

/// The marker holds a path's *raw bytes*, not text. `to_string_lossy` (what
/// this used to write) turns a non-UTF8 project path into replacement
/// characters — a path that can never exist — so every differently-rooted
/// pass afterwards would read a false "gone" and reap a live session. These
/// two helpers are the byte-exact round trip `String`/`read_to_string`
/// cannot provide. The `not(unix)` halves exist only to keep the module
/// compiling: resh is unix-only in practice (dtach, PTYs, `ps`).
#[cfg(unix)]
fn marker_bytes(dir: &std::path::Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    dir.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn marker_bytes(dir: &std::path::Path) -> Vec<u8> {
    dir.to_string_lossy().as_bytes().to_vec()
}

#[cfg(unix)]
fn path_from_marker(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn path_from_marker(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// The single writer of a `.origin` marker, shared by `record_origin` and
/// `confirmed_gone` so the atomicity below can never be true of one and not
/// the other.
///
/// Atomic — temp file, then `rename` — rather than `fs::write`, which is
/// `O_TRUNC` followed by a write. The file being truncated is the *only*
/// record of where this project lives, and the whole reason it exists is
/// that a *second*, differently-rooted resh sharing this state dir
/// reads it. That reader landing inside the truncate-then-write window would
/// see `""` (or a prefix like `/home/pet`), conclude "gone", and SIGKILL the
/// live session this mechanism was built to protect — a window reopened
/// every few seconds, per project, forever. Worse, a crash or `ENOSPC`
/// mid-write leaves the damage *permanent*. `rename` makes the marker only
/// ever whole-or-untouched, and an interrupted `.origin.tmp` is a dotfile,
/// which `reconcile` already skips.
///
/// Skips the write when the marker already holds these exact bytes — the
/// common case, since `reconcile` reconfirms the same resolution on every
/// pass — so steady-state reaping does no disk writes at all. A marker
/// damaged by an older, non-atomic writer differs, so it gets repaired.
///
/// 0600 for the same reason `attach` chmods the socket directory 0700:
/// nothing outside this user has any business reading it.
///
/// Best-effort throughout: a failure here just means a later pass falls back
/// to "no marker, never reap" — the safe direction to fail in.
fn write_origin(key_dir: &std::path::Path, dir: &std::path::Path) {
    let bytes = marker_bytes(dir);
    let marker = origin_marker_path(key_dir);
    if std::fs::read(&marker).map(|cur| cur == bytes).unwrap_or(false) {
        return;
    }
    // Pid in the temp name: two resh instances sharing this state dir
    // is the whole premise of the marker, and a *shared* temp name would
    // hand the truncate-then-write window straight back — one process's
    // `rename` could publish the other's half-written file. A crash can
    // strand one of these, but it is a dotfile (skipped by every enumeration
    // of this directory) and the unchanged-content skip above means the
    // write path runs at most once per changed resolution, not once per pass.
    let tmp = key_dir.join(format!(".origin.tmp.{}", std::process::id()));
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

/// Records the resolved absolute directory a project maps to, so a *later*
/// check for "is this project gone" can consult this exact path directly —
/// independent of whatever `RESH_ROOTS` the process performing that
/// later check happens to be configured with. See `confirmed_gone`'s doc
/// comment for why that independence matters.
///
/// Called by `session::attach` the moment a session's socket directory is
/// created (its caller has already resolved `dir`). `reconcile` refreshes
/// the same marker on every pass that reconfirms a resolution, but goes
/// straight to `write_origin` — it already holds the key directory, so
/// re-deriving it from a project string here would be a round trip through
/// `decode_key`/`storage_key` for no gain.
pub fn record_origin(project: &str, dir: &std::path::Path) {
    let key_dir = sock_root().join(crate::projects::storage_key(project));
    let _ = std::fs::create_dir_all(&key_dir);
    write_origin(&key_dir, dir);
}

/// Given a `.origin` marker's raw contents, is the directory it records
/// *positively confirmed* to be gone?
///
/// Every answer other than "yes, confirmed" must be `false`, because the
/// only thing a `true` here causes is SIGKILL plus unlink, which is
/// unrecoverable. Two ways this used to get that backwards, both of them the
/// same shape as the bug the marker was introduced to fix:
///
/// - `!Path::new("").is_dir()` is `true`, so an empty or truncated marker
///   read as "gone" — the loudest possible absence of evidence.
/// - `Path::is_dir()` swallows *every* error into `false`: `EACCES` on a
///   parent after a `chmod`, an unmounted disk, a downed autofs/NFS mount,
///   a path component that is no longer a directory. "I could not check"
///   became "confirmed gone", which contradicts this module's own rule that
///   an unreadable root suspends reaping.
///
/// So: only `NotFound` counts. `symlink_metadata`, not `metadata`, so a
/// symlink standing where the recorded directory used to be is observed
/// rather than followed (`resolve_project` records a canonicalised path, so
/// a symlink appearing there is already unexpected). And `Ok(_) => false`
/// even for a non-directory: something that is *there* is not evidence of
/// gone, and being wrong in that direction cannot be undone.
fn recorded_path_is_gone(recorded: &[u8]) -> bool {
    // Only a trailing newline is stripped, never `trim()`: a project
    // directory name may legally end in a space (see `process_snapshot` —
    // only *session* names are restricted), and trimming that would look up
    // a path that never existed and answer "gone".
    let mut end = recorded.len();
    while end > 0 && recorded[end - 1] == b'\n' {
        end -= 1;
    }
    let trimmed = &recorded[..end];
    if trimmed.is_empty() {
        return false;
    }
    match std::fs::symlink_metadata(path_from_marker(trimmed)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
        Ok(_) => false,
    }
}

/// True only when a project is *confirmed* to have existed before — a
/// `.origin` marker recording its resolved absolute directory was written
/// by `record_origin` — and that exact recorded path no longer exists.
///
/// Deliberately not "resolve_project(roots, url).is_none()" alone: that
/// conflates two situations that look identical from inside a single
/// `reconcile` call — "the directory was deleted" and "the directory
/// exists, just not under the roots *this particular process* happens to
/// be configured with". Two resh processes — or the same one,
/// restarted with a different `RESH_ROOTS` — can share one
/// `RESH_STATE_DIR`: `docs/deploy.md` tells users to override
/// `RESH_ROOTS` to run a second instance locally and says nothing
/// about also separating `RESH_STATE_DIR`, so the documented way to
/// run resh locally reaches this. Reproduced directly against a real
/// `dtach` session, outside the test suite entirely: a live session for
/// `victimproj` was SIGKILLed and its socket unlinked simply because
/// resh was started with different roots against the same state dir
/// — logging "project directory is gone" about a directory that existed
/// the whole time. This is the eighth instance in this feature of one
/// shape: a false "gone" verdict destroying a live session. The rule the
/// previous seven taught applies exactly: "I cannot map this key to a
/// directory" is not "the directory is gone."
///
/// So: `resolve_project` succeeding refreshes the marker (this project
/// genuinely is where we think it is, under *these* roots specifically).
/// `resolve_project` failing checks the marker's *recorded* path directly,
/// independent of whatever roots this particular pass happens to be
/// running with. No marker at all — a key this process has never
/// positively confirmed under any configuration it has seen — is never
/// treated as gone: it is left completely alone, neither killed nor
/// unlinked (`known_projects` may still list it; that's a separate,
/// unaffected concern from reaping).
///
/// An unreadable *or* damaged marker is the same as no marker: see
/// `recorded_path_is_gone`, which is where "positive evidence only" is
/// actually enforced.
fn confirmed_gone(key_dir: &std::path::Path, roots: &[PathBuf], url: &str) -> bool {
    match crate::projects::resolve_project(roots, url) {
        Some(dir) => {
            write_origin(key_dir, &dir);
            false
        }
        None => match std::fs::read(origin_marker_path(key_dir)) {
            Ok(recorded) => recorded_path_is_gone(&recorded),
            Err(_) => false,
        },
    }
}

/// The age of the longest-running session, or `None` when not one of them has
/// a readable age.
///
/// `filter_map`, not `map`: a session whose age the OS could not supply (a pid
/// `ps` cannot read, or the `0` sentinel used when no pid was available) must
/// not contribute a `0` that then wins `max()` and renders as "just started".
/// Unknown has to stay unknown all the way to the tooltip — the same rule as
/// everywhere else in this module, in a rendering rather than a destructive
/// position. Kept as a pure function so the all-unknown and mixed cases are
/// testable without contriving an unreadable process.
fn oldest_age(sessions: &[crate::session::SessionInfo]) -> Option<u64> {
    sessions.iter().filter_map(|s| s.age_secs).max()
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
/// after a restart. A `dtach` session that outlived the previous resh
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
/// permissions problem, a `RESH_ROOTS` typo, or just running on a host
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
///
/// The identical "cannot verify" discipline applies to the process listing
/// itself: if `process_snapshot` returns `None` (see its doc comment — a
/// failed or empty `ps`), this function cannot safely tell a dead socket
/// from a live one for *any* entry, so it does nothing at all this pass
/// rather than guess. `kill_and_unlink` independently applies the same rule
/// to its own snapshot, since it can also be reached directly from
/// `session::kill_project` outside of a `reconcile` sweep.
pub fn reconcile(roots: &[PathBuf]) -> ReapReport {
    reconcile_with(roots, &process_snapshot)
}

// Both flip only on a genuine state *change*, not every suspended pass —
// `known_projects` calls `reconcile` (throttled, but still every few
// seconds) on every project listing or header-strip load, so a
// permanently broken `ps` or a permanently unreadable root would otherwise
// spam the journal forever with the identical line. `Relaxed` is enough:
// these coordinate nothing beyond "did I already say this", and a stray
// duplicate or missing log line from a race between two sweeps is
// harmless, unlike the sockets/processes reconcile actually touches.
static ROOTS_WERE_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static PROCESS_LIST_WAS_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

fn reconcile_with(roots: &[PathBuf], snapshot_fn: SnapshotFn) -> ReapReport {
    let mut report = ReapReport { dead_sockets: 0, gone_projects: 0 };
    let roots_ok = !roots.is_empty() && roots.iter().all(|r| r.canonicalize().is_ok());
    let roots_were_ok = ROOTS_WERE_OK.swap(roots_ok, std::sync::atomic::Ordering::Relaxed);
    if !roots_ok && roots_were_ok {
        eprintln!(
            "resh: none of the configured roots could be read ({roots:?}) — \
             reaping sessions for missing projects is suspended until this recovers"
        );
    } else if roots_ok && !roots_were_ok {
        eprintln!("resh: configured roots are readable again — reaping resumed");
    }
    let Ok(rd) = std::fs::read_dir(sock_root()) else { return report };
    // One process listing for the whole sweep (see process_snapshot's doc
    // comment) rather than one `ps` per socket file. `None` here means the
    // listing itself could not be trusted, not that nothing is running —
    // bail out of the entire pass rather than reap anything on a guess;
    // the next pass (throttled, but not indefinitely deferred — see
    // `known_projects`) gets another chance.
    let snapshot_opt = snapshot_fn();
    let process_list_ok = snapshot_opt.is_some();
    let process_list_was_ok =
        PROCESS_LIST_WAS_OK.swap(process_list_ok, std::sync::atomic::Ordering::Relaxed);
    let Some(snapshot) = snapshot_opt else {
        if process_list_was_ok {
            eprintln!(
                "resh: could not read the process list (ps failed, exited non-zero, or \
                 returned nothing) — reaping is suspended until this recovers"
            );
        }
        return report;
    };
    if !process_list_was_ok {
        eprintln!("resh: process listing is readable again — reaping resumed");
    }
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
        // A key holding a control byte cannot be reasoned about safely here:
        // `pids_holding` matches against line-oriented `ps` output, and a
        // newline inside the socket path splits one process's argv across two
        // lines, so a live holder is unfindable and its socket would be
        // unlinked as an orphan. `storage_key` escapes those bytes now, so this
        // is unreachable for anything resh wrote — it guards a key written
        // by an older build, or one dropped in by hand. Skipping costs a stale
        // row; reaping costs a running shell.
        //
        // This does NOT cover a non-UTF8 key: `to_string_lossy` turns those
        // bytes into U+FFFD, which is not a control character, so such a key
        // sails past here. It is safe for a different reason — `pids_holding`
        // lossy-converts the `ps` line and the socket path the same way, so
        // they still match — and that is worth knowing rather than assuming
        // this guard handles it.
        // `is_ascii_control` already covers 0x7F (DEL), so this is the whole set.
        if key.bytes().any(|b| b.is_ascii_control()) {
            eprintln!(
                "resh: refusing to reap session key {key:?} — it contains a control byte, \
                 so who holds its socket cannot be determined from `ps` output"
            );
            continue;
        }
        // Not "resolve_project(...).is_none()" alone — see confirmed_gone's
        // doc comment. A key that doesn't resolve under *these* roots is
        // not evidence the directory is gone; only a key whose resolved
        // path was previously recorded, and is now confirmed missing,
        // counts.
        let project_gone = roots_ok && confirmed_gone(&entry.path(), roots, &url);
        // In-process sessions first, once per project rather than once per
        // socket file below: `session::kill_project` now ends each session's
        // dtach master too (via `kill_and_unlink`, the same helper the loop
        // below uses), not just resh's own client, so any socket it
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
                    "resh: reaped {in_process_ended} in-process session(s) for {key} — its recorded directory no longer exists"
                );
            }
        }
        let Ok(inner) = std::fs::read_dir(entry.path()) else { continue };
        for sock in inner.flatten() {
            // `.origin` (this pass's own `confirmed_gone` may have just
            // written or refreshed it, right above) and any `.origin.tmp`
            // left behind by an interrupted write are metadata about the
            // project key, not session sockets — a real session name can
            // never start with `.` (`^[A-Za-z0-9_-]{1,32}$`), so this is an
            // unambiguous, safe skip. Without it, the marker — held by no
            // process — reads as a dead socket and gets reaped by this same
            // sweep instants after being written, wiping out the exact
            // cross-restart record `confirmed_gone` depends on.
            if sock.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let pids = pids_holding(&snapshot, &sock.path());
            let held_before = !pids.is_empty();
            let name = sock.file_name().to_string_lossy().into_owned();

            if held_before && project_gone {
                // Covers a session this process never had in memory (one
                // that outlived a restart) as well as a retry for one
                // `kill_project` above tried and failed to fully end.
                if kill_and_unlink_with(&sock.path(), snapshot_fn) {
                    report.gone_projects += 1;
                    eprintln!(
                        "resh: reaped session {key}/{name} — its recorded directory no longer exists (pids {pids:?})"
                    );
                } else {
                    eprintln!(
                        "resh: could not end session {key}/{name} for a missing project — pids {pids:?} did not fully die (or the process list was unverifiable); socket left in place so it stays discoverable"
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
                    // `socket_has_process_with` also treats an unverifiable
                    // recheck as "assume held", so a `ps` that starts
                    // failing mid-sweep can never cause a delete here either.
                    if !socket_has_process_with(&sock.path(), snapshot_fn) {
                        let _ = std::fs::remove_file(sock.path());
                        report.dead_sockets += 1;
                        eprintln!("resh: reaped dead socket {}", sock.path().display());
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

/// Every project resh knows about: those with a saved layout, those with
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
                    oldest_age_secs: None,
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
            // `reconcile` (just run, above) guarantees every socket file still
            // here is process-backed: a socket with no process is reaped
            // outright, and a live one for a since-deleted project is killed
            // before its socket is removed. So the number of socket files under
            // this key is a truthful *lower bound* on how many sessions are
            // live — including ones this process knows nothing about, because
            // dtach sessions outlive resh and a restart leaves the
            // in-memory map empty while the shells keep running.
            //
            // Excludes `.origin` (see reconcile's identical skip): it is
            // metadata about the project key, not a session socket, and would
            // otherwise inflate the floor by one for every project reconcile
            // has ever positively confirmed.
            let floor = std::fs::read_dir(e.path())
                .map(|rd| {
                    rd.flatten()
                        .filter(|f| !f.file_name().to_string_lossy().starts_with('.'))
                        .count()
                })
                .unwrap_or(0);
            // The MAXIMUM of the two, not whichever branch happens to apply.
            // Preferring the in-memory list whenever it is non-empty made a
            // project's count *drop* to just the sessions this process started:
            // observed in production as "1 session" for a project holding two
            // live shells, because one predated the last restart. Those
            // survivors are precisely what the strip exists to surface, so the
            // count must never be smaller than the sockets on disk.
            let live = sessions.len().max(floor);
            // Age still comes only from sessions this process can inspect; a
            // socket file supplies a count, never an age.
            let oldest = oldest_age(&sessions);
            // Only ever *create* a listing here when there's an actual live
            // session. Since C2, `sock/<key>/` can contain nothing but a
            // `.origin` marker — deliberately persisted indefinitely (see
            // `record_origin`/`confirmed_gone`), not just until the next
            // sweep — for any project ever opened, even one closed long ago
            // with no saved layout. Without this guard, every such project
            // would show up as a permanent phantom row (`has_layout: false,
            // live: 0`) forever, not merely for the up-to-3-second window a
            // still-emptying directory used to cause. A project that
            // already has a listing (a saved layout, from the `.json` scan
            // above) still gets its live count refreshed either way.
            if live > 0 {
                let slot = by_key.entry(key.clone()).or_insert(ProjectStatus {
                    key: key.clone(),
                    url: decode_key(&key),
                    live: 0,
                    oldest_age_secs: None,
                    has_layout: false,
                    branch: String::new(),
                    parent: None,
                    reachable: true,
                });
                slot.live = live;
                slot.oldest_age_secs = oldest;
            } else if let Some(slot) = by_key.get_mut(&key) {
                slot.live = 0;
                slot.oldest_age_secs = None;
            }
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
                        oldest_age_secs: None,
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
                        oldest_age_secs: None,
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
        std::env::set_var("RESH_STATE_DIR", d.path());
        let out = f(d.path());
        std::env::remove_var("RESH_STATE_DIR");
        out
    }

    /// Test-only cleanup helper: kills off whatever a test's real detached
    /// `dtach` process left behind, so it doesn't linger past the test.
    /// Unwraps `process_snapshot`'s `Option` with an empty-vec fallback —
    /// fine here since this is best-effort teardown, not the behavior under
    /// test (which is exactly why production code must never do this; see
    /// `kill_and_unlink`/`reconcile`'s handling of a `None` snapshot).
    /// stderr silenced for the same reason `kill_and_unlink` silences it: a
    /// process that already exited makes `kill(1)` complain about a pid
    /// nobody needed killed, and that noise masks real test output.
    fn kill_test_pids(sock_path: &std::path::Path) {
        for pid in pids_holding(&process_snapshot().unwrap_or_default(), sock_path) {
            let _ = std::process::Command::new("kill")
                .arg("-9")
                .arg(pid.to_string())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    fn si(name: &str, age: Option<u64>) -> crate::session::SessionInfo {
        crate::session::SessionInfo { name: name.into(), pid: 1, age_secs: age, attached: 0 }
    }

    /// An unknown age must never be passed off as `0`. Before this, a session
    /// whose pid `ps` could not read contributed `0` to the max, and the strip
    /// rendered "oldest 0s" — claiming a possibly days-old shell had just
    /// started, on precisely the question a session age is asked for. The
    /// all-unknown case is the one that matters: the answer is "unknown", not
    /// "zero seconds".
    #[test]
    fn oldest_age_reports_unknown_rather_than_zero() {
        assert_eq!(oldest_age(&[]), None, "no sessions at all: nothing to report");
        assert_eq!(
            oldest_age(&[si("a", None), si("b", None)]),
            None,
            "every age unknown must stay unknown, not collapse to Some(0)"
        );
        assert_eq!(
            oldest_age(&[si("a", None), si("b", Some(7200)), si("c", None)]),
            Some(7200),
            "a known age must win over unknown siblings rather than being dragged to 0"
        );
        assert_eq!(oldest_age(&[si("a", Some(5)), si("b", Some(90))]), Some(90));
        // The specific regression: one unknown next to one genuinely new
        // session must not report the unknown one as the oldest.
        assert_eq!(oldest_age(&[si("a", None), si("b", Some(3))]), Some(3));
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

    // Side effect of C2's fix, caught while implementing it: `.origin`
    // markers persist indefinitely (not just until the next sweep) for any
    // project ever confirmed — that's the whole point, it's what lets a
    // *later* pass tell "genuinely deleted" apart from "not under my
    // current roots". But `known_projects`'s sock/ scan used to treat any
    // directory under sock/ as evidence a project is "known"; without this
    // guard, a project closed long ago with no saved layout would show up
    // as a permanent phantom row forever, not merely for the brief window
    // an emptying-out directory used to cause before C2.
    #[test]
    fn known_projects_does_not_list_a_closed_project_with_only_an_origin_marker() {
        with_state(|_state| {
            let root = tempfile::tempdir().unwrap();
            let project_dir = root.path().join("onceopened");
            fs::create_dir_all(&project_dir).unwrap();

            // Simulates a project opened once (record_origin ran, as
            // session::attach does) and since closed with no saved layout:
            // sock/<key>/ now holds only the marker, no session files.
            record_origin("onceopened", &project_dir);

            let ps = known_projects(&[root.path().to_path_buf()]);
            assert!(
                !ps.iter().any(|p| p.key == "onceopened"),
                "a project with no live session and no saved layout must not be listed \
                 just because it was once confirmed to exist"
            );
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

    /// A session that survived a restart is not in this process's memory, but
    /// its socket is on disk. Reported live count must therefore never be
    /// *smaller* than the socket count.
    ///
    /// This regressed in production: preferring the in-memory list whenever it
    /// was non-empty meant that starting one new session in a project made the
    /// count drop to 1 even though two shells were running, hiding the older
    /// survivor — exactly the long-running session the strip exists to surface.
    #[test]
    fn a_restart_survivor_still_counts_even_once_a_new_session_exists() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("proj")).unwrap();
            let sock = state.join("sock/proj");
            fs::create_dir_all(&sock).unwrap();
            // The sockets must be genuinely process-backed. `known_projects`
            // runs `reconcile` first, whose job is to delete a socket no
            // process holds — so empty files would be reaped before the count
            // ever happened, and the fixture would prove nothing. Each holder
            // just carries the socket path in its argv, which is precisely what
            // `pids_holding` matches on.
            let mut holders = Vec::new();
            for name in ["shell", "claude"] {
                let sp = sock.join(name);
                fs::write(&sp, "").unwrap();
                holders.push(
                    std::process::Command::new("python3")
                        .args(["-c", "import time; time.sleep(30)"])
                        .arg(&sp)
                        .spawn()
                        .expect("spawn a socket holder"),
                );
            }
            // The marker must not be counted as a session.
            fs::write(sock.join(".origin"), root.path().join("proj").to_string_lossy().as_bytes())
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(500)); // appear in `ps`

            // The bug needs BOTH: something in this process's memory *and* more
            // sockets than that on disk. With an empty map both the old and new
            // logic fall through to the socket floor and agree — a fixture
            // without this attach passes either way and proves nothing.
            // `RESH_CMD=cat` means this creates no socket of its own (that
            // is gated on the command being dtach), which is exactly the shape
            // wanted: one in memory, two on disk.
            let _sg = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::set_var("RESH_CMD", "cat");
            let _att = crate::session::attach("proj", "shell", &root.path().join("proj")).unwrap();
            assert_eq!(
                crate::session::list_sessions("proj").len(),
                1,
                "fixture must have exactly one session in memory, or it tests nothing"
            );

            let ps = known_projects(&[root.path().to_path_buf()]);
            let found = ps.iter().find(|p| p.key == "proj").map(|p| p.live);
            for mut h in holders {
                let _ = h.kill();
                let _ = h.wait();
            }
            crate::session::kill_project("proj");
            std::env::remove_var("RESH_CMD");
            assert_eq!(
                found,
                Some(2),
                "both sockets must count: after a restart the in-memory map is a partial view, \
                 not an authoritative one (and `.origin` must not inflate it)"
            );
        });
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

    /// The mirror image of the test above, and the reason the control-byte
    /// skip exists. An identical stale socket under a key containing a raw
    /// newline must be left alone, because who holds a socket is only knowable
    /// from line-oriented `ps` output, and a newline in the path splits one
    /// process's argv across two lines — so a *live* holder is invisible and
    /// the socket would be unlinked out from under a running shell.
    ///
    /// `storage_key` escapes control bytes now, so this key can only come from
    /// an older build or a hand-made directory; that is exactly what the guard
    /// is for. Pairing it with `a_socket_with_no_live_process_is_reaped` is
    /// what makes it meaningful: the same fixture, differing only in the key,
    /// gets opposite treatment — so this cannot pass merely because reaping is
    /// broken in general.
    #[test]
    fn a_socket_under_a_control_byte_key_is_never_reaped() {
        with_state(|state| {
            let sock = state.join("sock/my\nproj");
            fs::create_dir_all(&sock).unwrap();
            fs::write(sock.join("shell"), "").unwrap();

            let report = reconcile(&[PathBuf::from("/nonexistent-root")]);

            assert!(
                sock.join("shell").exists(),
                "a socket under a key whose holder cannot be identified in `ps` output must be \
                 left in place — unlinking it would orphan a running shell"
            );
            assert_eq!(
                report.dead_sockets, 0,
                "and it must not be counted as reaped either"
            );
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
    /// resh process leaves behind. Returns `None` (rather than
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
    // resh process has no entry in `session`'s in-memory map, so
    // `session::kill_project` alone is a no-op. Uses a real `dtach` process
    // so this actually proves the OS-level pid is killed, not just forgotten
    // about by having its socket file deleted out from under it.
    //
    // Two `reconcile` passes, not one — this is the corrected shape after
    // C2: a key must be *positively confirmed* (a `.origin` marker written
    // while its directory genuinely resolved) before a later failure to
    // resolve counts as "gone". The first pass runs while "ghost-proj"
    // still exists on disk, purely to produce that confirmation; only the
    // second pass, after the directory is actually removed, is expected to
    // reap. Skipping the first pass (as an earlier version of this test
    // did, pointing roots at a directory that never contained "ghost-proj"
    // at all) is now indistinguishable from the cross-configuration case
    // C2 exists to protect — a key never confirmed under any roots this
    // process has seen — and must NOT be reaped.
    #[test]
    fn a_live_session_for_a_deleted_project_is_actually_killed_not_just_forgotten() {
        with_state(|state| {
            let sock_dir = state.join("sock/ghost-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            let root = tempfile::tempdir().unwrap();
            let project_dir = root.path().join("ghost-proj");
            fs::create_dir_all(&project_dir).unwrap(); // exists, for now

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }
            assert!(
                socket_has_process(&sock_path),
                "test setup: the detached process must be observable before reconcile runs"
            );

            // Confirming pass: the directory exists, so this must not reap
            // anything — and it's what writes the `.origin` marker the
            // second pass needs.
            let confirming = reconcile(&[root.path().to_path_buf()]);
            assert_eq!(confirming.gone_projects, 0, "must not be reaped while the directory still exists");
            assert!(socket_has_process(&sock_path), "must survive the confirming pass");

            fs::remove_dir_all(&project_dir).unwrap();

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
            kill_test_pids(&sock_path);
        });
    }

    // C3: a root that cannot be verified must never be mistaken for "the
    // project is gone". Without this guard, a transiently unreadable mount
    // or a RESH_ROOTS typo would make startup reconcile SIGKILL every
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

            kill_test_pids(&sock_path);

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

            kill_test_pids(&sock_path);
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

            kill_test_pids(&sock_path);
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

            kill_test_pids(&sock_path);
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

            kill_test_pids(&sock_path);
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

            kill_test_pids(&sock_path);
        });
    }

    // C1: the exact boundary parse_ps_snapshot must draw. Real subprocess
    // `Output`s, not hand-built ones — `false` and `true` are genuine OS
    // exit statuses, so this proves the check against what `ps` can
    // actually report, not a synthetic stand-in.
    #[test]
    fn parse_ps_snapshot_treats_failure_and_empty_output_as_unverifiable() {
        let failed = std::process::Command::new("false").output().unwrap();
        assert!(
            parse_ps_snapshot(&failed).is_none(),
            "a nonzero exit must be unverifiable, not \"no processes\""
        );

        let empty = std::process::Command::new("true").output().unwrap();
        assert!(
            parse_ps_snapshot(&empty).is_none(),
            "empty stdout must be unverifiable — a live host always lists something"
        );

        // The positive case, for contrast: real ps output must still parse.
        let real = std::process::Command::new("ps").args(["-Ao", "pid=,args="]).output().unwrap();
        assert!(parse_ps_snapshot(&real).is_some(), "real ps output must be treated as verifiable");
    }

    // C1, downstream: an unverifiable snapshot must never be treated as "no
    // process holds this socket". Before the fix, `process_snapshot`
    // returning an empty `Vec` on failure was indistinguishable from a
    // genuinely dead socket, so `kill_and_unlink` unlinked a live session's
    // socket on a `ps` hiccup. This injects `|| None` directly — no global
    // env var, so no risk of racing any other test — and would fail exactly
    // as the old code did if `None` were treated as `Some(vec![])`: the
    // socket would be deleted outright instead of surviving.
    #[test]
    fn kill_and_unlink_never_unlinks_when_the_process_list_is_unverifiable() {
        with_state(|state| {
            let sock_dir = state.join("sock/unverifiable");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            // A plain file stands in for a real socket; nothing needs to
            // actually hold it — the point is that "can't tell" must never
            // be treated as "safe to delete" regardless.
            fs::write(&sock_path, "").unwrap();

            let ok = kill_and_unlink_with(&sock_path, &|| None);

            assert!(!ok, "an unverifiable process list must never report success");
            assert!(sock_path.exists(), "the socket must survive when the process list can't be trusted");
        });
    }

    // C1, through the whole sweep: the same guarantee, exercised via
    // `reconcile_with` rather than `kill_and_unlink_with` directly, so the
    // "bail out of the entire pass" behavior for an unverifiable top-level
    // snapshot is proven too, not just the per-socket helper.
    #[test]
    fn reconcile_never_reaps_anything_when_the_process_list_is_unverifiable() {
        with_state(|state| {
            let sock_dir = state.join("sock/unverifiable-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            fs::write(&sock_path, "").unwrap();

            let report = reconcile_with(&[PathBuf::from("/nonexistent-root")], &|| None);

            assert_eq!(
                report.dead_sockets, 0,
                "an unverifiable process list must never be treated as \"nothing holds this socket\""
            );
            assert_eq!(report.gone_projects, 0);
            assert!(sock_path.exists(), "the socket must survive when the process list is unverifiable");
        });
    }

    // C2: reproduced directly against real dtach, outside the test suite
    // entirely — a session for a project genuinely on disk was SIGKILLed
    // and its socket unlinked simply because reconcile was run with
    // different RESH_ROOTS than what created the session, sharing the
    // same RESH_STATE_DIR (exactly what docs/deploy.md tells users to
    // do to run a second instance locally). "Not resolvable under my
    // roots" is not evidence of "gone"; only a key whose recorded
    // directory is confirmed missing counts.
    //
    // Goes through the real `session::attach` (not a raw `dtach -n` spawn
    // like the other tests in this file use) so the whole production path is
    // exercised. It does *not*, on its own, prove `attach` records anything:
    // survival is equally true with no marker at all, since a key never
    // confirmed anywhere is also never reaped — a claim this comment used to
    // make wrongly. `attach_records_an_origin_...` below is the test that
    // actually pins that down, from the other side.
    #[test]
    fn a_session_outside_the_configured_roots_survives_reconcile() {
        with_state(|state| {
            let _g = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("RESH_CMD");
            let real_root = tempfile::tempdir().unwrap();
            let project_dir = real_root.path().join("victimproj");
            fs::create_dir_all(&project_dir).unwrap();

            let Ok(_att) = crate::session::attach("victimproj", "shell", &project_dir) else {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            };
            let sock_path = state.join("sock/victimproj/shell");
            let mut waited = 0;
            while !(sock_path.exists() && socket_has_process(&sock_path)) && waited < 200 {
                std::thread::sleep(std::time::Duration::from_millis(25));
                waited += 1;
            }
            assert!(
                socket_has_process(&sock_path),
                "test setup: the session must be observable before reconcile runs"
            );

            // A completely different root than the one victimproj actually
            // lives under — simulating a second resh instance (or the
            // same one, restarted with an overridden RESH_ROOTS)
            // sharing this state dir.
            let foreign_root = tempfile::tempdir().unwrap();
            let report = reconcile(&[foreign_root.path().to_path_buf()]);

            assert_eq!(
                report.gone_projects, 0,
                "a project outside the current roots must never be treated as gone"
            );
            assert!(sock_path.exists(), "its socket must survive");
            assert!(socket_has_process(&sock_path), "its process must survive");

            kill_test_pids(&sock_path);
        });
    }

    // The other half of C2's marker mechanism, and the one nothing covered:
    // `session::attach`'s call to `record_origin`. Deleting that call left
    // the entire suite green, because the cross-roots test above only asserts
    // *survival* — which also holds with no marker at all. So this asserts
    // from the opposite side: with the real roots, a project whose directory
    // is genuinely deleted must still be reaped, and the only thing that can
    // have recorded its origin is `attach` itself — this test deliberately
    // never runs a `reconcile` pass while the directory still exists, so no
    // pass can have written the marker on its behalf.
    #[test]
    fn attach_records_an_origin_that_lets_a_genuinely_deleted_project_be_reaped() {
        with_state(|state| {
            let _g = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("RESH_CMD");
            let root = tempfile::tempdir().unwrap();
            let project_dir = root.path().join("attachreap");
            fs::create_dir_all(&project_dir).unwrap();

            let Ok(_att) = crate::session::attach("attachreap", "shell", &project_dir) else {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            };
            let sock_dir = state.join("sock/attachreap");
            let sock_path = sock_dir.join("shell");
            let mut waited = 0;
            while !(sock_path.exists() && socket_has_process(&sock_path)) && waited < 200 {
                std::thread::sleep(std::time::Duration::from_millis(25));
                waited += 1;
            }
            assert!(
                socket_has_process(&sock_path),
                "test setup: the session must be observable before reconcile runs"
            );
            assert_eq!(
                fs::read(origin_marker_path(&sock_dir)).ok(),
                Some(marker_bytes(&project_dir)),
                "attach must record the exact resolved directory it spawned the session in"
            );

            fs::remove_dir_all(&project_dir).unwrap();
            let report = reconcile(&[root.path().to_path_buf()]);

            assert!(
                report.gone_projects >= 1,
                "a session whose project directory is genuinely deleted must be reaped"
            );
            assert!(
                !socket_has_process(&sock_path),
                "the dtach master must actually be killed, not merely forgotten"
            );
            assert!(!sock_path.exists(), "the socket goes only once the process is confirmed gone");
        });
    }

    // C3 and I5 at their source. Both were one expression:
    // `!Path::new(recorded.trim()).is_dir()`, which answers "gone" — the one
    // verdict that causes SIGKILL and unlink — for an empty marker and for a
    // path it merely failed to check. Every case below except the first must
    // be `false`; the first is here so the rest cannot pass by the function
    // having simply stopped ever returning `true`.
    #[test]
    fn only_a_confirmed_missing_recorded_path_counts_as_gone() {
        let d = tempfile::tempdir().unwrap();

        let deleted = d.path().join("deleted");
        assert!(
            recorded_path_is_gone(&marker_bytes(&deleted)),
            "an absent recorded path is the one case that must reap — without this the \
             rest of this test would prove nothing"
        );

        // C3: `Path::new("").is_dir()` is false, so this used to answer
        // "gone". A truncated write (the old non-atomic `fs::write`, read by
        // a second instance mid-truncate) and a crash or ENOSPC mid-write
        // both produce exactly this.
        assert!(!recorded_path_is_gone(b""), "an empty marker records nothing at all");
        assert!(!recorded_path_is_gone(b"\n"), "nor does one holding only a newline");

        let existing = d.path().join("existing");
        fs::create_dir(&existing).unwrap();
        assert!(!recorded_path_is_gone(&marker_bytes(&existing)));
        let mut with_newline = marker_bytes(&existing);
        with_newline.push(b'\n');
        assert!(
            !recorded_path_is_gone(&with_newline),
            "a trailing newline is not part of the path (older or hand-edited markers)"
        );

        // A directory name may legally end in a space here (only *session*
        // names are restricted), so the old `.trim()` looked up a path that
        // never existed and answered "gone" for a project sitting right there.
        let spaced = d.path().join("trailing space ");
        fs::create_dir(&spaced).unwrap();
        assert!(
            !recorded_path_is_gone(&marker_bytes(&spaced)),
            "a legal trailing space must survive into the lookup"
        );

        // I5: cannot tell. Reproduced with a path under a regular file
        // (ENOTDIR) rather than a chmod (EACCES) because it behaves
        // identically for root and non-root and needs no cleanup — the
        // branch is the same one an unmounted disk or a downed NFS mount
        // reaches. Asserting on *why* it errors, per CLAUDE.md: if this were
        // NotFound the case would be vacuous.
        let not_a_dir = d.path().join("regular-file");
        fs::write(&not_a_dir, "").unwrap();
        let under_a_file = not_a_dir.join("inner");
        let err = std::fs::symlink_metadata(&under_a_file)
            .expect_err("a path under a regular file must not be stattable");
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "this case only means something if the error is *other* than NotFound; got {err:?}"
        );
        assert!(
            !recorded_path_is_gone(&marker_bytes(&under_a_file)),
            "\"I could not check\" must never be treated as \"it is gone\""
        );
    }

    /// The end-to-end shape the two damaged-marker regressions share: a real
    /// `dtach` session, a `.origin` that is not positive evidence of
    /// anything, and a `reconcile` under *foreign* roots — the multi-instance
    /// setup the marker exists for, and the only situation in which the
    /// marker is consulted at all. Nothing may be reaped.
    ///
    /// A plain file cannot stand in for the socket here (as it can in this
    /// module's `ps`-failure tests): with no process holding it, `reconcile`
    /// removes it via the dead-socket branch whatever the project verdict, so
    /// the test would pass with or without the fix.
    fn a_marker_proving_nothing_must_not_cause_a_reap(key: &str, marker: &[u8]) {
        with_state(|state| {
            let sock_dir = state.join("sock").join(key);
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");

            if spawn_detached_dtach(&sock_path).is_none() {
                eprintln!("dtach not available; skipping (it is a runtime prerequisite elsewhere)");
                return;
            }
            assert!(
                socket_has_process(&sock_path),
                "test setup: the detached process must be observable before reconcile runs"
            );
            fs::write(origin_marker_path(&sock_dir), marker).unwrap();

            let foreign_root = tempfile::tempdir().unwrap();
            let report = reconcile(&[foreign_root.path().to_path_buf()]);

            assert_eq!(
                report.gone_projects, 0,
                "a marker that proves nothing must never reap a live session"
            );
            assert!(sock_path.exists(), "its socket must survive");
            assert!(socket_has_process(&sock_path), "its process must survive");

            kill_test_pids(&sock_path);
        });
    }

    // C3, end to end: this is what a second instance reads inside the old
    // truncate-then-write window, and what a crash mid-write leaves behind
    // permanently.
    #[test]
    fn a_zero_length_origin_marker_never_reaps_a_live_session() {
        a_marker_proving_nothing_must_not_cause_a_reap("truncated-proj", b"");
    }

    // I5, end to end: the recorded path is there in the marker, but the
    // filesystem cannot answer whether it exists.
    #[test]
    fn an_uncheckable_origin_marker_never_reaps_a_live_session() {
        let d = tempfile::tempdir().unwrap();
        let not_a_dir = d.path().join("regular-file");
        fs::write(&not_a_dir, "").unwrap();
        let recorded = not_a_dir.join("inner");
        let err = std::fs::symlink_metadata(&recorded)
            .expect_err("a path under a regular file must not be stattable");
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "this test only means something if the error is *other* than NotFound; got {err:?}"
        );
        a_marker_proving_nothing_must_not_cause_a_reap("uncheckable-proj", &marker_bytes(&recorded));
    }

    // The marker is the only record of where a project lives, so its write
    // must be all-or-nothing: no reader may ever observe it empty or
    // half-written (that is C3's second, permanent form — a crash or ENOSPC
    // mid-`fs::write`). Proven by the artifact the atomic path leaves: the
    // content arrives by `rename`, so a temp file appears and disappears
    // rather than the marker itself being truncated first. Also pins the
    // byte-exactness a non-UTF8 path needs, and the 0600 mode.
    #[test]
    fn the_origin_marker_is_written_atomically_and_privately() {
        let d = tempfile::tempdir().unwrap();
        let key_dir = d.path().join("key");
        fs::create_dir_all(&key_dir).unwrap();
        let marker = origin_marker_path(&key_dir);

        write_origin(&key_dir, std::path::Path::new("/some/where"));
        assert_eq!(fs::read(&marker).unwrap(), b"/some/where");
        let left_behind: Vec<String> = fs::read_dir(&key_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != ".origin")
            .collect();
        assert!(left_behind.is_empty(), "no temp file may be left behind; found {left_behind:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&marker).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the marker grants no access to anyone else");
        }

        // An identical rewrite is skipped entirely (reconcile reconfirms the
        // same resolution every few seconds, forever), but a differing one —
        // including repairing a marker some older, non-atomic writer
        // truncated — must go through. Asserted on the inode rather than a
        // timestamp: `rename` necessarily installs a *different* inode (the
        // temp file is created while the old marker still exists, so it
        // cannot have the same one), which makes this exact rather than
        // dependent on filesystem timestamp granularity.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let ino = fs::metadata(&marker).unwrap().ino();
            write_origin(&key_dir, std::path::Path::new("/some/where"));
            assert_eq!(
                fs::metadata(&marker).unwrap().ino(),
                ino,
                "an unchanged marker must not be rewritten at all"
            );
            fs::write(&marker, b"").unwrap(); // as a crash mid-write would leave it
            write_origin(&key_dir, std::path::Path::new("/some/where"));
            assert_eq!(fs::read(&marker).unwrap(), b"/some/where", "a damaged marker must be repaired");
            assert_ne!(
                fs::metadata(&marker).unwrap().ino(),
                ino,
                "the repair must arrive by rename, not by truncating the marker in place"
            );
        }
    }
}
