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

/// Peers in one project, plus how many records could not be judged.
#[derive(Debug, Default)]
pub struct Roster {
    pub peers: Vec<Session>,
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
) -> Roster {
    let mut out = Roster::default();
    for s in entries {
        if self_pids.contains(&s.pid) {
            continue;
        }
        if let (Some(mine), Some(theirs)) = (self_sid, s.session_id.as_deref()) {
            if mine == theirs {
                continue;
            }
        }
        // A background job is not a second pair of hands on the keyboard.
        if s.kind.as_deref() == Some("bg") {
            continue;
        }
        if normalise(&s.cwd) != here {
            continue;
        }
        match liveness(&s, probe) {
            Liveness::Live => out.peers.push(s),
            Liveness::Unknown => out.uncheckable += 1,
            Liveness::Gone => {}
        }
    }
    out.peers.sort_by_key(|s| s.started_at.unwrap_or(0));
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
    if r.peers.is_empty() && r.uncheckable == 0 {
        return None;
    }
    let mut out = String::new();
    if r.peers.is_empty() {
        out.push_str(&format!(
            "resh: {} session record(s) in {project} could not be checked, \
             so another Claude may be working here unnoticed.",
            r.uncheckable
        ));
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
    out.push_str(
        "\nA resh project is one directory and one branch. Coordinate before editing, \
         or start this work in a git worktree.",
    );
    Some(out)
}
