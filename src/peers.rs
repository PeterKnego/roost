//! Which other Claude sessions are already working in this project.
//!
//! Two Claude sessions in one project edit the same files on the same branch
//! with no idea the other exists — `worktree.rs` records that a worktree is
//! its own project, so "same project" here means one directory and therefore
//! one branch. The collision is silent: each session sees only its own edits
//! until one overwrites the other's.
//!
//! resh does not keep the roster. Claude Code already writes one — a file per
//! live session under `~/.claude/sessions/<pid>.json`, carrying its `cwd` —
//! and each session removes its own on exit. Reading that store instead of
//! building another is what makes this survive a resh restart, a resh crash,
//! and resh never having run: `ide.rs`'s `CONNS` is in-memory and
//! connection-scoped, so a restart empties it and every already-running
//! Claude vanishes from it with no way to rebuild. What resh adds is the
//! *project* — mapping a cwd to a resh project name — which is the part of
//! the question only resh can answer.
//!
//! Three outcomes, not two, exactly as `idecwd.rs` insists for the same
//! kernel question: "I could not determine whether that pid is alive" is not
//! "that pid is dead". Here the cost of collapsing them is only a missed or
//! spurious warning rather than a killed shell, but the direction still
//! matters — an under-warn is the failure this module exists to prevent, so
//! an uncheckable record is *reported*, never silently dropped.
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// One entry of Claude Code's live-session registry.
///
/// Every field but `pid` and `cwd` is optional: this is another program's
/// file format, and a record that has gained or lost a field must degrade to
/// a less precise answer rather than failing to parse. A record that fails to
/// parse is a peer that never gets mentioned.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub pid: i32,
    pub cwd: String,
    #[serde(default)]
    pub session_id: Option<String>,
    /// The kernel's `starttime` for `pid` at the moment the session
    /// registered. Without it a pid is not an identity — pids are reused —
    /// so its absence forces `Unknown` rather than a hopeful `Live`.
    #[serde(default, deserialize_with = "flexible_string")]
    pub proc_start: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub started_at: Option<u64>,
}

/// `procStart` is written as a JSON string today. Accept a number too: a
/// future version writing `42652645` instead of `"42652645"` would otherwise
/// fail the whole record, and a peer that fails to parse is a peer nobody is
/// warned about — the exact silent under-warn this module exists to prevent.
fn flexible_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    use serde_json::Value;
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::String(s)) => Some(s),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    })
}

impl Session {
    pub fn label(&self) -> &str {
        self.name.as_deref().filter(|s| !s.is_empty()).unwrap_or("unnamed")
    }
}

/// What the kernel could tell us about a pid. `Unreadable` is not
/// `NoSuchProcess`: `/proc/<pid>` can exist and refuse to be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Started(String),
    NoSuchProcess,
    Unreadable,
}

/// Whether a registry entry describes a process that is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Live,
    /// Positive evidence: no such process, or the pid was reused by an
    /// unrelated one.
    Gone,
    /// Cannot tell. Counted and reported, never treated as either.
    Unknown,
}

pub fn liveness(s: &Session, probe: &dyn Fn(i32) -> Probe) -> Liveness {
    match probe(s.pid) {
        Probe::NoSuchProcess => Liveness::Gone,
        Probe::Unreadable => Liveness::Unknown,
        Probe::Started(actual) => match s.proc_start.as_deref() {
            // A pid alone proves nothing once pids wrap: the slot may hold a
            // process that has nothing to do with the session that wrote this
            // file. Without a recorded starttime there is no way to tell.
            None => Liveness::Unknown,
            Some(recorded) if recorded == actual => Liveness::Live,
            Some(_) => Liveness::Gone,
        },
    }
}

/// Read `/proc/<pid>/stat` field 22 (`starttime`).
pub fn probe_proc(pid: i32) -> Probe {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Probe::NoSuchProcess,
        Err(_) => Probe::Unreadable,
        Ok(stat) => match starttime_field(&stat) {
            Some(s) => Probe::Started(s),
            // The file was readable but not in the shape we expect. That is
            // "cannot tell", not "not running".
            None => Probe::Unreadable,
        },
    }
}

/// `comm` is field 2, is wrapped in parentheses, and may itself contain both
/// spaces and `)` — a process can name itself `foo) bar (baz`. Splitting the
/// whole line on whitespace therefore misaligns every later field. Everything
/// after the LAST `)` is fixed-width, so that is where parsing starts;
/// `starttime` is field 22, which is index 19 of the remainder.
pub fn starttime_field(stat: &str) -> Option<String> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(19).map(str::to_string)
}

/// Which repository a directory belongs to, as git sees it.
///
/// Every worktree of one repository shares a *common* git dir — the main
/// checkout's `.git` — so two directories with the same common dir are two
/// views of one repository. That is the whole test; nothing here needs to know
/// what a worktree looks like on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Repo {
    At(PathBuf),
    /// git could not tell us. Never read as "a different repository".
    Unknown,
}

/// Ask git, rather than looking for a `.git` entry.
///
/// `worktree.rs` established the rule for the same reason it applies here: a
/// session's cwd may be a subdirectory rather than a checkout root, and a
/// worktree can live anywhere — a path convention would answer confidently and
/// wrongly. `run` is injected so the parsing is testable without a repository.
pub fn git_common_dir(cwd: &Path, run: &dyn Fn(&Path) -> Option<String>) -> Repo {
    // Three outcomes, and this is the one that needs saying: the runner
    // returns None for "did not run, or ran and I cannot trust the output"
    // (non-zero exit, or empty stdout where a live repository must produce
    // some). Folding that into "not the same repository" would silently drop
    // the warning rather than mis-state it, which is the quieter half of the
    // same mistake.
    let Some(out) = run(cwd) else { return Repo::Unknown };
    let out = out.trim();
    if out.is_empty() {
        return Repo::Unknown;
    }
    // `--git-common-dir` answers relatively when cwd is the checkout root
    // (".git"), so it is only comparable once resolved against that cwd.
    let path = PathBuf::from(out);
    let abs = if path.is_absolute() { path } else { cwd.join(path) };
    match abs.canonicalize() {
        Ok(p) => Repo::At(p),
        Err(_) => Repo::Unknown,
    }
}

/// Run `git rev-parse --git-common-dir` in `cwd`.
pub fn run_git_common_dir(cwd: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .ok()?;
    // `status.success()` is not optional here: `git rev-parse` outside a
    // repository exits non-zero and still prints to stderr, and a caller that
    // only read stdout would treat the empty result as an answer.
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// Peers in one project, plus how many records could not be judged.
#[derive(Debug, Default)]
pub struct Roster {
    /// Same directory: they will overwrite each other's files.
    pub peers: Vec<Session>,
    /// Same repository, a different worktree. They cannot collide over files
    /// — git will not check one branch out twice — but they share `.git` and
    /// whatever build output the repo's tooling shares.
    pub siblings: Vec<Session>,
    /// Same-directory records that could not be judged. Deliberately counts
    /// only those: a missed peer means overwritten work, while a missed
    /// sibling means a missed advisory, and reporting uncertainty about the
    /// quieter case would cost more noise than it buys.
    pub uncheckable: usize,
}

/// A path as it should be compared: canonical where that is possible.
///
/// Falls back to the path as written rather than dropping the record — a
/// session whose directory has since been renamed is still a session, and
/// dropping it here would be an under-warn.
pub fn normalise(p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    path.canonicalize().unwrap_or(path)
}

/// Select the sessions that share `here`, excluding this one.
///
/// `self_pids` is the caller's own process ancestry rather than a single pid:
/// the hook runs as a descendant of the Claude process it belongs to, so the
/// session's own record is always in that set even when nothing on stdin
/// identifies it.
pub fn roster(
    entries: Vec<Session>,
    here: &Path,
    self_pids: &[i32],
    self_sid: Option<&str>,
    probe: &dyn Fn(i32) -> Probe,
    repo: &dyn Fn(&Path) -> Repo,
) -> Roster {
    let mut out = Roster::default();
    // Resolving a repository costs a subprocess, so it is spent as late as
    // possible: once per distinct directory rather than once per session
    // (sessions commonly share a cwd), and not at all until some candidate is
    // somewhere other than here. A session starting alone — the ordinary case,
    // on every project on the host — therefore runs no git at all, and one
    // that only has peers in its own directory runs none either.
    let mut resolved: std::collections::HashMap<PathBuf, Repo> =
        std::collections::HashMap::new();
    let mut here_repo: Option<Repo> = None;
    for s in entries {
        let is_self = match (self_sid, s.session_id.as_deref()) {
            // Both sides named an id, so the match is exact and nothing else
            // is us. Ancestry must NOT also be consulted here: it excludes
            // every Claude this process descends from, and a session that
            // merely happened to spawn us is still a separate session editing
            // the same files. Hiding it is exactly the under-warn this module
            // exists to prevent — found by launching a session from inside
            // another one's shell and watching the parent vanish from its own
            // warning.
            (Some(mine), Some(theirs)) => mine == theirs,
            // One side is silent — no payload on stdin, or a record predating
            // the field. Fall back to process ancestry, which is a fact about
            // this process rather than about another program's payload shape.
            _ => self_pids.contains(&s.pid),
        };
        if is_self {
            continue;
        }
        // A background job is not a second pair of hands on the keyboard.
        if s.kind.as_deref() == Some("bg") {
            continue;
        }
        let their_cwd = normalise(&s.cwd);
        let same_dir = their_cwd == here;
        // A sibling only when git says both directories share one common dir.
        // `Repo::Unknown` on either side is not a match and not a mismatch —
        // it simply yields no line, which is the direction this failure should
        // fall for an advisory.
        let same_repo = if same_dir {
            false
        } else {
            let ours = here_repo.get_or_insert_with(|| repo(here)).clone();
            let theirs =
                resolved.entry(their_cwd.clone()).or_insert_with(|| repo(&their_cwd)).clone();
            matches!((ours, theirs), (Repo::At(a), Repo::At(b)) if a == b)
        };
        if !same_dir && !same_repo {
            continue;
        }
        match (liveness(&s, probe), same_dir) {
            (Liveness::Live, true) => out.peers.push(s),
            (Liveness::Live, false) => out.siblings.push(s),
            (Liveness::Unknown, true) => out.uncheckable += 1,
            _ => {}
        }
    }
    out.peers.sort_by_key(|s| s.started_at.unwrap_or(0));
    out.siblings.sort_by_key(|s| s.started_at.unwrap_or(0));
    out
}

/// Every `*.json` in `dir` that parses as a session record.
///
/// A file that does not parse is skipped, not fatal: the registry is written
/// by other processes, so reading one mid-write is expected rather than
/// exceptional.
pub fn read_dir(dir: &Path) -> Vec<Session> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&p) {
            if let Ok(s) = serde_json::from_str::<Session>(&text) {
                out.push(s);
            }
        }
    }
    out
}

/// The resh project name for an absolute path, mirroring `resolve_project`'s
/// forward rule (a project is a canonical path under a canonical root).
/// `None` means the path is not inside any configured root.
pub fn project_name(roots: &[PathBuf], here: &Path) -> Option<String> {
    for root in roots {
        let Ok(base) = root.canonicalize() else { continue };
        if let Ok(rel) = here.strip_prefix(&base) {
            let name = rel.to_string_lossy().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

fn ago(started_ms: u64, now_ms: u64) -> String {
    let mins = now_ms.saturating_sub(started_ms) / 60_000;
    match mins {
        0 => "just now".to_string(),
        1..=59 => format!("{mins}m ago"),
        60..=1439 => format!("{}h ago", mins / 60),
        _ => format!("{}d ago", mins / 1440),
    }
}

/// The warning, or `None` when there is nothing to say.
///
/// `uncheckable` alone still produces a message: it means the detection ran
/// degraded, and reporting "I could not tell" is the whole point of keeping
/// it as a third outcome rather than folding it into "no peers".
pub fn message(project: &str, r: &Roster, now_ms: u64) -> Option<String> {
    if r.peers.is_empty() && r.siblings.is_empty() && r.uncheckable == 0 {
        return None;
    }
    let mut out = String::new();
    if r.peers.is_empty() && r.uncheckable > 0 {
        out.push_str(&format!(
            "resh: {} session record(s) in {project} could not be checked, \
             so another Claude may be working here unnoticed.",
            r.uncheckable
        ));
        if r.siblings.is_empty() {
            return Some(out);
        }
        out.push_str(&siblings_block(r));
        return Some(out);
    }
    if r.peers.is_empty() {
        // Nobody in this directory, but the repository is shared. The loud
        // section would be a lie here, so the quiet one stands alone.
        out.push_str(&format!("No other Claude session is in {project}."));
        out.push_str(&siblings_block(r));
        return Some(out);
    }
    let n = r.peers.len();
    out.push_str(&format!(
        "{n} other Claude session{} already working in {project}:",
        if n == 1 { "" } else { "s" }
    ));
    for p in &r.peers {
        out.push_str(&format!(
            "\n  - {} (pid {}, {}",
            p.label(),
            p.pid,
            p.status.as_deref().unwrap_or("unknown state"),
        ));
        match p.started_at {
            Some(t) => out.push_str(&format!(", started {})", ago(t, now_ms))),
            None => out.push(')'),
        }
    }
    if r.uncheckable > 0 {
        out.push_str(&format!(
            "\n  ({} further record(s) could not be checked)",
            r.uncheckable
        ));
    }
    // Two leading spaces, like the bullets above. The terminal renders these
    // newlines faithfully — this is for the copies. A warning like this gets
    // pasted into chat, logs and issue threads, and those flatten newlines
    // routinely; a segment whose only separator is its `\n` then welds onto
    // the end of the previous one ("started 3h ago)A resh project is..."),
    // which is exactly how this was found. Every line after the first carries
    // its own leading whitespace, so no copy of the message can join two
    // words. It also reads better rendered: the closing note sits indented
    // under the peers rather than flush against the left margin.
    out.push_str(
        "\n  A resh project is one directory and one branch. Coordinate before editing, \
         or start this work in a git worktree.",
    );
    // Stated as a capability, not an instruction. Told to notify its peers, a
    // starting session would message every one of them unprompted, and a
    // message wakes the receiver mid-task — turning a warning meant to prevent
    // disruption into a source of it. Whether the interruption is worth it is
    // a judgement about what the peer is doing, which only the reader has.
    //
    // The name is already the address: `ListAgents` describes a session's name
    // as "the name other sessions use to message it", and it is the same
    // string the registry stores, so the lines above need nothing added.
    out.push_str("\n  Each name above is a SendMessage address, if you want to coordinate directly.");
    out.push_str(&siblings_block(r));
    Some(out)
}

/// This process and every ancestor up to init.
///
/// The hook runs as a descendant of the Claude process whose session it
/// belongs to, so that session's own registry record is always in this set.
///
/// This is the *fallback* route, used only when no session id is available on
/// either side. `SessionStart` does carry one, and an exact id is strictly
/// better: ancestry excludes every Claude this process descends from, not just
/// us, so preferring it would hide a separate session that merely spawned us.
/// The walk survives because a caller that pipes no payload still needs to
/// avoid reporting itself, and because a payload shape belongs to another
/// program and may change. `parent` is injected so the walk itself is testable
/// without a process tree to stand in.
///
/// Bounded at 64 hops: a `/proc` that reports a cycle must not hang a hook
/// that runs before every session.
pub fn ancestry(start: i32, parent: &dyn Fn(i32) -> Option<i32>) -> Vec<i32> {
    let mut out = Vec::new();
    let mut pid = start;
    for _ in 0..64 {
        if pid <= 1 || out.contains(&pid) {
            break;
        }
        out.push(pid);
        match parent(pid) {
            Some(p) => pid = p,
            None => break,
        }
    }
    out
}

/// `ppid` is field 4 of `/proc/<pid>/stat` — index 1 after the last `)`, for
/// the same reason `starttime_field` starts counting there.
pub fn ppid_of(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}


/// The same-repository section. Separate from the peers above because the
/// hazard is different: these cannot touch your files — git will not check one
/// branch out twice — but they share the repository around them.
///
/// The advice names no specific tool. Which build directory a repository's
/// tooling shares is a property of the machine, not of resh, so stating it
/// concretely here would be true of one host and wrong elsewhere.
fn siblings_block(r: &Roster) -> String {
    if r.siblings.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n  Also in this repository, in other worktrees:");
    for s in &r.siblings {
        out.push_str(&format!("\n    - {} ({}) in {}", s.label(), s.status.as_deref().unwrap_or("unknown state"), s.cwd));
    }
    out.push_str(
        "\n    They cannot collide over your files, but they share .git and whatever \
         build output this repository's tooling shares.",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess(pid: i32, cwd: &str) -> Session {
        Session {
            pid,
            cwd: cwd.to_string(),
            session_id: Some(format!("sid-{pid}")),
            proc_start: Some("1000".to_string()),
            kind: Some("interactive".to_string()),
            name: Some(format!("peer-{pid}")),
            status: Some("busy".to_string()),
            started_at: Some(0),
        }
    }

    /// Every pid is running and started when it says it did.
    fn all_live(_: i32) -> Probe {
        Probe::Started("1000".to_string())
    }

    /// git can tell us nothing, so no directory is a sibling of any other.
    /// The default for every test that predates worktree detection, so their
    /// assertions still mean exactly what they meant.
    fn no_repo(_: &Path) -> Repo {
        Repo::Unknown
    }

    #[test]
    fn a_live_peer_in_this_directory_is_named() {
        let r = roster(vec![sess(2, "/w")], Path::new("/w"), &[], None, &all_live, &no_repo);
        assert_eq!(r.peers.len(), 1);
        assert_eq!(r.peers[0].label(), "peer-2");
        assert_eq!(r.uncheckable, 0);
    }

    /// The registry outlives the process it describes: a session killed with
    /// SIGKILL never removes its own file. Believing a stale record would
    /// warn about a session that no longer exists, every single startup.
    #[test]
    fn a_record_whose_process_is_gone_is_not_a_peer() {
        let r = roster(
            vec![sess(2, "/w")],
            Path::new("/w"),
            &[],
            None,
            &|_| Probe::NoSuchProcess,
            &no_repo,
        );
        assert!(r.peers.is_empty(), "a dead pid must not be reported as a peer");
        assert_eq!(r.uncheckable, 0, "'not running' is positive evidence, not uncertainty");
    }

    /// The case a bare `/proc/<pid>` existence check gets wrong. Pids wrap;
    /// the slot can hold an unrelated process. `procStart` is what makes the
    /// pid an identity rather than a guess.
    #[test]
    fn a_reused_pid_is_not_the_session_that_registered_it() {
        let r = roster(
            vec![sess(2, "/w")],
            Path::new("/w"),
            &[],
            None,
            // Alive, but started at a different moment than the record claims.
            &|_| Probe::Started("999999".to_string()),
            &no_repo,
        );
        assert!(r.peers.is_empty(), "a reused pid must not be reported as its old owner");
        assert_eq!(r.uncheckable, 0, "a starttime mismatch is positive evidence of Gone");
    }

    /// The third outcome. `/proc/<pid>` can exist and refuse to be read; a
    /// record can predate the `procStart` field. Neither is "not running",
    /// and neither may be silently dropped — a swallowed uncertainty is an
    /// under-warn, which is the failure this module exists to prevent.
    #[test]
    fn what_cannot_be_judged_is_counted_rather_than_assumed_either_way() {
        let unreadable = roster(
            vec![sess(2, "/w")],
            Path::new("/w"),
            &[],
            None,
            &|_| Probe::Unreadable,
            &no_repo,
        );
        assert!(unreadable.peers.is_empty());
        assert_eq!(unreadable.uncheckable, 1, "an unreadable /proc must be reported, not dropped");

        let mut no_start = sess(3, "/w");
        no_start.proc_start = None;
        let old = roster(vec![no_start], Path::new("/w"), &[], None, &all_live, &no_repo);
        assert!(old.peers.is_empty(), "a pid with no recorded starttime proves nothing");
        assert_eq!(old.uncheckable, 1);
    }

    /// The pair matters, not either half: asserting only that a `bg` record
    /// produces no peer is also true of a roster that found nothing at all.
    /// The second half proves the record was otherwise visible, so the first
    /// half can only pass by filtering.
    #[test]
    fn a_background_job_is_filtered_and_the_same_record_otherwise_is_not() {
        let mut bg = sess(2, "/w");
        bg.kind = Some("bg".to_string());
        let hidden = roster(vec![bg.clone()], Path::new("/w"), &[], None, &all_live, &no_repo);
        assert!(hidden.peers.is_empty(), "a bg job is not a second pair of hands");
        assert_eq!(hidden.uncheckable, 0, "filtering must happen before the liveness verdict");

        let mut interactive = bg;
        interactive.kind = Some("interactive".to_string());
        let shown = roster(vec![interactive], Path::new("/w"), &[], None, &all_live, &no_repo);
        assert_eq!(shown.peers.len(), 1, "the very same record, interactive, must appear");
    }

    #[test]
    fn a_session_in_another_directory_is_another_project() {
        let r = roster(vec![sess(2, "/elsewhere")], Path::new("/w"), &[], None, &all_live, &no_repo);
        assert!(r.peers.is_empty());
    }

    /// Self-exclusion has two independent routes because neither is always
    /// available: stdin may carry no session id, and the ancestry walk may be
    /// defeated by an exec chain. Each is asserted on its own so one silently
    /// breaking cannot be masked by the other.
    #[test]
    fn this_session_is_not_its_own_peer_by_either_route() {
        let by_pid = roster(vec![sess(2, "/w")], Path::new("/w"), &[2], None, &all_live, &no_repo);
        assert!(by_pid.peers.is_empty(), "own pid, found via process ancestry");

        let by_sid = roster(vec![sess(2, "/w")], Path::new("/w"), &[], Some("sid-2"), &all_live, &no_repo);
        assert!(by_sid.peers.is_empty(), "own session id, found on stdin");

        let other = roster(vec![sess(2, "/w")], Path::new("/w"), &[99], Some("sid-99"), &all_live, &no_repo);
        assert_eq!(other.peers.len(), 1, "a different session must still be reported");
    }

    /// `comm` sits in parentheses and may contain spaces and `)` — a process
    /// can name itself anything. Splitting the whole line on whitespace
    /// misaligns every later field, which would silently compare the wrong
    /// number against `procStart` and call every live peer Gone.
    #[test]
    fn a_hostile_process_name_cannot_shift_the_starttime_field() {
        let mut fields: Vec<String> = (3..=52).map(|i| i.to_string()).collect();
        fields[19] = "424242".to_string(); // field 22 overall
        let evil = format!("7 (evil) name (with parens) {}", fields.join(" "));
        assert_eq!(
            starttime_field(&evil).as_deref(),
            Some("424242"),
            "parsing must start after the LAST ')', not the first"
        );
        // And against the real kernel, so the field index itself is pinned.
        let mine = std::fs::read_to_string("/proc/self/stat").unwrap();
        let got = starttime_field(&mine).expect("our own stat line must parse");
        assert!(got.parse::<u64>().is_ok(), "starttime must be numeric, got {got:?}");
    }

    /// A record that fails to parse is a peer nobody is warned about, so the
    /// tolerated shapes are pinned rather than left to chance.
    #[test]
    fn a_proc_start_written_as_a_number_parses_the_same_as_a_string() {
        let as_str: Session =
            serde_json::from_str(r#"{"pid":1,"cwd":"/w","procStart":"77"}"#).unwrap();
        let as_num: Session =
            serde_json::from_str(r#"{"pid":1,"cwd":"/w","procStart":77}"#).unwrap();
        assert_eq!(as_str.proc_start.as_deref(), Some("77"));
        assert_eq!(as_num.proc_start.as_deref(), Some("77"));
        // And a record carrying only the two required fields still parses.
        let minimal: Session = serde_json::from_str(r#"{"pid":1,"cwd":"/w"}"#).unwrap();
        assert_eq!(minimal.proc_start, None);
        assert_eq!(minimal.label(), "unnamed");
    }

    /// The registry is written by other processes, so reading one mid-write
    /// is expected. A torn file must cost its own record and nothing else.
    #[test]
    fn a_half_written_registry_file_costs_only_its_own_record() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("good.json"), r#"{"pid":5,"cwd":"/w","name":"real"}"#).unwrap();
        std::fs::write(d.path().join("torn.json"), r#"{"pid":6,"cwd":"#).unwrap();
        std::fs::write(d.path().join("notjson.txt"), "ignored").unwrap();
        let got = read_dir(d.path());
        assert_eq!(got.len(), 1, "the good record must survive its torn neighbour");
        assert_eq!(got[0].label(), "real");
    }

    #[test]
    fn silence_when_alone_and_a_message_that_names_who_is_here() {
        assert!(message("proj", &Roster::default(), 0).is_none(), "no peers, nothing to say");

        let r = roster(vec![sess(2, "/w")], Path::new("/w"), &[], None, &all_live, &no_repo);
        let m = message("resh", &r, 60_000).expect("a peer must produce a message");
        assert!(m.contains("peer-2"), "the peer must be named: {m}");
        assert!(m.contains("resh"), "the project must be named: {m}");
        assert!(m.contains("1m ago"), "age is rendered from the caller's clock: {m}");
        assert!(m.starts_with("1 other Claude session already"), "singular, not '1 sessions': {m}");
    }

    /// Degraded detection is itself worth saying. Reporting nothing here
    /// would be indistinguishable from "you are alone", which is the one
    /// thing this module must never assert without evidence.
    #[test]
    fn uncertainty_alone_still_produces_a_message() {
        let r = Roster { peers: Vec::new(), siblings: Vec::new(), uncheckable: 2 };
        let m = message("resh", &r, 0).expect("uncertainty must not be silent");
        assert!(m.contains("could not be checked"), "{m}");
    }

    #[test]
    fn a_project_name_is_the_path_below_its_root() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("karpie/src")).unwrap();
        let roots = vec![root.clone()];
        assert_eq!(
            project_name(&roots, &root.join("karpie/src")).as_deref(),
            Some("karpie/src"),
            "a nested project keeps its rel path, the way resolve_project resolves it"
        );
        assert_eq!(project_name(&roots, &root), None, "a root itself is not a project");
        assert_eq!(project_name(&roots, Path::new("/outside")), None);
    }

    /// A `/proc` that reports a cycle (or a walk that revisits a pid) must not
    /// hang a hook that runs before every session starts.
    #[test]
    fn the_ancestry_walk_terminates_on_a_cycle_and_at_init() {
        let normal = ancestry(10, &|p| match p {
            10 => Some(5),
            5 => Some(1),
            _ => None,
        });
        assert_eq!(normal, vec![10, 5], "init itself is not an ancestor worth listing");

        let cyclic = ancestry(10, &|p| Some(if p == 10 { 11 } else { 10 }));
        assert_eq!(cyclic, vec![10, 11], "a cycle must stop the walk, not spin it");

        // Against the real kernel: our own parent must actually be found.
        let real = ancestry(std::process::id() as i32, &|p| ppid_of(p));
        assert!(real.len() >= 2, "we have at least one ancestor, got {real:?}");
        assert_eq!(real[0], std::process::id() as i32);
    }

    /// A `\n` must not be a segment's only separator. The terminal renders
    /// the newlines fine, but copies of the message — pasted into chat, a log,
    /// an issue — routinely lose them, and the closing sentence then welded
    /// onto the last peer ("started 3h ago)A resh project is..."). That paste
    /// is how this was found, and no test could have found it: every test
    /// here asserts on the multi-line string, where the `\n` genuinely is a
    /// separator.
    ///
    /// The invariant is general rather than a spot check on that one
    /// sentence, so a line added later without an indent fails too.
    #[test]
    fn every_line_survives_having_its_newline_stripped() {
        let r = roster(
            vec![sess(2, "/w"), sess(3, "/w")],
            Path::new("/w"),
            &[],
            None,
            &all_live,
            &no_repo,
        );
        let m = message("resh", &r, 60_000).expect("two peers must produce a message");
        for line in m.split('\n').skip(1) {
            assert!(
                line.starts_with(' '),
                "every continuation line must lead with a space so it survives flattening: {line:?}"
            );
        }
        let flat = m.replace('\n', "");
        assert!(
            !flat.contains(")A") && !flat.contains("wA"),
            "flattened, nothing may weld onto the previous segment: {flat}"
        );
    }

    /// The names resh prints are `SendMessage` addresses, which is the only
    /// thing that lets the arriving session close the asymmetry — it learns
    /// about the sessions already here, and they learn nothing about it.
    ///
    /// Absent when there is nobody to address: the uncertainty-only message
    /// names no peers, so offering a way to reach them would be nonsense.
    #[test]
    fn the_warning_says_the_names_are_addresses_but_only_when_it_names_someone() {
        let r = roster(vec![sess(2, "/w")], Path::new("/w"), &[], None, &all_live, &no_repo);
        let named = message("resh", &r, 0).expect("a peer must produce a message");
        assert!(named.contains("SendMessage"), "the address hint must be present: {named}");

        let uncertain = Roster { peers: Vec::new(), siblings: Vec::new(), uncheckable: 2 };
        let vague = message("resh", &uncertain, 0).expect("uncertainty must not be silent");
        assert!(
            !vague.contains("SendMessage"),
            "with no peer named there is no address to offer: {vague}"
        );
    }

    /// The false negative that a real session start exposed: a Claude launched
    /// from inside another Claude's shell inherits that parent's pid in its
    /// ancestry, so an ancestry-based exclusion hid the parent from the very
    /// warning meant to name it. They edit the same files regardless of who
    /// launched whom.
    ///
    /// `SessionStart` carries a `session_id`, so when both sides name one the
    /// match is exact and ancestry is not consulted at all.
    #[test]
    fn a_session_that_spawned_us_is_still_a_peer_when_we_know_our_own_id() {
        // pid 2 is in our ancestry — it launched us — but it is not us.
        let spawner = roster(
            vec![sess(2, "/w")],
            Path::new("/w"),
            &[2],
            Some("sid-99"),
            &all_live,
            &no_repo,
        );
        assert_eq!(
            spawner.peers.len(),
            1,
            "a session we descend from is a separate session and must be named"
        );

        // And we are still not our own peer: same ancestry, our own id.
        let ourselves = roster(
            vec![sess(2, "/w")],
            Path::new("/w"),
            &[2],
            Some("sid-2"),
            &all_live,
            &no_repo,
        );
        assert!(ourselves.peers.is_empty(), "an exact id match is still us");
    }

    /// The fallback has to survive: a record written before `sessionId`
    /// existed, or a caller that pipes no payload, still must not report the
    /// running session as its own peer.
    #[test]
    fn ancestry_still_excludes_self_when_either_side_names_no_id() {
        let mut anon = sess(2, "/w");
        anon.session_id = None;
        let known_id = roster(
            vec![anon.clone()],
            Path::new("/w"),
            &[2],
            Some("sid-99"),
            &all_live,
            &no_repo,
        );
        assert!(known_id.peers.is_empty(), "a record with no id falls back to ancestry");

        let no_payload = roster(vec![anon], Path::new("/w"), &[2], None, &all_live, &no_repo);
        assert!(no_payload.peers.is_empty(), "no payload at all falls back to ancestry");
    }

    fn repo_a() -> Repo { Repo::At(PathBuf::from("/repo/.git")) }

    /// The collision this exists for: a session in another worktree of the
    /// same repository. It cannot touch your files — git will not check one
    /// branch out twice — but it shares .git and whatever build output the
    /// repo's tooling shares, which on one host meant a build from a sibling
    /// worktree leaving the shared binary built from the other tree.
    #[test]
    fn a_session_in_another_worktree_of_this_repo_is_a_sibling_not_a_peer() {
        let r = roster(
            vec![sess(2, "/repo/wt")],
            Path::new("/repo"),
            &[],
            None,
            &all_live,
            &|_| repo_a(),
        );
        assert!(r.peers.is_empty(), "a different directory is never a peer");
        assert_eq!(r.siblings.len(), 1, "but the same repository makes it a sibling");
        assert_eq!(r.siblings[0].label(), "peer-2");
    }

    /// Same directory always wins: a session here is a peer, never demoted to
    /// the quieter section just because it also shares the repository.
    #[test]
    fn a_session_in_this_very_directory_is_a_peer_even_though_the_repo_matches() {
        let r = roster(
            vec![sess(2, "/repo")],
            Path::new("/repo"),
            &[],
            None,
            &all_live,
            &|_| repo_a(),
        );
        assert_eq!(r.peers.len(), 1, "same directory is the loud case");
        assert!(r.siblings.is_empty(), "and must not be counted twice");
    }

    /// A different repository is not a sibling, and — the part that matters —
    /// neither is one git could not resolve. "I cannot tell" must not become
    /// "same repository": that would invent a warning rather than miss one.
    #[test]
    fn a_different_repo_and_an_unresolvable_one_both_yield_no_sibling() {
        let different = roster(
            vec![sess(2, "/other/wt")],
            Path::new("/repo"),
            &[],
            None,
            &all_live,
            &|p| {
                if p.starts_with("/repo") { repo_a() } else { Repo::At(PathBuf::from("/other/.git")) }
            },
        );
        assert!(different.siblings.is_empty(), "a different repository is not a sibling");

        let theirs_unknown = roster(
            vec![sess(2, "/repo/wt")],
            Path::new("/repo"),
            &[],
            None,
            &all_live,
            &|p| if p == Path::new("/repo") { repo_a() } else { Repo::Unknown },
        );
        assert!(theirs_unknown.siblings.is_empty(), "their repo unresolvable: claim nothing");

        let ours_unknown = roster(
            vec![sess(2, "/repo/wt")],
            Path::new("/repo"),
            &[],
            None,
            &all_live,
            &|p| if p == Path::new("/repo") { Repo::Unknown } else { repo_a() },
        );
        assert!(ours_unknown.siblings.is_empty(), "our own repo unresolvable: claim nothing");
    }

    /// A subprocess has three results, not two. `git rev-parse` outside a
    /// repository exits non-zero while still writing to stderr, so a caller
    /// reading only stdout would take the empty result for an answer.
    #[test]
    fn git_that_failed_or_said_nothing_is_unknown_not_a_different_repo() {
        assert_eq!(git_common_dir(Path::new("/x"), &|_| None), Repo::Unknown, "did not run / non-zero exit");
        assert_eq!(git_common_dir(Path::new("/x"), &|_| Some(String::new())), Repo::Unknown, "empty stdout");
        assert_eq!(git_common_dir(Path::new("/x"), &|_| Some("   \n".into())), Repo::Unknown, "whitespace only");
    }

    /// `--git-common-dir` answers relatively when cwd is the checkout root, so
    /// two directories under one repo would otherwise both report ".git" and
    /// compare equal to every other repository's root.
    #[test]
    fn a_relative_git_dir_is_resolved_against_the_directory_it_came_from() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        assert_eq!(
            git_common_dir(&root, &|_| Some(".git".into())),
            Repo::At(root.join(".git")),
            "a bare \".git\" must resolve against the cwd that produced it"
        );
        assert_eq!(
            git_common_dir(&root, &|_| Some(root.join(".git").to_string_lossy().into())),
            Repo::At(root.join(".git")),
            "an absolute answer passes through"
        );
    }

    #[test]
    fn the_message_carries_the_quieter_worktree_section() {
        let mut sib = sess(3, "/repo/wt");
        sib.name = Some("sibling-one".into());
        let r = roster(
            vec![sess(2, "/repo"), sib],
            Path::new("/repo"),
            &[],
            None,
            &all_live,
            &|_| repo_a(),
        );
        let m = message("resh", &r, 0).expect("peers and siblings must produce a message");
        assert!(m.contains("peer-2"), "the same-directory peer is named: {m}");
        assert!(m.contains("Also in this repository"), "the quieter section is present: {m}");
        assert!(m.contains("sibling-one"), "the sibling is named: {m}");
        assert!(m.contains("/repo/wt"), "and located: {m}");
        // The advice must not name a specific build tool: which directory a
        // repo's tooling shares is a property of the machine, not of resh.
        assert!(!m.to_lowercase().contains("cargo"), "no host-specific tool may be asserted: {m}");
        for line in m.split('\n').skip(1) {
            assert!(line.starts_with(' '), "flattening invariant still holds: {line:?}");
        }
    }

    /// Nobody in this directory but the repository is shared: the loud opening
    /// would be a lie, so the quiet section stands on its own.
    #[test]
    fn siblings_alone_produce_a_message_without_claiming_a_peer_is_here() {
        let r = roster(
            vec![sess(3, "/repo/wt")],
            Path::new("/repo"),
            &[],
            None,
            &all_live,
            &|_| repo_a(),
        );
        assert!(r.peers.is_empty());
        let m = message("resh", &r, 0).expect("a sibling alone is still worth saying");
        assert!(!m.contains("already working in resh:"), "must not claim a peer is here: {m}");
        assert!(m.contains("No other Claude session is in resh"), "{m}");
        assert!(m.contains("Also in this repository"), "{m}");
    }

    /// Resolving a repository costs a subprocess, so the code claims two
    /// things about when it spends one: never for a session already in this
    /// directory, and once per distinct directory rather than once per
    /// session. Both are claims in a comment, which is worth exactly nothing
    /// unless something fails when they stop being true.
    ///
    /// Revert-checked: dropping the `!same_dir &&` guard makes the first
    /// assertion fail. Note it does NOT move anyone between buckets — the
    /// bucket is chosen by `same_dir` alone — so this is the only test that
    /// can catch that edit at all.
    #[test]
    fn a_repository_is_resolved_once_per_directory_and_never_for_this_one() {
        use std::cell::RefCell;
        let seen: RefCell<Vec<PathBuf>> = RefCell::new(Vec::new());
        let count = |entries: Vec<Session>| {
            seen.borrow_mut().clear();
            let r = roster(
                entries,
                Path::new("/repo"),
                &[],
                None,
                &all_live,
                &|p| {
                    seen.borrow_mut().push(p.to_path_buf());
                    repo_a()
                },
            );
            (r, seen.borrow().clone())
        };

        // Alone, and peers-in-this-directory-only: no git at all. This is the
        // ordinary case on every project on the host, so it must stay free.
        let (_, calls) = count(Vec::new());
        assert!(calls.is_empty(), "a session with no candidates must run no git: {calls:?}");
        let (r, calls) = count(vec![sess(2, "/repo"), sess(3, "/repo")]);
        assert_eq!(r.peers.len(), 2);
        assert!(calls.is_empty(), "peers in this very directory need no repository: {calls:?}");

        // One elsewhere: our own directory resolved once, theirs once, and a
        // second session sharing their directory reuses it.
        let (r, calls) = count(vec![sess(4, "/repo/wt"), sess(5, "/repo/wt")]);
        assert_eq!(r.siblings.len(), 2);
        assert_eq!(
            calls.iter().filter(|p| *p == Path::new("/repo/wt")).count(),
            1,
            "two sessions sharing a directory must share one resolution: {calls:?}"
        );
        assert_eq!(calls.len(), 2, "ours and theirs, nothing more: {calls:?}");
    }
}
