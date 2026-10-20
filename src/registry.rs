//! The project registry: what roost knows about, and what is running.
//!
//! Rebuilt at startup rather than accumulated in memory, because dtach
//! sessions deliberately outlive roost. An in-memory-only registry would
//! forget every running shell on restart — which is exactly how nine
//! orphaned sessions for deleted directories accumulated unnoticed in
//! production on 2026-08-17.
use std::path::PathBuf;

#[derive(Debug, Clone)]
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
    /// The real directory of a worktree outside every root that the user
    /// allowed (#188, `outside.rs`) and whose record still resolves with its
    /// pinned identity. Such a row's `url` is the virtual
    /// `<parent>/.claude/outside/<name>`, so this is the only place the real
    /// path survives — for the tooltip, nothing else. `None` for every other
    /// row, including an outside worktree whose record no longer matches:
    /// that one is `reachable: false` again, offered Allow… afresh.
    pub outside: Option<String>,
    /// Populated only by `known_projects_with_state`, only for reachable
    /// linked worktrees. `known_projects` always leaves this `None` — it is
    /// two git calls per worktree, paid only when the switcher panel opens.
    pub wt: Option<WorktreeStatus>,
}

/// Per-worktree state for the switcher panel. Groups the four things a row
/// can say into one `Option` on `ProjectStatus`, so a main-worktree entry
/// (or a plain project) carries no half-filled state — there is nothing to
/// leave blank, just `None`.
#[derive(Debug, Clone)]
pub struct WorktreeStatus {
    pub claude: crate::claudes::ClaudeEvidence,
    pub dirty: Option<bool>,
    pub ahead: Option<u32>,
    /// The branch this worktree's ahead-count is measured against.
    pub base: String,
    /// True when `base` came from the `.base` file roost wrote at
    /// `create` time; false when it fell back to the main worktree's
    /// current branch (a worktree roost did not create, or a lost record).
    pub base_recorded: bool,
}

/// Can this worktree be offered for removal? Every axis positively clean;
/// a single unknown says no. The hub re-derives this at the moment of the
/// intent — the row is a hint, not an authorisation.
pub fn removable(w: &WorktreeStatus, live: usize) -> bool {
    live == 0
        && w.claude == crate::claudes::ClaudeEvidence::Absent
        && w.dirty == Some(false)
        && w.ahead == Some(0)
}

/// `known_projects` plus per-worktree state. Costs two git calls per linked
/// worktree, so it is requested only when the switcher panel opens.
pub fn known_projects_with_state(roots: &[PathBuf]) -> Vec<ProjectStatus> {
    let mut ps = known_projects(roots);
    let state_dir = crate::wsstate::state_dir();
    let main_branch: std::collections::HashMap<String, String> =
        ps.iter().filter(|p| p.parent.is_none()).map(|p| (p.key.clone(), p.branch.clone())).collect();
    // A worktree's directory comes from git's own listing of its parent, not
    // from `resolve_project`: that refuses dot segments (`.claude/…`), and
    // `is_vouched_worktree` is an exception to *naming* only. Keyed by the
    // child's storage key, not its bare branch name: this codebase's own
    // auto-naming (claude-1, claude-2, … per repo) makes two different
    // projects sharing a branch name the expected common case, and a
    // branch-keyed map would silently hand one project's directory (and
    // thus its dirty/ahead state) to another's row.
    let mut dirs: std::collections::HashMap<String, PathBuf> = Default::default();
    for p in ps.iter().filter(|p| p.parent.is_none()) {
        if let Some(pd) = crate::projects::resolve_project(roots, &p.url) {
            for w in crate::worktree::list(&pd).into_iter().filter(|w| !w.is_main) {
                match rel_under_roots(roots, &w.path) {
                    Some(child_url) => {
                        if let Ok(c) = w.path.canonicalize() {
                            dirs.insert(crate::projects::storage_key(&child_url), c);
                        }
                    }
                    // An allowed outside worktree (#188): its row is keyed
                    // by the virtual URL, and its state is the real
                    // directory's, which `resolve` just vouched for.
                    None => {
                        if let Some((virt, real)) = allowed_outside(&p.url, &w.path) {
                            dirs.insert(crate::projects::storage_key(&virt), PathBuf::from(real));
                        }
                    }
                }
            }
        }
    }
    // One /proc walk for every worktree row, not one per row.
    let scan = crate::claudes::claude_terminals(std::path::Path::new("/proc"));
    for p in ps.iter_mut() {
        let Some(parent) = p.parent.as_deref() else { continue };
        if !p.reachable { continue }
        let Some(dir) = dirs.get(&p.key).cloned() else { continue };
        let (base, base_recorded) = match crate::worktree::read_base(&state_dir, &p.key) {
            Some(b) => (b, true),
            None => (main_branch.get(parent).cloned().unwrap_or_default(), false),
        };
        // `dirty` never depends on knowing a base — only `ahead` does. When
        // no base is known (no record, and no main-worktree branch to fall
        // back to), a null sha is passed as the range endpoint: `git
        // rev-list` genuinely cannot resolve it (unlike an empty string,
        // which `..HEAD` silently accepts as shorthand and answers `0` for
        // — that would be exactly the "unknown read as clean" bug this
        // codebase keeps shipping), so `state`'s own `.ok()` plumbing
        // naturally yields `ahead: None` while `status --porcelain` still
        // runs unconditionally and answers `dirty` normally.
        const NULL_SHA: &str = "0000000000000000000000000000000000000000";
        let base_for_ahead = if base.is_empty() { NULL_SHA } else { base.as_str() };
        let st = crate::worktree::state(&dir, base_for_ahead, &crate::worktree::real_git);
        p.wt = Some(WorktreeStatus {
            claude: crate::claudes::claude_evidence_with_scan(&p.url, &scan),
            dirty: st.dirty,
            ahead: st.ahead,
            base,
            base_recorded,
        });
    }
    ps
}

pub struct ReapReport {
    pub dead_sockets: usize,
    pub gone_projects: usize,
    /// Projects this pass found renamed rather than deleted (#178's inode
    /// evidence) whose new directory resolves under the roots. Acted on by
    /// [`follow_moves`], never inside the sweep: following refreshes hubs,
    /// and the sweep runs under `reconcile_gate`.
    pub moved: Vec<Moved>,
}

/// One followable rename: `old_url`'s directory is now `new_url`'s, and
/// `live` are the key's live sessions. Which of them follow, and where, is
/// each one's own shell's say (`shells`, #183 §3). `fresh` when this pass
/// wrote or rewrote `.moved-to` — the only pass that announces; a later
/// pass seeing the same unchanged record has nothing new to say, and only
/// finishes a follow left undone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub old_url: String,
    pub new_url: String,
    pub live: Vec<String>,
    pub fresh: bool,
    /// The URL `.moved-to` named before this pass rewrote it — the target
    /// of an earlier follow, now itself renamed away (A→B→C). Records
    /// re-pointed at it by that follow are re-pointed at `new_url`.
    pub prev: Option<String>,
    /// The `live` sessions whose session leader's cwd is under a directory
    /// carrying one of the key's full identities, with that directory — the
    /// deepest such ancestor. Not necessarily `new_url`'s: a key whose old
    /// name held an occupant has sessions in the directory followed earlier
    /// *and* in wherever the occupant went, and each follows its own. Not
    /// yet resolved to a URL: that may run git, and this is computed under
    /// `reconcile_gate`.
    pub shells: Vec<(String, PathBuf)>,
}

/// The follow-up to a sweep's [`ReapReport::moved`] (#158 §5): re-home each
/// renamed key's sessions (`home::follow_move`), refresh both projects' hubs
/// and tell their browsers. Must run with no lock held — not `REGISTRY`,
/// not `SESSIONS`, not a hub, not `reconcile_gate`: each call below takes
/// its own lock briefly and releases it before the next, which is what
/// keeps a hub thread that reaches `reconcile` from deadlocking against it.
pub fn follow_moves(moved: &[Moved]) {
    // Every move, not only fresh ones, and first: a Claude in a followed
    // session goes on editing whether or not anyone reopens a page, and the
    // old key's listener — built over the directory that was renamed away —
    // refuses every path under the new one until it is retargeted. Cheap
    // when there is nothing to do: a registry lookup, and the resolve (which
    // may run git) only for a key that has a listener at all. Runs with no
    // lock held, like the rest of this function.
    for m in moved {
        if crate::ide::workspace_of(&m.old_url).is_none() {
            continue;
        }
        if let Some(dir) = crate::projects::resolve_project(&crate::projects::roots(), &m.new_url) {
            if crate::ide::retarget(&m.old_url, &dir) {
                eprintln!("roost: the IDE listener for {} now serves {}", m.old_url, dir.display());
            }
        }
    }
    for m in moved {
        // Off every lock: resolving may run git. Each session's own
        // directory, as a project under the roots that is not the key's
        // current one — the only evidence a native session follows on.
        let roots = crate::projects::roots();
        let new_dir = crate::projects::resolve_project(&roots, &m.new_url);
        let shells = shells_at(&roots, m, new_dir.as_deref());
        let targets: std::collections::BTreeSet<&str> =
            shells.iter().map(|s| s.url.as_str()).filter(|u| *u != m.new_url).collect();
        let refresh_targets = || {
            for t in &targets {
                crate::hub::refresh_and_broadcast(t);
                crate::hub::broadcast_all(&crate::proto::Event::ProjectsChanged { project: t.to_string() });
            }
        };
        if !m.fresh {
            // Not news, but maybe unfinished: a session that did not follow
            // on the fresh pass (a failed write, a restart in between, or
            // an occupant renamed while the first target still stands) is
            // followed now. Quietly — the banner went out once already —
            // and only when something actually changed.
            if crate::home::own_follow_pending(&m.old_url, &m.new_url, new_dir.as_deref(), &m.live, &shells, m.prev.as_deref())
                && crate::home::follow_move(&m.old_url, &m.new_url, new_dir.as_deref(), &m.live, &shells, m.prev.as_deref())
            {
                eprintln!("roost: {}'s sessions caught up with their shells", m.old_url);
                crate::hub::refresh_and_broadcast(&m.old_url);
                crate::hub::refresh_and_broadcast(&m.new_url);
                refresh_targets();
            }
            continue;
        }
        crate::home::follow_move(&m.old_url, &m.new_url, new_dir.as_deref(), &m.live, &shells, m.prev.as_deref());
        eprintln!("roost: {} was renamed to {}; its {} session(s) follow their own shells", m.old_url, m.new_url, m.live.len());
        // `git worktree move` touches only `.git/worktrees/<id>/gitdir`,
        // which the watcher ignores, so the main checkout's hub still knows
        // the old path — and would announce the new one as having appeared
        // on its next unrelated worktree event.
        if let Some(new_dir) = &new_dir {
            let key_dir = sock_root().join(crate::projects::storage_key(&m.old_url));
            let mut olds: Vec<PathBuf> = std::fs::read(origin_marker_path(&key_dir))
                .ok()
                .map(|b| path_from_marker(b.strip_suffix(b"\n").unwrap_or(&b)))
                .into_iter()
                .collect();
            if let Some(p) = &m.prev {
                // The earlier follow put this path in the known set, built
                // the same way: a root, canonicalised, joined with the URL.
                olds.extend(crate::projects::roots().iter().filter_map(|r| r.canonicalize().ok()).map(|r| r.join(p)));
            }
            crate::hub::rename_known_worktree(&olds, new_dir);
        }
        crate::hub::refresh_and_broadcast(&m.old_url);
        crate::hub::refresh_and_broadcast(&m.new_url);
        refresh_targets();
        let ev = crate::proto::Event::ProjectMoved { from: m.old_url.clone(), to: m.new_url.clone() };
        crate::hub::broadcast_to_project(&m.old_url, &ev);
        crate::hub::broadcast_to_project(&m.new_url, &ev);
        crate::hub::broadcast_all(&crate::proto::Event::ProjectsChanged { project: m.new_url.clone() });
    }
}

/// `m.shells` turned into URLs: each directory confirmed by the resolver
/// to be a project under the roots, and not the key's own current one (a
/// shell in the directory standing at the old name is that directory's,
/// and has nowhere to follow: its URL, taken from the same canonical path
/// the resolver returns, is the old URL itself). One that fails stays out of the list, which
/// leaves its session where it is — no evidence, never a guess. Runs git
/// for a worktree URL, so never under a lock; `new_dir` is reused rather
/// than resolved twice.
fn shells_at(roots: &[PathBuf], m: &Moved, new_dir: Option<&std::path::Path>) -> Vec<crate::home::ShellAt> {
    let mut by_dir: std::collections::BTreeMap<&std::path::Path, Option<String>> = Default::default();
    m.shells
        .iter()
        .filter_map(|(name, dir)| {
            let url = by_dir
                .entry(dir.as_path())
                .or_insert_with(|| {
                    url_under_roots(roots, dir).filter(|u| *u != m.old_url).filter(|u| {
                        let got = if *u == m.new_url { new_dir.map(|d| d.to_path_buf()) } else { crate::projects::resolve_project(roots, u) };
                        got.as_deref() == Some(dir.as_path())
                    })
                })
                .clone()?;
            Some(crate::home::ShellAt { name: name.clone(), url, dir: dir.clone() })
        })
        .collect()
}

/// `dir` as the project URL it would be under the first root containing
/// it — `/`-separated, relative — or `None` when it is under no root (or is
/// a root itself, or is not UTF-8). The caller still has to confirm it with
/// `projects::resolve_project`: being under a root is not being a project.
pub(crate) fn url_under_roots(roots: &[PathBuf], dir: &std::path::Path) -> Option<String> {
    for r in roots {
        let Ok(base) = r.canonicalize() else { continue };
        let Ok(rel) = dir.strip_prefix(&base) else { continue };
        let segs: Option<Vec<&str>> = rel.components().map(|c| match c {
            std::path::Component::Normal(s) => s.to_str(),
            _ => None,
        }).collect();
        let segs = segs?;
        if segs.is_empty() {
            return None;
        }
        return Some(segs.join("/"));
    }
    None
}

/// Whether `key_dir` holds anything but dotfiles — a session socket, live or
/// not yet reaped. "Cannot look" answers yes: it only ever keeps a file.
fn key_has_sockets(key_dir: &std::path::Path) -> bool {
    match std::fs::read_dir(key_dir) {
        Ok(rd) => rd.flatten().any(|e| !e.file_name().to_string_lossy().starts_with('.')),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// What one key's sessions say about its directory having been renamed,
/// once the evidence has been turned into a record (#158 §5).
#[derive(Debug)]
pub(crate) enum MoveRecord {
    /// No evidence of a move: the sweep proceeds as before #178.
    NoEvidence,
    /// Could not tell — or moved followably but `.moved-to` could not be
    /// written; suspend anything that would act on "not moved".
    CannotTell,
    /// Moved, but not somewhere roost can follow (outside the roots). Alive,
    /// stranded, as #178 leaves it.
    Unfollowable,
    /// Moved to a project under the roots; `.moved-to` now names it.
    Followable(Moved),
}

/// Evidence to record: `move_evidence` on `key_dir`, the new directory's URL
/// confirmed by the resolver itself, and `.moved-to` written — what
/// `reconcile` decides about a key as a whole. Never what moves a resident's
/// record: the R18 sweep asks [`find_by_identity`] about that record's own
/// directory instead (#183 §1), since a key may since have held an occupant.
/// Takes no lock; may run git (through `resolve_project` on a worktree URL).
pub(crate) fn record_move_with(
    roots: &[PathBuf],
    old_url: &str,
    key_dir: &std::path::Path,
    snapshot: &[(u32, String)],
    proc_root: &std::path::Path,
    log: bool,
) -> MoveRecord {
    // `log` is reconcile's: the R18 sweep words (and rate-limits) its own,
    // since "not reaping" is the wrong verb for a record it is not forgetting.
    let say = |m: String| if log { eprintln!("{m}") };
    let key = key_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // The record first, the shells second: once a rename has been followed,
    // its sessions' shells are free to `cd` out of the directory, and the
    // record plus the directory's own inode is still positive evidence.
    let mut live: Vec<String> = std::fs::read_dir(key_dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| crate::session::valid_name(n))
                .filter(|n| !pids_holding(snapshot, &key_dir.join(n)).is_empty())
                .collect()
        })
        .unwrap_or_default();
    live.sort();
    // And the directory's siblings last (#182): a rename made while no
    // shell was under the inode — the first one, or a later hop after a
    // `cd` — leaves the directory itself as the only witness. Only for a
    // key that still holds a live session, which is what this evidence
    // exists to save from a reap: with none, the recorded identity may
    // describe a directory long since followed and left, and a new
    // directory's resident must not be handed to it (the scan found B for
    // `a_moved_to_does_not_outlive_its_sessions` before this gate).
    let evidence = moved_to_evidence(roots, key_dir).unwrap_or_else(|| match move_evidence(key_dir, snapshot, proc_root) {
        MoveEvidence::None if !live.is_empty() => sibling_evidence(roots, key_dir),
        e => e,
    });
    let new_dir = match evidence {
        MoveEvidence::None => return MoveRecord::NoEvidence,
        MoveEvidence::CannotTell => {
            say(format!("roost: not reaping {key} — its directory is gone, but whether it was moved cannot be told from its sessions' shells"));
            return MoveRecord::CannotTell;
        }
        MoveEvidence::Moved { dir, legacy: true } => {
            say(format!(
                "roost: not reaping {key} — its directory was moved to {}, not deleted; matched only by a pre-birth-time identity, not following",
                dir.display()
            ));
            return MoveRecord::Unfollowable;
        }
        MoveEvidence::Moved { dir, legacy: false } => dir,
    };
    // #178: a live shell is still inside the same directory under another
    // name. Followed only when that name is a project under the roots.
    let new_url = url_under_roots(roots, &new_dir)
        .filter(|u| u != old_url)
        .filter(|u| crate::projects::resolve_project(roots, u).as_deref() == Some(new_dir.as_path()));
    let Some(new_url) = new_url else {
        say(format!(
            "roost: not reaping {key} — its directory was moved to {}, not deleted; moved outside the roots, not following",
            new_dir.display()
        ));
        return MoveRecord::Unfollowable;
    };
    say(format!("roost: not reaping {key} — its directory was moved to {new_url}, not deleted"));
    // The target this key was followed to before, when this pass moves it
    // on (a second rename). Only while that URL no longer resolves: a
    // directory standing there now is a different one — the evidence above
    // would have named it otherwise — and records naming it are its own.
    let prev = std::fs::read(moved_to_marker_path(key_dir))
        .ok()
        .and_then(|b| parse_moved_to(&b))
        .filter(|p| *p != new_url)
        // Positively absent, not merely unresolved: a resolver that ran git
        // and failed would otherwise hand a live directory's records on.
        .filter(|p| roots.iter().all(|r| matches!(std::fs::symlink_metadata(r.join(p)), Err(e) if e.kind() == std::io::ErrorKind::NotFound)));
    // Each session's own evidence (#183 §3), read here because the snapshot
    // is; turned into URLs by `follow_moves`, off the gate.
    let ids = identities(key_dir);
    let shells: Vec<(String, PathBuf)> =
        live.iter().filter_map(|n| shell_dir(&key_dir.join(n), snapshot, proc_root, &ids).map(|d| (n.clone(), d))).collect();
    match write_moved_to(key_dir, &new_url) {
        Ok(fresh) => MoveRecord::Followable(Moved { old_url: old_url.to_string(), new_url, live, fresh, prev, shells }),
        // Positive evidence of a followable rename that could not be
        // recorded is not "unfollowable": R18 must keep the record and try
        // again, not forget a terminal whose worktree is alive under B.
        Err(e) => {
            say(format!("roost: {key} was moved to {new_url}, but its .moved-to could not be written ({e}); not following yet"));
            MoveRecord::CannotTell
        }
    }
}

/// The directory the session at `sock`'s shell is working in, as far as
/// the key can vouch for it: the deepest ancestor of its session leader's
/// cwd carrying one of `ids` as a *full* identity (a legacy one never
/// licenses a follow). `None` when no shell is under any of them, when
/// they disagree (two leaders in two of the key's directories say nothing
/// about which one the terminal is), or when anything cannot be read: this
/// only ever licenses writing a record.
#[cfg(unix)]
fn shell_dir(sock: &std::path::Path, snapshot: &[(u32, String)], proc_root: &std::path::Path, ids: &[crate::dirid::Stored]) -> Option<PathBuf> {
    if ids.is_empty() {
        return None;
    }
    let own = match crate::procsess::session_of(proc_root, std::process::id()) {
        crate::procsess::Sid::In(s) => Some(s),
        _ => None,
    };
    let shells = crate::procsess::target_sessions(proc_root, &pids_holding(snapshot, sock), own)?;
    let found: std::collections::BTreeSet<PathBuf> = shells
        .iter()
        .filter_map(|shell| {
            let cwd = std::fs::read_link(proc_root.join(shell.to_string()).join("cwd")).ok()?;
            cwd.ancestors()
                .find(|p| std::fs::symlink_metadata(p).is_ok_and(|m| hit(ids, &m) == Hit::Full))
                .map(|p| p.to_path_buf())
        })
        .collect();
    let mut it = found.into_iter();
    match (it.next(), it.next()) {
        (Some(d), None) => Some(d),
        _ => None,
    }
}

#[cfg(not(unix))]
fn shell_dir(_sock: &std::path::Path, _snapshot: &[(u32, String)], _proc_root: &std::path::Path, _ids: &[crate::dirid::Stored]) -> Option<PathBuf> {
    None
}

fn moved_to_marker_path(key_dir: &std::path::Path) -> PathBuf {
    key_dir.join(".moved-to")
}

/// A `.moved-to`'s URL: one line, relative, no control byte, no `.`/`..`
/// or empty segment. Anything else is damaged, and `None`.
fn parse_moved_to(bytes: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(bytes).ok()?;
    let s = s.strip_suffix('\n').unwrap_or(s);
    if s.is_empty() || s.starts_with('/') || s.bytes().any(|b| b.is_ascii_control()) {
        return None;
    }
    if s.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..") {
        return None;
    }
    Some(s.to_string())
}

/// `sock/<key>/` for a project URL — where its sockets and markers live.
pub(crate) fn key_dir_of(url: &str) -> PathBuf {
    sock_root().join(crate::projects::storage_key(url))
}

/// The recorded identity of the key's directory (`.origin-ino`): `Full`, or
/// `Legacy` from a roost that predates birth time (#183). `None` for a
/// missing, unreadable or damaged marker — every caller treats all three as
/// "no identity", which only ever withholds evidence.
pub(crate) fn recorded_identity(key_dir: &std::path::Path) -> Option<crate::dirid::Stored> {
    identity_at(&origin_identity_path(key_dir))
}

/// A one-identity marker. A second non-blank line makes it damaged rather
/// than silently read as its first: `parse_line` splits on whitespace, so
/// two two-field lines would otherwise read as one four-field identity.
fn identity_at(path: &std::path::Path) -> Option<crate::dirid::Stored> {
    let s = std::fs::read_to_string(path).ok()?;
    let mut lines = s.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    crate::dirid::parse_line(first)
}

/// `.occupant-ino`'s identities, oldest first. A damaged line contributes
/// nothing — the same as a damaged single-slot marker did before #183 — and
/// an unreadable file contributes nothing either.
fn occupants(key_dir: &std::path::Path) -> Vec<crate::dirid::Stored> {
    std::fs::read_to_string(occupant_identity_path(key_dir))
        .map(|s| s.lines().filter_map(crate::dirid::parse_line).collect())
        .unwrap_or_default()
}

/// Every directory identity the key's live sessions may be working in: the
/// recorded one, and every occupant's that held the old name while a
/// `.moved-to` lived (#183 §3 — a list, so a third directory cannot erase a
/// second's). Empty means no evidence can be had at all.
pub(crate) fn identities(key_dir: &std::path::Path) -> Vec<crate::dirid::Stored> {
    recorded_identity(key_dir).into_iter().chain(occupants(key_dir)).collect()
}

/// How a directory matches a key's identities. `Weak` is a match made on
/// `(dev, ino)` alone — a `Legacy` record, or a `Full` one against a
/// directory whose birth time cannot be read. It protects from the reap
/// exactly as a match did before #183, and never licenses a follow: ext4
/// hands a freed inode to the next directory made, so two fields cannot say
/// *which* directory was found (`dirid`'s module doc).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Hit {
    No,
    Weak,
    Full,
}

#[cfg(unix)]
fn hit(ids: &[crate::dirid::Stored], md: &std::fs::Metadata) -> Hit {
    use std::os::unix::fs::MetadataExt;
    let full = crate::dirid::of_metadata(md).ok();
    let same_inode = |dev: u64, ino: u64| dev == md.dev() && ino == md.ino();
    ids.iter()
        .map(|s| match (*s, full) {
            (crate::dirid::Stored::Full(_), Some(id)) if crate::dirid::matches(s, &id) => Hit::Full,
            (crate::dirid::Stored::Full(f), None) if same_inode(f.dev, f.ino) => Hit::Weak,
            (crate::dirid::Stored::Legacy { dev, ino }, _) if same_inode(dev, ino) => Hit::Weak,
            _ => Hit::No,
        })
        .max()
        .unwrap_or(Hit::No)
}

/// Whether `dir` carries one of the key's identities in full — a directory
/// the key's sessions may have followed their shells into (#183 §3). What
/// lets `home::owner_dir_with` show a session in the directory its record
/// names when that is not the key's `.moved-to` (an occupant renamed while
/// the first target still stands). Fork-free: two small reads and a stat.
#[cfg(unix)]
pub(crate) fn carries_identity(key_dir: &std::path::Path, dir: &std::path::Path) -> bool {
    let ids = identities(key_dir);
    !ids.is_empty() && std::fs::symlink_metadata(dir).is_ok_and(|m| hit(&ids, &m) == Hit::Full)
}

#[cfg(not(unix))]
pub(crate) fn carries_identity(_key_dir: &std::path::Path, _dir: &std::path::Path) -> bool {
    false
}

/// `Moved` for a hit — `legacy` when it was only `Weak`.
fn moved(dir: PathBuf, h: Hit) -> MoveEvidence {
    MoveEvidence::Moved { dir, legacy: h == Hit::Weak }
}

/// What the key's own `.moved-to` says, checked against the directory
/// itself: `Some(Moved { dir, .. })` when it names a URL resolving to a
/// directory carrying one of the key's identities — the one followed earlier is
/// still alive there, whatever its shells' cwds are now. `Some(CannotTell)`
/// when the record or that directory cannot be read. `None` — no record,
/// no identity, the URL gone, or a different directory there — hands the
/// question to the shell scan, so a deleted B still reaps.
///
/// Without this, a followed session whose shell `cd`s out of B leaves no
/// shell under the inode, the scan answers "no evidence", and the sweep
/// SIGKILLs a session that is working in a directory that exists.
#[cfg(unix)]
fn moved_to_evidence(roots: &[PathBuf], key_dir: &std::path::Path) -> Option<MoveEvidence> {
    let ids = identities(key_dir);
    if ids.is_empty() {
        return None;
    }
    let raw = match std::fs::read(moved_to_marker_path(key_dir)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(MoveEvidence::CannotTell),
    };
    // Written atomically by `write_moved_to`, so a damaged one is foreign or
    // corrupt — and a record we cannot read is not a record of nothing.
    let Some(url) = parse_moved_to(&raw) else { return Some(MoveEvidence::CannotTell) };
    if let Some(dir) = crate::projects::resolve_project(roots, &url) {
        return match std::fs::symlink_metadata(&dir) {
            Ok(m) => match hit(&ids, &m) {
                Hit::No => None,
                h => Some(moved(dir, h)),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => Some(MoveEvidence::CannotTell),
        };
    }
    // Not resolving is "not there" only when every root says NotFound; a
    // path we cannot stat may be the very directory, unreadable. And a
    // directory standing there with the recorded identity is the very
    // directory, whatever the resolver said: for a worktree URL it asks
    // git, and a git that failed vouches for nothing (final review N1).
    for r in roots {
        match std::fs::symlink_metadata(r.join(&url)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Some(MoveEvidence::CannotTell),
            Ok(m) if hit(&ids, &m) != Hit::No => return Some(MoveEvidence::CannotTell),
            Ok(_) => {}
        }
    }
    None
}

#[cfg(not(unix))]
fn moved_to_evidence(_roots: &[PathBuf], _key_dir: &std::path::Path) -> Option<MoveEvidence> {
    None
}

/// The last evidence asked for before a gone key is reaped (#182): the
/// directory itself, looked for by identity among the siblings of where it
/// was last known to be — the parent of `.origin`'s recorded path, and of
/// `.moved-to`'s target. That is where `git worktree move` and a same-parent
/// `mv` put it, and it needs no shell: a session whose shell `cd`d out, or
/// that never had one inside, is otherwise indistinguishable from a delete.
///
/// `Moved(dir)` on a match (the caller still requires its URL to resolve
/// under the roots before following); `CannotTell` when a sibling directory
/// cannot be listed or an entry cannot be stat'd; `None` when nothing there
/// carries the identity, which leaves a real delete a delete. No git —
/// `worktree::list` turns a failed git into an empty list, which would read
/// "could not ask" as "not there".
#[cfg(unix)]
fn sibling_evidence(roots: &[PathBuf], key_dir: &std::path::Path) -> MoveEvidence {
    let ids = identities(key_dir);
    if ids.is_empty() {
        return MoveEvidence::None;
    }
    let mut parents: Vec<PathBuf> = origin_parent(key_dir).into_iter().collect();
    if let Some(url) = std::fs::read(moved_to_marker_path(key_dir)).ok().and_then(|b| parse_moved_to(&b)) {
        parents.extend(roots.iter().filter_map(|r| r.join(&url).parent().map(|d| d.to_path_buf())));
    }
    // A full match wins over a weak one wherever each was found: the weak
    // one protects, but only the full one can be followed.
    let mut weak: Option<PathBuf> = None;
    let scanned = scan_siblings(parents, |path, m| match hit(&ids, m) {
        Hit::No => std::ops::ControlFlow::Continue(()),
        Hit::Weak if weak.is_some() => std::ops::ControlFlow::Continue(()),
        h => {
            let Ok(dir) = path.canonicalize() else { return std::ops::ControlFlow::Break(MoveEvidence::CannotTell) };
            if h == Hit::Full {
                return std::ops::ControlFlow::Break(moved(dir, h));
            }
            weak = Some(dir);
            std::ops::ControlFlow::Continue(())
        }
    });
    match scanned {
        Err(_) => MoveEvidence::CannotTell,
        Ok(Some(e)) => e,
        Ok(None) => weak.map_or(MoveEvidence::None, |dir| moved(dir, Hit::Weak)),
    }
}

/// The parent of the key's recorded `.origin` path — where its directory
/// was last known to be, and so where a same-parent rename put it.
#[cfg(unix)]
fn origin_parent(key_dir: &std::path::Path) -> Option<PathBuf> {
    let recorded = std::fs::read(origin_marker_path(key_dir)).ok()?;
    let p = path_from_marker(recorded.strip_suffix(b"\n").unwrap_or(&recorded));
    p.parent().filter(|d| d.is_absolute()).map(|d| d.to_path_buf())
}

/// The one scan behind #182's sibling evidence and #183's
/// [`find_by_identity`]: every directory entry of every one of `parents`
/// (deduplicated), with its own `symlink_metadata`, handed to `visit` until
/// it breaks. A parent or entry that vanished meanwhile is skipped — a rename
/// between the listing and the stat is ordinary — and one that cannot be
/// listed or stat'd is `Err`: the match may be exactly what could not be
/// looked at. No git: `worktree::list` turns a failed git into an empty
/// list, which would read "could not ask" as "not there".
#[cfg(unix)]
fn scan_siblings<T>(
    mut parents: Vec<PathBuf>,
    mut visit: impl FnMut(&std::path::Path, &std::fs::Metadata) -> std::ops::ControlFlow<T>,
) -> Result<Option<T>, String> {
    parents.sort();
    parents.dedup();
    for parent in &parents {
        let entries = match std::fs::read_dir(parent) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{} cannot be listed ({e})", parent.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|e| format!("{} cannot be listed ({e})", parent.display()))?;
            let path = entry.path();
            match std::fs::symlink_metadata(&path) {
                Ok(m) if m.is_dir() => {
                    if let std::ops::ControlFlow::Break(t) = visit(&path, &m) {
                        return Ok(Some(t));
                    }
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{} cannot be looked at ({e})", path.display())),
            }
        }
    }
    Ok(None)
}

/// Where [`find_by_identity`] found a record's directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Found {
    /// The directory carrying the identity, canonical, and the URL that
    /// resolves to it under the roots.
    At(PathBuf, String),
    /// No candidate carries it: a delete, or a move roost cannot see.
    Nowhere,
    /// A candidate could not be looked at — it may be exactly the one.
    CannotTell(String),
}

/// #183 §1: where is the directory carrying `id`, for a record that pointed
/// at `old_url`? Evidence about *this record's* directory — never about
/// `old_url`'s key as a whole, which may since have held an occupant whose
/// residents are not the first directory's (chain a). Candidates, in order:
///
/// 1. the `.moved-to` target of `old_url`'s key, when that key dir exists;
/// 2. the entries beside `old_url`'s last known path — the parent of the
///    key's `.origin`, and of `<root>/<old_url>` for each root ([`scan_siblings`]);
/// 3. the ancestors of `cwd` (a resident Claude's), when given. This only
///    decides anything for a move to a different parent — out of #183's
///    scope, and so with no discriminating test: a same-parent rename is
///    found by (2) first.
///
/// A candidate counts only when it carries `id` as a **Full** identity
/// (birth time included: ext4 hands a freed inode to the next directory
/// made, so `(dev, ino)` alone cannot say which directory this is), its URL
/// resolves under the roots to that same directory, and that URL is not
/// `old_url`. `NotFound` anywhere means try the next; any other error is
/// `CannotTell`, and so is a directory sharing `id`'s `(dev, ino)` whose
/// birth time cannot be read, and one carrying `id` under the roots whose
/// URL does not resolve to it (git may have failed to vouch for it). Fork-free except `resolve_project`, which may
/// run git — so never call this under a hub, `REGISTRY`, `SESSIONS` or any
/// other lock.
pub(crate) fn find_by_identity(roots: &[PathBuf], old_url: &str, id: &crate::dirid::DirId, cwd: Option<&std::path::Path>) -> Found {
    find_by_identity_in(roots, &key_dir_of(old_url), old_url, id, cwd)
}

#[cfg(unix)]
fn find_by_identity_in(
    roots: &[PathBuf],
    key_dir: &std::path::Path,
    old_url: &str,
    id: &crate::dirid::DirId,
    cwd: Option<&std::path::Path>,
) -> Found {
    use std::ops::ControlFlow::{Break, Continue};
    // Is `path` (whose metadata is `md`) the directory, followable?
    // `Ok(None)` = not this one; `Err` = cannot tell.
    let accept = |path: &std::path::Path, md: &std::fs::Metadata| -> Result<Option<(PathBuf, String)>, String> {
        use std::os::unix::fs::MetadataExt;
        match crate::dirid::of_metadata(md) {
            Ok(here) if here == *id => {}
            Ok(_) => return Ok(None),
            Err(e) if md.dev() == id.dev && md.ino() == id.ino => return Err(format!("{}: {e}", path.display())),
            Err(_) => return Ok(None),
        }
        let dir = match path.canonicalize() {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{} cannot be resolved ({e})", path.display())),
        };
        let Some(url) = url_under_roots(roots, &dir).filter(|u| u != old_url) else { return Ok(None) };
        // The record's own directory, standing under the roots, that the
        // resolver will not vouch for: for a worktree URL it asks git, and
        // `worktree::list` turns a failed git into an empty list. That is
        // "cannot tell", never "not there" (the N1 ruling in
        // `moved_to_evidence`) — folding it into `Nowhere` would forget the
        // terminal into its home, where Close Project would end it.
        if crate::projects::resolve_project(roots, &url).as_deref() != Some(dir.as_path()) {
            return Err(format!("{} carries the identity, but {url} does not resolve to it", dir.display()));
        }
        Ok(Some((dir, url)))
    };
    let stat = |path: &std::path::Path, follow: bool| -> Result<Option<std::fs::Metadata>, String> {
        let r = if follow { std::fs::metadata(path) } else { std::fs::symlink_metadata(path) };
        match r {
            Ok(m) => Ok(Some(m)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{} cannot be looked at ({e})", path.display())),
        }
    };
    let found = |r: Result<Option<(PathBuf, String)>, String>| match r {
        Ok(Some((dir, url))) => Some(Found::At(dir, url)),
        Ok(None) => None,
        Err(e) => Some(Found::CannotTell(e)),
    };

    // (1) Where the key itself was followed to.
    match stat(key_dir, false) {
        Err(e) => return Found::CannotTell(e),
        Ok(None) => {}
        Ok(Some(_)) => match std::fs::read(moved_to_marker_path(key_dir)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Found::CannotTell(format!("{}'s .moved-to cannot be read ({e})", key_dir.display())),
            Ok(raw) => {
                let Some(url) = parse_moved_to(&raw) else {
                    return Found::CannotTell(format!("{}'s .moved-to is damaged", key_dir.display()));
                };
                for r in roots {
                    let path = r.join(&url);
                    match stat(&path, true) {
                        Err(e) => return Found::CannotTell(e),
                        Ok(None) => {}
                        Ok(Some(m)) => {
                            if let Some(f) = found(accept(&path, &m)) {
                                return f;
                            }
                        }
                    }
                }
            }
        },
    }

    // (2) Beside where it was last known to be.
    let mut parents: Vec<PathBuf> = origin_parent(key_dir).into_iter().collect();
    parents.extend(roots.iter().filter_map(|r| r.join(old_url).parent().map(|d| d.to_path_buf())));
    match scan_siblings(parents, |path, m| match found(accept(path, m)) {
        Some(f) => Break(f),
        None => Continue(()),
    }) {
        Err(e) => return Found::CannotTell(e),
        Ok(Some(f)) => return f,
        Ok(None) => {}
    }

    // (3) Up from the resident Claude's own cwd.
    for p in cwd.into_iter().flat_map(|c| c.ancestors()) {
        match stat(p, false) {
            Err(e) => return Found::CannotTell(e),
            Ok(None) => {}
            Ok(Some(m)) => {
                if let Some(f) = found(accept(p, &m)) {
                    return f;
                }
            }
        }
    }
    Found::Nowhere
}

#[cfg(not(unix))]
fn find_by_identity_in(
    _roots: &[PathBuf],
    _key_dir: &std::path::Path,
    _old_url: &str,
    _id: &crate::dirid::DirId,
    _cwd: Option<&std::path::Path>,
) -> Found {
    Found::Nowhere
}

#[cfg(not(unix))]
fn sibling_evidence(_roots: &[PathBuf], _key_dir: &std::path::Path) -> MoveEvidence {
    MoveEvidence::None
}

/// Writes `sock/<key>/.moved-to` (atomic, 0600, pid-unique temp — the same
/// discipline as `write_origin`) and says whether it changed anything.
/// Rewritten only when the URL differs: a second rename moves it on, an
/// unchanged one costs one small read per pass.
pub(crate) fn write_moved_to(key_dir: &std::path::Path, new_url: &str) -> Result<bool, String> {
    let bytes = format!("{new_url}\n").into_bytes();
    let marker = moved_to_marker_path(key_dir);
    if std::fs::read(&marker).map(|cur| cur == bytes).unwrap_or(false) {
        return Ok(false);
    }
    let tmp = key_dir.join(format!(".moved-to.tmp.{}", crate::wsstate::tmp_suffix()));
    let out = (|| {
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp, &marker).map_err(|e| e.to_string())
    })();
    if out.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    out.map(|()| true)
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
/// One process listing for the overview's ages, `None` when `ps` could not
/// be trusted — the caller then renders every age unknown.
pub fn holders_snapshot() -> Option<Vec<(u32, String)>> {
    process_snapshot()
}

/// Public face of `pids_holding` for the overview.
pub fn pids_holding_path(snapshot: &[(u32, String)], path: &std::path::Path) -> Vec<u32> {
    pids_holding(snapshot, path)
}

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
/// a socket, not merely the in-process *client* roost itself spawned.
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
///
/// **Ends the session, not merely the socket's holders.** Killing the dtach
/// master closes the pty, and the kernel turns that into a `SIGHUP` for the
/// slave side — which anything that handles the hangup simply declines.
/// Measured 2026-09-04: a `trap "" HUP` child survived exactly this and
/// reparented to init, while the socket went unheld, so the old confirmation
/// read "finished". Claude Code is that shape, so Close Project routinely left
/// one running and reported it ended.
///
/// The sessions to sweep are derived *before* the holders are killed: once the
/// master dies its children reparent and the link is gone. Anything that left
/// the session on purpose — Claude's `daemon run` and `bg-pty-host` both do,
/// measured — is out of scope by construction, and must stay that way: the
/// daemon is shared across projects, so following it would end other projects'
/// Claudes.
fn kill_and_unlink_with(sock_path: &std::path::Path, snapshot_fn: SnapshotFn, proc_root: &std::path::Path) -> bool {
    let Some(snapshot) = snapshot_fn() else {
        eprintln!(
            "roost: could not verify what holds {} (process listing unavailable) — leaving it in place",
            sock_path.display()
        );
        return false;
    };
    let pids = pids_holding(&snapshot, sock_path);

    // Derived before any kill, for the reason in the doc comment above.
    let own = match crate::procsess::session_of(proc_root, std::process::id()) {
        crate::procsess::Sid::In(s) => Some(s),
        _ => None,
    };
    let Some(targets) = crate::procsess::target_sessions(proc_root, &pids, own) else {
        eprintln!(
            "roost: could not determine which session holds {} — leaving it in place",
            sock_path.display()
        );
        return false;
    };

    kill_pids(&pids);
    kill_sessions(proc_root, &targets);

    // Re-killed on every pass, not only re-checked: a process forked by the
    // shell between the derivation and the kill would otherwise be seen
    // forever and never signalled, and the loop would time out on it.
    let mut still = session_or_socket_alive(sock_path, snapshot_fn, proc_root, &targets);
    for _ in 0..20 {
        if matches!(still, StillHeld::No) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
        kill_sessions(proc_root, &targets);
        still = session_or_socket_alive(sock_path, snapshot_fn, proc_root, &targets);
    }
    match still {
        StillHeld::No => {}
        // Same doubt as the `target_sessions` bail above, just discovered
        // later: a session's membership that never became readable during
        // the confirm loop is not a survivor, and must not be reported with
        // the same words as one — that is the wrong-label defect this arm
        // exists to fix.
        StillHeld::Undetermined => {
            eprintln!(
                "roost: could not determine whether {} has fully ended — leaving it in place",
                sock_path.display()
            );
            return false;
        }
        StillHeld::Yes => return false,
    }
    let _ = std::fs::remove_file(sock_path);
    // The mode sidecar is a sidecar of the socket, so it goes when the socket
    // goes — here and in `reconcile`'s dead-socket arm, the only two places
    // that unlink one. Never from the pump's exit path: that also fires on a
    // detach, where the shell is still alive behind its dtach master and its
    // contract is still true.
    crate::modes::forget(sock_path);
    true
}

/// `kill -9` for a set of pids, in one invocation.
///
/// pid 0 is refused outright: `kill -9 0` signals this process's whole group,
/// i.e. roost itself. It should be unreachable — the kernel's swapper cannot
/// hold a socket and cannot lead a session — but an impossibility that costs
/// one comparison to make impossible is worth the comparison. stderr is
/// silenced because a pid that exited on its own in between has already
/// achieved what was wanted, and "No such process" noise masks real output.
fn kill_pids(pids: &[u32]) {
    let args: Vec<String> = pids.iter().filter(|p| **p != 0).map(|p| p.to_string()).collect();
    if args.is_empty() {
        return;
    }
    let _ = std::process::Command::new("kill")
        .arg("-9")
        .args(&args)
        .stderr(std::process::Stdio::null())
        .status();
}

/// `kill -9` every member of each target session, never roost itself.
///
/// A session this pass cannot enumerate is skipped rather than aborting the
/// whole sweep for every other target: killing nothing from it is always
/// safe, and `session_or_socket_alive` sees the same `None` afterwards and
/// refuses to confirm the socket ended, so the doubt still blocks the
/// unlink — it just doesn't block *other* sessions' members from dying too.
fn kill_sessions(proc_root: &std::path::Path, targets: &[u32]) {
    for sid in targets {
        // A membership roost could not determine is not an empty one, and
        // there is nothing safe to kill from it. The confirmation below sees
        // the same `None` and refuses to finish, which is the outcome wanted.
        let Some(members) = crate::procsess::members_of(proc_root, *sid) else { continue };
        let me = std::process::id();
        let victims: Vec<u32> = members.into_iter().filter(|p| *p != me).collect();
        kill_pids(&victims);
    }
}

/// Finer than a bool on purpose: "held" and "could not tell" both block the
/// unlink identically, but they must not print the same sentence — a
/// survivor is something roost saw, doubt is something roost couldn't see
/// through. Folding the two into `bool` is exactly the label bug Finding 1
/// fixes.
enum StillHeld {
    /// Nothing holds the socket and every target session came back empty.
    No,
    /// A process holds the socket, or a target session still has a member —
    /// confirmed, not merely undetermined.
    Yes,
    /// A target session's membership could not be read (`members_of`
    /// returned `None`) and no *other* target was confirmed to still have a
    /// member. Treated the same as `Yes` for the purpose of blocking the
    /// unlink, but reported with different words.
    Undetermined,
}

/// True while anything holds the socket **or** any target session still has a
/// member, or either question could not be answered. Both halves must be
/// clear before a socket is unlinked and a session reported ended.
fn session_or_socket_alive(
    sock_path: &std::path::Path,
    snapshot_fn: SnapshotFn,
    proc_root: &std::path::Path,
    targets: &[u32],
) -> StillHeld {
    if socket_has_process_with(sock_path, snapshot_fn) {
        return StillHeld::Yes;
    }
    // A confirmed member anywhere wins over doubt found elsewhere: it is the
    // stronger, more specific claim, and it is what actually happened.
    let mut undetermined = false;
    for sid in targets {
        match crate::procsess::members_of(proc_root, *sid) {
            None => undetermined = true,
            Some(m) if !m.is_empty() => return StillHeld::Yes,
            Some(_) => {}
        }
    }
    if undetermined {
        StillHeld::Undetermined
    } else {
        StillHeld::No
    }
}

/// The only public entry point for `kill_and_unlink_with`'s logic — see its
/// doc comment for the full contract. Used by `reconcile`'s project-gone
/// branch and by `session::kill_project` (an explicit "Close Project" from
/// the UI), so both end a session's `dtach` master, not merely the
/// in-process client, through one shared, confirm-before-unlink
/// implementation.
pub fn kill_and_unlink(sock_path: &std::path::Path) -> bool {
    kill_and_unlink_with(sock_path, &process_snapshot, std::path::Path::new("/proc"))
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
/// compiling: roost is unix-only in practice (dtach, PTYs, `ps`).
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
/// that a *second*, differently-rooted roost sharing this state dir
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
    // A new directory at a renamed key's old name is not the one its
    // sessions followed (#158 §5): rewriting the identity to it would make
    // the next pass read "moved back" and drop the `.moved-to` they hang
    // on. `.origin` itself names the same path either way.
    if occupied_by_another(key_dir, dir) {
        // Its identity goes in a sidecar of its own instead, so that a
        // rename of *this* directory is still told from its delete (the
        // sweep matches shells against every identity). Appended: an
        // earlier occupant's renamed sessions still need theirs.
        let _ = append_occupant(key_dir, dir);
        return;
    }
    // First, not after the unchanged-marker return below: a project whose
    // `.origin` predates `.origin-ino` takes that return on every pass, and
    // would never gain the identity the rename check needs (#178). It does
    // its own unchanged check, so this costs one small read per pass.
    write_origin_identity(key_dir, dir);
    let bytes = marker_bytes(dir);
    let marker = origin_marker_path(key_dir);
    if std::fs::read(&marker).map(|cur| cur == bytes).unwrap_or(false) {
        return;
    }
    // Pid in the temp name: two roost instances sharing this state dir
    // is the whole premise of the marker, and a *shared* temp name would
    // hand the truncate-then-write window straight back — one process's
    // `rename` could publish the other's half-written file. A crash can
    // strand one of these, but it is a dotfile (skipped by every enumeration
    // of this directory) and the unchanged-content skip above means the
    // write path runs at most once per changed resolution, not once per pass.
    let tmp = key_dir.join(format!(".origin.tmp.{}", crate::wsstate::tmp_suffix()));
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

/// The project directory's identity, kept beside `.origin` (#178): `dev ino
/// btime_secs btime_nsec` since #183, two fields before it (`dirid`).
///
/// `.origin` alone cannot tell "deleted" from "moved": both leave the recorded
/// path `NotFound`. The inode can — a rename keeps it, a delete destroys it —
/// and `move_evidence` looks for it from the sessions' own shells. A
/// sidecar rather than a second line in `.origin`, so the marker older
/// binaries read as a path stays byte-identical.
fn origin_identity_path(key_dir: &std::path::Path) -> std::path::PathBuf {
    key_dir.join(".origin-ino")
}

/// The identities of *different* directories occupying a renamed key's old
/// name (#158 §5, I2; a list since #183): the key's sessions now fall in
/// groups — the followed ones, in the renamed directory (`.origin-ino`), and
/// each occupant's own (this, one identity per line, oldest first, at most
/// `OCCUPANT_CAP`). Written only while `.moved-to` lives, and removed with
/// it.
fn occupant_identity_path(key_dir: &std::path::Path) -> std::path::PathBuf {
    key_dir.join(".occupant-ino")
}

/// Ends a rename's redirect for this key: `.moved-to` and the occupant's
/// identity go together.
fn remove_moved_to(key_dir: &std::path::Path) {
    let _ = std::fs::remove_file(moved_to_marker_path(key_dir));
    let _ = std::fs::remove_file(occupant_identity_path(key_dir));
}

/// How many occupant identities `.occupant-ino` keeps (#183 §3). Each is a
/// different directory made at a renamed key's old name while its
/// `.moved-to` lived; eight is far past anything seen, and the bound keeps a
/// loop that keeps recreating the directory from growing the file forever.
pub(crate) const OCCUPANT_CAP: usize = 8;

/// Appends `dir`'s identity to `.occupant-ino` unless it is already listed,
/// dropping the oldest past `OCCUPANT_CAP`. Returns the cap's log line when
/// one was dropped (also printed), so the drop is testable.
///
/// Appends, never overwrites: one slot let a third directory at the old
/// name erase the second's identity, and that occupant's renamed sessions
/// were reaped as a delete (#183 chain c). A file that exists but cannot be
/// read is left alone — rewriting it would erase exactly the identities this
/// list exists to keep — and so is a directory whose identity cannot be read
/// (no `(dev, ino)` fallback, `dirid`'s rule).
fn append_occupant(key_dir: &std::path::Path, dir: &std::path::Path) -> Option<String> {
    let Ok(Some(id)) = crate::dirid::of(dir) else { return None };
    let path = occupant_identity_path(key_dir);
    let mut lines: Vec<String> = match std::fs::read_to_string(&path) {
        Ok(s) => s.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(_) => return None,
    };
    if lines.iter().any(|l| crate::dirid::parse_line(l) == Some(crate::dirid::Stored::Full(id))) {
        return None;
    }
    // A legacy line for this `(dev, ino)` is this directory, upgraded in
    // place rather than listed twice. It cannot be an earlier occupant still
    // alive elsewhere: two live directories never share `(dev, ino)`, so an
    // earlier one with that pair is gone, and its line protected nothing.
    let legacy = lines.iter().position(|l| {
        matches!(crate::dirid::parse_line(l), Some(crate::dirid::Stored::Legacy { dev, ino }) if dev == id.dev && ino == id.ino)
    });
    if let Some(i) = legacy {
        lines[i] = crate::dirid::format(&id);
        let _ = crate::dirid::write_lines(&path, &lines);
        return None;
    }
    lines.push(crate::dirid::format(&id));
    let mut said = None;
    if lines.len() > OCCUPANT_CAP {
        lines.drain(..lines.len() - OCCUPANT_CAP);
        let key = key_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let msg = format!(
            "roost: {key}'s .occupant-ino is at its cap of {OCCUPANT_CAP} identities; dropped the oldest to record {}",
            dir.display()
        );
        eprintln!("{msg}");
        said = Some(msg);
    }
    crate::dirid::write_lines(&path, &lines).ok()?;
    said
}

/// Writes `.origin-ino` four-field (#183), and rewrites it when it differs —
/// which is also what upgrades a two-field one from #178's roost on the next
/// pass in which the directory resolves. Metadata that cannot be read at all
/// writes nothing, as before #183.
#[cfg(unix)]
fn write_origin_identity(key_dir: &std::path::Path, dir: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = std::fs::metadata(dir) else { return };
    let marker = origin_identity_path(key_dir);
    if let Some(line) = origin_line(identity_at(&marker), crate::dirid::of_metadata(&m), (m.dev(), m.ino())) {
        let _ = crate::dirid::write_lines(&marker, &[line]);
    }
}

#[cfg(not(unix))]
fn write_origin_identity(_key_dir: &std::path::Path, _dir: &std::path::Path) {}

/// The `.origin-ino` line to write, given what is recorded now, the
/// directory's identity (`Err` = birth time unreadable) and its `(dev,
/// ino)` — or `None` to leave the file as it is.
///
/// With no birth time the directory is recorded **Legacy**, two fields.
/// Writing nothing instead left a new key with no identity at all on any
/// filesystem whose `created()` errs (NFSv3, FUSE, a seccomp'd `statx`):
/// `identities()` empty, every scan "no evidence", and a renamed project's
/// live sessions SIGKILLed — protection #178 gave and #183 must not take
/// away. That is not the fallback `dirid` forbids: what is forbidden is a
/// `(dev, ino)` *match* counting as a full one, and a Legacy record can only
/// ever produce a weak match, which protects and never licenses a follow.
/// A recorded identity that already names these `(dev, ino)` is kept — a
/// Full one is not downgraded for want of a birth time — and anything else
/// (a stale pair from a different directory, a damaged file) is replaced.
fn origin_line(
    current: Option<crate::dirid::Stored>,
    id: Result<crate::dirid::DirId, String>,
    (dev, ino): (u64, u64),
) -> Option<String> {
    use crate::dirid::Stored;
    match id {
        Ok(id) => (current != Some(Stored::Full(id))).then(|| crate::dirid::format(&id)),
        Err(_) => {
            let same = match current {
                Some(Stored::Full(f)) => f.dev == dev && f.ino == ino,
                Some(Stored::Legacy { dev: d, ino: i }) => d == dev && i == ino,
                None => false,
            };
            (!same).then(|| format!("{dev} {ino}"))
        }
    }
}

/// What a live shell says about a project whose recorded path is `NotFound`.
///
/// Three-valued on purpose (see CLAUDE.md, "absence of evidence is not
/// evidence of absence"): "cannot tell" must never collapse into "no
/// evidence of a move", because the two license opposite actions — the first
/// suspends the kill, the second lets the sweep proceed as before #178.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MoveEvidence {
    /// No evidence of a move (no identity recorded, or no shell inside it).
    None,
    /// The recorded directory is alive at this absolute path. `legacy` when
    /// it was matched on `(dev, ino)` alone (`Hit::Weak`): enough to keep a
    /// session from the reap, never enough to follow it (#183 §3), so
    /// `record_move_with` turns it into `Unfollowable`.
    Moved { dir: std::path::PathBuf, legacy: bool },
    /// Could not tell (unnamed shell, unreadable cwd, unreadable key dir).
    CannotTell,
}

/// Where a project the sweep is about to call gone actually went, according
/// to a live session's shell — or whether that cannot be ruled out, in which
/// case nothing may be killed (#178).
///
/// The evidence is a live session's shell: its `/proc/<pid>/cwd` still resolves
/// (not `… (deleted)`), and some directory on that path has the
/// identity recorded for the project — that directory is the new path. A
/// rename keeps the inode and its birth time; a delete destroys them. No recorded identity
/// (a marker from an older roost) is no evidence, and the sweep proceeds
/// exactly as before. A shell roost cannot name, or a cwd it cannot read, is
/// "cannot tell" and suspends the kill: a stale row is recoverable, a killed
/// shell is not.
#[cfg(unix)]
pub(crate) fn move_evidence(key_dir: &std::path::Path, snapshot: &[(u32, String)], proc_root: &std::path::Path) -> MoveEvidence {
    let ids = identities(key_dir);
    if ids.is_empty() {
        return MoveEvidence::None;
    }
    let Ok(entries) = std::fs::read_dir(key_dir) else { return MoveEvidence::CannotTell };
    let own = match crate::procsess::session_of(proc_root, std::process::id()) {
        crate::procsess::Sid::In(s) => Some(s),
        _ => None,
    };
    // As in `sibling_evidence`: a weak match is kept while a full one may
    // still turn up under another shell.
    let mut weak: Option<PathBuf> = None;
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let pids = pids_holding(snapshot, &e.path());
        if pids.is_empty() {
            continue;
        }
        let Some(shells) = crate::procsess::target_sessions(proc_root, &pids, own) else { return MoveEvidence::CannotTell };
        for shell in shells {
            let cwd = match std::fs::read_link(proc_root.join(shell.to_string()).join("cwd")) {
                Ok(p) => p,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return MoveEvidence::CannotTell,
            };
            if cwd.to_string_lossy().ends_with(" (deleted)") {
                continue;
            }
            let mut at = Some(cwd.as_path());
            while let Some(p) = at {
                if let Ok(m) = std::fs::symlink_metadata(p) {
                    match hit(&ids, &m) {
                        Hit::Full => return moved(p.to_path_buf(), Hit::Full),
                        Hit::Weak if weak.is_none() => weak = Some(p.to_path_buf()),
                        _ => {}
                    }
                }
                at = p.parent();
            }
        }
    }
    weak.map_or(MoveEvidence::None, |dir| moved(dir, Hit::Weak))
}

#[cfg(not(unix))]
pub(crate) fn move_evidence(_key_dir: &std::path::Path, _snapshot: &[(u32, String)], _proc_root: &std::path::Path) -> MoveEvidence {
    MoveEvidence::None
}

/// Records the resolved absolute directory a project maps to, so a *later*
/// check for "is this project gone" can consult this exact path directly —
/// independent of whatever `ROOST_ROOTS` the process performing that
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
    let key_dir = key_dir_of(project);
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
/// be configured with". Two roost processes — or the same one,
/// restarted with a different `ROOST_ROOTS` — can share one
/// `ROOST_STATE_DIR`: `docs/deploy.md` tells users to override
/// `ROOST_ROOTS` to run a second instance locally and says nothing
/// about also separating `ROOST_STATE_DIR`, so the documented way to
/// run roost locally reaches this. Reproduced directly against a real
/// `dtach` session, outside the test suite entirely: a live session for
/// `victimproj` was SIGKILLed and its socket unlinked simply because
/// roost was started with different roots against the same state dir
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
            // Resolving again is a move back only when it is the same
            // directory. A new one made under the old name is not: the
            // followed sessions are still working in the renamed one, so
            // their `.moved-to` and the old identity stay (and with them the
            // owner widening that keeps them out of the new A's Close
            // Project). The `!any_held` cleanup still ends both with the
            // last session.
            if occupied_by_another(key_dir, &dir) {
                log_occupied_once(key_dir, &dir);
                // Records the occupant's identity (and nothing else).
                write_origin(key_dir, &dir);
                return false;
            }
            write_origin(key_dir, &dir);
            // Moved back: a `.moved-to` from the earlier rename now
            // describes a different place, and must stop widening or
            // redirecting anything for this key.
            remove_moved_to(key_dir);
            false
        }
        None => match std::fs::read(origin_marker_path(key_dir)) {
            Ok(recorded) => recorded_path_is_gone(&recorded),
            Err(_) => false,
        },
    }
}

/// Whether `dir`, which the key's URL resolves to again, is *not* the
/// directory the key's sessions followed: a `.moved-to` is present (or
/// cannot be looked at) and `dir`'s identity differs from the recorded one
/// (or cannot be read, or none usable is recorded). Every doubt answers yes — the answer only ever
/// keeps a record; "no" is what deletes one. A `Full` record needs a full
/// match (a recreated directory reusing the inode is an occupant); a
/// `Legacy` one compares `(dev, ino)`, exactly as before #183.
fn occupied_by_another(key_dir: &std::path::Path, dir: &std::path::Path) -> bool {
    match std::fs::symlink_metadata(moved_to_marker_path(key_dir)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) | Ok(_) => {}
    }
    // A `.moved-to` stands, so a missing, unreadable or damaged identity is
    // a doubt about which directory this is, and doubt answers "occupied".
    // Answering "not occupied" sent `confirmed_gone` down the moved-back
    // arm: it stamped the occupant's identity as the key's own and removed
    // `.moved-to`, and R18 then forgot a resident into the occupant (#183
    // §2). With no `.moved-to` there is no rename to protect: `false` above.
    let Some(recorded) = recorded_identity(key_dir) else { return true };
    #[cfg(unix)]
    {
        let same = match std::fs::metadata(dir) {
            Ok(m) => match recorded {
                crate::dirid::Stored::Full(_) => hit(&[recorded], &m) == Hit::Full,
                crate::dirid::Stored::Legacy { .. } => hit(&[recorded], &m) == Hit::Weak,
            },
            Err(_) => false,
        };
        !same
    }
    #[cfg(not(unix))]
    {
        let _ = (dir, recorded);
        false
    }
}

fn log_occupied_once(key_dir: &std::path::Path, dir: &std::path::Path) {
    static SAID: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> = std::sync::OnceLock::new();
    let first = SAID
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key_dir.to_path_buf());
    if first {
        let key = key_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        eprintln!(
            "roost: a new directory now occupies {key}'s old name ({}); its renamed sessions keep following their .moved-to",
            dir.display()
        );
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
/// after a restart. A `dtach` session that outlived the previous roost
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
/// permissions problem, a `ROOST_ROOTS` typo, or just running on a host
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
    let mut report = ReapReport { dead_sockets: 0, gone_projects: 0, moved: Vec::new() };
    let roots_ok = !roots.is_empty() && roots.iter().all(|r| r.canonicalize().is_ok());
    let roots_were_ok = ROOTS_WERE_OK.swap(roots_ok, std::sync::atomic::Ordering::Relaxed);
    if !roots_ok && roots_were_ok {
        // Two different states wear the same suspension. An empty list is
        // not a broken one: roost now starts with no roots at all, and
        // "none of the configured roots could be read ([])" reads as a
        // failure to whoever has simply not added one yet.
        if roots.is_empty() {
            eprintln!("roost: no project roots configured — session reaping is suspended until one is added");
        } else {
            eprintln!(
                "roost: none of the configured roots could be read ({roots:?}) — \
                 reaping sessions for missing projects is suspended until this recovers"
            );
        }
    } else if roots_ok && !roots_were_ok {
        eprintln!("roost: configured roots are readable again — reaping resumed");
    }

    // roost's own worktree markers: a `.base` whose directory is *positively*
    // gone (NotFound, nothing else) is a file about nothing. Same rule as
    // `.origin`: unreadable is "cannot tell", and cannot-tell keeps. Gated
    // on `roots_ok` for the same reason the project-gone sweep below is —
    // an unreadable root must suspend reaping, not be read as "not found".
    //
    // Deliberately placed before the `sock_root()` early return just below:
    // `sock/` is created lazily, on a project's first attach (session.rs),
    // so a state dir that has worktrees but no live or ever-live session
    // has no `sock/` at all — and this sweep must still run there, or an
    // orphaned `.base` from a project with no sessions is never reaped.
    if roots_ok {
        if let Ok(rd) = std::fs::read_dir(crate::wsstate::state_dir().join("worktrees")) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let Some(key) = name.strip_suffix(".base") else { continue };
                if key.starts_with('.') {
                    continue; // a temp file mid-write (write_base's `.{key}.base.tmp.{pid}`)
                }
                let url = decode_key(key);
                let mut found_or_uncertain = false;
                for r in roots {
                    let p = r.join(&url);
                    match std::fs::symlink_metadata(&p) {
                        Ok(_) => {
                            found_or_uncertain = true;
                            break;
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(_) => {
                            found_or_uncertain = true; // cannot tell: keep
                            break;
                        }
                    }
                }
                // A key with a socket still in `sock/` may be a rename this
                // pass has yet to see (#158 §5): the sock sweep below runs
                // after this one, and following copies this `.base` to the
                // new key. Kept until its sessions are gone — a pass later
                // for a real delete, and the base the rename needs otherwise.
                if !found_or_uncertain && !key_has_sockets(&sock_root().join(key)) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
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
                "roost: could not read the process list (ps failed, exited non-zero, or \
                 returned nothing) — reaping is suspended until this recovers"
            );
        }
        return report;
    };
    if !process_list_was_ok {
        eprintln!("roost: process listing is readable again — reaping resumed");
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
        // is unreachable for anything roost wrote — it guards a key written
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
                "roost: refusing to reap session key {key:?} — it contains a control byte, \
                 so who holds its socket cannot be determined from `ps` output"
            );
            continue;
        }
        // Not "resolve_project(...).is_none()" alone — see confirmed_gone's
        // doc comment. A key that doesn't resolve under *these* roots is
        // not evidence the directory is gone; only a key whose resolved
        // path was previously recorded, and is now confirmed missing,
        // counts.
        let project_gone = roots_ok
            && confirmed_gone(&entry.path(), roots, &url)
            && match record_move_with(roots, &url, &entry.path(), &snapshot, std::path::Path::new("/proc"), true) {
                MoveRecord::NoEvidence => true,
                // Moved (followable or not) or cannot tell: never killed.
                MoveRecord::CannotTell | MoveRecord::Unfollowable => false,
                MoveRecord::Followable(m) => {
                    report.moved.push(m);
                    false
                }
            };
        let mut any_held = false;
        // In-process sessions first, once per project rather than once per
        // socket file below: `session::kill_project` now ends each session's
        // dtach master too (via `kill_and_unlink`, the same helper the loop
        // below uses), not just roost's own client, so any socket it
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
                    "roost: reaped {in_process_ended} in-process session(s) for {key} — its recorded directory no longer exists"
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
            any_held |= held_before;

            if held_before && project_gone {
                // Covers a session this process never had in memory (one
                // that outlived a restart) as well as a retry for one
                // `kill_project` above tried and failed to fully end.
                if kill_and_unlink_with(&sock.path(), snapshot_fn, std::path::Path::new("/proc")) {
                    report.gone_projects += 1;
                    eprintln!(
                        "roost: reaped session {key}/{name} — its recorded directory no longer exists (pids {pids:?})"
                    );
                } else {
                    eprintln!(
                        "roost: could not end session {key}/{name} for a missing project — pids {pids:?} did not fully die (or the process list was unverifiable); socket left in place so it stays discoverable"
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
                        crate::modes::forget(&sock.path());
                        report.dead_sockets += 1;
                        eprintln!("roost: reaped dead socket {}", sock.path().display());
                    }
                }
                ReapAction::Leave => {} // live process, project still exists: untouched
                ReapAction::RemoveKilled => unreachable!("only reached via the project_gone branch above"),
            }
        }
        // Spec §5 "Cleanup": a `.moved-to` lives only as long as the
        // sessions that are its evidence. With none held, nothing can follow
        // it and nothing should be widened by it — and a later directory
        // under the old name must not inherit the redirect.
        if !any_held {
            remove_moved_to(&entry.path());
        }
        // Sidecars whose session is positively gone. dtach unlinks its own
        // socket when the program exits, so the two `forget` call sites miss
        // the commonest ending and these would otherwise accumulate for the
        // life of the state dir — and keep the `remove_dir` below from ever
        // succeeding. Done here because this is where the key directory is
        // already in hand and already being swept.
        crate::modes::sweep_orphans(&entry.path());
        // An emptied directory is noise; ignore failure when it is not empty
        // (a live session for a project that still exists must survive).
        let _ = std::fs::remove_dir(entry.path());
    }
    // Re-home records whose session is positively gone, once per pass and
    // after everything above has decided: a call, not an input — nothing in
    // this sweep reads them. Same three outcomes as every other sidecar here:
    // only `NotFound` forgets; a socket we cannot stat keeps its record.
    crate::home::sweep(&|home, name| {
        match std::fs::symlink_metadata(crate::session::socket_path(home, name)) {
            Ok(_) => Some(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
            Err(_) => None,
        }
    });
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
    let report = should_reconcile(*last, now).then(|| {
        let r = reconcile(roots);
        *last = Some(now);
        r
    });
    // Released before following: `follow_moves` locks hubs, and a hub thread
    // that reached `reconcile` would otherwise wait on this gate while we
    // waited on its hub.
    drop(last);
    if let Some(r) = report {
        follow_moves(&r.moved);
    }
}

/// Every project roost knows about: those with a saved layout, those with
/// live sessions, and those with both.
pub fn known_projects(roots: &[PathBuf]) -> Vec<ProjectStatus> {
    known_projects_inner(roots, true)
}

/// `group` pays git: `git worktree list` per repository, to fold linked
/// worktrees in as children. The header switcher wants that; the front page
/// does not — it lists every directory under the roots and loads a
/// project's worktrees only when the user opens it (`worktree_rows`).
fn known_projects_inner(roots: &[PathBuf], group: bool) -> Vec<ProjectStatus> {
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
                    outside: None,
                    wt: None,
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
            // dtach sessions outlive roost and a restart leaves the
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
                    outside: None,
                    wt: None,
                });
                slot.live = live;
                slot.oldest_age_secs = oldest;
            } else if let Some(slot) = by_key.get_mut(&key) {
                slot.live = 0;
                slot.oldest_age_secs = None;
            }
        }
    }

    // A layout file whose directory no longer resolves under any root — a
    // deleted worktree, a renamed project, test-fixture pollution — is not a
    // project the user can open, and it clutters the overview as a `○` row
    // that 404s. Keep it only while it has live sessions: those are exactly
    // what "what did I leave running?" must still show, and reaping them is
    // `reconcile`'s job, never a listing's. Display-only, so an unreadable
    // root merely hides an idle row for one refresh; it destroys nothing.
    by_key.retain(|_, p| {
        p.live > 0
            || crate::projects::resolve_project(roots, &p.url).is_some()
            // `is_vouched_worktree` runs git, so it is only asked when the
            // caller is already paying for worktree discovery.
            || (group && crate::worktree::is_vouched_worktree(roots, &p.url))
    });
    // A virtual outside URL (#188) whose record was revoked, pruned, or no
    // longer matches its directory survives the retain above only on a live
    // session — and that session must stay visible, since revoking ends no
    // shell. But its page no longer resolves, so it is not a link. Fork-free
    // (`resolve` is two reads and a stat).
    for p in by_key.values_mut() {
        if let Some((parent, name)) = crate::outside::split_url(&p.url) {
            if crate::outside::resolve(parent, name).is_none() {
                p.reachable = false;
            }
        }
    }
    if group {
        group_worktrees(roots, &mut by_key);
    }
    // A final, uniform correction pass: `owned_live_names(url)` is "what
    // this project's workspace shows" for *any* project string, home or
    // owner, so it is correct whether this row was discovered via a saved
    // layout, a socket directory, or — a pure resident, re-homed by a `cd`
    // and never itself opened — only just now via `group_worktrees` above
    // (which always inserts a `live: 0` placeholder row for every worktree
    // git reports). Running it over every row, not only the ones the
    // earlier scans already touched, is what makes a resident's owner
    // count correctly even when the owner has no socket or layout of its
    // own, and makes a home whose only session is now away drop to 0
    // rather than keep counting a socket file the re-home record says is
    // no longer shown there.
    for p in by_key.values_mut() {
        let owned = crate::session::owned_live_names(&p.url);
        p.live = owned.len();
        // Age still comes only from sessions this process can inspect (see
        // the per-key scan's identical comment above) — a restart-survived
        // socket contributes to the count but not the age either way. Each
        // name's age is read from wherever its session is actually
        // *attached*: the home, for a resident, since re-homing never moves
        // the dtach socket.
        let mut by_home: std::collections::HashMap<String, Vec<String>> = Default::default();
        for (name, home) in owned {
            by_home.entry(home.unwrap_or_else(|| p.url.clone())).or_default().push(name);
        }
        p.oldest_age_secs = by_home
            .into_iter()
            .filter_map(|(home, names)| {
                crate::session::list_sessions(&home)
                    .into_iter()
                    .filter(|s| names.contains(&s.name))
                    .filter_map(|s| s.age_secs)
                    .max()
            })
            .max();
    }
    order_with_children(by_key.into_values().collect())
}

/// The checked-out branch, read straight out of `.git` — a file read, never
/// a subprocess.
///
/// `group_worktrees` learns branches from `git worktree list`, which is the
/// right tool when it is already spawning git for worktree discovery. The
/// front page labels every directory under the roots, so a process per row
/// is not available to it: on the deploy host that would be 28 spawns every
/// five seconds. A linked worktree's `.git` is a file pointing at the real
/// gitdir, so that indirection is followed. Empty when this is not a
/// repository, when HEAD is detached, or when anything cannot be read — the
/// caller then shows no branch and no expander rather than guessing one.
pub fn head_branch(dir: &std::path::Path) -> String {
    let dot = dir.join(".git");
    let gitdir = match std::fs::symlink_metadata(&dot) {
        Ok(m) if m.is_dir() => dot,
        Ok(_) => {
            let Ok(text) = std::fs::read_to_string(&dot) else { return String::new() };
            let Some(p) = text.lines().find_map(|l| l.strip_prefix("gitdir:")) else {
                return String::new();
            };
            PathBuf::from(p.trim())
        }
        Err(_) => return String::new(),
    };
    let Ok(head) = std::fs::read_to_string(gitdir.join("HEAD")) else { return String::new() };
    head.trim().strip_prefix("ref: refs/heads/").unwrap_or("").to_string()
}

/// The overview's top level: every project directory under the roots, with
/// the live counts and layouts roost knows, and no git subprocess at all.
///
/// Worktree-shaped keys are left out — a worktree is a child row, loaded by
/// `worktree_rows` when its project is opened. Without that filter a saved
/// layout for one would sit at the top level showing its whole dot-path.
///
/// An allowed outside worktree's virtual URL (#188) is the same kind of
/// child, so it is left out on the same grounds — but only while its record
/// still resolves, because only then does `worktree_rows` list it under its
/// parent. One that no longer resolves (revoked, pruned, its directory
/// replaced) survived `known_projects_inner`'s retain only on a live
/// session, already marked unreachable there; dropping it here too would
/// hide a running shell the user can no longer reach from any other row.
pub fn project_rows(roots: &[PathBuf]) -> Vec<ProjectStatus> {
    let mut ps = known_projects_inner(roots, false);
    ps.retain(|p| !p.url.contains("/.claude/worktrees/"));
    ps.retain(|p| match crate::outside::split_url(&p.url) {
        Some((parent, name)) => crate::outside::resolve(parent, name).is_none(),
        None => true,
    });
    let known: std::collections::HashSet<String> = ps.iter().map(|p| p.key.clone()).collect();
    for p in crate::projects::list_projects(roots) {
        let key = crate::projects::storage_key(&p.name);
        if known.contains(&key) {
            continue;
        }
        ps.push(ProjectStatus {
            key,
            url: p.name,
            live: 0,
            oldest_age_secs: None,
            has_layout: false,
            branch: String::new(),
            parent: None,
            reachable: true,
            outside: None,
            wt: None,
        });
    }
    for p in ps.iter_mut() {
        if let Some(dir) = crate::projects::resolve_project(roots, &p.url) {
            p.branch = head_branch(&dir);
        }
    }
    order_with_children(ps)
}

/// One project's linked worktrees, with the state chips — the expensive half
/// of the old listing, now paid only for a project the user has opened.
pub fn worktree_rows(roots: &[PathBuf], parent_key: &str) -> Vec<ProjectStatus> {
    let url = decode_key(parent_key);
    let Some(pd) = crate::projects::resolve_project(roots, &url) else { return Vec::new() };
    let state_dir = crate::wsstate::state_dir();
    let main_branch = head_branch(&pd);
    let scan = crate::claudes::claude_terminals(std::path::Path::new("/proc"));
    let mut out = Vec::new();
    let listed = crate::worktree::list(&pd);
    let canon = listed_paths(&listed);
    for w in listed.into_iter().filter(|w| !w.is_main) {
        let mut outside = None;
        let (child_url, reachable) = match rel_under_roots(roots, &w.path) {
            Some(u) => (u, true),
            None => match allowed_outside(&url, &w.path) {
                Some((virt, real)) => {
                    outside = Some(real);
                    (virt, true)
                }
                None => (w.path.to_string_lossy().into_owned(), false),
            },
        };
        let key = crate::projects::storage_key(&child_url);
        // `owned_live_names`, not `session_rows`: a resident's dtach socket
        // is still under its home, so this worktree's own native-session
        // count alone would miss a running Claude re-homed into it.
        let live = crate::session::owned_live_names(&child_url).len();
        let wt = if reachable {
            let (base, base_recorded) = match crate::worktree::read_base(&state_dir, &key) {
                Some(b) => (b, true),
                None => (main_branch.clone(), false),
            };
            const NULL_SHA: &str = "0000000000000000000000000000000000000000";
            let base_for_ahead = if base.is_empty() { NULL_SHA } else { base.as_str() };
            let st = crate::worktree::state(&w.path, base_for_ahead, &crate::worktree::real_git);
            Some(WorktreeStatus {
                claude: crate::claudes::claude_evidence_with_scan(&child_url, &scan),
                dirty: st.dirty,
                ahead: if base.is_empty() { None } else { st.ahead },
                base,
                base_recorded,
            })
        } else {
            None
        };
        out.push(ProjectStatus {
            key,
            url: child_url,
            live,
            oldest_age_secs: None,
            has_layout: false,
            branch: w.branch,
            parent: Some(parent_key.to_string()),
            reachable,
            outside,
            wt,
        });
    }
    // As in `group_worktrees`: never prune on a listing git did not give.
    if !canon.is_empty() {
        crate::outside::prune(&url, &canon);
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// Every path a `git worktree list` reported, main included, canonicalised
/// the way `outside`'s records are, for `outside::prune`. The main checkout
/// is part of the evidence: a listing of the main worktree alone is git
/// positively answering "nothing else", and a list narrowed to non-main
/// entries would turn that answer into an empty slice, which `prune`
/// refuses. A path that cannot be canonicalised (git still lists a
/// worktree whose directory is gone or unreadable) is kept as git spelled
/// it rather than dropped — dropping it would read "could not look" as
/// "not listed" and prune a record git still vouches for.
fn listed_paths(listed: &[crate::worktree::Worktree]) -> Vec<PathBuf> {
    listed.iter().map(|w| w.path.canonicalize().unwrap_or_else(|_| w.path.clone())).collect()
}

/// The virtual URL and real path of an outside worktree the user allowed
/// under `parent_url` (#188), only when its record still resolves with the
/// identity it was pinned at. A record that `find` returns but `resolve`
/// refuses is a directory that changed under the record (deleted,
/// recreated, swapped) — shown as not allowed, never as the old allow.
/// Fork-free (`outside`'s reads and a stat), though both callers are
/// off-lock anyway.
fn allowed_outside(parent_url: &str, path: &std::path::Path) -> Option<(String, String)> {
    let canon = path.canonicalize().ok()?;
    let rec = crate::outside::find(parent_url, &canon)?;
    let real = crate::outside::resolve(parent_url, &rec.name)?;
    Some((crate::outside::url_for(parent_url, &rec.name), real.to_string_lossy().into_owned()))
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
        let listed = crate::worktree::list(&dir);
        let canon = listed_paths(&listed);
        for w in listed {
            if w.is_main {
                // The repo's own entry is the main worktree; its branch is
                // git's branch, not a separate row.
                if let Some(slot) = by_key.get_mut(&key) {
                    slot.branch = w.branch;
                }
                continue;
            }
            // An allowed outside worktree (#188) is placed exactly as one
            // under a root is, under its virtual URL — including adopting
            // an entry a saved layout under that URL already put at the
            // top level. `outside` is the only difference.
            let placed = match rel_under_roots(roots, &w.path) {
                Some(u) => Some((u, None)),
                None => allowed_outside(&url, &w.path).map(|(virt, real)| (virt, Some(real))),
            };
            match placed {
                Some((child_url, outside)) => {
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
                        outside: None,
                        wt: None,
                    });
                    slot.branch = w.branch;
                    slot.parent = Some(key.clone());
                    slot.outside = outside;
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
                        outside: None,
                        wt: None,
                    });
                }
            }
        }
        // Only a listing git actually gave: an empty one is git not
        // answering, and must cost no allow its record (CLAUDE.md, absence
        // of evidence). `outside::prune` refuses it too; this is the line
        // that holds the distinction, that one the second.
        if !canon.is_empty() {
            crate::outside::prune(&url, &canon);
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
        std::env::set_var("ROOST_STATE_DIR", d.path());
        let out = f(d.path());
        std::env::remove_var("ROOST_STATE_DIR");
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
            // A layout is only a project while its directory resolves under
            // a root (see `known_projects`'s retain); the absent-directory
            // case is `a_layout_whose_directory_is_gone_is_not_listed_unless_live`.
            let root = tempfile::tempdir().unwrap();
            fs::create_dir_all(root.path().join("karpie")).unwrap();
            let roots = vec![root.path().to_path_buf()];
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

    /// A repo under `root` (known through a saved layout, so
    /// `group_worktrees` lists it at all) plus one real linked worktree,
    /// `<outside>/wt` on branch `side`, under no root. Returns the
    /// worktree's canonical path — the form `outside::allow` insists on.
    fn repo_with_outside_worktree(
        state: &std::path::Path,
        root: &std::path::Path,
        outside: &std::path::Path,
    ) -> PathBuf {
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        fs::write(repo.join("a.txt"), "x").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "init"]);
        let wt = outside.join("wt");
        run_git(&repo, &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()]);
        fs::create_dir_all(state).unwrap();
        fs::write(state.join("repo.json"), "{}").unwrap();
        wt.canonicalize().unwrap()
    }

    /// #188 §2: an allowed outside worktree is an ordinary reachable row in
    /// both listings, under its virtual URL, carrying the real path — and
    /// in the overview's rows its chips are computed on that real path
    /// (`dirty: Some(false)` is git answering about a clean checkout; the
    /// virtual URL names no directory, so git run there could only fail).
    /// The record survives both listings, which list it: a prune that
    /// compared un-canonicalised paths would have deleted it here.
    ///
    /// Revert-checked: making `allowed_outside` return `None` (the pre-#188
    /// behaviour) fails the first assertion — observed `panicked ... the
    /// allowed row must be reachable: ProjectStatus { key:
    /// "%2Fhome%2F...%2F.tmpzKr1jj%2Fwt", url: ".../.tmpzKr1jj/wt", ...
    /// reachable: false, outside: None, wt: None }`. Restored.
    #[test]
    fn an_allowed_outside_worktree_is_a_reachable_row() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let wt = repo_with_outside_worktree(state, root.path(), outside.path());
            let rec = crate::outside::allow("repo", &wt).unwrap();
            let url = crate::outside::url_for("repo", &rec.name);
            let roots = vec![root.path().to_path_buf()];

            let ps = known_projects(&roots);
            let row = ps.iter().find(|p| p.branch == "side").expect("the worktree must be listed");
            assert!(row.reachable, "the allowed row must be reachable: {row:?}");
            assert_eq!(row.url, url);
            assert_eq!(row.key, crate::projects::storage_key(&url));
            assert_eq!(row.outside.as_deref(), Some(wt.to_str().unwrap()));
            assert_eq!(row.parent.as_deref(), Some("repo"));

            let rows = worktree_rows(&roots, "repo");
            let row = rows.iter().find(|p| p.branch == "side").expect("the worktree must be listed");
            assert!(row.reachable, "{row:?}");
            assert_eq!(row.url, url);
            assert_eq!(row.outside.as_deref(), Some(wt.to_str().unwrap()));
            let st = row.wt.as_ref().expect("an allowed row gets worktree state like any reachable row");
            assert_eq!(st.dirty, Some(false), "git status must have run on the real path: {st:?}");

            assert!(crate::outside::read("repo", &rec.name).is_some(), "a listed record must not be pruned");
        });
    }

    /// No record, or a record whose identity no longer matches the
    /// directory (the spec's third row): unreachable, the raw path as the
    /// url, no `outside`. The mismatched record is the case that tells
    /// "looked the record up" from "resolved it": `outside::find` alone
    /// returns it, and only `resolve`'s identity check refuses it.
    ///
    /// Revert-checked: dropping the `outside::resolve` check from
    /// `allowed_outside` (taking `find`'s `rec.path` as-is) fails the
    /// mismatched-record assertion — observed `panicked ... a record whose
    /// identity does not match must not make the row reachable:
    /// ProjectStatus { key: "repo%2F.claude%2Foutside%2Fwt", url:
    /// "repo/.claude/outside/wt", ... reachable: true, outside:
    /// Some(".../.tmpP2j5ag/wt"), wt: None }`. Restored.
    #[test]
    fn an_unallowed_one_stays_unreachable() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let wt = repo_with_outside_worktree(state, root.path(), outside.path());
            let roots = vec![root.path().to_path_buf()];
            let raw = wt.to_string_lossy().into_owned();

            let check = |what: &str| {
                let ps = known_projects(&roots);
                let row = ps.iter().find(|p| p.branch == "side").expect("listed");
                assert!(!row.reachable && row.outside.is_none() && row.url == raw, "{what}: {row:?}");
                let rows = worktree_rows(&roots, "repo");
                let row = rows.iter().find(|p| p.branch == "side").expect("listed");
                assert!(!row.reachable && row.outside.is_none() && row.url == raw, "{what}: {row:?}");
                assert!(row.wt.is_none(), "{what}: no git state for an unreachable row: {row:?}");
            };
            check("no record at all");

            // A well-formed record naming this very path, with an identity
            // that cannot be this directory's.
            let dir = state.join("outside").join("repo");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("wt"), format!("{raw}\n1 2 3 4\n")).unwrap();
            assert!(crate::outside::find("repo", &wt).is_some(), "fixture: find must see the record");
            assert!(crate::outside::resolve("repo", "wt").is_none(), "fixture: resolve must refuse it");
            check("a record whose identity does not match must not make the row reachable");
        });
    }

    /// #188 §1 Revocation: a listing git did answer, without the recorded
    /// path, deletes the record — in both listings. What git answers after
    /// `worktree remove` is the main worktree alone, so this also pins that
    /// the main checkout's path is part of what is handed to `prune`: a
    /// listing narrowed to non-main worktrees would be empty here, and
    /// `prune` (rightly) refuses to act on an empty one.
    ///
    /// Revert-checked, three ways, each observed and restored:
    /// - `listed_paths` filtering out the main worktree: `panicked ... the
    ///   switcher's listing must prune the record of a worktree git no
    ///   longer reports`;
    /// - deleting the `prune` call from `group_worktrees`: the same;
    /// - deleting the `prune` call from `worktree_rows`: `panicked ... the
    ///   overview's listing must prune the record too`.
    #[test]
    fn a_nonempty_listing_without_the_path_prunes_its_record() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let wt = repo_with_outside_worktree(state, root.path(), outside.path());
            let repo = root.path().join("repo");
            let roots = vec![root.path().to_path_buf()];

            let rec = crate::outside::allow("repo", &wt).unwrap();
            run_git(&repo, &["worktree", "remove", "--force", wt.to_str().unwrap()]);
            let _ = known_projects(&roots);
            assert!(
                crate::outside::read("repo", &rec.name).is_none(),
                "the switcher's listing must prune the record of a worktree git no longer reports"
            );

            // The overview's listing, separately: a second worktree, allowed
            // after the switcher already ran, so only `worktree_rows` can
            // remove its record.
            let wt2 = outside.path().join("wt2");
            run_git(&repo, &["worktree", "add", "-q", "-b", "side2", wt2.to_str().unwrap()]);
            let wt2 = wt2.canonicalize().unwrap();
            let rec2 = crate::outside::allow("repo", &wt2).unwrap();
            run_git(&repo, &["worktree", "remove", "--force", wt2.to_str().unwrap()]);
            let _ = worktree_rows(&roots, "repo");
            assert!(
                crate::outside::read("repo", &rec2.name).is_none(),
                "the overview's listing must prune the record too"
            );
        });
    }

    /// I1 of #188's final review: a saved layout under an allowed outside
    /// worktree's virtual key put it at the overview's *top level* as well as
    /// under its parent — `project_rows` filtered only `.claude/worktrees/`,
    /// so the row came through as `parent: None, reachable: true`, its whole
    /// dot-path as a project name. It is a child: `worktree_rows` lists it,
    /// and the header switcher's grouping adopts the layout's row under its
    /// parent rather than listing it twice. The positive halves (the parent
    /// is at the top level, the child is in `worktree_rows`, the switcher
    /// has it exactly once) are what keep the absence from passing on a
    /// listing that simply failed.
    ///
    /// Revert-checked: with the new `split_url`/`resolve` retain removed
    /// from `project_rows` (cp backup, restored), fails on `an allowed
    /// outside worktree is a child, never a top-level project:
    /// [ProjectStatus { key: "repo%2F.claude%2Foutside%2Fwt", url:
    /// "repo/.claude/outside/wt", … has_layout: true, … parent: None,
    /// reachable: true, … }]`.
    #[test]
    fn an_allowed_outside_worktree_with_a_layout_is_not_a_top_level_project() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let wt = repo_with_outside_worktree(state, root.path(), outside.path());
            let roots = vec![root.path().to_path_buf()];
            let rec = crate::outside::allow("repo", &wt).unwrap();
            let url = crate::outside::url_for("repo", &rec.name);
            let key = crate::projects::storage_key(&url);
            fs::write(state.join(format!("{key}.json")), "{}").unwrap();

            let top = project_rows(&roots);
            assert!(top.iter().any(|p| p.url == "repo" && p.parent.is_none()), "fixture: the parent is listed: {top:?}");
            let stray: Vec<_> = top.iter().filter(|p| p.url == url).collect();
            assert!(stray.is_empty(), "an allowed outside worktree is a child, never a top-level project: {stray:?}");

            let kids = worktree_rows(&roots, "repo");
            let child = kids.iter().find(|p| p.url == url).unwrap_or_else(|| panic!("the overview lists it under its parent: {kids:?}"));
            assert!(child.reachable, "{child:?}");

            let sw = known_projects(&roots);
            let in_switcher: Vec<_> = sw.iter().filter(|p| p.url == url).collect();
            assert_eq!(in_switcher.len(), 1, "the switcher lists it once: {sw:?}");
            assert_eq!(in_switcher[0].parent.as_deref(), Some("repo"), "under its parent: {sw:?}");
        });
    }

    /// #188 §2 for the header switcher: `known_projects_with_state` keys
    /// its directory map by the row's storage key, so an allowed outside
    /// row (keyed by its virtual URL) needs its own entry there, pointing at
    /// the real directory — or its chips are silently missing. `dirty ==
    /// Some(false)` is git answering about the real, clean checkout.
    ///
    /// Revert-checked: deleting the `allowed_outside` fallback from that
    /// loop fails the first assertion — observed `panicked ... an allowed row
    /// gets the switcher's state: ProjectStatus { key:
    /// "repo%2F.claude%2Foutside%2Fwt", ... reachable: true, outside:
    /// Some(".../wt"), wt: None }`. Restored.
    #[test]
    fn known_projects_with_state_gives_an_allowed_outside_row_its_state() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let wt = repo_with_outside_worktree(state, root.path(), outside.path());
            let rec = crate::outside::allow("repo", &wt).unwrap();
            let url = crate::outside::url_for("repo", &rec.name);

            let ps = known_projects_with_state(&[root.path().to_path_buf()]);
            let row = ps.iter().find(|p| p.url == url).expect("the allowed row must be listed");
            let st = row.wt.as_ref().unwrap_or_else(|| panic!("an allowed row gets the switcher's state: {row:?}"));
            assert_eq!(st.dirty, Some(false), "git status must have run on the real path: {st:?}");
            assert!(row.outside.is_some(), "{row:?}");
        });
    }

    /// Revoking ends no shell (spec §1), so a live session under a revoked
    /// virtual URL keeps its row — but that URL no longer resolves, so the
    /// row must not be a link. Uses a real dtach socket (a `ROOST_CMD=cat`
    /// session creates none, so the row would vanish rather than dim).
    /// Before the revoke the same row is reachable,
    /// which is what shows the flip is the revoke's doing.
    ///
    /// Revert-checked: deleting the `split_url`/`resolve` pass after
    /// `known_projects_inner`'s retain fails the last assertion — observed
    /// `revoked: still listed, still live, no longer a link` with `left:
    /// Some((true, 1))`, `right: Some((false, 1))`. Restored.
    #[test]
    fn a_live_session_under_a_revoked_outside_url_is_listed_unreachable() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let wt = repo_with_outside_worktree(state, root.path(), outside.path());
            let rec = crate::outside::allow("repo", &wt).unwrap();
            let url = crate::outside::url_for("repo", &rec.name);
            let key = crate::projects::storage_key(&url);
            let roots = vec![root.path().to_path_buf()];

            // A real dtach session under the virtual URL's key, as one
            // started on that page leaves it: the socket the listing counts.
            let sock_dir = state.join("sock").join(&key);
            fs::create_dir_all(&sock_dir).unwrap();
            let sock = sock_dir.join("term1");
            if spawn_detached_dtach(&sock).is_none() {
                eprintln!("skipped: dtach unavailable");
                return;
            }
            let ps = known_projects(&roots);
            let before = ps.iter().find(|p| p.key == key).map(|p| (p.reachable, p.live));
            crate::outside::revoke("repo", &rec.name).unwrap();
            let ps = known_projects(&roots);
            let after = ps.iter().find(|p| p.key == key).map(|p| (p.reachable, p.live));
            let alive = sock.exists();
            kill_test_pids(&sock);

            assert_eq!(before, Some((true, 1)), "fixture: allowed and live, the row is a link");
            assert!(alive, "listing must not have reaped the session");
            assert_eq!(after, Some((false, 1)), "revoked: still listed, still live, no longer a link");
        });
    }

    /// I2 of #188's final review: revoking an allow, or the directory being
    /// replaced under it, makes the virtual URL stop resolving — and
    /// `resolve_project` answering `None` must never read as "deleted" to a
    /// sweep. The earlier survival tests asserted after the revoke intent,
    /// which contains no kill code, so they could not tell protection from
    /// no sweep at all. This one runs the real sweep, `reconcile`, against
    /// a real dtach master working in the outside directory, with `.origin`
    /// recording its real path, in each state the URL can be in: (a)
    /// allowed, (b) revoked, (c) re-allowed and then its directory replaced
    /// at the same path, so a record exists but its identity no longer
    /// matches. The complement — the directory really deleted — is reaped,
    /// which is what shows the sweep was live and able to kill all along.
    ///
    /// Revert-checked: with `confirmed_gone`'s `None` arm replaced by
    /// `true` (cp backup, restored), fails at state (c) — `replaced: nothing
    /// reaped` with `left: 1, right: 0`, the master SIGKILLed. State (b)
    /// stayed green under that revert: the directory still stands at its
    /// recorded path with its recorded inode, so #178/#182's move evidence
    /// (the shell's cwd, and the sibling scan) rescued it as a second line,
    /// reading it as `Unfollowable`. In (c) the inode changed and the
    /// shell's cwd is `(deleted)`, so `.origin`'s recorded path was the
    /// only thing standing between a replaced directory and a dead shell.
    /// The companion test below pins (b)'s two layers.
    #[test]
    fn a_live_session_under_an_outside_url_survives_the_sweep_until_its_directory_is_deleted() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let wt = repo_with_outside_worktree(state, root.path(), outside.path());
            let roots = vec![root.path().to_path_buf()];
            let rec = crate::outside::allow("repo", &wt).unwrap();
            let url = crate::outside::url_for("repo", &rec.name);
            let key = crate::projects::storage_key(&url);
            let sock_dir = state.join("sock").join(&key);
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            spawn_detached_dtach_in(&sock_path, &wt).expect("dtach must be available for this test");
            record_origin(&url, &wt);
            assert_eq!(
                fs::read(sock_dir.join(".origin")).ok().map(|b| String::from_utf8_lossy(&b).trim_end().to_string()),
                Some(wt.display().to_string()),
                "setup: .origin records the real path, not the virtual URL"
            );
            assert!(socket_has_process(&sock_path), "setup: the session is live");

            // Each check kills the master before failing, so a failure leaves
            // no `sleep 30` behind.
            let check = |what: &str, report: ReapReport| {
                let alive = socket_has_process(&sock_path);
                if report.gone_projects != 0 || !alive {
                    kill_test_pids(&sock_path);
                }
                assert_eq!(report.gone_projects, 0, "{what}: nothing reaped");
                assert!(alive, "{what}: the master is alive");
            };

            assert!(crate::projects::resolve_project(&roots, &url).is_some(), "(a) setup: the URL resolves");
            check("allowed", reconcile(&roots));

            crate::outside::revoke("repo", &rec.name).unwrap();
            assert!(crate::projects::resolve_project(&roots, &url).is_none(), "(b) setup: the URL no longer resolves");
            check("revoked", reconcile(&roots));

            let again = crate::outside::allow("repo", &wt).unwrap();
            assert_eq!(again.name, rec.name, "(c) setup: re-allowed under the same name");
            fs::remove_dir_all(&wt).unwrap();
            fs::create_dir(&wt).unwrap();
            assert!(crate::outside::read("repo", &rec.name).is_some(), "(c) setup: the record is still there");
            assert!(
                crate::projects::resolve_project(&roots, &url).is_none(),
                "(c) setup: the identity no longer matches, so the URL does not resolve"
            );
            check("replaced", reconcile(&roots));

            // The complement: really deleted. The shell's cwd is a deleted
            // directory, so there is no move evidence either.
            fs::remove_dir_all(&wt).unwrap();
            let gone = reconcile(&roots);
            let alive = socket_has_process(&sock_path);
            kill_test_pids(&sock_path);
            assert!(gone.gone_projects >= 1, "deleted: the session is reaped ({})", gone.gone_projects);
            assert!(!alive, "deleted: its master is ended");
        });
    }

    /// The companion to the test above for state (b) alone: a session
    /// under the virtual key whose shell is in `/`, not in the outside
    /// directory, so #178's shell-cwd evidence cannot vouch for it.
    /// Revoked, the URL no longer resolves; `.origin`'s recorded path,
    /// still present, and #182's sibling scan are what stand against
    /// "gone".
    ///
    /// Two layers hold this, and either alone is enough — the same shape as
    /// `an_empty_listing_prunes_nothing`. Revert-checked (cp backup,
    /// restored): with only `confirmed_gone`'s `None` arm replaced by
    /// `true`, this stays green (observed) — `sibling_evidence` (#182)
    /// finds the recorded inode standing at its own path and the sweep
    /// reads it as `Unfollowable`. With that revert *and*
    /// `record_move_with`'s `sibling_evidence` call replaced by
    /// `MoveEvidence::None`, fails `revoked: nothing reaped` with `left: 1,
    /// right: 0`, the master SIGKILLed.
    #[test]
    fn a_revoked_outside_url_is_not_reaped_on_a_shell_outside_its_directory() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let wt = repo_with_outside_worktree(state, root.path(), outside.path());
            let roots = vec![root.path().to_path_buf()];
            let rec = crate::outside::allow("repo", &wt).unwrap();
            let url = crate::outside::url_for("repo", &rec.name);
            let sock_dir = state.join("sock").join(crate::projects::storage_key(&url));
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            spawn_detached_dtach_in(&sock_path, std::path::Path::new("/")).expect("dtach must be available for this test");
            record_origin(&url, &wt);
            let first = reconcile(&roots);
            assert_eq!(first.gone_projects, 0, "setup: allowed, nothing reaped");

            crate::outside::revoke("repo", &rec.name).unwrap();
            assert!(crate::projects::resolve_project(&roots, &url).is_none(), "setup: the URL no longer resolves");
            let report = reconcile(&roots);
            let alive = socket_has_process(&sock_path);
            kill_test_pids(&sock_path);
            assert_eq!(report.gone_projects, 0, "revoked: nothing reaped");
            assert!(alive, "revoked: the master is alive");
        });
    }

    /// Restores a directory's mode on drop, so a failing assertion cannot
    /// leave a tempdir behind that its own cleanup cannot enter.
    struct RestoreMode(PathBuf, u32);
    impl Drop for RestoreMode {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(self.1));
        }
    }

    /// An empty listing is git not answering, never "no worktrees left"
    /// (CLAUDE.md, absence of evidence): neither listing may prune on it.
    /// `worktree::list` has no injection seam, so git is made unable to
    /// answer for real by an unreadable `.git`. The row vanishing from both
    /// listings is the positive check that git really did fail — without
    /// it this would pass just as well with git answering normally.
    ///
    /// Revert-checked: replacing both callers' `!canon.is_empty()` guards
    /// with `true` *and* deleting `outside::prune_in`'s own empty-listing
    /// guard fails the last assertion — observed `panicked ... an empty
    /// listing must prune nothing`. Replacing only the callers' guards
    /// leaves this green (observed, 4 passed): `prune_in`'s guard is a
    /// second line, and either one alone holds. Restored.
    #[test]
    fn an_empty_listing_prunes_nothing() {
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipped: root reads an unreadable .git anyway");
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let wt = repo_with_outside_worktree(state, root.path(), outside.path());
            let roots = vec![root.path().to_path_buf()];
            let rec = crate::outside::allow("repo", &wt).unwrap();

            let dotgit = root.path().join("repo").join(".git");
            let _restore = RestoreMode(dotgit.clone(), 0o755);
            fs::set_permissions(&dotgit, fs::Permissions::from_mode(0o000)).unwrap();

            let ps = known_projects(&roots);
            assert!(!ps.iter().any(|p| p.branch == "side"), "fixture: git must not have answered: {ps:?}");
            let rows = worktree_rows(&roots, "repo");
            assert!(rows.is_empty(), "fixture: git must not have answered: {rows:?}");
            assert!(crate::outside::read("repo", &rec.name).is_some(), "an empty listing must prune nothing");
        });
    }

    /// A resident's dtach socket is physically under its *home*, never the
    /// worktree the re-home record names as its owner (home.rs's module
    /// doc) — so `worktree_rows`' per-worktree `live` count must come from
    /// `owned_live_names(&child_url)`, which follows the record, not
    /// `session_rows(&child_url)`, which only ever sees the worktree's own
    /// native sessions and would report 0 for a running re-homed Claude.
    ///
    /// `ROOST_ROOTS` matters here in a way `with_state`'s other tests never
    /// need: `owned_live_names` calls the production, fork-free
    /// `home::resolves`, which reads `projects::roots()` from the
    /// environment regardless of the `roots` slice this test passes
    /// explicitly to `worktree_rows` — so without it the record would be
    /// real but unresolved, and the resident would fall back to showing at
    /// home, proving nothing about this function's own counting.
    ///
    /// Revert-checked: reverting the `live` line back to
    /// `session_rows(&child_url).len()` fails the assertion — observed:
    /// `panicked ... the resident must be counted under its owner:
    /// assertion \`left == right\` failed\n  left: 0\n right: 1`.
    #[test]
    fn worktree_rows_counts_a_resident_not_just_its_own_native_sessions() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", root.path().join("state"));
        std::env::set_var("ROOST_ROOTS", root.path());
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        fs::write(repo.join("a.txt"), "x").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "init"]);
        run_git(&repo, &["worktree", "add", "-q", "-b", "feat", ".claude/worktrees/feat"]);
        let child_url = "repo/.claude/worktrees/feat".to_string();

        // The resident's socket is under its home, "repo"; only the record
        // names the worktree as its owner.
        let _att = crate::session::reserve_and_attach("repo", "term3", &repo).unwrap();
        crate::home::record("repo", "term3", &child_url).unwrap();

        let roots = vec![root.path().to_path_buf()];
        let rows = worktree_rows(&roots, "repo");
        let row = rows.iter().find(|r| r.url == child_url).expect("the worktree must be listed");
        assert_eq!(row.live, 1, "the resident must be counted under its owner");

        crate::session::kill_project("repo");
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// `group_worktrees` inserts a row for every worktree git reports,
    /// `live: 0`, whether or not anyone ever opened it or saved a layout
    /// for it (see the sibling test above) — which is exactly what lets a
    /// *pure* resident (re-homed by a `cd`, never shown as its own tab) get
    /// a correct count here too: `known_projects_inner`'s final pass must
    /// recompute every row's `live` from `owned_live_names`, not just the
    /// rows a socket directory or saved layout already produced.
    ///
    /// Also pins the other half: the home's own `live` must drop to 0 once
    /// its only session is away, which a raw socket-file *count* (the
    /// pre-existing per-key floor) could never express — it has no session
    /// names to exclude by.
    ///
    /// Revert-checked: reverting the final correction pass (so a row's
    /// `live` is whatever the socket/layout scan already set, 0 for a never
    /// -opened worktree) fails the worktree assertion — observed: `panicked
    /// ... the resident must be counted under the worktree it was re-homed
    /// into: assertion \`left == right\` failed\n  left: 0\n right: 1`.
    #[test]
    fn known_projects_counts_a_resident_under_a_worktree_that_was_never_separately_opened() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_CMD", "cat");
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        std::env::set_var("ROOST_STATE_DIR", &state);
        std::env::set_var("ROOST_ROOTS", root.path());
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        fs::write(repo.join("a.txt"), "x").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "init"]);
        run_git(&repo, &["worktree", "add", "-q", "-b", "feat", ".claude/worktrees/feat"]);

        // Only the repo has ever been opened: the worktree has neither its
        // own saved layout nor any socket of its own.
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("repo.json"), "{}").unwrap();

        let _att = crate::session::reserve_and_attach("repo", "term3", &repo).unwrap();
        crate::home::record("repo", "term3", "repo/.claude/worktrees/feat").unwrap();

        let ps = known_projects(&[root.path().to_path_buf()]);
        let child = ps
            .iter()
            .find(|p| p.url == "repo/.claude/worktrees/feat")
            .expect("the worktree must be listed even though it was never separately opened");
        assert_eq!(child.live, 1, "the resident must be counted under the worktree it was re-homed into");
        let parent = ps.iter().find(|p| p.key == "repo").unwrap();
        assert_eq!(parent.live, 0, "and no longer under its home, now that it is away");

        crate::session::kill_project("repo");
        std::env::remove_var("ROOST_ROOTS");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    // Regression: `known_projects_with_state` used to key its worktree
    // directory map by bare branch name. This codebase's own auto-naming
    // (claude-1, claude-2, ... per repo, minted independently in every
    // project) makes two different projects sharing a branch name the
    // expected common case, not a corner case — when it happens, a
    // branch-keyed map lets one project's row silently read another
    // project's directory, including reporting a dirty worktree as clean
    // and eligible for the remove button.
    //
    // Revert-checked: reverting the fix (keying `dirs` by `w.branch.clone()`
    // and looking up via `dirs.get(&p.branch)`, as it originally shipped)
    // makes this fail — observed:
    //   thread '...' panicked at src/registry.rs:...:
    //   assertion `left == right` failed: a's worktree is untouched
    //     left: Some(true)
    //    right: Some(false)
    // ("a"'s row read "b"'s directory: both repos mint "claude-1", the
    // branch-keyed map's second insert — "b", sorted after "a" — silently
    // overwrote "a"'s entry, so both rows resolved to "b"'s worktree).
    // Restored afterward.
    #[test]
    fn worktree_state_is_not_confused_across_projects_sharing_a_branch_name() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            let repo_a = root.path().join("a");
            let repo_b = root.path().join("b");
            for repo in [&repo_a, &repo_b] {
                fs::create_dir_all(repo).unwrap();
                run_git(repo, &["init", "-q", "-b", "main"]);
                run_git(repo, &["config", "user.email", "t@t"]);
                run_git(repo, &["config", "user.name", "t"]);
                fs::write(repo.join("f.txt"), "x").unwrap();
                run_git(repo, &["add", "."]);
                run_git(repo, &["commit", "-qm", "init"]);
            }

            // Both repos independently create a worktree of the same name —
            // the collision this test exists to catch.
            let key_of_a = |n: &str| crate::projects::storage_key(&format!("a/.claude/worktrees/{n}"));
            let key_of_b = |n: &str| crate::projects::storage_key(&format!("b/.claude/worktrees/{n}"));
            let ca = crate::worktree::create_named(&repo_a, state, "dup", &key_of_a, &crate::worktree::real_git).unwrap();
            let cb = crate::worktree::create_named(&repo_b, state, "dup", &key_of_b, &crate::worktree::real_git).unwrap();
            assert_eq!(ca.name, "worktree-dup");
            assert_eq!(cb.name, "worktree-dup", "both repos independently create the same name");

            // Dirty in b's worktree only.
            fs::write(cb.path.join("untracked.txt"), "y").unwrap();

            fs::create_dir_all(state).unwrap();
            fs::write(state.join("a.json"), "{}").unwrap();
            fs::write(state.join("b.json"), "{}").unwrap();

            let ps = known_projects_with_state(&[root.path().to_path_buf()]);
            let a_child = ps.iter().find(|p| p.parent.as_deref() == Some("a"))
                .expect("a's worktree must be listed");
            let b_child = ps.iter().find(|p| p.parent.as_deref() == Some("b"))
                .expect("b's worktree must be listed");
            assert_eq!(a_child.wt.as_ref().and_then(|w| w.dirty), Some(false), "a's worktree is untouched");
            assert_eq!(b_child.wt.as_ref().and_then(|w| w.dirty), Some(true), "b's worktree has an untracked file");
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
            // `ROOST_CMD=cat` means this creates no socket of its own (that
            // is gated on the command being dtach), which is exactly the shape
            // wanted: one in memory, two on disk.
            let _sg = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::set_var("ROOST_CMD", "cat");
            let _att = crate::session::reserve_and_attach("proj", "shell", &root.path().join("proj")).unwrap();
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
            std::env::remove_var("ROOST_CMD");
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
            let root = tempfile::tempdir().unwrap();
            fs::create_dir_all(root.path().join("karpie/src")).unwrap();
            let ps = known_projects(&[root.path().to_path_buf()]);
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
    /// roost process leaves behind. Returns `None` (rather than
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
    // roost process has no entry in `session`'s in-memory map, so
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

    /// `spawn_detached_dtach`, with the session's program started inside `cwd`
    /// — where a real roost shell starts — so `/proc/<shell>/cwd` says
    /// something about the project directory.
    fn spawn_detached_dtach_in(sock_path: &std::path::Path, cwd: &std::path::Path) -> Option<()> {
        let status = std::process::Command::new("dtach")
            .args(["-n", sock_path.to_str().unwrap(), "sleep", "30"])
            .current_dir(cwd)
            .status()
            .ok()?;
        status.success().then_some(())
    }

    /// #178: `git worktree move`, or a plain `mv` of a project, leaves the
    /// recorded path `NotFound` — but the directory was moved, not deleted,
    /// and the shell inside it is still working in it. A shell whose cwd
    /// still reaches the recorded directory (same device and inode) is
    /// positive evidence of a move, and the sweep must not kill it.
    ///
    /// Revert-checked: on the pre-fix code, and again with the inode match in
    /// `moved_not_deleted` disabled, this fails — "a moved project is not a
    /// gone project", the session reaped.
    #[test]
    fn a_live_session_whose_project_was_moved_is_not_killed() {
        with_state(|state| {
            let sock_dir = state.join("sock/moved-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            let root = tempfile::tempdir().unwrap();
            let project_dir = root.path().join("moved-proj");
            fs::create_dir_all(&project_dir).unwrap();
            spawn_detached_dtach_in(&sock_path, &project_dir).expect("dtach must be available for this test");
            assert!(socket_has_process(&sock_path), "test setup: the session must be live");

            let confirming = reconcile(&[root.path().to_path_buf()]);
            assert_eq!(confirming.gone_projects, 0, "test setup: nothing reaped while the directory exists");

            fs::rename(&project_dir, root.path().join("renamed-proj")).unwrap();
            let report = reconcile(&[root.path().to_path_buf()]);

            assert_eq!(report.gone_projects, 0, "a moved project is not a gone project");
            assert!(socket_has_process(&sock_path), "the shell working in the moved directory must survive");
            kill_test_pids(&sock_path);
        });
    }

    /// `move_evidence` names the *new* path, not just yes/no — #158 needs
    /// that to follow the rename. The shell sits in a subdirectory, so this
    /// also pins down which ancestor the match has to be: the one carrying
    /// the recorded inode, not the cwd itself.
    ///
    /// Revert-checked: returning `MoveEvidence::Moved(cwd.clone())` instead
    /// of the matching ancestor fails this test with
    /// `Moved(".../ev-renamed/sub")` where `Moved(".../ev-renamed")` was
    /// expected — the cwd one level too deep, because `sub` itself does not
    /// carry the recorded inode, only its parent does.
    #[test]
    fn move_evidence_names_the_new_directory() {
        with_state(|state| {
            let sock_dir = state.join("sock/ev-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            let root = tempfile::tempdir().unwrap();
            let project_dir = root.path().join("ev-proj");
            fs::create_dir_all(project_dir.join("sub")).unwrap();
            // The shell sits in a subdirectory: the evidence is the ancestor
            // that carries the recorded inode, not the cwd itself.
            spawn_detached_dtach_in(&sock_path, &project_dir.join("sub")).expect("dtach must be available");
            let _ = reconcile(&[root.path().to_path_buf()]);
            let moved_to = root.path().join("ev-renamed");
            fs::rename(&project_dir, &moved_to).unwrap();
            let snap = process_snapshot().expect("ps");
            assert_eq!(
                move_evidence(&sock_dir, &snap, std::path::Path::new("/proc")),
                MoveEvidence::Moved { dir: moved_to.canonicalize().unwrap(), legacy: false },
            );
            kill_test_pids(&sock_path);
        });
    }

    fn git_in(dir: &std::path::Path, args: &[&str]) {
        let st = std::process::Command::new("git").arg("-C").arg(dir).args(args).status().unwrap();
        assert!(st.success(), "git {args:?} in {}", dir.display());
    }

    /// #158 §5: a worktree renamed with `git worktree move` while a session
    /// is working in it. The sweep must write `.moved-to` naming the new URL,
    /// and following must make the session a resident of the new project and
    /// hand the new key the old key's `.base` — all without moving the
    /// session itself: its socket, and so its liveness, stay at the old key.
    ///
    /// `owned_live_names` is the real one, under `ROOST_ROOTS`: only the
    /// `.moved-to` widening of `home::resolves` lets it see a resident whose
    /// home is itself a worktree (#158 §5b). And through the old URL,
    /// `session::attach_existing` — what `term.rs` calls for it — reaches the
    /// session and refuses to start one.
    ///
    /// Revert-checked, run and restored (Task 8, and again once the arm moved
    /// into `owner_dir_with`): with the `.moved-to` arm of
    /// `home::owner_dir_with` removed, fails "the new project lists the
    /// session as its resident" — the real resolver, not a stand-in, now
    /// carries it.
    ///
    /// Revert-checked, run and restored: `home::follow_move` skipped in
    /// `follow_moves` fails "the session follows the rename" — left `Here`;
    /// `key_has_sockets` forced to `false` in the `.base` sweep fails "the
    /// new key inherits the base" — left `None`, the old `.base` reaped by
    /// the same pass, before following could copy it; the `.moved-to`
    /// removal in `confirmed_gone` dropped fails "a stale .moved-to is
    /// removed once the old URL resolves again".
    #[test]
    fn a_renamed_worktree_takes_its_sessions_with_it() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("ROOST_CMD");
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let repo = root.path().join("mvrepo");
            fs::create_dir_all(&repo).unwrap();
            git_in(&repo, &["init", "-q", "-b", "main"]);
            git_in(&repo, &["config", "user.email", "t@t"]);
            git_in(&repo, &["config", "user.name", "t"]);
            fs::write(repo.join("a.txt"), "x").unwrap();
            git_in(&repo, &["add", "."]);
            git_in(&repo, &["commit", "-qm", "init"]);
            git_in(&repo, &["worktree", "add", "-q", "-b", "worktree-a", ".claude/worktrees/worktree-a"]);
            let old = "mvrepo/.claude/worktrees/worktree-a";
            let new = "mvrepo/.claude/worktrees/worktree-b";
            let (old_key, new_key) = (crate::projects::storage_key(old), crate::projects::storage_key(new));
            crate::worktree::write_base(state, &old_key, "main").unwrap();

            let sock_dir = state.join("sock").join(&old_key);
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            spawn_detached_dtach_in(&sock_path, &repo.join(".claude/worktrees/worktree-a"))
                .unwrap_or_else(|| panic!("dtach must be available for this test"));
            let confirming = reconcile(&roots);
            assert!(confirming.gone_projects == 0 && confirming.moved.is_empty(), "test setup: nothing moved yet");
            assert!(origin_identity_path(&sock_dir).exists(), "test setup: the identity is recorded");

            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
            let report = reconcile(&roots);
            assert_eq!(report.gone_projects, 0, "a renamed worktree is not a gone one");
            assert_eq!(
                report.moved,
                vec![Moved {
                    old_url: old.into(),
                    new_url: new.into(),
                    live: vec!["shell".into()],
                    fresh: true,
                    prev: None,
                    shells: vec![("shell".into(), repo.join(".claude/worktrees/worktree-b").canonicalize().unwrap())],
                }]
            );
            follow_moves(&report.moved);

            assert_eq!(fs::read_to_string(sock_dir.join(".moved-to")).unwrap(), format!("{new}\n"));
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(sock_dir.join(".moved-to")).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, ".moved-to is private");
            }
            assert_eq!(crate::home::moved_to(old).as_deref(), Some(new));
            assert_eq!(crate::home::owner_of(old, "shell"), crate::home::Owner::Elsewhere(new.into()), "the session follows the rename");
            assert!(
                crate::session::owned_live_names(new).contains(&("shell".to_string(), Some(old.to_string()))),
                "the new project lists the session as its resident"
            );
            let new_dir = crate::projects::resolve_project(&roots, new).expect("the new URL resolves");
            let att = crate::session::attach_existing(old, "shell", &new_dir, new)
                .unwrap_or_else(|e| panic!("an attach through the old URL reaches the existing session: {e}"));
            crate::session::detach(&att.key, att.id);
            crate::session::reserve(old, "fresh", None);
            let err = match crate::session::attach_existing(old, "fresh", &new_dir, new) {
                Ok(_) => panic!("an attach that would spawn under the old key spawned"),
                Err(e) => e,
            };
            assert!(err.contains("renamed"), "{err}");
            assert!(
                matches!(fs::symlink_metadata(sock_dir.join("fresh")), Err(ref e) if e.kind() == std::io::ErrorKind::NotFound),
                "no socket under the old key"
            );
            crate::session::release(old, "fresh");
            assert_eq!(crate::worktree::read_base(state, &new_key).as_deref(), Some("main"), "the new key inherits the base");
            assert!(socket_has_process(&sock_path), "the master is still alive, at its home socket");

            let again = reconcile(&roots);
            assert_eq!(again.moved.len(), 1);
            assert!(!again.moved[0].fresh, "an unchanged .moved-to is not a new rename");

            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-b", ".claude/worktrees/worktree-a"]);
            let back = reconcile(&roots);
            assert!(back.moved.is_empty(), "the old URL resolves again: nothing is moved");
            assert_eq!(back.gone_projects, 0, "moving back ends nothing");
            assert!(!sock_dir.join(".moved-to").exists(), "a stale .moved-to is removed once the old URL resolves again");
            assert!(
                crate::session::owned_live_names(old).contains(&("shell".to_string(), None)),
                "moved back: the session shows natively at A again, whatever its record says"
            );
            assert!(!crate::session::owned_live_names(new).iter().any(|(n, _)| n == "shell"), "and no longer at B");
            assert!(socket_has_process(&sock_path), "moving back ends nothing");
            kill_test_pids(&sock_path);
            std::env::remove_var("ROOST_ROOTS");
        });
    }

    /// A git repo under `root` named `name` with one commit, and worktree
    /// `.claude/worktrees/worktree-a` on branch `worktree-a`.
    fn repo_with_worktree_a(root: &std::path::Path, name: &str) -> PathBuf {
        let repo = root.join(name);
        fs::create_dir_all(&repo).unwrap();
        git_in(&repo, &["init", "-q", "-b", "main"]);
        git_in(&repo, &["config", "user.email", "t@t"]);
        git_in(&repo, &["config", "user.name", "t"]);
        fs::write(repo.join("a.txt"), "x").unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "init"]);
        git_in(&repo, &["worktree", "add", "-q", "-b", "worktree-a", ".claude/worktrees/worktree-a"]);
        repo
    }

    /// Spec §5 "Ordering with the R18 sweep": the gone-owner sweep runs every
    /// tick, page loads or not, so it can be first to notice a rename. With
    /// no `reconcile` between `git worktree move` and the sweep, there is no
    /// `.moved-to` yet; the sweep must find the evidence itself (A's native
    /// session) rather than forget the main-checkout terminal re-homed into A.
    ///
    /// #183: the evidence is the resident record's own identity, found by
    /// `find_by_identity` beside A's recorded `.origin` — not A's key as a
    /// whole, so A's own session is left for `reconcile` to follow.
    ///
    /// Revert-checked, run and restored: with `find_by_identity`'s sibling
    /// scan (candidate 2) skipped, fails "R18 follows the rename it found
    /// first" — left `Here`, the record forgotten. (Before #183: with
    /// `follow_gone_owner`'s `record_move` branch removed, the same.) With
    /// R18's `.moved-to` write put back (gated on the key's own identity, as
    /// Task 4 first had it), fails "R18 leaves the key's .moved-to to
    /// reconcile" — left `Some(".../worktree-b\n")` — and, with that
    /// assertion removed, "so the next pass records the rename fresh" —
    /// left `[(…a, …b, false)]`: R18's marker used up the fresh follow, so
    /// no `ProjectMoved` and no `rename_known_worktree`.
    #[test]
    fn the_gone_owner_sweep_follows_a_rename_no_reconcile_has_seen() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let repo = repo_with_worktree_a(root.path(), "ordrepo");
            let a = "ordrepo/.claude/worktrees/worktree-a";
            let b = "ordrepo/.claude/worktrees/worktree-b";
            let sock_dir = state.join("sock").join(crate::projects::storage_key(a));
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            spawn_detached_dtach_in(&sock_path, &repo.join(".claude/worktrees/worktree-a"))
                .unwrap_or_else(|| panic!("dtach must be available for this test"));
            let _ = reconcile(&roots); // records A's identity; nothing after this
            crate::home::record("ordrepo", "term3", a).unwrap();
            crate::home::record_identity("ordrepo", "term3", &repo.join(".claude/worktrees/worktree-a")).unwrap();

            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
            let home_dir_of = |p: &str| (p == "ordrepo").then(|| repo.clone());
            crate::claudes::sweep_gone_owners(&home_dir_of, &std::sync::Mutex::new(Default::default()), &[]);

            let got = crate::home::owner_of("ordrepo", "term3");
            let moved_to = fs::read_to_string(sock_dir.join(".moved-to")).ok();
            let shell_after_r18 = crate::home::owner_of(a, "shell");
            let next = reconcile(&roots);
            let fresh: Vec<(String, String, bool)> = next.moved.iter().map(|m| (m.old_url.clone(), m.new_url.clone(), m.fresh)).collect();
            follow_moves(&next.moved);
            let shell = crate::home::owner_of(a, "shell");
            let recorded = fs::read_to_string(sock_dir.join(".moved-to")).ok();
            kill_test_pids(&sock_path);
            std::env::remove_var("ROOST_ROOTS");
            assert_eq!(got, crate::home::Owner::Elsewhere(b.into()), "R18 follows the rename it found first");
            assert_eq!(moved_to, None, "R18 leaves the key's .moved-to to reconcile");
            assert_eq!(shell_after_r18, crate::home::Owner::Here, "R18 moves only the record it found; A's own session is reconcile's");
            assert_eq!(fresh, vec![(a.to_string(), b.to_string(), true)], "so the next pass records the rename fresh, and announces it");
            assert_eq!(recorded.as_deref(), Some(&*format!("{b}\n")), "and that pass records it");
            assert_eq!(shell, crate::home::Owner::Elsewhere(b.into()), "A's own session follows it");
        });
    }

    /// Positive evidence of a followable rename whose `.moved-to` cannot be
    /// written is "cannot tell", not "unfollowable": the R18 sweep must keep
    /// the record and retry, rather than forget a terminal whose worktree is
    /// alive under its new name. The key directory is made read-only, so the
    /// temp file cannot be created.
    ///
    /// Revert-checked, run and restored: with the write-failure arm back to
    /// `MoveRecord::Unfollowable`, fails "an unrecordable rename is cannot
    /// tell" — observed `Unfollowable`.
    #[test]
    fn a_rename_that_cannot_be_recorded_is_cannot_tell() {
        use std::os::unix::fs::PermissionsExt;
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            let roots = vec![root.path().to_path_buf()];
            let repo = repo_with_worktree_a(root.path(), "rorepo");
            let a = "rorepo/.claude/worktrees/worktree-a";
            let sock_dir = state.join("sock").join(crate::projects::storage_key(a));
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            spawn_detached_dtach_in(&sock_path, &repo.join(".claude/worktrees/worktree-a"))
                .unwrap_or_else(|| panic!("dtach must be available for this test"));
            let _ = reconcile(&roots);
            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);

            fs::set_permissions(&sock_dir, fs::Permissions::from_mode(0o500)).unwrap();
            let writable = fs::write(sock_dir.join(".probe"), "").is_ok();
            let snap = process_snapshot().expect("ps");
            let got = record_move_with(&roots, a, &sock_dir, &snap, std::path::Path::new("/proc"), false);
            fs::set_permissions(&sock_dir, fs::Permissions::from_mode(0o700)).unwrap();
            kill_test_pids(&sock_path);
            if writable {
                eprintln!("skipped: this user can write a 0o500 directory");
                return;
            }
            assert!(matches!(got, MoveRecord::CannotTell), "an unrecordable rename is cannot tell: {got:?}");
        });
    }

    /// Spec §5 "Cleanup", the sequence that makes a lingering `.moved-to`
    /// dangerous: A renamed to B and followed; A's sessions end; a *new* A
    /// is made, a terminal re-homed into it, and that A removed. R18 must
    /// forget the new resident — not redirect it to B on the strength of a
    /// record about a different directory.
    ///
    /// Revert-checked, run and restored: without the `!any_held` removal in
    /// `reconcile_with`, fails "a .moved-to does not outlive its sessions".
    /// The final assertion is held twice: with that removal *and* R18
    /// trusting a resolving `.moved-to` (the first fix's branch put back
    /// ahead of `record_move`), fails "R18 forgets a resident of the new A"
    /// — left `Elsewhere(".../worktree-b")`. And again by #182's sibling
    /// scan run for a key with no live session: the stale identity is B's,
    /// the scan finds B, and R18 hands the new A's resident to it — the same
    /// failure, which is why `record_move_with` gates the scan on `live`.
    #[test]
    fn a_moved_to_does_not_outlive_its_sessions() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let repo = repo_with_worktree_a(root.path(), "stalerepo");
            let a = "stalerepo/.claude/worktrees/worktree-a";
            let b = "stalerepo/.claude/worktrees/worktree-b";
            let sock_dir = state.join("sock").join(crate::projects::storage_key(a));
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            spawn_detached_dtach_in(&sock_path, &repo.join(".claude/worktrees/worktree-a"))
                .unwrap_or_else(|| panic!("dtach must be available for this test"));
            let _ = reconcile(&roots);
            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
            follow_moves(&reconcile(&roots).moved);
            assert!(sock_dir.join(".moved-to").exists(), "setup: the rename was recorded");

            kill_test_pids(&sock_path);
            let _ = reconcile(&roots);
            let lingered = sock_dir.join(".moved-to").exists();

            git_in(&repo, &["worktree", "add", "-q", "-b", "worktree-a2", ".claude/worktrees/worktree-a"]);
            crate::home::record("stalerepo", "term4", a).unwrap();
            record_origin("stalerepo", &repo); // as attach writes it for term4's socket
            git_in(&repo, &["worktree", "remove", ".claude/worktrees/worktree-a"]);
            let home_dir_of = |p: &str| (p == "stalerepo").then(|| repo.clone());
            crate::claudes::sweep_gone_owners(&home_dir_of, &std::sync::Mutex::new(Default::default()), &[]);
            let got = crate::home::owner_of("stalerepo", "term4");
            std::env::remove_var("ROOST_ROOTS");

            assert!(!lingered, "a .moved-to does not outlive its sessions");
            assert_eq!(got, crate::home::Owner::Here, "R18 forgets a resident of the new A, not redirect it to {b}");
        });
    }

    /// `dtach -n` running `sh -c script sh args…` in `cwd`.
    fn spawn_detached_sh_in(sock_path: &std::path::Path, cwd: &std::path::Path, script: &str, args: &[&str]) -> Option<()> {
        let status = std::process::Command::new("dtach")
            .args(["-n", sock_path.to_str().unwrap(), "sh", "-c", script, "sh"])
            .args(args)
            .current_dir(cwd)
            .status()
            .ok()?;
        status.success().then_some(())
    }

    /// The session leaders of whatever holds `sock_path`, as the sweep sees
    /// them — what `move_evidence` reads the cwd of.
    fn leaders_of(sock_path: &std::path::Path) -> Vec<u32> {
        let proc_root = std::path::Path::new("/proc");
        let own = match crate::procsess::session_of(proc_root, std::process::id()) {
            crate::procsess::Sid::In(s) => Some(s),
            _ => None,
        };
        let holders = pids_holding(&process_snapshot().unwrap_or_default(), sock_path);
        crate::procsess::target_sessions(proc_root, &holders, own).unwrap_or_default()
    }

    /// Final review C1: a rename is followed, and then the user `cd`s out of
    /// B in that terminal. No shell is under the recorded inode any more, so
    /// the shell scan alone answers "no evidence" — and the sweep, which
    /// sees A's recorded path `NotFound`, used to SIGKILL a session working
    /// fine in a directory that exists. The key's own `.moved-to`, checked
    /// against B's inode, is the evidence that survives the `cd`. And only
    /// while B is that directory: B deleted, the same session is reaped.
    ///
    /// Revert-checked, run and restored: with `record_move_with`'s
    /// `moved_to_evidence` check removed (the shell scan alone), fails "a
    /// followed session whose shell left B is not reaped" — left: 1, right:
    /// 0, the master SIGKILLed.
    #[test]
    fn a_followed_session_survives_its_shell_leaving_the_renamed_directory() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let repo = repo_with_worktree_a(root.path(), "leaverepo");
            let a = "leaverepo/.claude/worktrees/worktree-a";
            let b = "leaverepo/.claude/worktrees/worktree-b";
            let sock_dir = state.join("sock").join(crate::projects::storage_key(a));
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            let flag = root.path().join("leave-now");
            spawn_detached_sh_in(
                &sock_path,
                &repo.join(".claude/worktrees/worktree-a"),
                r#"while [ ! -e "$1" ]; do sleep 0.05; done; cd /; exec sleep 60"#,
                &[flag.to_str().unwrap()],
            )
            .unwrap_or_else(|| panic!("dtach must be available for this test"));
            let _ = reconcile(&roots);
            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
            let followed = reconcile(&roots);
            assert_eq!(followed.moved.len(), 1, "setup: the rename is found from the shell in B");
            follow_moves(&followed.moved);
            assert_eq!(crate::home::owner_of(a, "shell"), crate::home::Owner::Elsewhere(b.into()), "setup: followed");

            fs::write(&flag, "").unwrap();
            let left = (0..100).any(|_| {
                let at_root = leaders_of(&sock_path)
                    .iter()
                    .any(|p| fs::read_link(format!("/proc/{p}/cwd")).is_ok_and(|c| c == std::path::Path::new("/")));
                if !at_root {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                at_root
            });
            assert!(left, "setup: the session leader has left B");

            let after = reconcile(&roots);
            let alive = socket_has_process(&sock_path);
            let still_recorded = sock_dir.join(".moved-to").exists();
            if after.gone_projects != 0 || !alive {
                kill_test_pids(&sock_path);
                std::env::remove_var("ROOST_ROOTS");
            }
            assert_eq!(after.gone_projects, 0, "a followed session whose shell left B is not reaped");
            assert!(alive, "its master is alive");
            assert!(still_recorded, "and its .moved-to stays");
            assert_eq!(after.moved.len(), 1, "the record is still the evidence: {:?}", after.moved);
            assert!(!after.moved[0].fresh, "an unchanged record is not a new rename");

            // The complement: B deleted. Nothing is under the inode and the
            // record names nothing, so this is the delete it always was.
            git_in(&repo, &["worktree", "remove", "--force", ".claude/worktrees/worktree-b"]);
            let gone = reconcile(&roots);
            let alive = socket_has_process(&sock_path);
            kill_test_pids(&sock_path);
            std::env::remove_var("ROOST_ROOTS");
            assert!(gone.gone_projects >= 1, "B deleted: the session is reaped");
            assert!(!alive, "and its master is ended");
        });
    }

    /// A session whose leader sits in `/` after the flag file appears: the
    /// shape every sibling-scan test needs (no shell under any inode).
    fn spawn_leaver(sock_path: &std::path::Path, dir: &std::path::Path, flag: &std::path::Path) {
        spawn_detached_sh_in(
            sock_path,
            dir,
            r#"while [ ! -e "$1" ]; do sleep 0.05; done; cd /; exec sleep 60"#,
            &[flag.to_str().unwrap()],
        )
        .unwrap_or_else(|| panic!("dtach must be available for this test"));
    }

    fn wait_left(sock_path: &std::path::Path, flag: &std::path::Path) {
        fs::write(flag, "").unwrap();
        let left = (0..100).any(|_| {
            let at_root = leaders_of(sock_path)
                .iter()
                .any(|p| fs::read_link(format!("/proc/{p}/cwd")).is_ok_and(|c| c == std::path::Path::new("/")));
            if !at_root {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            at_root
        });
        assert!(left, "setup: the session leader has left its directory");
    }

    /// #182: A→B followed, the shell `cd`s out of B, then B→C. `.moved-to`
    /// names B, which is gone; no shell is under the inode. C — a sibling of
    /// B carrying the recorded identity — is the evidence, and the session
    /// follows it rather than being reaped. Removing C instead is still the
    /// delete it always was.
    ///
    /// Revert-checked, run and restored: with `record_move_with` not
    /// consulting `sibling_evidence`, fails "a second rename with no shell
    /// inside is not a delete" — left: 1, right: 0, the master SIGKILLed.
    ///
    /// #183 §3: the shell is in `/`, so it is no evidence for C; the record
    /// is — part (a) wrote B's full identity beside it on the first follow,
    /// and the sibling scan found that identity at C. Revert-checked, run
    /// and restored: with `own_follow`'s record-identity fallback removed,
    /// fails "and followed to C" — left `Elsewhere(".../worktree-b")`; with
    /// part (a)'s `record_identity_in` removed, the same.
    #[test]
    fn a_second_rename_after_the_shell_left_is_followed_not_reaped() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let repo = repo_with_worktree_a(root.path(), "hoprepo");
            let a = "hoprepo/.claude/worktrees/worktree-a";
            let b = "hoprepo/.claude/worktrees/worktree-b";
            let c = "hoprepo/.claude/worktrees/worktree-c";
            let sock_dir = state.join("sock").join(crate::projects::storage_key(a));
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            let flag = root.path().join("leave-now");
            let _guard = KillOnDrop(vec![sock_path.clone()]);
            spawn_leaver(&sock_path, &repo.join(".claude/worktrees/worktree-a"), &flag);
            let _ = reconcile(&roots);
            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
            let first = reconcile(&roots);
            assert_eq!(first.moved.len(), 1, "setup: the first rename is found from the shell in B");
            follow_moves(&first.moved);
            wait_left(&sock_path, &flag);

            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-b", ".claude/worktrees/worktree-c"]);
            let second = reconcile(&roots);
            let alive = socket_has_process(&sock_path);
            if second.gone_projects != 0 || !alive {
                kill_test_pids(&sock_path);
                std::env::remove_var("ROOST_ROOTS");
            }
            assert_eq!(second.gone_projects, 0, "a second rename with no shell inside is not a delete");
            assert!(alive, "its master is alive");
            assert_eq!(second.moved.iter().map(|m| m.new_url.as_str()).collect::<Vec<_>>(), vec![c], "found at C");
            assert!(second.moved[0].fresh, "a new target is a new rename");
            follow_moves(&second.moved);
            assert_eq!(crate::home::owner_of(a, "shell"), crate::home::Owner::Elsewhere(c.into()), "and followed to C");
            let _ = b;

            git_in(&repo, &["worktree", "remove", "--force", ".claude/worktrees/worktree-c"]);
            let gone = reconcile(&roots);
            let alive = socket_has_process(&sock_path);
            kill_test_pids(&sock_path);
            std::env::remove_var("ROOST_ROOTS");
            assert!(gone.gone_projects >= 1, "C deleted: the session is reaped");
            assert!(!alive, "and its master is ended");
        });
    }

    /// #178's own gap, closed by the same scan: the very first rename, made
    /// while no shell is inside A. There is no `.moved-to` yet, so the
    /// siblings of A's recorded path are where the identity is looked for.
    ///
    /// Revert-checked, run and restored: without the sibling scan, fails "a
    /// rename with no shell inside is not a delete" — left: 1, right: 0.
    #[test]
    fn a_first_rename_with_no_shell_inside_is_followed_not_reaped() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let repo = repo_with_worktree_a(root.path(), "firstrepo");
            let a = "firstrepo/.claude/worktrees/worktree-a";
            let b = "firstrepo/.claude/worktrees/worktree-b";
            let sock_dir = state.join("sock").join(crate::projects::storage_key(a));
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            let flag = root.path().join("leave-now");
            spawn_leaver(&sock_path, &repo.join(".claude/worktrees/worktree-a"), &flag);
            let _ = reconcile(&roots);
            assert!(sock_dir.join(".origin-ino").exists(), "setup: A's identity is recorded");
            wait_left(&sock_path, &flag);

            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
            let after = reconcile(&roots);
            let alive = socket_has_process(&sock_path);
            kill_test_pids(&sock_path);
            std::env::remove_var("ROOST_ROOTS");
            assert_eq!(after.gone_projects, 0, "a rename with no shell inside is not a delete");
            assert!(alive, "its master is alive");
            assert_eq!(after.moved.iter().map(|m| m.new_url.as_str()).collect::<Vec<_>>(), vec![b], "found at B");
        });
    }

    /// A sibling directory that cannot be listed is "cannot tell", never
    /// "not there": the sweep keeps the session rather than guessing.
    ///
    /// Revert-checked, run and restored: with an unlistable parent skipped
    /// (`Err(_) => continue`) instead of answering `CannotTell`, fails "an
    /// unlistable sibling directory is not evidence of a delete" — left: 1,
    /// right: 0. Without the scan at all it fails the same way.
    #[cfg(unix)]
    #[test]
    fn an_unlistable_parent_suspends_the_sweep() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return; // root reads through 0o000; nothing to assert
        }
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let repo = repo_with_worktree_a(root.path(), "lockrepo");
            let a = "lockrepo/.claude/worktrees/worktree-a";
            let sock_dir = state.join("sock").join(crate::projects::storage_key(a));
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            let flag = root.path().join("leave-now");
            spawn_leaver(&sock_path, &repo.join(".claude/worktrees/worktree-a"), &flag);
            let _ = reconcile(&roots);
            wait_left(&sock_path, &flag);
            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
            let parent = repo.join(".claude/worktrees");
            // Search but not read: the sweep can still stat A's recorded
            // path and see it `NotFound` (so A is "gone"), but cannot list
            // the siblings that might hold it under another name. 0o400 —
            // read, no search — would fail that stat first, and the test
            // would pass without reaching the scan.
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o300)).unwrap();
            let after = reconcile(&roots);
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
            let alive = socket_has_process(&sock_path);
            kill_test_pids(&sock_path);
            std::env::remove_var("ROOST_ROOTS");
            assert_eq!(after.gone_projects, 0, "an unlistable sibling directory is not evidence of a delete");
            assert!(alive, "its master is alive");
        });
    }

    /// Final review I1, spec §5 ".moved-to is rewritten … after a second
    /// rename": A→B followed, then B→C. A's session's record says B, which
    /// no longer resolves, so following must move it — and the main
    /// checkout's terminal that (b) re-pointed at B — on to C. And a pass
    /// that is not fresh still finishes a follow an earlier pass left
    /// undone, without announcing it again.
    ///
    /// Revert-checked, run and restored: with (a)'s `Elsewhere(prev)` arm
    /// removed from `home::follow_move_in`, fails "A's session follows the
    /// second rename" — left `Elsewhere(".../worktree-b")`. With the
    /// non-fresh branch of `follow_moves` skipping straight to `continue`,
    /// fails "a non-fresh pass finishes the follow" — left `Here`.
    #[test]
    fn a_second_rename_moves_followed_sessions_on() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("ROOST_CMD");
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let repo = repo_with_worktree_a(root.path(), "tworepo");
            let a = "tworepo/.claude/worktrees/worktree-a";
            let b = "tworepo/.claude/worktrees/worktree-b";
            let c = "tworepo/.claude/worktrees/worktree-c";
            let sock_dir = state.join("sock").join(crate::projects::storage_key(a));
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            spawn_detached_dtach_in(&sock_path, &repo.join(".claude/worktrees/worktree-a"))
                .unwrap_or_else(|| panic!("dtach must be available for this test"));
            // A main-checkout terminal re-homed into A, live (a record whose
            // socket is gone is swept by the very next pass).
            let home_sock = state.join("sock/tworepo/term3");
            fs::create_dir_all(home_sock.parent().unwrap()).unwrap();
            spawn_detached_dtach_in(&home_sock, &repo).unwrap_or_else(|| panic!("dtach must be available for this test"));
            let _ = reconcile(&roots);
            crate::home::record("tworepo", "term3", a).unwrap();
            // #183 §1: (b) follows a record only on its own identity.
            crate::home::record_identity("tworepo", "term3", &repo.join(".claude/worktrees/worktree-a")).unwrap();

            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
            follow_moves(&reconcile(&roots).moved);
            assert_eq!(crate::home::owner_of(a, "shell"), crate::home::Owner::Elsewhere(b.into()), "setup: first follow");
            assert_eq!(crate::home::owner_of("tworepo", "term3"), crate::home::Owner::Elsewhere(b.into()), "setup: (b) followed");

            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-b", ".claude/worktrees/worktree-c"]);
            let second = reconcile(&roots);
            assert_eq!(second.gone_projects, 0);
            assert_eq!(
                second.moved,
                vec![Moved {
                    old_url: a.into(),
                    new_url: c.into(),
                    live: vec!["shell".into()],
                    fresh: true,
                    prev: Some(b.into()),
                    shells: vec![("shell".into(), repo.join(".claude/worktrees/worktree-c").canonicalize().unwrap())],
                }]
            );
            follow_moves(&second.moved);
            let shell = crate::home::owner_of(a, "shell");
            let term3 = crate::home::owner_of("tworepo", "term3");
            let owned_c = crate::session::owned_live_names(c);
            let marker = fs::read_to_string(sock_dir.join(".moved-to")).ok();

            // A follow left undone (a write that failed, say): the next,
            // non-fresh pass picks it up.
            crate::home::forget(a, "shell");
            let third = reconcile(&roots);
            let third_fresh = third.moved.iter().any(|m| m.fresh);
            follow_moves(&third.moved);
            let caught_up = crate::home::owner_of(a, "shell");
            let alive = socket_has_process(&sock_path);
            kill_test_pids(&sock_path);
            kill_test_pids(&home_sock);
            std::env::remove_var("ROOST_ROOTS");

            assert_eq!(shell, crate::home::Owner::Elsewhere(c.into()), "A's session follows the second rename");
            assert_eq!(term3, crate::home::Owner::Elsewhere(c.into()), "and so does the resident (b) re-pointed at B");
            assert!(owned_c.contains(&("shell".to_string(), Some(a.to_string()))), "C lists it as its resident: {owned_c:?}");
            assert_eq!(marker.as_deref(), Some(&*format!("{c}\n")), ".moved-to names C");
            assert!(!third_fresh, "setup: the third pass is not fresh");
            assert_eq!(caught_up, crate::home::Owner::Elsewhere(c.into()), "a non-fresh pass finishes the follow");
            assert!(alive, "nothing was ended on the way");
        });
    }

    /// Final review I2: A renamed to B and followed, then a *new* A made
    /// under the old name. The old URL resolves again, but to a different
    /// directory — not a move back. The followed session must stay B's: its
    /// `.moved-to` kept, B listing it, the new A not claiming it natively,
    /// and Close Project on the new A not ending it. A new session's attach
    /// in the new A (`record_origin`) must not rewrite the identity either,
    /// or the next pass would read "moved back".
    ///
    /// Revert-checked, run and restored: with the `occupied_by_another`
    /// check in `confirmed_gone` removed, fails "the followed session's
    /// .moved-to stays"; with only the one in `write_origin` removed, fails
    /// "a new session's attach in the new A does not undo it".
    #[test]
    fn a_new_directory_at_the_old_name_does_not_capture_followed_sessions() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("ROOST_CMD");
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let repo = repo_with_worktree_a(root.path(), "occrepo");
            let a = "occrepo/.claude/worktrees/worktree-a";
            let b = "occrepo/.claude/worktrees/worktree-b";
            let sock_dir = state.join("sock").join(crate::projects::storage_key(a));
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            spawn_detached_dtach_in(&sock_path, &repo.join(".claude/worktrees/worktree-a"))
                .unwrap_or_else(|| panic!("dtach must be available for this test"));
            let _ = reconcile(&roots);
            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
            follow_moves(&reconcile(&roots).moved);
            assert_eq!(crate::home::owner_of(a, "shell"), crate::home::Owner::Elsewhere(b.into()), "setup: followed");

            git_in(&repo, &["worktree", "add", "-q", "-b", "worktree-a2", ".claude/worktrees/worktree-a"]);
            let new_a = crate::projects::resolve_project(&roots, a).expect("setup: the new A resolves");
            let pass = reconcile(&roots);
            let kept = sock_dir.join(".moved-to").exists();
            let owned_b = crate::session::owned_live_names(b);
            let owned_a = crate::session::owned_live_names(a);

            crate::registry::record_origin(a, &new_a);
            let _ = reconcile(&roots);
            let kept_after_attach = sock_dir.join(".moved-to").exists();

            let ended = crate::session::kill_project(a);
            let alive = socket_has_process(&sock_path);
            kill_test_pids(&sock_path);
            std::env::remove_var("ROOST_ROOTS");

            assert_eq!(pass.gone_projects, 0);
            assert!(kept, "the followed session's .moved-to stays");
            assert!(owned_b.contains(&("shell".to_string(), Some(a.to_string()))), "B still lists it: {owned_b:?}");
            assert!(!owned_a.contains(&("shell".to_string(), None)), "the new A does not claim it natively: {owned_a:?}");
            assert!(kept_after_attach, "a new session's attach in the new A does not undo it");
            assert_eq!(ended, 0, "Close Project on the new A ends nothing of B's");
            assert!(alive, "the followed session's master is alive");
        });
    }

    /// Waits until a session leader of `sock_path` has `/` as its cwd.
    fn wait_leader_at_root(sock_path: &std::path::Path) -> bool {
        (0..100).any(|_| {
            let at_root = leaders_of(sock_path)
                .iter()
                .any(|p| fs::read_link(format!("/proc/{p}/cwd")).is_ok_and(|c| c == std::path::Path::new("/")));
            if !at_root {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            at_root
        })
    }

    /// Final review N1: C1's evidence, with git failing. A worktree URL
    /// resolves only once git vouches for it, and `worktree::list` turns a
    /// git error into an empty list — so with the followed shell `cd`'d out
    /// of B, a transient git failure used to read as "B does not resolve",
    /// then "no evidence", then a SIGKILL, while B stood there with the
    /// recorded inode. The seam: the main checkout's `.git/HEAD` moved
    /// aside, which makes every git call there fail.
    ///
    /// Revert-checked, run and restored: with the identity-match arm in
    /// `moved_to_evidence`'s root loop removed, fails "a git failure is
    /// cannot tell" — observed `NoEvidence`; and with the direct call
    /// skipped, the reconcile reaps (`gone_projects` 1).
    #[test]
    fn a_git_failure_is_not_evidence_that_a_followed_directory_is_gone() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let repo = repo_with_worktree_a(root.path(), "gitfailrepo");
            let a = "gitfailrepo/.claude/worktrees/worktree-a";
            let b = "gitfailrepo/.claude/worktrees/worktree-b";
            let sock_dir = state.join("sock").join(crate::projects::storage_key(a));
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            let flag = root.path().join("leave-now");
            spawn_detached_sh_in(
                &sock_path,
                &repo.join(".claude/worktrees/worktree-a"),
                r#"while [ ! -e "$1" ]; do sleep 0.05; done; cd /; exec sleep 60"#,
                &[flag.to_str().unwrap()],
            )
            .unwrap_or_else(|| panic!("dtach must be available for this test"));
            let _ = reconcile(&roots);
            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
            follow_moves(&reconcile(&roots).moved);
            fs::write(&flag, "").unwrap();
            assert!(wait_leader_at_root(&sock_path), "setup: the session leader has left B");

            fs::rename(repo.join(".git/HEAD"), repo.join(".git/HEAD.away")).unwrap();
            let unresolved = crate::projects::resolve_project(&roots, b).is_none();
            let b_there = repo.join(".claude/worktrees/worktree-b").is_dir();
            let snap = process_snapshot().expect("ps");
            let got = record_move_with(&roots, a, &sock_dir, &snap, std::path::Path::new("/proc"), false);
            let pass = reconcile(&roots);
            let alive = socket_has_process(&sock_path);
            fs::rename(repo.join(".git/HEAD.away"), repo.join(".git/HEAD")).unwrap();
            kill_test_pids(&sock_path);
            std::env::remove_var("ROOST_ROOTS");

            assert!(unresolved && b_there, "setup: B is there, but git cannot vouch for it");
            assert!(matches!(got, MoveRecord::CannotTell), "a git failure is cannot tell: {got:?}");
            assert_eq!(pass.gone_projects, 0, "and the session is not reaped");
            assert!(alive, "its master is alive");
        });
    }

    /// Final review N2: the identity I2 keeps for a renamed key's followed
    /// sessions is not the occupant's. A→B followed; a new A made, with its
    /// own live session; the followed session ends; the new A is renamed to
    /// D with its shell inside. The shell scan matched only the old
    /// identity, so with B gone that rename read as a delete and the
    /// occupant's session was killed. Now the occupant's identity is kept
    /// beside it (`.occupant-ino`, written by the new session's attach) and
    /// matched too. And while B is still there, the pass is not fresh and
    /// the occupant's session — working in D, not B — is not re-homed into
    /// B: it follows D, on its own shell's evidence (#183 chain (d), which
    /// used to leave it `Here`); once B is gone, it is still D's.
    ///
    /// The reviewer's order was "remove B, then rename"; renaming first and
    /// removing B after covers that pass *and* the B-alive one.
    ///
    /// Revert-checked, run and restored: with `move_evidence` matching only
    /// the recorded identity (`recorded_identity(key_dir).into_iter()`
    /// instead of `identities`), fails "the occupant's session survives the
    /// rename of the new A" — left: 1, right: 0. With `follow_moves`'
    /// restricted list given every live name (the `inside` filter made
    /// `true ||`), fails "not re-homed into B" — left
    /// `Elsewhere(".../worktree-b")`, right `Here`. Since #183 Task 6 the
    /// same assertion expects `Elsewhere(D)`, and the revert that fails it
    /// is `own_follow` taking `new_url` whatever the shell said.
    #[test]
    fn a_rename_of_the_new_directory_at_the_old_name_is_followed_not_reaped() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("ROOST_CMD");
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let repo = repo_with_worktree_a(root.path(), "occmvrepo");
            let a = "occmvrepo/.claude/worktrees/worktree-a";
            let b = "occmvrepo/.claude/worktrees/worktree-b";
            let d = "occmvrepo/.claude/worktrees/worktree-d";
            let sock_dir = state.join("sock").join(crate::projects::storage_key(a));
            fs::create_dir_all(&sock_dir).unwrap();
            let followed = sock_dir.join("shell");
            spawn_detached_dtach_in(&followed, &repo.join(".claude/worktrees/worktree-a"))
                .unwrap_or_else(|| panic!("dtach must be available for this test"));
            let _ = reconcile(&roots);
            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
            follow_moves(&reconcile(&roots).moved);
            assert_eq!(crate::home::owner_of(a, "shell"), crate::home::Owner::Elsewhere(b.into()), "setup: followed");

            git_in(&repo, &["worktree", "add", "-q", "-b", "worktree-a2", ".claude/worktrees/worktree-a"]);
            let new_a = crate::projects::resolve_project(&roots, a).expect("setup: the new A resolves");
            let own = sock_dir.join("own");
            spawn_detached_dtach_in(&own, &new_a).unwrap_or_else(|| panic!("dtach must be available for this test"));
            crate::registry::record_origin(a, &new_a); // the new session's attach
            kill_test_pids(&followed);

            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-d"]);
            let with_b = reconcile(&roots);
            follow_moves(&with_b.moved);
            let owner_with_b = crate::home::owner_of(a, "own");
            let alive_with_b = socket_has_process(&own);

            git_in(&repo, &["worktree", "remove", "--force", ".claude/worktrees/worktree-b"]);
            let without_b = reconcile(&roots);
            follow_moves(&without_b.moved);
            let owner = crate::home::owner_of(a, "own");
            let owned_d = crate::session::owned_live_names(d);
            let alive = socket_has_process(&own);
            kill_test_pids(&own);
            std::env::remove_var("ROOST_ROOTS");

            assert_eq!(with_b.gone_projects, 0, "B alive: nothing reaped");
            assert!(alive_with_b, "B alive: the occupant's session lives");
            assert_eq!(owner_with_b, crate::home::Owner::Elsewhere(d.into()), "not re-homed into B: it follows D, its own shell's");
            assert_eq!(without_b.gone_projects, 0, "the occupant's session survives the rename of the new A");
            assert!(alive, "its master is alive");
            assert_eq!(owner, crate::home::Owner::Elsewhere(d.into()), "it follows D");
            assert!(owned_d.contains(&("own".to_string(), Some(a.to_string()))), "D lists it: {owned_d:?}");
        });
    }

    /// `$HOME`-based, for the tests below where inode reuse is the subject:
    /// this host's `/tmp` is tmpfs, and the reuse `dirid` exists for is an
    /// ext4 property (see `dirid`'s module doc).
    fn ext4_tempdir() -> tempfile::TempDir {
        let base = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        tempfile::Builder::new().prefix("registry-ext4-").tempdir_in(base).unwrap()
    }

    /// The parsed lines of a key's `.occupant-ino`, damaged ones as `None`.
    fn occupant_lines(key_dir: &std::path::Path) -> Vec<Option<crate::dirid::Stored>> {
        fs::read_to_string(occupant_identity_path(key_dir))
            .unwrap_or_default()
            .lines()
            .map(crate::dirid::parse_line)
            .collect()
    }

    fn full_id(dir: &std::path::Path) -> crate::dirid::Stored {
        crate::dirid::Stored::Full(crate::dirid::of(dir).unwrap().expect("the directory exists"))
    }

    /// #183 §3: `.origin-ino` carries birth time. A new key's attach writes
    /// the four-field form, and a two-field one from #178's roost is
    /// rewritten four-field on the next pass in which the directory
    /// resolves (and is not an occupant) — the upgrade Review Focus 3
    /// needs on the deploy host, where every `.origin-ino` is two-field.
    ///
    /// Revert-checked, run and restored: with `write_origin_identity`
    /// writing `dev ino` only, fails "a new key's .origin-ino is four-field"
    /// (left `"64512 9730149\n"`); with its unchanged-check accepting any
    /// `matches` (so a matching legacy line is left alone), fails "a legacy
    /// .origin-ino is upgraded on the next resolving pass", left the same
    /// two-field line. Before the implementation, failed the first.
    #[test]
    fn origin_ino_is_written_four_field_and_upgrades_a_legacy_one() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            let project_dir = root.path().join("fourproj");
            fs::create_dir_all(&project_dir).unwrap();
            let key_dir = state.join("sock/fourproj");
            let want = format!("{}\n", crate::dirid::format(&crate::dirid::of(&project_dir).unwrap().unwrap()));

            record_origin("fourproj", &project_dir);
            let fresh = fs::read_to_string(origin_identity_path(&key_dir)).unwrap();

            use std::os::unix::fs::MetadataExt;
            let m = fs::metadata(&project_dir).unwrap();
            fs::write(origin_identity_path(&key_dir), format!("{} {}\n", m.dev(), m.ino())).unwrap();
            assert!(
                matches!(recorded_identity(&key_dir), Some(crate::dirid::Stored::Legacy { .. })),
                "setup: a legacy identity is recorded"
            );
            let _ = reconcile(&[root.path().to_path_buf()]);
            let upgraded = fs::read_to_string(origin_identity_path(&key_dir)).unwrap();

            assert_eq!(fresh, want, "a new key's .origin-ino is four-field");
            assert_eq!(upgraded, want, "a legacy .origin-ino is upgraded on the next resolving pass");
        });
    }

    /// Fix round 1: with no birth time, `.origin-ino` is recorded Legacy
    /// (protect-only) rather than not at all, a stale pair is replaced, and a
    /// Full record naming the same `(dev, ino)` is not downgraded. A pure
    /// seam: this host always has birth time, and nothing in roost can make
    /// `created()` fail on demand, so there is no reconcile-level version.
    ///
    /// Revert-checked, run and restored: with the early return restored
    /// (`Err(_) => None`, i.e. nothing written without a birth time), fails
    /// "no birth time still records a protect-only identity" — left `None`,
    /// right `Some("7 9")`.
    #[test]
    fn without_a_birth_time_the_origin_is_recorded_legacy() {
        use crate::dirid::{DirId, Stored};
        let id = DirId { dev: 7, ino: 9, bsec: 100, bnsec: 5 };
        let no_btime = || Err::<DirId, String>("birth time unreadable".into());
        assert_eq!(origin_line(None, no_btime(), (7, 9)), Some("7 9".into()), "no birth time still records a protect-only identity");
        assert_eq!(origin_line(Some(Stored::Legacy { dev: 7, ino: 3 }), no_btime(), (7, 9)), Some("7 9".into()), "a stale pair is replaced");
        assert_eq!(origin_line(Some(Stored::Legacy { dev: 7, ino: 9 }), no_btime(), (7, 9)), None, "a matching pair is left");
        assert_eq!(origin_line(Some(Stored::Full(id)), no_btime(), (7, 9)), None, "a matching Full record is not downgraded");
        assert_eq!(
            origin_line(Some(Stored::Full(DirId { ino: 3, ..id })), no_btime(), (7, 9)),
            Some("7 9".into()),
            "a Full record of a different directory is replaced"
        );
        assert_eq!(origin_line(Some(Stored::Full(id)), Ok(id), (7, 9)), None, "unchanged is not rewritten");
        assert_eq!(origin_line(Some(Stored::Legacy { dev: 7, ino: 9 }), Ok(id), (7, 9)), Some("7 9 100 5".into()), "legacy upgrades");
    }

    /// Fix round 1: an occupant line written two-field is upgraded in place
    /// when the same directory's full identity arrives, not listed twice.
    ///
    /// Revert-checked, run and restored: with the in-place upgrade removed,
    /// fails "the legacy line is upgraded in place, not duplicated" — left
    /// `[Some(Legacy { .. }), Some(Full(..))]`, right `[Some(Full(..))]`.
    #[test]
    fn a_legacy_occupant_line_is_upgraded_in_place() {
        with_state(|state| {
            use std::os::unix::fs::MetadataExt;
            let base = ext4_tempdir();
            let key_dir = state.join("sock/upocc");
            fs::create_dir_all(&key_dir).unwrap();
            let m = fs::metadata(base.path()).unwrap();
            fs::write(occupant_identity_path(&key_dir), format!("{} {}\n", m.dev(), m.ino())).unwrap();

            let said = append_occupant(&key_dir, base.path());

            assert_eq!(occupant_lines(&key_dir), vec![Some(full_id(base.path()))], "the legacy line is upgraded in place, not duplicated");
            assert_eq!(said, None);
        });
    }

    /// #183 §3: occupants are a list. Each new occupant's identity is
    /// appended, one already listed is not appended twice, and the list
    /// holds at most `OCCUPANT_CAP`: the ninth drops the oldest and says so,
    /// naming the cap. On ext4, so nine directories made in a row are real
    /// distinct identities rather than a tmpfs artefact.
    ///
    /// Revert-checked, run and restored: `append_occupant` ignoring the
    /// existing lines (the old overwrite) fails "nine occupants leave the
    /// newest eight", left one entry; the `drain` removed fails the same
    /// assertion with nine entries, the oldest still first; the dedup check
    /// disabled fails it too (the newest appended twice, a second cap line
    /// logged).
    #[test]
    fn the_occupant_list_appends_and_caps() {
        with_state(|state| {
            let base = ext4_tempdir();
            let key_dir = state.join("sock/capproj");
            fs::create_dir_all(&key_dir).unwrap();
            let dirs: Vec<PathBuf> = (0..9)
                .map(|i| {
                    let d = base.path().join(format!("occ{i}"));
                    fs::create_dir(&d).unwrap();
                    d
                })
                .collect();
            let said: Vec<Option<String>> = dirs.iter().map(|d| append_occupant(&key_dir, d)).collect();
            let again = append_occupant(&key_dir, &dirs[8]);
            let lines = occupant_lines(&key_dir);

            let want: Vec<Option<crate::dirid::Stored>> = dirs[1..].iter().map(|d| Some(full_id(d))).collect();
            assert_eq!(OCCUPANT_CAP, 8);
            assert_eq!(lines, want, "nine occupants leave the newest eight, the oldest dropped");
            assert!(said[..8].iter().all(Option::is_none), "nothing is dropped below the cap: {said:?}");
            let cap_line = said[8].as_deref().unwrap_or_default();
            assert!(cap_line.contains("cap of 8"), "the drop is logged, naming the cap: {cap_line:?}");
            assert_eq!(again, None, "an occupant already listed is not appended twice");
        });
    }

    /// #183 §3: an `.occupant-ino` written before the list — one line, two
    /// fields — reads as a one-entry list holding a `Legacy` identity, after
    /// the origin's.
    ///
    /// Revert-checked, run and restored: with `identities` dropping the
    /// occupant lines (`.take(0)`), fails with only the origin's identity.
    #[test]
    fn an_existing_single_occupant_line_is_a_one_entry_list() {
        with_state(|state| {
            let base = ext4_tempdir();
            let key_dir = state.join("sock/oneocc");
            fs::create_dir_all(&key_dir).unwrap();
            write_origin_identity(&key_dir, base.path());
            fs::write(occupant_identity_path(&key_dir), "64512 4242\n").unwrap();

            assert_eq!(
                identities(&key_dir),
                vec![full_id(base.path()), crate::dirid::Stored::Legacy { dev: 64512, ino: 4242 }],
            );
        });
    }

    /// #183 chain (c), spec §3's testing bullet: A→B followed, an occupant
    /// of A renamed to D and followed there, then a *third* directory made
    /// at A and removed. With one slot, the third's identity overwrote the
    /// occupant's, so when it went the D session had no identity left
    /// anywhere — not at `.moved-to`'s D, not under its shell, not among A's
    /// siblings — and was reaped as a delete. On ext4, so the third
    /// directory may well reuse B's freed inode; birth time is what keeps it
    /// from reading as "moved back".
    ///
    /// Revert-checked, run and restored: with `append_occupant` overwriting
    /// (existing lines ignored, the single-slot behaviour), fails "the D
    /// session survives the third directory's removal" — left 1, right 0,
    /// logged "reaped session …/own — its recorded directory no longer
    /// exists": the live master SIGKILLed. Before the implementation (the
    /// old single slot), failed "occupant 1's identity is still listed".
    #[test]
    fn a_third_directory_does_not_erase_an_earlier_occupants_identity() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("ROOST_CMD");
            let root = ext4_tempdir();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let repo = repo_with_worktree_a(root.path(), "thirdrepo");
            let a = "thirdrepo/.claude/worktrees/worktree-a";
            let b = "thirdrepo/.claude/worktrees/worktree-b";
            let d = "thirdrepo/.claude/worktrees/worktree-d";
            let sock_dir = state.join("sock").join(crate::projects::storage_key(a));
            fs::create_dir_all(&sock_dir).unwrap();
            let followed = sock_dir.join("shell");
            spawn_detached_dtach_in(&followed, &repo.join(".claude/worktrees/worktree-a"))
                .unwrap_or_else(|| panic!("dtach must be available for this test"));
            let _ = reconcile(&roots);
            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
            follow_moves(&reconcile(&roots).moved);
            assert_eq!(crate::home::owner_of(a, "shell"), crate::home::Owner::Elsewhere(b.into()), "setup: followed to B");

            git_in(&repo, &["worktree", "add", "-q", "-b", "worktree-a2", ".claude/worktrees/worktree-a"]);
            let occupant = crate::projects::resolve_project(&roots, a).expect("setup: the new A resolves");
            let own = sock_dir.join("own");
            spawn_detached_dtach_in(&own, &occupant).unwrap_or_else(|| panic!("dtach must be available for this test"));
            crate::registry::record_origin(a, &occupant); // the new session's attach
            kill_test_pids(&followed);
            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-d"]);
            git_in(&repo, &["worktree", "remove", "--force", ".claude/worktrees/worktree-b"]);
            follow_moves(&reconcile(&roots).moved);
            let occ1 = full_id(&repo.join(".claude/worktrees/worktree-d"));
            assert_eq!(crate::home::owner_of(a, "own"), crate::home::Owner::Elsewhere(d.into()), "setup: occupant 1 followed to D");

            git_in(&repo, &["worktree", "add", "-q", "-b", "worktree-a3", ".claude/worktrees/worktree-a"]);
            let third = crate::projects::resolve_project(&roots, a).expect("setup: the third A resolves");
            let id3 = full_id(&third);
            let while_third = reconcile(&roots);
            follow_moves(&while_third.moved);
            let listed = occupant_lines(&sock_dir);
            git_in(&repo, &["worktree", "remove", "--force", ".claude/worktrees/worktree-a"]);
            let after = reconcile(&roots);
            follow_moves(&after.moved);
            let alive = socket_has_process(&own);
            let owner = crate::home::owner_of(a, "own");
            kill_test_pids(&own);
            std::env::remove_var("ROOST_ROOTS");

            assert_eq!(while_third.gone_projects, 0, "nothing reaped while the third directory stands");
            assert_eq!(after.gone_projects, 0, "the D session survives the third directory's removal");
            assert!(alive, "its master is alive");
            assert_eq!(owner, crate::home::Owner::Elsewhere(d.into()), "and it is still owned by D");
            assert!(listed.contains(&Some(occ1)), "occupant 1's identity is still listed: {listed:?}");
            assert!(listed.contains(&Some(id3)), "and the third directory's beside it: {listed:?}");
        });
    }

    /// Review Focus 3: a two-field `.origin-ino` (every one on the deploy
    /// host today) still protects a renamed project's session from the reap
    /// — the keep direction, exactly as before #183 — but never licenses a
    /// follow: ext4 hands a freed inode straight to the next directory, so
    /// `(dev, ino)` alone cannot say *which* directory it found. So the
    /// rename is `Unfollowable`: no `.moved-to`, nothing in `moved`.
    ///
    /// Revert-checked, run and restored: with `moved()` never setting
    /// `legacy` (a weak match licensing `Followable`), fails "a legacy-only
    /// match is unfollowable" — got `Followable(Moved { new_url:
    /// "legacy-renamed", fresh: true, .. })`. Same failure before the
    /// implementation.
    #[test]
    fn a_legacy_origin_protects_but_never_licenses_a_follow() {
        with_state(|state| {
            let root = ext4_tempdir();
            let roots = vec![root.path().to_path_buf()];
            let project_dir = root.path().join("legacy-proj");
            fs::create_dir_all(&project_dir).unwrap();
            let sock_dir = state.join("sock/legacy-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            spawn_detached_dtach_in(&sock_path, &project_dir).unwrap_or_else(|| panic!("dtach must be available for this test"));
            let _ = reconcile(&roots);
            use std::os::unix::fs::MetadataExt;
            let m = fs::metadata(&project_dir).unwrap();
            fs::write(origin_identity_path(&sock_dir), format!("{} {}\n", m.dev(), m.ino())).unwrap();

            fs::rename(&project_dir, root.path().join("legacy-renamed")).unwrap();
            let snap = process_snapshot().expect("ps");
            let got = record_move_with(&roots, "legacy-proj", &sock_dir, &snap, std::path::Path::new("/proc"), false);
            let report = reconcile(&roots);
            let alive = socket_has_process(&sock_path);
            let moved_to = sock_dir.join(".moved-to").exists();
            kill_test_pids(&sock_path);

            assert!(matches!(got, MoveRecord::Unfollowable), "a legacy-only match is unfollowable: {got:?}");
            assert_eq!(report.gone_projects, 0, "a legacy identity still protects from the reap");
            assert!(alive, "the session's master is alive");
            assert!(!moved_to, "no .moved-to is written on a legacy-only match");
            assert!(report.moved.is_empty(), "nothing follows: {:?}", report.moved);
        });
    }

    /// Spec §5, out of scope by design: a directory moved outside every root
    /// has no URL to follow to. #178 still keeps the session alive, and
    /// nothing is written.
    ///
    /// Revert-checked, run and restored: with `url_under_roots` replaced by
    /// the bare leaf name (any directory "resolves" as `out-renamed`) and the
    /// `resolve_project` confirmation dropped, fails "nothing is followed to
    /// outside the roots" — `moved` held `Moved { new_url: "out-renamed", … }`.
    #[test]
    fn a_move_outside_every_root_writes_nothing() {
        with_state(|state| {
            let sock_dir = state.join("sock/out-proj");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            let root = tempfile::tempdir().unwrap();
            let elsewhere = tempfile::tempdir().unwrap();
            let project_dir = root.path().join("out-proj");
            fs::create_dir_all(&project_dir).unwrap();
            spawn_detached_dtach_in(&sock_path, &project_dir).unwrap_or_else(|| panic!("dtach must be available for this test"));
            let _ = reconcile(&[root.path().to_path_buf()]);

            fs::rename(&project_dir, elsewhere.path().join("out-renamed")).unwrap();
            let report = reconcile(&[root.path().to_path_buf()]);

            assert_eq!(report.gone_projects, 0, "#178: a moved project is not a gone project");
            assert!(report.moved.is_empty(), "nothing is followed to outside the roots: {:?}", report.moved);
            assert!(!sock_dir.join(".moved-to").exists(), "no .moved-to for a directory under no root");
            assert!(socket_has_process(&sock_path), "the session survives, stranded as #178 leaves it");
            kill_test_pids(&sock_path);
        });
    }

    /// The upgrade case: a project whose `.origin` was written by a roost that
    /// predates `.origin-ino` must gain the identity on the next confirming
    /// pass. Found live after deploying #179: 1 of 40 projects had one,
    /// because `write_origin` returned early on an unchanged `.origin` before
    /// ever reaching the identity write — so the rename protection never
    /// switched on for any project that already existed.
    ///
    /// Revert-checked: this failed on a5dced3 (the deployed #179) with "a
    /// confirming pass must backfill .origin-ino for a project whose .origin
    /// is unchanged".
    #[test]
    fn an_existing_origin_gains_its_identity_on_the_next_pass() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            let project_dir = root.path().join("old-proj");
            fs::create_dir_all(&project_dir).unwrap();
            let key_dir = state.join("sock/old-proj");
            fs::create_dir_all(&key_dir).unwrap();
            // Exactly what an older roost left: `.origin` up to date, no identity.
            fs::write(origin_marker_path(&key_dir), marker_bytes(&project_dir)).unwrap();
            assert!(!origin_identity_path(&key_dir).exists(), "test setup: no identity yet");

            let _ = reconcile(&[root.path().to_path_buf()]);

            assert!(
                origin_identity_path(&key_dir).exists(),
                "a confirming pass must backfill .origin-ino for a project whose .origin is unchanged"
            );
        });
    }

    /// The other half of the same evidence: a shell whose cwd was *in* the
    /// project when the project was deleted reads `(deleted)`, which is not a
    /// move — the session is reaped exactly as before.
    #[test]
    fn a_live_session_inside_a_deleted_project_is_still_killed() {
        with_state(|state| {
            let sock_dir = state.join("sock/deleted-in");
            fs::create_dir_all(&sock_dir).unwrap();
            let sock_path = sock_dir.join("shell");
            let root = tempfile::tempdir().unwrap();
            let project_dir = root.path().join("deleted-in");
            fs::create_dir_all(&project_dir).unwrap();
            spawn_detached_dtach_in(&sock_path, &project_dir).expect("dtach must be available for this test");
            let _ = reconcile(&[root.path().to_path_buf()]);
            fs::remove_dir_all(&project_dir).unwrap();
            let report = reconcile(&[root.path().to_path_buf()]);
            assert_eq!(report.gone_projects, 1, "a deleted project's session is still reaped");
            assert!(!socket_has_process(&sock_path), "and its process really ended");
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
    // or a ROOST_ROOTS typo would make startup reconcile SIGKILL every
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

            let ok = kill_and_unlink_with(&sock_path, &|| None, std::path::Path::new("/proc"));

            assert!(!ok, "an unverifiable process list must never report success");
            assert!(sock_path.exists(), "the socket must survive when the process list can't be trusted");
        });
    }

    /// A `/proc` fixture: a dtach master holding `sock`, its shell leading its
    /// own session, and a HUP-ignoring child in that session but in a process
    /// group of its own — the measured shape of the survivor this exists for.
    fn proc_with_a_session(dir: &std::path::Path) {
        let stat = |pid: u32, comm: &str, ppid: u32, pgrp: u32, sid: u32| {
            let p = dir.join(pid.to_string());
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("stat"), format!("{pid} ({comm}) S {ppid} {pgrp} {sid} 0 1 0 0 0 0\n")).unwrap();
        };
        let children = |pid: u32, kids: &str| {
            let p = dir.join(pid.to_string()).join("task").join(pid.to_string());
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("children"), format!("{kids} ")).unwrap();
        };
        // Pids above `/proc/sys/kernel/pid_max` (4194304 here), so the `kill -9`
        // this test drives can never reach a real process on the host. A fixture
        // using plausible pids would SIGKILL whatever happened to own them.
        stat(9000700, "dtach", 1, 9000700, 9000700);
        children(9000700, "9000701");
        stat(9000701, "bash", 9000700, 9000701, 9000701);
        children(9000701, "9000702");
        stat(9000702, "claude", 9000701, 9000702, 9000701); // own pgrp, shell's session
        children(9000702, "");
        // roost itself, so `own_sid` resolves and the guard is exercised rather
        // than short-circuited.
        stat(std::process::id(), "roost", 1, std::process::id(), std::process::id());
        children(std::process::id(), "");
    }

    #[test]
    fn a_session_member_left_alive_is_not_confirmed_and_the_socket_stays() {
        // The defect in one assertion: the socket's holder is gone, so the old
        // confirmation ("does anything hold the socket") says yes, finished —
        // while pid 702 is still in the shell's session. Reporting that as ended
        // is what tells the user a number that is too large.
        let d = tempfile::tempdir().unwrap();
        let procd = tempfile::tempdir().unwrap();
        proc_with_a_session(procd.path());
        let sock = d.path().join("term");
        std::fs::write(&sock, b"").unwrap();
        // The master is reported as holding the socket exactly once — the
        // read `kill_and_unlink_with` derives `pids`/`target_sessions` from —
        // and gone on every read after, exactly what a real `kill -9` against
        // a real dtach master leaves in `ps`. A closure that kept reporting
        // it held on every call (the brief's literal fixture) made this test
        // pass whether or not the session sweep below existed at all, since
        // `socket_has_process_with` alone would then never clear — caught by
        // the revert-check this comment documents.
        let seen = std::cell::Cell::new(false);
        let snap = || {
            if seen.replace(true) {
                Some(vec![])
            } else {
                Some(vec![(9000700, format!("dtach -A {} -E", sock.display()))])
            }
        };
        let ok = kill_and_unlink_with(&sock, &snap, procd.path());
        // Revert-checked: with `session_or_socket_alive` reverted to just
        // `socket_has_process_with` (the pre-fix confirmation, blind to the
        // session's surviving members), this panics: "a session with a live
        // member must not be reported as ended".
        assert!(!ok, "a session with a live member must not be reported as ended");
        assert!(sock.exists(), "and its socket must stay, so the session stays discoverable");
    }

    #[test]
    fn a_session_undeterminable_only_at_confirmation_time_is_reported_as_doubt_not_a_survivor() {
        // Distinct from `an_undeterminable_session_leaves_the_socket_in_place`
        // below: that one is undeterminable at *derivation* (`target_sessions`
        // returns `None` before anything is killed), which was already
        // labelled correctly. This fixture derives cleanly — the same target
        // session (9000701) as the "member left alive" test above — and only
        // trips `procsess::members_of` into `None` during the confirmation
        // poll, via a stray `/proc` entry with an unparseable `stat` that
        // nothing in derivation ever reads (derivation only reads the
        // holder's own stat/children and its direct child's stat; `members_of`
        // reads every entry in `/proc`). That asymmetry is exactly the
        // `procsess::members_of` -> `None` path Finding 1 is about.
        let d = tempfile::tempdir().unwrap();
        let procd = tempfile::tempdir().unwrap();
        proc_with_a_session(procd.path());
        std::fs::create_dir_all(procd.path().join("9000703")).unwrap();
        std::fs::write(procd.path().join("9000703").join("stat"), b"not a valid stat line\n").unwrap();

        let sock = d.path().join("term");
        std::fs::write(&sock, b"").unwrap();
        let seen = std::cell::Cell::new(false);
        let snap = || {
            if seen.replace(true) {
                Some(vec![])
            } else {
                Some(vec![(9000700, format!("dtach -A {} -E", sock.display()))])
            }
        };
        let ok = kill_and_unlink_with(&sock, &snap, procd.path());
        assert!(!ok, "undeterminable membership at confirmation time must not be reported as ended");
        assert!(sock.exists());

        // The distinction Finding 1 is about: a bare `!ok` is identical for
        // "confirmed alive" and "could not tell" (both already returned
        // `false` before this fix), so it cannot see a mislabelled message.
        // Calling the confirmation check directly, against the same target
        // session, pins down *why* — the one thing the return value alone
        // never could.
        let never_held = || Some(vec![]);
        let result = session_or_socket_alive(&sock, &never_held, procd.path(), &[9000701]);
        // Revert-checked: with the `None => undetermined = true` arm changed
        // to `None => return StillHeld::Yes` (folding "could not tell" back
        // into "confirmed alive", the pre-fix shape under the new type),
        // this panics: "a session whose membership could not be read must be
        // reported as undetermined doubt, not a confirmed survivor".
        assert!(
            matches!(result, StillHeld::Undetermined),
            "a session whose membership could not be read must be reported as undetermined doubt, not a confirmed survivor"
        );
    }

    #[test]
    fn an_undeterminable_session_leaves_the_socket_in_place() {
        // `target_sessions` returning None must reach the same "leave it" path as
        // an unverifiable `ps`, not be treated as "no sessions to sweep".
        let d = tempfile::tempdir().unwrap();
        // Empty: neither the holder's children nor roost's own session can be
        // read from it, which is the "cannot tell" state this asserts on.
        let procd = tempfile::tempdir().unwrap();
        let sock = d.path().join("term");
        std::fs::write(&sock, b"").unwrap();
        // Stateful for the same reason as the test above: a mock that keeps
        // reporting the holder on every call makes the socket check alone
        // account for `!ok`, masking whether the `target_sessions == None`
        // bail this test names ever ran at all.
        let seen = std::cell::Cell::new(false);
        let snap = || {
            if seen.replace(true) {
                Some(vec![])
            } else {
                Some(vec![(9000700, format!("dtach -A {} -E", sock.display()))])
            }
        };
        let ok = kill_and_unlink_with(&sock, &snap, procd.path());
        // Revert-checked: with the `target_sessions == None` bail replaced by
        // `.unwrap_or_default()` (folding "cannot determine" into "nothing to
        // sweep"), this panics: "an underivable session is doubt, not an
        // empty sweep".
        assert!(!ok, "an underivable session is doubt, not an empty sweep");
        assert!(sock.exists());
    }

    #[test]
    fn nothing_holding_and_no_session_still_unlinks() {
        // The vacuous case must stay vacuous: a socket nobody holds, with no
        // session behind it, is cleaned up and reported as ended.
        let d = tempfile::tempdir().unwrap();
        let procd = tempfile::tempdir().unwrap();
        let sock = d.path().join("term");
        std::fs::write(&sock, b"").unwrap();
        let ok = kill_and_unlink_with(&sock, &|| Some(vec![]), procd.path());
        assert!(ok, "no holder and no session is ended, not doubt");
        assert!(!sock.exists());
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
    // different ROOST_ROOTS than what created the session, sharing the
    // same ROOST_STATE_DIR (exactly what docs/deploy.md tells users to
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
            std::env::remove_var("ROOST_CMD");
            let real_root = tempfile::tempdir().unwrap();
            let project_dir = real_root.path().join("victimproj");
            fs::create_dir_all(&project_dir).unwrap();

            let Ok(_att) = crate::session::reserve_and_attach("victimproj", "shell", &project_dir) else {
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
            // lives under — simulating a second roost instance (or the
            // same one, restarted with an overridden ROOST_ROOTS)
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
            std::env::remove_var("ROOST_CMD");
            let root = tempfile::tempdir().unwrap();
            let project_dir = root.path().join("attachreap");
            fs::create_dir_all(&project_dir).unwrap();

            let Ok(_att) = crate::session::reserve_and_attach("attachreap", "shell", &project_dir) else {
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

    // Revert-checked: changed the `Err(_) => { found_or_uncertain = true; ... }`
    // arm in the reaping loop to `Err(_) => continue` (treat "cannot tell" as
    // "not here, keep looking at other roots" — with only one root,
    // indistinguishable from "gone"). Observed failure: `panicked at
    // src/registry.rs:2363:13: cannot tell: kept` — `blocked` was reaped
    // instead of kept, exactly the case `Path::exists()` vs
    // `symlink_metadata` alone would not have caught. Restored; this test
    // passes again. (Non-root run; see the guard below for why this needs
    // it.)
    /// Revert-checked (Task 4): removing the `home::sweep` call fails on
    /// "reaped this pass: forgotten".
    ///
    /// A re-home record goes with its socket, and only on `NotFound` — the
    /// same pass, after the sweep above has made its own decisions, so a
    /// socket reaped as dead in this pass takes its record with it.
    #[test]
    fn reconcile_forgets_a_rehome_record_only_when_its_socket_is_gone() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        std::env::set_var("ROOST_STATE_DIR", &state);
        let roots = vec![root.path().to_path_buf()];
        std::fs::create_dir_all(root.path().join("repo")).unwrap();
        let owner = "repo/.claude/worktrees/worktree-x";
        let held = crate::session::socket_path("repo", "held");
        let dead = crate::session::socket_path("repo", "dead");
        std::fs::create_dir_all(held.parent().unwrap()).unwrap();
        std::fs::write(&held, "").unwrap();
        std::fs::write(&dead, "").unwrap();
        for n in ["held", "dead", "nosock"] {
            crate::home::record("repo", n, owner).unwrap();
        }
        // A holder for `held` only (a pid that is not ours to signal: the
        // project exists, so a held socket is left alone, never killed).
        let args = format!("dtach -A {} -E -r none bash", held.display());
        reconcile_with(&roots, &|| Some(vec![(999_999, args.clone())]));

        assert!(held.exists() && !dead.exists(), "setup: reconcile reaped the unheld socket only");
        assert!(matches!(crate::home::owner_of("repo", "held"), crate::home::Owner::Elsewhere(_)), "held: kept");
        assert!(matches!(crate::home::owner_of("repo", "dead"), crate::home::Owner::Here), "reaped this pass: forgotten");
        assert!(matches!(crate::home::owner_of("repo", "nosock"), crate::home::Owner::Here), "no socket: forgotten");
        std::env::remove_var("ROOST_STATE_DIR");
    }

    #[test]
    fn a_base_file_for_a_worktree_that_no_longer_exists_is_reaped_and_one_that_does_is_kept() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        std::env::set_var("ROOST_STATE_DIR", &state);
        let roots = vec![root.path().to_path_buf()];
        std::fs::create_dir_all(root.path().join("repo/.claude/worktrees/claude-1")).unwrap();
        let live = crate::projects::storage_key("repo/.claude/worktrees/claude-1");
        let gone = crate::projects::storage_key("repo/.claude/worktrees/claude-2");
        crate::worktree::write_base(&state, &live, "main").unwrap();
        crate::worktree::write_base(&state, &gone, "main").unwrap();
        // A base whose directory cannot be looked at: parent unreadable.
        let blocked = crate::projects::storage_key("repo/.claude/worktrees/blocked/deep");
        std::fs::create_dir_all(root.path().join("repo/.claude/worktrees/blocked")).unwrap();
        crate::worktree::write_base(&state, &blocked, "main").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.path().join("repo/.claude/worktrees/blocked"),
                std::fs::Permissions::from_mode(0o000),
            )
            .unwrap();
        }
        // SnapshotFn is `&dyn Fn() -> Option<Vec<(u32, String)>>` (see
        // `type SnapshotFn` above), not `Option<String>` — an empty process
        // list, not an empty string. This pass reaps `.base` files, not
        // sockets, so no process needs to be "running" for this test.
        reconcile_with(&roots, &|| Some(Vec::new()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.path().join("repo/.claude/worktrees/blocked"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        assert!(crate::worktree::read_base(&state, &live).is_some(), "kept");
        assert!(crate::worktree::read_base(&state, &gone).is_none(), "reaped");
        // chmod 000 does not block root, so this assertion is vacuous under
        // a root-run suite — skip just it rather than the whole test, and
        // rely on code inspection (the `Err(_) => keep` arm above) for that
        // case. The other two assertions above still run unconditionally.
        if std::env::var("USER").as_deref() != Ok("root") {
            assert!(crate::worktree::read_base(&state, &blocked).is_some(), "cannot tell: kept");
        }
        std::env::remove_var("ROOST_STATE_DIR");
    }

    /// A layout file for a directory that no longer exists is not a
    /// project; one with a real directory is. Revert-checked: without the
    /// `retain`, `ghost` is listed and the first assertion fails.
    #[test]
    fn a_layout_whose_directory_is_gone_is_not_listed_unless_live() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::env::set_var("ROOST_STATE_DIR", &state);
        let root = d.path().join("root");
        std::fs::create_dir_all(root.join("realproj")).unwrap();
        std::fs::write(state.join("ghost.json"), "{}").unwrap();
        std::fs::write(state.join("realproj.json"), "{}").unwrap();
        let ps = known_projects(&[root.clone()]);
        let keys: Vec<&str> = ps.iter().map(|p| p.key.as_str()).collect();
        assert!(!keys.contains(&"ghost"), "ghost has no directory: {keys:?}");
        assert!(keys.contains(&"realproj"), "{keys:?}");
    }

    /// The front page must list a directory nobody has opened yet — the
    /// picker it replaced showed every directory under the roots.
    /// Revert-checked: with the filesystem merge dropped this failed with
    /// `neveropened missing: ["opened"]`.
    #[test]
    fn project_rows_lists_directories_that_were_never_opened() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::env::set_var("ROOST_STATE_DIR", &state);
        let root = d.path().join("root");
        std::fs::create_dir_all(root.join("opened")).unwrap();
        std::fs::create_dir_all(root.join("neveropened")).unwrap();
        std::fs::write(state.join("opened.json"), "{}").unwrap();
        let ps = project_rows(&[root.clone()]);
        let keys: Vec<&str> = ps.iter().map(|p| p.key.as_str()).collect();
        assert!(keys.contains(&"neveropened"), "neveropened missing: {keys:?}");
        assert!(keys.contains(&"opened"), "{keys:?}");
        let never = ps.iter().find(|p| p.key == "neveropened").unwrap();
        assert!(!never.has_layout && never.live == 0 && never.reachable, "listed as plain and idle");
        // Opened-ness is still visible: only that one has a layout.
        assert!(ps.iter().find(|p| p.key == "opened").unwrap().has_layout);
    }

    /// #183 chain (a)'s state. A renamed to B and A's session `shell`
    /// followed; a new A (the occupant) made at the old name, and a live
    /// main-checkout terminal `term3` re-homed into it — so its record names
    /// A and carries the *occupant's* identity; then the occupant renamed to
    /// D while B still stands. Real git, real dtach.
    struct ChainA {
        repo: PathBuf,
        a: String,
        b: String,
        d: String,
        followed: PathBuf,
        resident: PathBuf,
    }

    fn chain_a(state: &std::path::Path, root: &std::path::Path, name: &str) -> ChainA {
        let roots = vec![root.to_path_buf()];
        let repo = repo_with_worktree_a(root, name);
        let a = format!("{name}/.claude/worktrees/worktree-a");
        let b = format!("{name}/.claude/worktrees/worktree-b");
        let d = format!("{name}/.claude/worktrees/worktree-d");
        let sock_dir = state.join("sock").join(crate::projects::storage_key(&a));
        fs::create_dir_all(&sock_dir).unwrap();
        let followed = sock_dir.join("shell");
        spawn_detached_dtach_in(&followed, &repo.join(".claude/worktrees/worktree-a"))
            .unwrap_or_else(|| panic!("dtach must be available for this test"));
        let _ = reconcile(&roots);
        git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
        follow_moves(&reconcile(&roots).moved);
        assert_eq!(crate::home::owner_of(&a, "shell"), crate::home::Owner::Elsewhere(b.clone()), "setup: A's session followed B");

        git_in(&repo, &["worktree", "add", "-q", "-b", "worktree-a2", ".claude/worktrees/worktree-a"]);
        let new_a = crate::projects::resolve_project(&roots, &a).expect("setup: the new A resolves");
        let resident = state.join("sock").join(crate::projects::storage_key(name)).join("term3");
        fs::create_dir_all(resident.parent().unwrap()).unwrap();
        spawn_detached_dtach_in(&resident, &repo).unwrap_or_else(|| panic!("dtach must be available for this test"));
        crate::hub::rehome_to(name, "term3", &a).unwrap();
        let occupant = full_id(&new_a);
        assert_eq!(crate::home::identity_of(name, "term3"), Ok(Some(occupant)), "setup: the resident carries the occupant's identity");
        assert_ne!(occupant, full_id(&repo.join(".claude/worktrees/worktree-b")), "setup: the occupant is not B");
        let _ = reconcile(&roots); // the occupied pass

        git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-d"]);
        ChainA { repo, a, b, d, followed, resident }
    }

    /// #183 chain (a): the occupant's resident is re-pointed to D, where its
    /// own identity is — never to B, where A's *key* says its directory
    /// went. Through `follow_moves` (part (b) of `home::follow_move_in`,
    /// which used to re-point every record owned by A; the pass is made
    /// fresh, as roost's startup makes every pass) and through R18 with the
    /// resident's Claude working in D (whose first arm used to take
    /// `record_move(A)`'s key-level `Followable(B)`).
    ///
    /// Revert-checked, run and restored: on the pre-#183 code, fails
    /// "follow_moves does not re-point the occupant's resident to B" — left
    /// `Elsewhere(".../worktree-b")`. With the key-level `record_move` arm
    /// restored at the top of `follow_gone_owner_inner` (the rest of this
    /// task kept), fails "R18 follows the resident to D, by its own
    /// identity" — left `Elsewhere("chainarepo/.claude/worktrees/worktree-b")`.
    /// With an R18 `.moved-to` write put back ungated (Task 4's first
    /// version had one, gated on the key's own identity; fix round 1
    /// removed the write), fails "A's .moved-to still names B" — left
    /// `"chainarepo/.claude/worktrees/worktree-d\n"`.
    #[test]
    fn an_occupants_resident_is_not_repointed_to_b() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("ROOST_CMD");
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let home = "chainarepo";
            let c = chain_a(state, root.path(), home);
            let d_dir = c.repo.join(".claude/worktrees/worktree-d").canonicalize().unwrap();

            let pass = reconcile(&roots);
            let a_to_b = pass.moved.iter().any(|m| m.old_url == c.a && m.new_url == c.b);
            let fresh: Vec<Moved> = pass.moved.into_iter().map(|m| Moved { fresh: true, ..m }).collect();
            follow_moves(&fresh);
            let after_follow_moves = crate::home::owner_of(home, "term3");

            let procs = vec![crate::claudes::ClaudeProc {
                project: home.into(),
                session: "term3".into(),
                owner: c.a.clone(),
                sse_port: None,
                cwd: Some(d_dir.clone()),
            }];
            let home_dir_of = |p: &str| (p == home).then(|| c.repo.clone());
            crate::claudes::sweep_gone_owners(&home_dir_of, &std::sync::Mutex::new(Default::default()), &procs);
            let after_r18 = crate::home::owner_of(home, "term3");
            let moved_to = fs::read_to_string(key_dir_of(&c.a).join(".moved-to")).ok();
            let shell = crate::home::owner_of(&c.a, "shell");

            follow_moves(&reconcile(&roots).moved);
            let after_again = crate::home::owner_of(home, "term3");
            let alive = socket_has_process(&c.resident) && socket_has_process(&c.followed);
            kill_test_pids(&c.followed);
            kill_test_pids(&c.resident);
            std::env::remove_var("ROOST_ROOTS");

            assert!(a_to_b, "setup: the pass still reports A→B, the move part (b) used to apply to every record of A");
            assert_eq!(after_follow_moves, crate::home::Owner::Elsewhere(c.a.clone()), "follow_moves does not re-point the occupant's resident to B");
            assert_eq!(after_r18, crate::home::Owner::Elsewhere(c.d.clone()), "R18 follows the resident to D, by its own identity");
            assert_eq!(moved_to.as_deref(), Some(&*format!("{}\n", c.b)), "A's .moved-to still names B: one resident's evidence is not the key's");
            assert_eq!(shell, crate::home::Owner::Elsewhere(c.b.clone()), "A's followed session stays B's");
            assert_eq!(after_again, crate::home::Owner::Elsewhere(c.d.clone()), "a later pass leaves the resident with D");
            assert!(alive, "nothing was ended on the way");
        });
    }

    /// #183 Review Focus 2: chain (a) with no Claude in the resident — it
    /// exited, so there is no cwd to look up from. The sibling scan alone
    /// finds D, beside A's recorded `.origin`, by the record's own identity.
    ///
    /// Revert-checked, run and restored: on the pre-#183 code, fails "the
    /// sibling scan finds D" — left `Elsewhere(".../worktree-b")`, the
    /// key-level arm; with `find_by_identity`'s candidate (2) skipped, fails
    /// the same assertion — left `Here`, the record forgotten.
    #[test]
    fn a_resident_with_no_claude_follows_by_its_identity() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("ROOST_CMD");
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let home = "nocladrepo";
            let c = chain_a(state, root.path(), home);
            let d_dir = c.repo.join(".claude/worktrees/worktree-d");

            let home_dir_of = |p: &str| (p == home).then(|| c.repo.clone());
            crate::claudes::sweep_gone_owners(&home_dir_of, &std::sync::Mutex::new(Default::default()), &[]);
            let got = crate::home::owner_of(home, "term3");
            let id = crate::home::identity_of(home, "term3");
            let alive = socket_has_process(&c.resident);
            kill_test_pids(&c.followed);
            kill_test_pids(&c.resident);
            std::env::remove_var("ROOST_ROOTS");

            assert_eq!(got, crate::home::Owner::Elsewhere(c.d.clone()), "the sibling scan finds D");
            assert_eq!(id, Ok(Some(full_id(&d_dir))), "and the record still carries D's identity");
            assert!(alive, "the resident's master is alive");
        });
    }

    /// #183 Review Focus 1, on ext4 where the reuse is real: the record's
    /// target is removed and a directory made at the same path, which ext4
    /// gives the same inode (it is made again until it does, so the fixture
    /// cannot pass without the reuse); that directory becomes a worktree and
    /// is renamed to C beside it. C has the record's `(dev, ino)` and a new
    /// birth time: it is not the record's directory, and the record must not
    /// follow into it — by the sibling scan, or by a Claude working in C.
    ///
    /// Revert-checked, run and restored: with `find_by_identity`'s compare
    /// cut to `(dev, ino)` (`here.dev == id.dev && here.ino == id.ino`),
    /// fails "nothing carries the record's identity" — left
    /// `At(".../worktree-c", "recrepo/.claude/worktrees/worktree-c")` — and
    /// "the record does not follow into a directory that merely reused its
    /// inode", left `Elsewhere("recrepo/.claude/worktrees/worktree-c")`.
    #[test]
    fn a_recreated_target_is_not_the_records_directory() {
        use std::os::unix::fs::MetadataExt;
        with_state(|_state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let root = ext4_tempdir();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let home = "recrepo";
            let repo = repo_with_worktree_a(root.path(), home);
            let a = format!("{home}/.claude/worktrees/worktree-a");
            let c = format!("{home}/.claude/worktrees/worktree-c");
            let a_dir = repo.join(".claude/worktrees/worktree-a");
            crate::hub::rehome_to(home, "term3", &a).unwrap();
            let Ok(Some(crate::dirid::Stored::Full(old))) = crate::home::identity_of(home, "term3") else {
                panic!("setup: rehome_to records a full identity")
            };

            fs::remove_dir_all(&a_dir).unwrap();
            let fillers = root.path().join("fillers");
            fs::create_dir_all(&fillers).unwrap();
            let reused = (0..64).any(|k| {
                fs::create_dir(&a_dir).unwrap();
                let same = fs::symlink_metadata(&a_dir).unwrap().ino() == old.ino;
                if !same {
                    fs::rename(&a_dir, fillers.join(k.to_string())).unwrap();
                }
                same
            });
            assert!(reused, "setup: ext4 handed the record's inode to a new directory at its path");
            git_in(&repo, &["worktree", "prune"]);
            git_in(&repo, &["worktree", "add", "-q", "-b", "worktree-a2", ".claude/worktrees/worktree-a"]);
            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-c"]);
            let c_dir = repo.join(".claude/worktrees/worktree-c").canonicalize().unwrap();
            let now = crate::dirid::of(&c_dir).unwrap().unwrap();

            let direct = find_by_identity(&roots, &a, &old, Some(&c_dir));
            let procs = vec![crate::claudes::ClaudeProc {
                project: home.into(),
                session: "term3".into(),
                owner: a.clone(),
                sse_port: None,
                cwd: Some(c_dir.clone()),
            }];
            let home_dir_of = |p: &str| (p == home).then(|| repo.clone());
            crate::claudes::sweep_gone_owners(&home_dir_of, &std::sync::Mutex::new(Default::default()), &procs);
            let got = crate::home::owner_of(home, "term3");
            let id = crate::home::identity_of(home, "term3");
            std::env::remove_var("ROOST_ROOTS");

            assert!((now.dev, now.ino) == (old.dev, old.ino) && now != old, "setup: C has the record's (dev, ino), and a new birth time");
            assert_eq!(direct, Found::Nowhere, "nothing carries the record's identity");
            assert_ne!(got, crate::home::Owner::Elsewhere(c.clone()), "the record does not follow into a directory that merely reused its inode");
            assert_ne!(id, Ok(Some(crate::dirid::Stored::Full(now))), "nor takes on its identity");
        });
    }

    /// #183 §1: every writer of a record writes its target's identity —
    /// `claudes::rewrite_owner` included. A rename keeps the identity, so
    /// the content alone cannot show a write; the `.ino` file's own inode
    /// can (an atomic rewrite is a new file).
    ///
    /// Revert-checked, run and restored: with `rewrite_owner`'s
    /// `record_identity` call removed, fails "the rewrite wrote the .ino" —
    /// left and right the same inode, the old file left in place. Before
    /// the implementation, failed earlier, "setup: the record followed B" —
    /// left `Here`: with no Claude, the old cwd walk was the only evidence.
    #[test]
    fn a_rewritten_record_carries_its_new_identity() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let home = "rwrepo";
            let repo = repo_with_worktree_a(root.path(), home);
            let a = format!("{home}/.claude/worktrees/worktree-a");
            let b = format!("{home}/.claude/worktrees/worktree-b");
            crate::hub::rehome_to(home, "term3", &a).unwrap();
            let ino_path = state.join("home").join(crate::projects::storage_key(home)).join(".term3.ino");
            let before = fs::metadata(&ino_path).expect("setup: rehome_to wrote the .ino").ino();

            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
            let home_dir_of = |p: &str| (p == home).then(|| repo.clone());
            crate::claudes::sweep_gone_owners(&home_dir_of, &std::sync::Mutex::new(Default::default()), &[]);
            let got = crate::home::owner_of(home, "term3");
            let id = crate::home::identity_of(home, "term3");
            let md = fs::metadata(&ino_path).ok();
            std::env::remove_var("ROOST_ROOTS");

            assert_eq!(got, crate::home::Owner::Elsewhere(b), "setup: the record followed B");
            assert_eq!(id, Ok(Some(full_id(&repo.join(".claude/worktrees/worktree-b")))), "it carries B's full identity");
            let md = md.expect("the .ino is there");
            assert_ne!(md.ino(), before, "the rewrite wrote the .ino");
            assert_eq!(md.permissions().mode() & 0o777, 0o600, "privately");
        });
    }

    /// #183 fix round 1: the record's directory stands under the roots
    /// carrying its identity, but git cannot vouch for its worktree URL
    /// (the main checkout's `.git/HEAD` moved aside, as in N1). That is
    /// "cannot tell": R18 keeps the record, rather than forgetting the
    /// terminal into the main checkout, whose Close Project would end it.
    /// `home_dir_of` points the gone-owner detection at a healthy decoy
    /// repo — its seam — so the detection still answers while the real
    /// repo's git fails; once git answers again the record follows B.
    ///
    /// Revert-checked, run and restored: with `accept`'s unresolved arm back
    /// to `Ok(None)`, fails "git that cannot vouch is cannot tell" — left
    /// `Nowhere` — and, with that assertion removed, "the record is kept,
    /// not forgotten" — left `Here`.
    #[test]
    fn a_record_whose_directory_git_cannot_vouch_for_is_kept() {
        with_state(|_state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let home = "vouchrepo";
            let repo = repo_with_worktree_a(root.path(), home);
            let decoy = repo_with_worktree_a(root.path(), "vouchdecoy");
            let a = format!("{home}/.claude/worktrees/worktree-a");
            let b = format!("{home}/.claude/worktrees/worktree-b");
            crate::hub::rehome_to(home, "term3", &a).unwrap();
            let Ok(Some(crate::dirid::Stored::Full(id))) = crate::home::identity_of(home, "term3") else {
                panic!("setup: rehome_to records a full identity")
            };
            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);

            fs::rename(repo.join(".git/HEAD"), repo.join(".git/HEAD.away")).unwrap();
            let unvouched = crate::projects::resolve_project(&roots, &b).is_none();
            let direct = find_by_identity(&roots, &a, &id, None);
            let decoy_of = |p: &str| (p == home).then(|| decoy.clone());
            crate::claudes::sweep_gone_owners(&decoy_of, &std::sync::Mutex::new(Default::default()), &[]);
            let kept = crate::home::owner_of(home, "term3");
            fs::rename(repo.join(".git/HEAD.away"), repo.join(".git/HEAD")).unwrap();
            let home_dir_of = |p: &str| (p == home).then(|| repo.clone());
            crate::claudes::sweep_gone_owners(&home_dir_of, &std::sync::Mutex::new(Default::default()), &[]);
            let later = crate::home::owner_of(home, "term3");
            std::env::remove_var("ROOST_ROOTS");

            assert!(unvouched, "setup: B stands, but git cannot vouch for it");
            assert!(matches!(direct, Found::CannotTell(_)), "git that cannot vouch is cannot tell: {direct:?}");
            assert_eq!(kept, crate::home::Owner::Elsewhere(a.clone()), "the record is kept, not forgotten");
            assert_eq!(later, crate::home::Owner::Elsewhere(b), "and follows B once git answers");
        });
    }

    /// Ends every test pid holding these sockets when dropped, so a failed
    /// assertion cannot leak a dtach master (#192's bug class).
    struct KillOnDrop(Vec<PathBuf>);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            for p in &self.0 {
                kill_test_pids(p);
            }
        }
    }

    /// A git repo at `dir` with one commit and an empty `.claude/worktrees`
    /// — the parent `detect_gone_owners` canonicalises — but no worktree.
    fn plain_repo(dir: &std::path::Path) {
        fs::create_dir_all(dir.join(".claude/worktrees")).unwrap();
        git_in(dir, &["init", "-q", "-b", "main"]);
        git_in(dir, &["config", "user.email", "t@t"]);
        git_in(dir, &["config", "user.name", "t"]);
        fs::write(dir.join("b.txt"), "y").unwrap();
        git_in(dir, &["add", "b.txt"]);
        git_in(dir, &["commit", "-qm", "occupant"]);
    }

    /// The shape R18 can forget into an occupant (#183 §2): main checkout A
    /// with a live resident `term3` re-homed into its worktree W, and a live
    /// session `shell` of its own. A is renamed to B and followed (so A's key
    /// holds `.moved-to`), then a new repository — the occupant — is made at
    /// A's URL. Real git, real dtach; the guard ends both shells.
    struct RenamedHome {
        key: PathBuf,
        resident: PathBuf,
        own: PathBuf,
        w: String,
        occupant: PathBuf,
        original: Option<crate::dirid::Stored>,
        own_followed: crate::home::Owner,
        _pids: KillOnDrop,
    }

    fn renamed_home(state: &std::path::Path, root: &std::path::Path, a: &str, b: &str) -> RenamedHome {
        let roots = vec![root.to_path_buf()];
        let repo = repo_with_worktree_a(root, a);
        let w = format!("{a}/.claude/worktrees/worktree-a");
        let key = state.join("sock").join(crate::projects::storage_key(a));
        fs::create_dir_all(&key).unwrap();
        let (resident, own) = (key.join("term3"), key.join("shell"));
        let pids = KillOnDrop(vec![resident.clone(), own.clone()]);
        spawn_detached_dtach_in(&resident, &repo.join(".claude/worktrees/worktree-a")).expect("dtach must be available for this test");
        spawn_detached_dtach_in(&own, &repo).expect("dtach must be available for this test");
        let _ = reconcile(&roots);
        crate::hub::rehome_to(a, "term3", &w).unwrap();
        let original = recorded_identity(&key);

        fs::rename(&repo, root.join(b)).unwrap();
        follow_moves(&reconcile(&roots).moved);
        let own_followed = crate::home::owner_of(a, "shell");
        let occupant = root.join(a);
        plain_repo(&occupant);
        RenamedHome { key, resident, own, w, occupant, original, own_followed, _pids: pids }
    }

    /// Moves B to a different parent, so W's directory — the record's
    /// identity — is nowhere R18 looks.
    fn move_b_away(root: &std::path::Path, b: &str) {
        let away = root.join("away");
        fs::create_dir_all(&away).unwrap();
        fs::rename(root.join(b), away.join(b)).unwrap();
    }

    /// #183 chain (b) in the shape R18 reaches: a resident whose home was
    /// renamed (see [`renamed_home`]), with an occupant standing at the home
    /// URL and B moved to a different parent. R18 then has positive
    /// evidence that W is gone from the home's `git worktree list` and none
    /// that it moved: before #183 Task 5 it forgot the record, `Here` became
    /// the occupant, and the occupant's Close Project ended a shell working
    /// in B. The record must be kept, `reconcile` must not reap it, and
    /// `kill_project(A)` must spare it.
    ///
    /// R18 never reaches a record homed at a worktree key: `owner_leaf`
    /// refuses an owner outside the home's own `.claude/worktrees/`, so
    /// `detect_gone_owners` skips it. A's own followed session (home A, owner
    /// B) is that kind of record. The last assertion checks that it also
    /// survives the same Close Project, but its `Elsewhere` record protects
    /// it, not `home_is_its_own`; reverting this task's rule does not
    /// change that assertion.
    ///
    /// Revert-checked, run and restored: with `sweep_gone_owners_with`
    /// forgetting unconditionally (the `home_is_its_own` gate removed), fails
    /// "the record is kept, not forgotten into the occupant" — left `Here`.
    /// With that assertion removed too, fails "the occupant's Close Project
    /// does not end it": the session was handed to the occupant and killed.
    #[test]
    fn a_renamed_homes_resident_is_not_forgotten_into_its_occupant() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("ROOST_CMD");
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let (a, b) = ("chbrepo", "chbrepo-b");
            let h = renamed_home(state, root.path(), a, b);
            let _ = reconcile(&roots); // the occupied pass
            move_b_away(root.path(), b);

            let home_dir_of = |p: &str| crate::projects::resolve_project(&roots, p);
            let detected = crate::claudes::detect_gone_owners(&home_dir_of);
            crate::claudes::sweep_gone_owners(&home_dir_of, &std::sync::Mutex::new(Default::default()), &[]);
            let after_r18 = crate::home::owner_of(a, "term3");
            let report = reconcile(&roots);
            let after_reconcile = socket_has_process(&h.resident);
            crate::session::kill_project(a);
            let survived = socket_has_process(&h.resident);
            let own_survived = socket_has_process(&h.own);
            std::env::remove_var("ROOST_ROOTS");

            assert_ne!(recorded_identity(&h.key), Some(full_id(&h.occupant)), "setup: A's recorded identity is not the occupant's");
            assert!(matches!(h.original, Some(crate::dirid::Stored::Full(_))), "setup: A's identity is recorded in full: {:?}", h.original);
            assert_eq!(h.own_followed, crate::home::Owner::Elsewhere(b.into()), "setup: A's own session followed B");
            assert_eq!(detected, vec![(a.to_string(), "term3".to_string(), h.w.clone())], "setup: R18 has evidence that W is gone");
            assert_eq!(after_r18, crate::home::Owner::Elsewhere(h.w.clone()), "the record is kept, not forgotten into the occupant");
            assert_eq!(report.gone_projects, 0, "reconcile reaps nothing");
            assert!(after_reconcile, "the resident survives reconcile");
            assert!(survived, "the occupant's Close Project does not end it");
            assert!(own_survived, "nor A's own followed session");
        });
    }

    /// #183 Task 5 fix round 1: the same renamed home, with its `.origin-ino`
    /// damaged before the occupant's first pass. `occupied_by_another` read
    /// "no identity" as "not occupied", so `reconcile` took the moved-back
    /// arm: it stamped the occupant's identity as the home's own and removed
    /// `.moved-to`, and on the next R18 tick `home_is_its_own` said `Same` and
    /// forgot the resident into the occupant. While `.moved-to` stands, a
    /// missing or unreadable identity is a doubt, and every doubt answers
    /// "occupied".
    ///
    /// Revert-checked, run and restored: with `occupied_by_another`'s
    /// `return false` for a missing identity put back, fails ".moved-to
    /// survives the occupant's pass" (and, with that assertion removed,
    /// "the record is kept, not forgotten into the occupant" — left `Here`).
    #[test]
    fn a_damaged_origin_identity_does_not_read_as_moved_back() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("ROOST_CMD");
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let (a, b) = ("dmgrepo", "dmgrepo-b");
            let h = renamed_home(state, root.path(), a, b);
            let had_moved_to = h.key.join(".moved-to").exists();
            fs::write(origin_identity_path(&h.key), "not an identity\n").unwrap();
            let _ = reconcile(&roots); // the occupant's first pass
            let moved_to = h.key.join(".moved-to").exists();
            move_b_away(root.path(), b);

            let home_dir_of = |p: &str| crate::projects::resolve_project(&roots, p);
            let detected = crate::claudes::detect_gone_owners(&home_dir_of);
            crate::claudes::sweep_gone_owners(&home_dir_of, &std::sync::Mutex::new(Default::default()), &[]);
            let after_r18 = crate::home::owner_of(a, "term3");
            let alive = socket_has_process(&h.resident);
            std::env::remove_var("ROOST_ROOTS");

            assert!(had_moved_to, "setup: the rename was recorded");
            assert_eq!(detected.len(), 1, "setup: R18 has evidence that W is gone: {detected:?}");
            assert!(moved_to, ".moved-to survives the occupant's pass");
            assert_eq!(after_r18, crate::home::Owner::Elsewhere(h.w.clone()), "the record is kept, not forgotten into the occupant");
            assert!(alive, "the resident is alive");
        });
    }

    /// #183 chain (b) when the second rename can be found: a main-checkout
    /// resident re-homed into A, A renamed to B and followed, an occupant
    /// made at A, then B renamed to C beside it. The record follows to C by
    /// its own identity — the sibling scan, with no Claude left to look
    /// from — and the occupant's Close Project does not touch it. The first
    /// follow is made through the resident's Claude's cwd, so the second is
    /// the sibling scan's alone.
    ///
    /// `to_c` and `id` are the assertions that discriminate. The last one,
    /// "the occupant's Close Project does not end it", cannot fail here:
    /// Close Project of a worktree URL ends only that key's own sessions and
    /// its residents, and `term3` is neither. It is kept as a statement of
    /// the outcome, not as a check.
    ///
    /// Revert-checked, run and restored: with `find_by_identity`'s candidate
    /// (2), the sibling scan, skipped, fails "the record follows B's rename
    /// to C" — left `Here`, the record forgotten into the main checkout,
    /// which is the record's own (Same) home.
    #[test]
    fn chain_b_follows_to_c_when_c_is_findable() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("ROOST_CMD");
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let home = "chcrepo";
            let repo = repo_with_worktree_a(root.path(), home);
            let [a, b, c] = ["a", "b", "c"].map(|x| format!("{home}/.claude/worktrees/worktree-{x}"));
            let key = state.join("sock").join(crate::projects::storage_key(home));
            fs::create_dir_all(&key).unwrap();
            let resident = key.join("term3");
            let _pids = KillOnDrop(vec![resident.clone()]);
            spawn_detached_dtach_in(&resident, &repo).expect("dtach must be available for this test");
            let _ = reconcile(&roots);
            crate::hub::rehome_to(home, "term3", &a).unwrap();
            let home_dir_of = |p: &str| crate::projects::resolve_project(&roots, p);

            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
            let b_dir = repo.join(".claude/worktrees/worktree-b").canonicalize().unwrap();
            let procs = vec![crate::claudes::ClaudeProc {
                project: home.into(),
                session: "term3".into(),
                owner: a.clone(),
                sse_port: None,
                cwd: Some(b_dir),
            }];
            crate::claudes::sweep_gone_owners(&home_dir_of, &std::sync::Mutex::new(Default::default()), &procs);
            let to_b = crate::home::owner_of(home, "term3");

            git_in(&repo, &["worktree", "add", "-q", "-b", "worktree-a2", ".claude/worktrees/worktree-a"]);
            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-b", ".claude/worktrees/worktree-c"]);
            let c_dir = repo.join(".claude/worktrees/worktree-c");
            crate::claudes::sweep_gone_owners(&home_dir_of, &std::sync::Mutex::new(Default::default()), &[]);
            let to_c = crate::home::owner_of(home, "term3");
            let id = crate::home::identity_of(home, "term3");
            crate::session::kill_project(&a);
            let alive = socket_has_process(&resident);
            std::env::remove_var("ROOST_ROOTS");

            assert_eq!(to_b, crate::home::Owner::Elsewhere(b.clone()), "setup: the record followed A's rename to B");
            assert_eq!(to_c, crate::home::Owner::Elsewhere(c.clone()), "the record follows B's rename to C");
            assert_eq!(id, Ok(Some(full_id(&c_dir))), "and carries C's identity");
            assert!(alive, "the occupant's Close Project does not end it");
        });
    }

    /// The case R18 exists for, which #183 must keep: Claude Code removed a
    /// worktree, so a main-checkout terminal re-homed into it comes home.
    /// The home is the directory its key recorded, so the forget proceeds.
    ///
    /// Revert-checked, run and restored: with the forget gated on `Gone`
    /// alone (`Same` kept), fails "a removed worktree's resident comes
    /// home" — left `Elsewhere("rmrepo/.claude/worktrees/worktree-a")`.
    #[test]
    fn a_removed_worktrees_resident_still_comes_home() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("ROOST_CMD");
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let home = "rmrepo";
            let repo = repo_with_worktree_a(root.path(), home);
            let a = format!("{home}/.claude/worktrees/worktree-a");
            let key = state.join("sock").join(crate::projects::storage_key(home));
            fs::create_dir_all(&key).unwrap();
            let resident = key.join("term3");
            let _pids = KillOnDrop(vec![resident.clone()]);
            spawn_detached_dtach_in(&resident, &repo).expect("dtach must be available for this test");
            let _ = reconcile(&roots);
            crate::hub::rehome_to(home, "term3", &a).unwrap();
            git_in(&repo, &["worktree", "remove", "--force", ".claude/worktrees/worktree-a"]);

            let home_dir_of = |p: &str| crate::projects::resolve_project(&roots, p);
            crate::claudes::sweep_gone_owners(&home_dir_of, &std::sync::Mutex::new(Default::default()), &[]);
            let got = crate::home::owner_of(home, "term3");
            let alive = socket_has_process(&resident);
            std::env::remove_var("ROOST_ROOTS");

            assert!(matches!(recorded_identity(&key), Some(crate::dirid::Stored::Full(_))), "setup: the home's identity is recorded");
            assert_eq!(got, crate::home::Owner::Here, "a removed worktree's resident comes home");
            assert!(alive, "and is not ended by coming home");
        });
    }

    /// #183 Review Focus 4: a record whose home has no key directory at all
    /// — no sockets, no markers — so nothing says which directory the home
    /// was. While the home URL still resolves that is "cannot tell", and the
    /// record is kept (nothing can be running under a key with no
    /// directory; `home::sweep` drops the record with its socket). Once the
    /// URL is gone — nothing at `<root>/<home>` — the forget proceeds, as
    /// today: `Here` names nothing a page or Close Project can act on. A
    /// non-directory standing at `<root>/<home>` does not resolve but is not
    /// gone either: kept. Detection needs the home to resolve (it asks the
    /// home's own git), so the home is moved away in the window between the
    /// evidence and the forget, through `sweep_gone_owners_with`'s hook.
    ///
    /// Revert-checked, run and restored: with a missing key directory read
    /// as `Same`, fails "no key directory, and the URL resolves: kept" —
    /// left `Here`. With the root walk's `Ok(_)` arm read as `NotFound`,
    /// fails "something at the URL that does not resolve: kept" — left
    /// `Here`.
    #[test]
    fn a_home_with_no_key_dir_is_forgotten_only_when_its_url_is_gone() {
        with_state(|state| {
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let home = "nokeyrepo";
            let repo = repo_with_worktree_a(root.path(), home);
            let a = format!("{home}/.claude/worktrees/worktree-a");
            crate::hub::rehome_to(home, "term3", &a).unwrap();
            git_in(&repo, &["worktree", "remove", "--force", ".claude/worktrees/worktree-a"]);
            let key = state.join("sock").join(crate::projects::storage_key(home));
            let home_dir_of = |p: &str| crate::projects::resolve_project(&[root.path().to_path_buf()], p);
            let moved = root.path().join("elsewhere");
            let sweep = |hook: &dyn Fn(&str, &str)| {
                crate::claudes::sweep_gone_owners_with(&home_dir_of, &std::sync::Mutex::new(Default::default()), &[], hook);
                crate::home::owner_of(home, "term3")
            };

            let resolving = sweep(&|_, _| {});
            let reached = std::cell::Cell::new(false);
            let a_file = sweep(&|_, _| {
                fs::rename(&repo, &moved).unwrap();
                fs::write(&repo, "not a directory").unwrap();
                reached.set(true);
            });
            if reached.get() {
                fs::remove_file(&repo).unwrap();
                fs::rename(&moved, &repo).unwrap();
            }
            let gone = sweep(&|_, _| fs::rename(&repo, &moved).unwrap());
            std::env::remove_var("ROOST_ROOTS");

            assert!(!key.exists(), "setup: the home has no key directory");
            assert_eq!(resolving, crate::home::Owner::Elsewhere(a.clone()), "no key directory, and the URL resolves: kept");
            assert!(reached.get(), "setup: the sweep had evidence and reached the forget");
            assert_eq!(a_file, crate::home::Owner::Elsewhere(a.clone()), "something at the URL that does not resolve: kept");
            assert_eq!(gone, crate::home::Owner::Here, "the URL gone: forgotten");
        });
    }

    /// #183 chain (d)'s shape: A's session followed to B and still working
    /// there; a new directory made at A, with sessions of its own; that A
    /// renamed to D while B still stands. Every live socket is in
    /// `sockets`, for a `KillOnDrop`.
    struct ChainD {
        repo: PathBuf,
        a: String,
        b: String,
        d: String,
        sock_dir: PathBuf,
        sockets: Vec<PathBuf>,
    }

    /// `own` names the occupant's sessions, each spawned in the new A by
    /// `spawn(sock, dir)`; `before_d` runs just before the occupant is
    /// renamed, with the guard already holding every socket.
    fn chain_d(
        state: &std::path::Path,
        root: &std::path::Path,
        name: &str,
        own: &[&str],
        spawn: &dyn Fn(&std::path::Path, &std::path::Path),
        guard: &mut KillOnDrop,
        before_d: &dyn Fn(&ChainD),
    ) -> ChainD {
        let roots = vec![root.to_path_buf()];
        let repo = repo_with_worktree_a(root, name);
        let a = format!("{name}/.claude/worktrees/worktree-a");
        let b = format!("{name}/.claude/worktrees/worktree-b");
        let d = format!("{name}/.claude/worktrees/worktree-d");
        let sock_dir = state.join("sock").join(crate::projects::storage_key(&a));
        fs::create_dir_all(&sock_dir).unwrap();
        let followed = sock_dir.join("shell");
        guard.0.push(followed.clone());
        spawn_detached_dtach_in(&followed, &repo.join(".claude/worktrees/worktree-a"))
            .unwrap_or_else(|| panic!("dtach must be available for this test"));
        let _ = reconcile(&roots);
        git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
        follow_moves(&reconcile(&roots).moved);
        assert_eq!(crate::home::owner_of(&a, "shell"), crate::home::Owner::Elsewhere(b.clone()), "setup: A's session followed B");

        git_in(&repo, &["worktree", "add", "-q", "-b", "worktree-a2", ".claude/worktrees/worktree-a"]);
        let new_a = crate::projects::resolve_project(&roots, &a).expect("setup: the new A resolves");
        for n in own {
            let sock = sock_dir.join(n);
            guard.0.push(sock.clone());
            spawn(&sock, &new_a);
        }
        record_origin(&a, &new_a); // the occupant's sessions' attach
        let _ = reconcile(&roots); // the occupied pass
        assert!(
            occupant_lines(&sock_dir).contains(&Some(full_id(&new_a))),
            "setup: the occupant's identity is listed: {:?}",
            occupant_lines(&sock_dir)
        );
        let mut sockets = vec![followed];
        sockets.extend(own.iter().map(|n| sock_dir.join(n)));
        let c = ChainD { repo, a, b, d, sock_dir, sockets };
        before_d(&c);
        git_in(&c.repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-d"]);
        c
    }

    fn spawn_in(sock: &std::path::Path, dir: &std::path::Path) {
        spawn_detached_dtach_in(sock, dir).unwrap_or_else(|| panic!("dtach must be available for this test"));
    }

    /// #183 chain (d): the occupant renamed to D while B still exists takes
    /// its own session to D — a record naming D, carrying D's identity — and
    /// A's earlier-followed session stays with B. The pass is not fresh (A's
    /// `.moved-to` still names a B that stands), which is the pass that
    /// used to take only sessions whose shell was in B. R16 still holds per
    /// target: an occupant session whose name is already live natively in D
    /// is refused, not shadowed.
    ///
    /// Revert-checked, run and restored: on the pre-Task-6 code (the
    /// fresh-or-inside narrowing), fails "the occupant's own session follows
    /// D while B exists" — left `Here`. With `own_follow`'s target taken as
    /// `new_url` whatever the shell said (`To(new_url, new_dir)`), fails the
    /// same assertion — left `Elsewhere(".../worktree-b")`. With part (a)'s
    /// R16 check removed, fails "R16: a name live in D is not shadowed" —
    /// left `Elsewhere(".../worktree-d")`. With part (a)'s
    /// `record_identity_in` removed, fails "its record carries D's identity"
    /// — left `Ok(None)`. With `home::owner_dir_with`'s identity widening
    /// removed, fails "D lists it" — left `[("dup", None)]`: followed on
    /// paper, shown nowhere, which is chain (d)'s strand.
    #[test]
    fn an_occupants_own_session_follows_d_while_b_exists() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("ROOST_CMD");
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let mut guard = KillOnDrop(vec![]);
            let d_native = state.join("sock").join(crate::projects::storage_key("chaindrepo/.claude/worktrees/worktree-d")).join("dup");
            guard.0.push(d_native.clone());
            let c = chain_d(state, root.path(), "chaindrepo", &["own", "dup"], &spawn_in, &mut guard, &|c| {
                // D's own native `dup`, live before the rename lands there.
                fs::create_dir_all(d_native.parent().unwrap()).unwrap();
                spawn_in(&d_native, &c.repo);
            });
            let d_dir = c.repo.join(".claude/worktrees/worktree-d").canonicalize().unwrap();

            let pass = reconcile(&roots);
            let shape: Vec<(String, String, bool)> = pass.moved.iter().map(|m| (m.old_url.clone(), m.new_url.clone(), m.fresh)).collect();
            follow_moves(&pass.moved);
            let own = crate::home::owner_of(&c.a, "own");
            let own_id = crate::home::identity_of(&c.a, "own");
            let dup = crate::home::owner_of(&c.a, "dup");
            let shell = crate::home::owner_of(&c.a, "shell");
            let owned_d = crate::session::owned_live_names(&c.d);
            let owned_b = crate::session::owned_live_names(&c.b);
            let alive = c.sockets.iter().all(|p| socket_has_process(p));
            drop(guard);
            std::env::remove_var("ROOST_ROOTS");

            assert_eq!(pass.gone_projects, 0, "nothing reaped");
            assert_eq!(shape, vec![(c.a.clone(), c.b.clone(), false)], "setup: the pass is A→B, not fresh");
            assert_eq!(own, crate::home::Owner::Elsewhere(c.d.clone()), "the occupant's own session follows D while B exists");
            assert_eq!(own_id, Ok(Some(full_id(&d_dir))), "its record carries D's identity");
            assert_eq!(dup, crate::home::Owner::Here, "R16: a name live in D is not shadowed");
            assert_eq!(shell, crate::home::Owner::Elsewhere(c.b.clone()), "A's earlier-followed session stays with B");
            assert!(owned_d.contains(&("own".to_string(), Some(c.a.clone()))), "D lists it: {owned_d:?}");
            assert!(owned_b.contains(&("shell".to_string(), Some(c.a.clone()))), "B still lists its own: {owned_b:?}");
            assert!(alive, "nothing was ended on the way");
            let _ = c.sock_dir;
        });
    }

    /// #183 Review Focus 5: one key, two live sessions, two directories —
    /// `shell` working in B (the earlier follow), `own` in D (the occupant,
    /// renamed). Each ends up owned by its own directory, on the pass
    /// roost's startup makes (every move `fresh`, `lib.rs`) — the pass that
    /// used to take every live session to the key's one target — and stays
    /// so on the ordinary pass after it.
    ///
    /// Revert-checked, run and restored: with the fresh branch of
    /// `follow_moves` given every live session as following `new_url`
    /// ("fresh takes every live session"), fails "the occupant's session
    /// follows D, not the key's B" — left `Elsewhere(".../worktree-b")`:
    /// the occupant's session is sent to B.
    #[test]
    fn each_session_follows_its_own_shell() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("ROOST_CMD");
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let mut guard = KillOnDrop(vec![]);
            let c = chain_d(state, root.path(), "eachrepo", &["own"], &spawn_in, &mut guard, &|_| {});
            let (b_dir, d_dir) = (
                c.repo.join(".claude/worktrees/worktree-b").canonicalize().unwrap(),
                c.repo.join(".claude/worktrees/worktree-d").canonicalize().unwrap(),
            );

            let pass = reconcile(&roots);
            let shells = pass.moved.first().map(|m| m.shells.clone()).unwrap_or_default();
            let startup: Vec<Moved> = pass.moved.into_iter().map(|m| Moved { fresh: true, ..m }).collect();
            follow_moves(&startup);
            let (shell, own) = (crate::home::owner_of(&c.a, "shell"), crate::home::owner_of(&c.a, "own"));
            follow_moves(&reconcile(&roots).moved);
            let (shell_after, own_after) = (crate::home::owner_of(&c.a, "shell"), crate::home::owner_of(&c.a, "own"));
            let alive = c.sockets.iter().all(|p| socket_has_process(p));
            drop(guard);
            std::env::remove_var("ROOST_ROOTS");

            assert_eq!(shells, vec![("own".to_string(), d_dir), ("shell".to_string(), b_dir)], "setup: each shell's own directory");
            assert_eq!(shell, crate::home::Owner::Elsewhere(c.b.clone()), "the followed session stays B's");
            assert_eq!(own, crate::home::Owner::Elsewhere(c.d.clone()), "the occupant's session follows D, not the key's B");
            assert_eq!((shell_after, own_after), (shell, own), "and the next pass changes neither");
            assert!(alive, "nothing was ended on the way");
        });
    }

    /// #183 §3: a session whose shell has left every one of the key's
    /// directories gives no evidence about where it belongs, and stays
    /// where it is (logged once) — neither handed to B, where the key's
    /// `.moved-to` points, nor to D. Nor is it reaped: the key's evidence
    /// still says moved. On both an ordinary pass and a startup (fresh) one.
    ///
    /// Revert-checked, run and restored: with `own_follow`'s `Stays` arm
    /// answering `To(new_url, new_dir)` (the key's target for a session
    /// with no evidence), fails "a session whose shell left stays where it
    /// is" — left `Elsewhere(".../worktree-b")`. It passes on the
    /// pre-Task-6 code too (run, observed): there the occupant gate held
    /// every pass to `inside`, so this is the regression guard for the rule
    /// that replaced that gate, not a test of a fixed defect.
    #[test]
    fn a_session_whose_shell_left_stays() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("ROOST_CMD");
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let flag = root.path().join("leave-now");
            let leave = |sock: &std::path::Path, dir: &std::path::Path| spawn_leaver(sock, dir, &flag);
            let mut guard = KillOnDrop(vec![]);
            let c = chain_d(state, root.path(), "leftrepo", &["own"], &leave, &mut guard, &|c| wait_left(&c.sockets[1], &flag));

            let pass = reconcile(&roots);
            let shells = pass.moved.first().map(|m| m.shells.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>());
            follow_moves(&pass.moved);
            let ordinary = crate::home::owner_of(&c.a, "own");
            let startup: Vec<Moved> = reconcile(&roots).moved.into_iter().map(|m| Moved { fresh: true, ..m }).collect();
            follow_moves(&startup);
            let fresh = crate::home::owner_of(&c.a, "own");
            let shell = crate::home::owner_of(&c.a, "shell");
            let alive = c.sockets.iter().all(|p| socket_has_process(p));
            drop(guard);
            std::env::remove_var("ROOST_ROOTS");

            assert_eq!(pass.gone_projects, 0, "not reaped");
            assert_eq!(shells, Some(vec!["shell".to_string()]), "setup: only the followed session's shell is in one of the key's directories");
            assert_eq!(ordinary, crate::home::Owner::Here, "a session whose shell left stays where it is");
            assert_eq!(fresh, crate::home::Owner::Here, "a session whose shell left stays where it is, on a fresh pass too");
            assert_eq!(shell, crate::home::Owner::Elsewhere(c.b.clone()), "the followed one stays B's");
            assert!(alive, "nothing was ended");
            let _ = c.d;
        });
    }

    /// Regression for #183 §3: a single rename still takes every session
    /// whose shell is inside — at the top of the directory or deeper in it —
    /// each with a record carrying the new directory's identity, which is
    /// what lets a later rename be followed by the record alone.
    ///
    /// Revert-checked, run and restored: with `shell_dir` matching only the
    /// leader's cwd itself rather than its ancestors (`.take(1)`), fails
    /// "a shell in a subdirectory follows too" — left `Here`. With part
    /// (a)'s `record_identity_in` removed, fails "each record carries B's
    /// identity" — left `[Ok(None), Ok(None)]`.
    #[test]
    fn a_single_rename_still_follows_every_session_inside() {
        with_state(|state| {
            let _s = crate::session::SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::env::remove_var("ROOST_CMD");
            let root = tempfile::tempdir().unwrap();
            std::env::set_var("ROOST_ROOTS", root.path());
            let roots = vec![root.path().to_path_buf()];
            let repo = repo_with_worktree_a(root.path(), "singlerepo");
            let a = "singlerepo/.claude/worktrees/worktree-a";
            let b = "singlerepo/.claude/worktrees/worktree-b";
            let wt = repo.join(".claude/worktrees/worktree-a");
            fs::create_dir_all(wt.join("deep/er")).unwrap();
            let sock_dir = state.join("sock").join(crate::projects::storage_key(a));
            fs::create_dir_all(&sock_dir).unwrap();
            let (top, deep) = (sock_dir.join("top"), sock_dir.join("deep"));
            let guard = KillOnDrop(vec![top.clone(), deep.clone()]);
            spawn_in(&top, &wt);
            spawn_in(&deep, &wt.join("deep/er"));
            let _ = reconcile(&roots);

            git_in(&repo, &["worktree", "move", ".claude/worktrees/worktree-a", ".claude/worktrees/worktree-b"]);
            let b_dir = repo.join(".claude/worktrees/worktree-b").canonicalize().unwrap();
            let pass = reconcile(&roots);
            let fresh = pass.moved.iter().map(|m| m.fresh).collect::<Vec<_>>();
            follow_moves(&pass.moved);
            let owners = [crate::home::owner_of(a, "top"), crate::home::owner_of(a, "deep")];
            let ids = [crate::home::identity_of(a, "top"), crate::home::identity_of(a, "deep")];
            let alive = socket_has_process(&top) && socket_has_process(&deep);
            drop(guard);
            std::env::remove_var("ROOST_ROOTS");

            assert_eq!(fresh, vec![true], "setup: one fresh rename");
            assert_eq!(owners[0], crate::home::Owner::Elsewhere(b.into()), "a shell at the top follows");
            assert_eq!(owners[1], crate::home::Owner::Elsewhere(b.into()), "a shell in a subdirectory follows too");
            assert_eq!(ids, [Ok(Some(full_id(&b_dir))), Ok(Some(full_id(&b_dir)))], "each record carries B's identity");
            assert!(alive, "nothing was ended");
        });
    }
}
