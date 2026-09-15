//! One project's roost state and its conversations, in one file.
//!
//! #18 step 3. Steps 1 and 2 gave a terminal a way back to its own
//! conversation after a reboot; neither survives losing the machine. This is
//! the container that does, and the module doc is mostly about one property.
//!
//! ## The archive contains no paths
//!
//! Every entry is a typed record with an *id* — a session name, a Claude
//! session id, a memory file's name — and never a filesystem location. Restore
//! derives each destination from the project it is restoring *into*, by the
//! same `claudehist::transcript_dir_in` the ✻ menu uses.
//!
//! That single decision answers two otherwise separate problems.
//!
//! #18's third difficulty was that "the transcript path encodes an absolute
//! cwd … Restore the same project at a different path on another server and
//! Claude will not find its own transcripts". An archive holding
//! `/home/claude/projects/roost/…` would have to rewrite it on the way in.
//! This one never recorded it, so there is nothing to rewrite: restoring at a
//! different path is not a special case, it is the only case.
//!
//! And it removes tar's extraction hazard by construction rather than by
//! vigilance. There is no entry a hostile archive could use to write outside
//! the destination, because there is no entry that names a destination.
//! `--no-absolute-names`, `../` stripping and symlink-entry handling are all
//! absent here because the class they defend against cannot be expressed.
//!
//! ## `parse` refuses; it never repairs
//!
//! A declared length that runs past the end of the buffer fails the archive —
//! it does not truncate the entry to what is there. An unknown kind fails — it
//! is not skipped, because a reader that skips what it does not understand
//! restores a subset and reports a restore. An id that fails validation is
//! refused, never sanitised into an acceptable one, because a sanitised id is
//! quietly a *different* id and the file it names is somebody else's.
//!
//! ## The format
//!
//! Hand-rolled, like this project's HTTP and its websocket framing. Line-
//! oriented JSON metadata with length-prefixed raw payloads, so it can be
//! recovered in twenty lines of Python by something that is not roost:
//!
//! ```text
//! ROOSTBAK1\n
//! {"project":"roost","created":1757750000,…}\n
//! {"kind":"transcript","id":"864ee734-…","at":1757600000,"bytes":41}\n
//! <41 bytes>\n
//! ```
//!
//! Uncompressed on purpose. gzip -6 on this host's largest roost transcript
//! managed 3.2×, which is real and still not worth putting `flate2` in every
//! shipped binary for something `gzip` does to the finished file — and the
//! point of this container is to be legible in five years.

use std::io::Write;

pub const MAGIC: &str = "ROOSTBAK1";

/// What one entry holds. The `id` means something different per kind and is
/// validated by a different rule per kind — see `Kind::valid_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The workspace layout, `wsstate`'s `<key>.json`. One per archive, no id.
    Workspace,
    /// `claudesess`'s record for one terminal. Id: the session name.
    Session,
    /// `cwds`'s marker for one terminal. Id: the session name.
    Cwd,
    /// `relaunch`'s marker for one terminal. Id: the session name.
    Launch,
    /// One conversation. Id: Claude Code's session id.
    Transcript,
    /// One file from the project's `memory/` directory. Id: its file name.
    Memory,
}

impl Kind {
    pub fn wire(self) -> &'static str {
        match self {
            Kind::Workspace => "workspace",
            Kind::Session => "session",
            Kind::Cwd => "cwd",
            Kind::Launch => "launch",
            Kind::Transcript => "transcript",
            Kind::Memory => "memory",
        }
    }

    /// An unknown wire name is `None`, and every caller turns that into a
    /// refused archive rather than a skipped entry. See the module doc.
    pub fn from_wire(s: &str) -> Option<Kind> {
        Some(match s {
            "workspace" => Kind::Workspace,
            "session" => Kind::Session,
            "cwd" => Kind::Cwd,
            "launch" => Kind::Launch,
            "transcript" => Kind::Transcript,
            "memory" => Kind::Memory,
            _ => return None,
        })
    }

    /// Whether `id` is one this kind may carry.
    ///
    /// Each kind defers to the rule that already governs the thing the id
    /// names, rather than inventing a fourth character class: a session name
    /// is `session::valid_name` because that is what a session name is
    /// anywhere else in roost, and a Claude id is
    /// `claudesess::valid_session_id` because that is the rule that let it be
    /// stored in the first place. "The rule that let it in is the rule that
    /// lets it out", as `claudesess` puts it about the same id.
    pub fn valid_id(self, id: &str) -> bool {
        match self {
            // No id, and an id present is a malformed entry rather than a
            // harmless extra: it would mean the writer thought this kind was
            // keyed by something.
            Kind::Workspace => id.is_empty(),
            Kind::Session | Kind::Cwd | Kind::Launch => crate::session::valid_name(id),
            Kind::Transcript => crate::claudesess::valid_session_id(id),
            Kind::Memory => valid_memory_name(id),
        }
    }
}

/// A name from the project's `memory/` directory: exactly one path segment.
///
/// Deliberately not a reuse of `valid_session_id` — these are `.md` files with
/// human names (`roost-test-run-on-this-vm.md`), so a dot has to be allowed,
/// and the moment a dot is allowed `.` and `..` have to be refused by name.
/// A leading dot goes too: it is not needed, and it is how a rule that reads
/// as "no traversal" lets `.ssh` through when the directory changes meaning.
pub fn valid_memory_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('.')
        && !s.contains('/')
        && !s.contains('\\')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub id: String,
    /// Last-modified, unix seconds, where the source had one. Carried for the
    /// human reading a listing; nothing is restored from it.
    pub at: u64,
    pub bytes: Vec<u8>,
}

/// The header line. `source` is recorded **for the human reading a listing and
/// is never resolved, joined, or restored to** — say so here, because the next
/// reader's instinct will be to use it, and using it would undo the property
/// this whole module exists for.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Header {
    pub project: String,
    pub created: u64,
    pub roost: String,
    /// Where the project was when this was written. Informational. See above.
    #[serde(default)]
    pub source: String,
    /// What the walk captured and what it did not, rendered by the restore
    /// dialog. In the archive rather than only in a log because the moment
    /// this matters is the moment someone opens the file, which may be years
    /// after the terminal that wrote it was closed.
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Archive {
    pub header: Header,
    pub entries: Vec<Entry>,
}

pub fn write_header(w: &mut impl Write, h: &Header) -> std::io::Result<()> {
    let json = serde_json::to_string(h)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write!(w, "{MAGIC}\n{json}\n")
}

/// One entry: a metadata line, then exactly `bytes.len()` raw bytes, then a
/// newline.
///
/// The trailing newline is not a separator the parser needs — the length
/// already delimits the payload — and that is why it is here: it keeps the
/// file line-readable for a human with `head`, and a parser that finds
/// something else there has positive evidence the length was wrong.
pub fn write_entry(w: &mut impl Write, e: &Entry) -> std::io::Result<()> {
    let meta = serde_json::json!({
        "kind": e.kind.wire(),
        "id": e.id,
        "at": e.at,
        "bytes": e.bytes.len(),
    });
    writeln!(w, "{meta}")?;
    w.write_all(&e.bytes)?;
    w.write_all(b"\n")
}

/// Reads a whole archive, or explains why it is not one.
///
/// Every error names what failed and where, because a refusal the user cannot
/// act on is the same as a crash with better manners. Nothing here is
/// recoverable-with-a-warning: see the module doc.
pub fn parse(buf: &[u8]) -> Result<Archive, String> {
    let mut pos = 0usize;
    let magic = take_line(buf, &mut pos).ok_or("not a roost backup: the file is empty")?;
    if magic != MAGIC.as_bytes() {
        return Err(format!(
            "not a roost backup: expected {MAGIC} on the first line, found {:?}",
            String::from_utf8_lossy(&magic[..magic.len().min(32)])
        ));
    }
    let hline = take_line(buf, &mut pos).ok_or("truncated: no header line after the magic")?;
    let header: Header = serde_json::from_slice(&hline)
        .map_err(|e| format!("the header line is not valid roost backup metadata: {e}"))?;
    let mut entries = Vec::new();
    while pos < buf.len() {
        let mline = take_line(buf, &mut pos)
            .ok_or_else(|| format!("truncated: entry {} has no metadata line", entries.len() + 1))?;
        // An empty line where metadata should be is a file that has been
        // concatenated or padded, not an entry to skip past.
        if mline.is_empty() {
            return Err(format!("entry {} has an empty metadata line", entries.len() + 1));
        }
        let v: serde_json::Value = serde_json::from_slice(&mline)
            .map_err(|e| format!("entry {} has unreadable metadata: {e}", entries.len() + 1))?;
        let kind_s = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        let kind = Kind::from_wire(kind_s).ok_or_else(|| {
            format!(
                "entry {} is a {kind_s:?}, which this roost does not understand — \
                 refusing the archive rather than restoring part of it",
                entries.len() + 1
            )
        })?;
        let id = v.get("id").and_then(|k| k.as_str()).unwrap_or("").to_string();
        if !kind.valid_id(&id) {
            return Err(format!(
                "entry {} is a {kind_s} with an unusable id {id:?}",
                entries.len() + 1
            ));
        }
        let n = v
            .get("bytes")
            .and_then(|b| b.as_u64())
            .ok_or_else(|| format!("entry {} does not say how long it is", entries.len() + 1))?
            as usize;
        // The refusal the module doc is about. `pos + n` is computed with
        // `checked_add` because a declared length near `usize::MAX` would
        // otherwise wrap and pass the bounds check it exists to fail.
        let end = pos.checked_add(n).ok_or_else(|| {
            format!("entry {} declares an impossible length", entries.len() + 1)
        })?;
        if end > buf.len() {
            return Err(format!(
                "truncated: entry {} ({kind_s} {id:?}) declares {n} bytes but only {} remain",
                entries.len() + 1,
                buf.len() - pos
            ));
        }
        let bytes = buf[pos..end].to_vec();
        pos = end;
        // The newline `write_entry` puts after the payload. Its absence means
        // the declared length was wrong even though it fit — positive
        // evidence, which is the only kind this codebase acts on.
        match buf.get(pos) {
            Some(b'\n') => pos += 1,
            _ => {
                return Err(format!(
                    "entry {} ({kind_s} {id:?}) does not end where it said it would",
                    entries.len() + 1
                ))
            }
        }
        let at = v.get("at").and_then(|a| a.as_u64()).unwrap_or(0);
        entries.push(Entry { kind, id, at, bytes });
    }
    Ok(Archive { header, entries })
}

/// One line, without its newline, advancing `pos` past it. `None` at the end
/// of the buffer. A final line with no newline is returned — a file that ends
/// mid-metadata is caught by the parse of that line, which says so better than
/// "truncated" would.
fn take_line(buf: &[u8], pos: &mut usize) -> Option<Vec<u8>> {
    if *pos >= buf.len() {
        return None;
    }
    let rest = &buf[*pos..];
    match rest.iter().position(|b| *b == b'\n') {
        Some(i) => {
            let line = rest[..i].to_vec();
            *pos += i + 1;
            Some(line)
        }
        None => {
            let line = rest.to_vec();
            *pos = buf.len();
            Some(line)
        }
    }
}

/// Whether a `?conversations=` query value is the opt-in.
///
/// A function, and a pure one, because the route test cannot check this: with
/// no transcript directory the response is byte-identical either way, so an
/// assertion there passes against a handler that ignores the query entirely.
/// This is the switch that decides whether every prompt, every file read and
/// every command output on the machine leaves it, so it matches one exact
/// string — `conversations=true`, `=yes` and `=on` are all *off*, deliberately,
/// because a switch of this consequence should be hard to trip by accident and
/// there is exactly one thing roost's own UI sends.
pub fn wants_conversations(v: Option<&str>) -> bool {
    v == Some("1")
}

/// Today, as `YYYY-MM-DD`, for the download's filename.
///
/// Civil-from-days rather than a date crate: this is the only date roost
/// formats, and the algorithm is Howard Hinnant's, which is exact for every
/// year this will ever see. A wrong day here misnames a file; it is not worth
/// a dependency, and it is worth the eight lines rather than a guess.
pub fn today_utc() -> String {
    civil_from_unix(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
}

/// The pure half, split so its test needs no clock — and so the four dates
/// that break a naive version can be asserted at all.
fn civil_from_unix(secs: u64) -> String {
    let z = (secs / 86400) as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Conversations examined, newest first. `claudehist::MAX_SCANNED` is 30
/// because it is filling ten menu rows; a backup should reach further than a
/// menu, and 50 covers 21 of this host's 24 project directories whole.
pub const MAX_TRANSCRIPTS: usize = 50;
/// One conversation. The largest measured on this host is 25.7 MB.
pub const MAX_TRANSCRIPT_BYTES: u64 = 32 * 1024 * 1024;
/// The archive.
///
/// Not the 512 MB an earlier draft of the spec had, and the reason is worth
/// keeping: an archive is restored by *uploading* it, so one larger than
/// `config::max_upload_bytes` (100 MB by default) is a backup roost cannot
/// read back. A ceiling above that default leaves headroom for a raised one
/// without ever promising a file that only a one-way trip can produce. It also
/// bounds peak memory — see `collect_in` on why the walk buffers.
pub const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
/// The project's `memory/` directory, in total. roost's is 20 KB; this is a
/// notebook, not a store.
pub const MAX_MEMORY_BYTES: u64 = 1024 * 1024;

/// Something the walk chose not to take, or could not.
///
/// Each variant carries **the cap it hit**, not just the fact that one did.
/// A bare count renders as "1 skipped" whichever rule fired, which is a
/// message that passes a test and answers no question the user has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    TooBig { id: String, bytes: u64, cap: u64 },
    TooMany { cap: usize, dropped: usize },
    TotalFull { cap: u64, dropped: usize },
    /// *Could not look* at one file inside a directory that was itself
    /// readable. Kept apart from the decisions above for the reason
    /// `search::Results` keeps its three counters apart: "could not look" and
    /// "chose not to" are different answers, and only one of them is a gap.
    Unreadable { id: String },
}

impl Skip {
    fn line(&self) -> String {
        match self {
            Skip::TooBig { id, bytes, cap } => format!(
                "{} is {} (MAX_TRANSCRIPT_BYTES is {})",
                short(id),
                mb(*bytes),
                mb(*cap)
            ),
            Skip::TooMany { cap, dropped } => {
                format!("{dropped} older than the newest {cap} (MAX_TRANSCRIPTS is {cap})")
            }
            Skip::TotalFull { cap, dropped } => {
                format!("{dropped} would not fit (MAX_TOTAL_BYTES is {})", mb(*cap))
            }
            Skip::Unreadable { id } => format!("{} could not be read", short(id)),
        }
    }
}

fn mb(n: u64) -> String {
    format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
}

/// An id, shortened the way the ✻ menu shortens one. Full ids are 36
/// characters and four of them in a note is a wall.
fn short(id: &str) -> String {
    if id.chars().count() > 8 {
        format!("{}…", id.chars().take(8).collect::<String>())
    } else {
        id.to_string()
    }
}

/// What the walk took and what it left. Rendered into the archive's header so
/// that whoever opens the file reads it at the moment it matters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub layout: bool,
    pub sessions: usize,
    pub conversations: usize,
    pub memories: usize,
    pub skipped: Vec<Skip>,
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

impl Report {
    /// The lines that go in the header, and the whole of what a user is told.
    ///
    /// A skip is never folded into the count above it. CLAUDE.md, on the
    /// search results this copies: "All three used to render as an empty note,
    /// which is the same defect as the table below wearing a quieter coat: no
    /// crash, no lost shell, and no way for the user to tell."
    pub fn notes(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut took: Vec<String> = Vec::new();
        if self.layout {
            took.push("layout".into());
        }
        if self.sessions > 0 {
            took.push(plural(self.sessions, "terminal"));
        }
        if self.conversations > 0 {
            took.push(plural(self.conversations, "conversation"));
        }
        if self.memories > 0 {
            took.push(plural(self.memories, "memory file"));
        }
        out.push(if took.is_empty() {
            // Not an empty string. A backup that captured nothing is a real
            // outcome and has to read as one, or it renders as a blank line
            // that looks like a rendering bug.
            "nothing was captured".to_string()
        } else {
            took.join(", ")
        });
        if !self.skipped.is_empty() {
            let n: usize = self
                .skipped
                .iter()
                .map(|s| match s {
                    Skip::TooMany { dropped, .. } | Skip::TotalFull { dropped, .. } => *dropped,
                    _ => 1,
                })
                .sum();
            out.push(format!("{} was not captured:", plural(n, "conversation")));
            out.extend(self.skipped.iter().map(|s| s.line()));
        }
        out
    }
}

/// Everything roost knows about `project`, written to `w` as one archive.
///
/// `conversations` is the opt-in #18 asks for and nothing else turns it on.
pub fn collect(
    project: &str,
    dir: &std::path::Path,
    conversations: bool,
    w: &mut impl Write,
) -> Result<Report, String> {
    let tdir = if conversations { crate::claudehist::transcript_dir(dir) } else { None };
    collect_in(&crate::wsstate::state_dir(), tdir.as_deref(), project, dir, w)
}

/// The walk, with both directories passed in.
///
/// Split for the reason `claudehist::transcript_dir_in` is split, and it is
/// the same reason: `HOME` and `ROOST_STATE_DIR` are process-global, so a test
/// that set them would race every other test in the binary for one variable —
/// "a flake in whichever test loses", as CLAUDE.md puts it. Here the split
/// buys something further: the restore-at-a-different-path property can be
/// tested by handing this two unrelated directories, which is the only way to
/// test it that a same-name round trip does not fake.
///
/// **Entries are built in memory before anything is written.** The header
/// carries the report, the report is not known until the walk finishes, and
/// the header is the first line — so the alternative is a trailer nobody would
/// look for or a second pass over files that may have changed underneath.
/// `MAX_TOTAL_BYTES` is what makes this safe, and is sized in its own comment.
pub fn collect_in(
    state: &std::path::Path,
    tdir: Option<&std::path::Path>,
    project: &str,
    dir: &std::path::Path,
    w: &mut impl Write,
) -> Result<Report, String> {
    let key = crate::projects::storage_key(project);
    let mut report = Report::default();
    let mut entries: Vec<Entry> = Vec::new();
    let mut total: u64 = 0;

    if let Ok(bytes) = std::fs::read(state.join(format!("{key}.json"))) {
        total += bytes.len() as u64;
        report.layout = true;
        entries.push(Entry { kind: Kind::Workspace, id: String::new(), at: 0, bytes });
    }

    // The three per-session markers. A session with a record in one and not
    // the others is normal — `cwds` only writes once a shell has moved, and
    // `relaunch` only for a terminal roost started something in.
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (kind, sub, suffix) in [
        (Kind::Session, "claude", ".json"),
        (Kind::Cwd, "cwd", ""),
        (Kind::Launch, "launch", ""),
    ] {
        let d = state.join(sub).join(&key);
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(session) = name.strip_suffix(suffix) else { continue };
            // A temp file from an interrupted atomic write (`.term.tmp.123`)
            // is not a record; `valid_name` refuses the leading dot, so this
            // is already handled, but the filter is what makes that true.
            if !crate::session::valid_name(session) {
                continue;
            }
            let Ok(bytes) = std::fs::read(e.path()) else {
                report.skipped.push(Skip::Unreadable { id: session.to_string() });
                continue;
            };
            total += bytes.len() as u64;
            seen.insert(session.to_string());
            entries.push(Entry { kind, id: session.to_string(), at: 0, bytes });
        }
    }
    report.sessions = seen.len();

    if let Some(tdir) = tdir {
        collect_conversations(tdir, &mut entries, &mut report, &mut total)?;
    }

    let header = Header {
        project: project.to_string(),
        created: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        roost: env!("CARGO_PKG_VERSION").to_string(),
        source: dir.to_string_lossy().to_string(),
        notes: report.notes(),
    };
    write_header(w, &header).map_err(|e| e.to_string())?;
    for e in &entries {
        write_entry(w, e).map_err(|e| e.to_string())?;
    }
    Ok(report)
}

/// The conversations half, and the one place this module departs from
/// `claudehist`'s error model on purpose.
///
/// `claudehist::recent_in` opens with `let Ok(entries) = read_dir(tdir) else {
/// return Vec::new() }`, and defends it at length: every failure folds to "no
/// history", because the decision it feeds is *offer a menu or don't* and no
/// shell dies of it.
///
/// **That reasoning does not transfer.** The decision here is "write a file
/// the user will rely on and tell them it worked". An unreadable directory
/// folded to an empty list produces a valid archive holding zero
/// conversations, a cheerful summary, and a user who finds out on the day they
/// restore. So a directory that cannot be enumerated at all is an `Err` and no
/// archive is written — while a single file that cannot be read inside a
/// directory that *was* enumerated is a `Skip::Unreadable`, named, and the
/// archive is still written. The difference is whether "zero" is
/// distinguishable from "none", and only in the first case is it not.
fn collect_conversations(
    tdir: &std::path::Path,
    entries: &mut Vec<Entry>,
    report: &mut Report,
    total: &mut u64,
) -> Result<(), String> {
    let rd = match std::fs::read_dir(tdir) {
        Ok(rd) => rd,
        // A directory that has never existed is not a failure: a project where
        // Claude has never run has no conversations, and that is an answer.
        // Anything else — a permission error, an I/O error — is "cannot look".
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(format!(
                "cannot read this project's conversations at {}: {e}. \
                 Refusing rather than writing a backup with none in it.",
                tdir.display()
            ))
        }
    };
    let mut found: Vec<(u64, String, std::path::PathBuf, u64)> = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let Some(id) = name.strip_suffix(".jsonl") else { continue };
        if !crate::claudesess::valid_session_id(id) {
            continue;
        }
        let Ok(md) = e.metadata() else {
            report.skipped.push(Skip::Unreadable { id: id.to_string() });
            continue;
        };
        if !md.is_file() {
            continue;
        }
        let at = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        found.push((at, id.to_string(), path, md.len()));
    }
    // Newest first, and by id where two share a timestamp, so the set taken is
    // the same set on a second run. `claudehist::recent_in` sorts identically.
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    if found.len() > MAX_TRANSCRIPTS {
        report
            .skipped
            .push(Skip::TooMany { cap: MAX_TRANSCRIPTS, dropped: found.len() - MAX_TRANSCRIPTS });
        found.truncate(MAX_TRANSCRIPTS);
    }
    let mut no_room = 0usize;
    for (at, id, path, len) in found {
        if len > MAX_TRANSCRIPT_BYTES {
            report.skipped.push(Skip::TooBig { id, bytes: len, cap: MAX_TRANSCRIPT_BYTES });
            continue;
        }
        if *total + len > MAX_TOTAL_BYTES {
            no_room += 1;
            continue;
        }
        // Read before the entry is written, so the declared length is the
        // length actually captured: a transcript is appended to by a live
        // Claude, and a header written from the `metadata` above would state a
        // size the payload no longer has.
        let Ok(bytes) = std::fs::read(&path) else {
            report.skipped.push(Skip::Unreadable { id });
            continue;
        };
        // And re-checked against the bytes in hand, for the same reason: the
        // file may have grown past the cap between the stat and the read.
        if bytes.len() as u64 > MAX_TRANSCRIPT_BYTES {
            report.skipped.push(Skip::TooBig {
                id,
                bytes: bytes.len() as u64,
                cap: MAX_TRANSCRIPT_BYTES,
            });
            continue;
        }
        *total += bytes.len() as u64;
        report.conversations += 1;
        entries.push(Entry { kind: Kind::Transcript, id, at, bytes });
    }
    if no_room > 0 {
        report.skipped.push(Skip::TotalFull { cap: MAX_TOTAL_BYTES, dropped: no_room });
    }
    collect_memory(&tdir.join("memory"), entries, report, total);
    Ok(())
}

/// The project's `memory/` directory, if it has one.
///
/// Included because a backup of "the conversations" that dropped it would be
/// quietly lossy: this is where Claude Code keeps what it was told to remember
/// about the project, it is small, and it is the part a user would most
/// notice missing. Unlike the transcripts it is *not* a reason to refuse the
/// archive when unreadable — a project with no memory directory is the common
/// case, so "cannot look" and "nothing there" are not distinguishable here in
/// a way that could mislead anyone.
fn collect_memory(
    mem: &std::path::Path,
    entries: &mut Vec<Entry>,
    report: &mut Report,
    total: &mut u64,
) {
    let Ok(rd) = std::fs::read_dir(mem) else { return };
    let mut used: u64 = 0;
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            valid_memory_name(&n).then_some(n)
        })
        .collect();
    // Alphabetical, so a memory directory over the cap keeps the same files
    // each time rather than whatever the directory happened to yield first.
    names.sort();
    for name in names {
        let Ok(bytes) = std::fs::read(mem.join(&name)) else { continue };
        let len = bytes.len() as u64;
        if used + len > MAX_MEMORY_BYTES || *total + len > MAX_TOTAL_BYTES {
            continue;
        }
        used += len;
        *total += len;
        report.memories += 1;
        entries.push(Entry { kind: Kind::Memory, id: name, at: 0, bytes });
    }
}

// ---------------------------------------------------------------------------
// Restore
// ---------------------------------------------------------------------------

/// One thing a restore would do, or would not.
///
/// A plan is built whole before anything is written, and `dry_run` renders
/// exactly this list. That is not a convenience: #18 calls restore "the most
/// destructive operation roost would have", and the failure worth catching —
/// a transcript directory re-derived to somewhere the user did not expect — is
/// visible in a listing and invisible in a success message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Write this entry to this path. `replacing` is the existing file moved
    /// aside first, which only the layout ever has.
    Write { kind: Kind, id: String, to: std::path::PathBuf, replacing: Option<std::path::PathBuf> },
    /// A transcript already at the destination. Never overwritten, no flag,
    /// see `plan`.
    Keep { id: String, at: std::path::PathBuf },
}

impl Step {
    pub fn line(&self) -> String {
        match self {
            Step::Write { kind, id, to, replacing } => {
                let what = match kind {
                    Kind::Workspace => "layout".to_string(),
                    Kind::Memory => format!("memory {id}"),
                    Kind::Transcript => format!("conversation {}", short(id)),
                    k => format!("{} {id}", k.wire()),
                };
                match replacing {
                    Some(old) => format!(
                        "{what} → {} (the current one is kept as {})",
                        to.display(),
                        old.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                    ),
                    None => format!("{what} → {}", to.display()),
                }
            }
            Step::Keep { id, at } => format!(
                "conversation {} is already here and is left alone ({})",
                short(id),
                at.display()
            ),
        }
    }
}

/// Everything a restore would do, computed before it does any of it.
pub fn plan(
    state: &std::path::Path,
    tdir: Option<&std::path::Path>,
    project: &str,
    archive: &Archive,
    now: u64,
) -> Result<Vec<Step>, String> {
    let key = crate::projects::storage_key(project);
    let mut steps = Vec::new();
    for e in &archive.entries {
        // Re-validated here even though `parse` already did it. The id becomes
        // a path component on the next line, and this codebase puts the check
        // next to the use — `claudehist::has` says the same thing about the
        // same id: "the row that offered the button is a hint, not an
        // authorisation".
        if !e.kind.valid_id(&e.id) {
            return Err(format!("entry {} carries an id this restore will not use", e.kind.wire()));
        }
        match e.kind {
            Kind::Workspace => {
                let to = state.join(format!("{key}.json"));
                // Renamed aside, never deleted. A restore onto the wrong
                // project is then one `mv` away from being undone, and
                // `wsstate::load` reads one exact path so the extra file is
                // invisible to it.
                let replacing = to
                    .exists()
                    .then(|| state.join(format!("{key}.json.before-restore-{now}")));
                steps.push(Step::Write { kind: e.kind, id: e.id.clone(), to, replacing });
            }
            Kind::Session => steps.push(Step::Write {
                kind: e.kind,
                id: e.id.clone(),
                to: state.join("claude").join(&key).join(format!("{}.json", e.id)),
                replacing: None,
            }),
            Kind::Cwd => steps.push(Step::Write {
                kind: e.kind,
                id: e.id.clone(),
                to: state.join("cwd").join(&key).join(&e.id),
                replacing: None,
            }),
            Kind::Launch => steps.push(Step::Write {
                kind: e.kind,
                id: e.id.clone(),
                to: state.join("launch").join(&key).join(&e.id),
                replacing: None,
            }),
            Kind::Transcript | Kind::Memory => {
                // The re-derivation. `tdir` is computed from the project being
                // restored *into*, never from `archive.header.source` — which
                // is why an archive taken at one path restores correctly at
                // another, and why `source` is documented as informational.
                let Some(tdir) = tdir else {
                    return Err(
                        "this archive holds conversations, but this project has no \
                         directory to put them in — is HOME set?"
                            .to_string(),
                    );
                };
                let to = if e.kind == Kind::Transcript {
                    tdir.join(format!("{}.jsonl", e.id))
                } else {
                    tdir.join("memory").join(&e.id)
                };
                // Positive evidence, the CLAUDE.md way: `symlink_metadata`,
                // not `exists()`. `exists()` follows symlinks and collapses
                // "not there" into "cannot look", and both of those answers
                // would be read here as "free to write", which for a
                // transcript means overwriting a conversation that is not in
                // this archive. `Err(NotFound)` is absent; anything else is
                // *cannot tell*, and cannot-tell keeps the file.
                match std::fs::symlink_metadata(&to) {
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        steps.push(Step::Write {
                            kind: e.kind,
                            id: e.id.clone(),
                            to,
                            replacing: None,
                        })
                    }
                    _ => steps.push(Step::Keep { id: e.id.clone(), at: to }),
                }
            }
        }
    }
    Ok(steps)
}

/// Carries out a plan. Returns the lines describing what it did.
///
/// Every refusal has already happened in `plan` and in the caller's checks, so
/// this is deliberately dull: it creates parents, renames one file aside, and
/// writes. A failure part-way leaves what it has already written, which is why
/// the layout's old copy is renamed rather than deleted — the one irreversible
/// thing in reach stays reversible.
pub fn apply(archive: &Archive, steps: &[Step]) -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    for step in steps {
        let Step::Write { kind, id, to, replacing } = step else {
            done.push(step.line());
            continue;
        };
        let Some(entry) =
            archive.entries.iter().find(|e| e.kind == *kind && e.id == *id)
        else {
            continue;
        };
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        if let Some(aside) = replacing {
            std::fs::rename(to, aside)
                .map_err(|e| format!("cannot set {} aside: {e}", to.display()))?;
        }
        write_private(to, &entry.bytes)
            .map_err(|e| format!("cannot write {}: {e}", to.display()))?;
        done.push(step.line());
    }
    Ok(done)
}

/// Write-then-rename at 0600, the discipline `claudesess::record` sets.
///
/// 0600 on the temp file before the rename, not on the target after it: the
/// window between the two is exactly when a restored transcript — every
/// prompt, file and command output of a past conversation — would be
/// world-readable.
fn write_private(to: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = to.parent().unwrap_or(std::path::Path::new("."));
    let stem = to.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = dir.join(format!(".{stem}.restore.{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    match std::fs::rename(&tmp, to) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> Header {
        Header {
            project: "roost".into(),
            created: 1757750000,
            roost: "0.5.2".into(),
            source: "/home/claude/projects/roost".into(),
            notes: vec!["layout, 1 conversation".into()],
        }
    }

    fn entry(kind: Kind, id: &str, body: &str) -> Entry {
        Entry { kind, id: id.into(), at: 7, bytes: body.as_bytes().to_vec() }
    }

    fn archive_of(entries: &[Entry]) -> Vec<u8> {
        let mut out = Vec::new();
        write_header(&mut out, &header()).unwrap();
        for e in entries {
            write_entry(&mut out, e).unwrap();
        }
        out
    }

    #[test]
    fn what_goes_in_comes_back_out() {
        let want = vec![
            entry(Kind::Workspace, "", r#"{"sizes":[1]}"#),
            entry(Kind::Session, "term", r#"{"session_id":"abc"}"#),
            entry(Kind::Transcript, "864ee734-e3ab-434e-8278-745a850b16ad", "{\"a\":1}\n{\"b\":2}\n"),
            entry(Kind::Memory, "MEMORY.md", "- a line\n"),
        ];
        let got = parse(&archive_of(&want)).expect("a roost-written archive parses");
        assert_eq!(got.header.project, "roost");
        assert_eq!(got.entries, want);
        // A transcript is JSONL: it contains newlines, which is exactly why
        // the payload is length-prefixed rather than line-delimited. A parser
        // that split on newlines would pass every other assertion here.
        assert!(got.entries[2].bytes.contains(&b'\n'), "the fixture must contain a newline");
    }

    #[test]
    fn an_empty_file_is_not_an_empty_archive() {
        // The distinction this codebase is built around: "I could not read an
        // archive" must never arrive as "an archive with nothing in it",
        // which a restore would apply as a no-op and report as success.
        let e = parse(b"").unwrap_err();
        assert!(e.contains("empty"), "{e}");
    }

    #[test]
    fn a_file_that_is_not_an_archive_says_so_rather_than_guessing() {
        let e = parse(b"PK\x03\x04 some zip\n").unwrap_err();
        assert!(e.contains("not a roost backup"), "{e}");
        // The message has to carry what was actually found, or the user's next
        // question ("then what is this file?") has no answer in it.
        assert!(e.contains("PK"), "the refusal must quote what it found: {e}");
    }

    #[test]
    fn a_truncated_payload_fails_the_archive_instead_of_being_shortened() {
        // The module doc's first property. A parser that returned the bytes it
        // had would restore a transcript missing its tail, with nothing
        // anywhere saying so.
        let mut raw = archive_of(&[entry(Kind::Transcript, "abc123", "0123456789")]);
        raw.truncate(raw.len() - 4);
        let e = parse(&raw).unwrap_err();
        assert!(e.contains("truncated"), "{e}");
        assert!(e.contains("abc123"), "the refusal must name the entry: {e}");
    }

    #[test]
    fn a_length_that_lies_within_the_file_is_still_caught() {
        // The interesting half of the length check: the payload *fits*, so a
        // bounds test alone passes. What fails is the newline that must follow
        // it — positive evidence that the length was wrong, rather than an
        // inference from a byte count.
        //
        // Revert-checked: replacing the match with
        // `if buf.get(pos) == Some(&b'\n') { pos += 1 }` — the tolerant
        // version anyone would write — fails here and nowhere else, because
        // every other test's lengths are honest. Without it the entry parses
        // to a 4-byte payload and the remaining 6 bytes are read as the next
        // entry's metadata line.
        let raw = archive_of(&[entry(Kind::Session, "term", "0123456789")]);
        let broken = String::from_utf8(raw).unwrap().replace("\"bytes\":10", "\"bytes\":4");
        let e = parse(broken.as_bytes()).unwrap_err();
        assert!(e.contains("does not end where it said it would"), "{e}");
    }

    #[test]
    fn an_impossible_length_does_not_wrap() {
        // `pos + n` on a declared length near usize::MAX wraps to something
        // small and sails through a naive `end > buf.len()`. Found by writing
        // the check with `checked_add` and then asking what the plain `+`
        // would have done.
        let raw = archive_of(&[entry(Kind::Session, "term", "x")]);
        let broken = String::from_utf8(raw)
            .unwrap()
            .replace("\"bytes\":1", &format!("\"bytes\":{}", u64::MAX));
        let e = parse(broken.as_bytes()).unwrap_err();
        assert!(
            e.contains("impossible length") || e.contains("truncated"),
            "a wrapped length was accepted: {e}"
        );
    }

    #[test]
    fn an_id_that_could_escape_is_refused_and_never_cleaned_up() {
        // Every one of these becomes a path component at restore. The
        // assertion that matters is the *pairing*: the archive is refused, and
        // — because a sanitised id is silently a different id — no entry comes
        // back carrying a tidied version of what went in.
        //
        // Revert-checked with the fix anyone reaching for "be lenient" would
        // write — strip the offending characters instead of refusing. Two
        // tests go red, and the pair is the point: `../../etc/passwd` becomes
        // `....etcpasswd`, which is harmless, while `../MEMORY.md` becomes
        // `..MEMORY.md` and `a/b.md` becomes `ab.md` — a valid name for a file
        // nobody wrote, restored over nothing, reported as restored.
        for (kind, bad) in [
            (Kind::Transcript, "../../etc/passwd"),
            (Kind::Transcript, "-rf"),
            (Kind::Session, "a/b"),
            (Kind::Session, ".."),
            (Kind::Memory, "../MEMORY.md"),
            (Kind::Memory, ".ssh"),
            (Kind::Memory, "a/b.md"),
        ] {
            let raw = archive_of(&[entry(kind, bad, "x")]);
            let e = parse(&raw).unwrap_err();
            assert!(e.contains("unusable id"), "{bad:?} was accepted: {e}");
        }
        // Asserts the state it negates. Without this the test above passes
        // against a `valid_id` that returns false for everything, which would
        // refuse every real archive ever written.
        for (kind, good) in [
            (Kind::Transcript, "864ee734-e3ab-434e-8278-745a850b16ad"),
            (Kind::Session, "term1"),
            (Kind::Memory, "roost-test-run-on-this-vm.md"),
        ] {
            parse(&archive_of(&[entry(kind, good, "x")]))
                .unwrap_or_else(|e| panic!("a real {} id was refused: {e}", kind.wire()));
        }
    }

    #[test]
    fn a_kind_from_a_newer_roost_refuses_the_archive_rather_than_skipping_it() {
        // The second half of "refuses, never repairs". Skipping an unknown
        // entry restores a subset and calls it a restore — and the user finds
        // out which subset on the day they need the part that was dropped.
        let raw = archive_of(&[entry(Kind::Session, "term", "x")]);
        let broken = String::from_utf8(raw).unwrap().replace("\"session\"", "\"holodeck\"");
        let e = parse(broken.as_bytes()).unwrap_err();
        assert!(e.contains("does not understand"), "{e}");
        assert!(e.contains("refusing the archive"), "{e}");
    }

    #[test]
    fn a_workspace_entry_carrying_an_id_is_malformed() {
        // There is one layout per archive and it is not keyed by anything. An
        // id here means the writer believed otherwise, and a restore that
        // shrugged would put the layout somewhere nobody intended.
        let raw = archive_of(&[entry(Kind::Workspace, "term", "{}")]);
        assert!(parse(&raw).unwrap_err().contains("unusable id"));
    }

    #[test]
    fn an_entry_with_no_declared_length_is_refused() {
        let raw = archive_of(&[entry(Kind::Session, "term", "x")]);
        let broken = String::from_utf8(raw).unwrap().replace(",\"bytes\":1", "");
        let e = parse(broken.as_bytes()).unwrap_err();
        assert!(e.contains("does not say how long it is"), "{e}");
    }

    #[test]
    fn an_archive_with_no_entries_is_valid_and_empty() {
        // A layout-only backup of a project with nothing recorded. Valid, and
        // distinguishable from every refusal above — which is the whole point
        // of the refusals carrying messages.
        let got = parse(&archive_of(&[])).expect("a header with no entries is an archive");
        assert!(got.entries.is_empty());
        assert_eq!(got.header.notes, vec!["layout, 1 conversation".to_string()]);
    }

    // ---- the walk ----

    /// A state directory and a transcript directory, neither of them the
    /// process's real ones. Everything below uses this rather than
    /// `set_state_dir_for_test`, because `collect_in` takes both as
    /// arguments — which is the point of the split, and what lets these tests
    /// run in parallel with the rest of the binary without racing `HOME`.
    struct Fixture {
        root: std::path::PathBuf,
        state: std::path::PathBuf,
        tdir: std::path::PathBuf,
        dir: std::path::PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Fixture {
            let root = std::env::temp_dir().join(format!(
                "roost-bk-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            let f = Fixture {
                state: root.join("state"),
                tdir: root.join("transcripts"),
                dir: root.join("project"),
                root,
            };
            for d in [&f.state, &f.tdir, &f.dir] {
                std::fs::create_dir_all(d).unwrap();
            }
            f
        }
        fn layout(&self, key: &str, body: &str) {
            std::fs::write(self.state.join(format!("{key}.json")), body).unwrap();
        }
        fn marker(&self, sub: &str, key: &str, name: &str, body: &str) {
            let d = self.state.join(sub).join(key);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(name), body).unwrap();
        }
        fn transcript(&self, id: &str, body: &str) {
            std::fs::write(self.tdir.join(format!("{id}.jsonl")), body).unwrap();
        }
        fn run(&self, project: &str, conversations: bool) -> (Report, Vec<u8>) {
            let mut out = Vec::new();
            let t = conversations.then(|| self.tdir.clone());
            let r = collect_in(&self.state, t.as_deref(), project, &self.dir, &mut out)
                .expect("the walk succeeded");
            (r, out)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn a_layout_only_backup_holds_no_conversation_even_when_there_are_some() {
        // The default, and #18's first security question answered in code:
        // "Is the transcript included by default, or is workspace layout only
        // the default and conversations an explicit extra?"
        let f = Fixture::new("layoutonly");
        f.layout("proj", r#"{"sizes":[1]}"#);
        f.transcript("864ee734-e3ab-434e-8278-745a850b16ad", "{}\n");
        let (r, raw) = f.run("proj", false);
        assert!(r.layout);
        assert_eq!(r.conversations, 0);
        let a = parse(&raw).unwrap();
        assert_eq!(a.entries.len(), 1, "only the layout");
        assert_eq!(a.entries[0].kind, Kind::Workspace);
        // The transcript existed and was readable, so a zero here is a
        // decision rather than a failure — and the same call with the flag on
        // must find it, or this test proves nothing about the flag.
        let (r2, _) = f.run("proj", true);
        assert_eq!(r2.conversations, 1, "the fixture's transcript is reachable");
    }

    #[test]
    fn conversations_come_with_the_flag_and_the_header_says_what_was_taken() {
        let f = Fixture::new("withconv");
        f.layout("proj", "{}");
        f.marker("claude", "proj", "term.json", r#"{"session_id":"abc","event":"Stop"}"#);
        f.marker("cwd", "proj", "term", "/home/claude/projects/proj");
        f.transcript("864ee734-e3ab-434e-8278-745a850b16ad", "{\"a\":1}\n");
        let (r, raw) = f.run("proj", true);
        assert_eq!((r.layout, r.sessions, r.conversations), (true, 1, 1));
        let a = parse(&raw).unwrap();
        assert_eq!(a.header.notes[0], "layout, 1 terminal, 1 conversation");
        // The same session appears under two kinds and is counted once: the
        // number is terminals, not markers, and a reader told "2 terminals"
        // for one terminal would go looking for the other.
        assert_eq!(a.entries.iter().filter(|e| e.kind == Kind::Session).count(), 1);
        assert_eq!(a.entries.iter().filter(|e| e.kind == Kind::Cwd).count(), 1);
    }

    #[test]
    fn the_cap_that_fired_is_named_and_it_is_the_right_one() {
        // A count of skips passes whichever cap fired. The message is the
        // whole user-facing contract, so the message is what is asserted.
        let f = Fixture::new("caps");
        f.layout("proj", "{}");
        f.transcript("aaaaaaaa-0000-0000-0000-000000000001", "small\n");
        let big = "x".repeat(MAX_TRANSCRIPT_BYTES as usize + 1);
        f.transcript("bbbbbbbb-0000-0000-0000-000000000002", &big);
        let (r, raw) = f.run("proj", true);
        assert_eq!(r.conversations, 1, "the small one was taken");
        assert_eq!(r.skipped.len(), 1);
        let notes = parse(&raw).unwrap().header.notes;
        let joined = notes.join("\n");
        assert!(joined.contains("1 conversation was not captured:"), "{joined}");
        assert!(joined.contains("MAX_TRANSCRIPT_BYTES"), "the cap must name itself: {joined}");
        assert!(joined.contains("bbbbbbbb…"), "and name what it dropped: {joined}");
        // Not the other caps. A note that named every constant would satisfy
        // the assertions above while telling the user nothing.
        assert!(!joined.contains("MAX_TRANSCRIPTS is"), "{joined}");
        assert!(!joined.contains("MAX_TOTAL_BYTES"), "{joined}");
    }

    #[test]
    fn nothing_captured_reads_as_an_answer_rather_than_a_blank_line() {
        let f = Fixture::new("empty");
        let (_, raw) = f.run("proj", true);
        assert_eq!(parse(&raw).unwrap().header.notes, vec!["nothing was captured".to_string()]);
    }

    #[test]
    fn an_unreadable_conversation_directory_refuses_instead_of_backing_up_none() {
        // The departure from claudehist's error model, and the reason this
        // module has one of its own. Folding this to an empty list produces a
        // valid archive with zero conversations and a summary that says so
        // — indistinguishable from a project where Claude never ran.
        //
        // Revert-checked by giving the read_dir the same `Err(_) => return
        // Ok(())` arm claudehist has. What came back was
        // `Report { layout: true, conversations: 0, memories: 0, skipped: [] }`
        // — not an error, not a skip, nothing anywhere to read as a warning.
        // That report is the defect: a backup of a project with transcripts
        // on disk, holding none of them, reporting complete success.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let f = Fixture::new("unreadable");
            f.layout("proj", "{}");
            f.transcript("aaaaaaaa-0000-0000-0000-000000000001", "x\n");
            std::fs::set_permissions(&f.tdir, std::fs::Permissions::from_mode(0o000)).unwrap();
            let mut out = Vec::new();
            let got = collect_in(&f.state, Some(&f.tdir), "proj", &f.dir, &mut out);
            std::fs::set_permissions(&f.tdir, std::fs::Permissions::from_mode(0o755)).unwrap();
            // Running as root defeats the fixture — the directory is readable
            // whatever its mode — so the assertion is skipped rather than
            // inverted. A test that silently passes as root is exactly the
            // "passes for the wrong reason" class CLAUDE.md names.
            if !nix_is_root() {
                let e = got.expect_err("an unreadable directory must refuse");
                assert!(e.contains("Refusing"), "{e}");
                assert!(out.is_empty(), "and must not have written a partial archive");
            }
        }
    }

    #[cfg(unix)]
    fn nix_is_root() -> bool {
        // No libc dependency: root is the one user that can read a 0000
        // directory it owns, so ask the filesystem instead of the uid.
        let probe = std::env::temp_dir().join(format!("roost-rootprobe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&probe);
        if std::fs::create_dir_all(&probe).is_err() {
            return false;
        }
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o000));
        let readable = std::fs::read_dir(&probe).is_ok();
        let _ = std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&probe);
        readable
    }

    #[test]
    fn a_missing_conversation_directory_is_an_answer_not_a_failure() {
        // The other side of the test above, and the distinction the whole
        // module turns on: "never existed" is *not there*, "cannot look" is a
        // refusal, and collapsing the first into the second would refuse a
        // backup of every project Claude has never been run in.
        let f = Fixture::new("notranscripts");
        f.layout("proj", "{}");
        let mut out = Vec::new();
        let r = collect_in(&f.state, Some(&f.root.join("nope")), "proj", &f.dir, &mut out)
            .expect("a project where Claude never ran still backs up");
        assert_eq!(r.conversations, 0);
        assert!(r.layout);
    }

    #[test]
    fn memory_files_travel_with_the_conversations() {
        let f = Fixture::new("memory");
        f.layout("proj", "{}");
        std::fs::create_dir_all(f.tdir.join("memory")).unwrap();
        std::fs::write(f.tdir.join("memory/MEMORY.md"), "- [a](a.md)\n").unwrap();
        std::fs::write(f.tdir.join("memory/a.md"), "a fact\n").unwrap();
        // Not a memory file: refused by name, not copied under a tidied one.
        std::fs::write(f.tdir.join("memory/.hidden"), "no\n").unwrap();
        let (r, raw) = f.run("proj", true);
        assert_eq!(r.memories, 2);
        let a = parse(&raw).unwrap();
        let mut names: Vec<&str> =
            a.entries.iter().filter(|e| e.kind == Kind::Memory).map(|e| e.id.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["MEMORY.md", "a.md"]);
        // And they are off by default, with the conversations they belong to.
        let (r2, _) = f.run("proj", false);
        assert_eq!(r2.memories, 0);
    }

    #[test]
    fn a_project_key_with_a_slash_reaches_its_own_state_and_no_one_elses() {
        // A worktree is a project whose key is percent-encoded
        // (`roost%2F.claude%2Fworktrees%2Fclaude-1`). The walk builds paths
        // from `storage_key`, so a backup of the worktree must not pick up the
        // parent's layout, and vice versa.
        let f = Fixture::new("worktree");
        f.layout("proj", r#"{"which":"parent"}"#);
        f.layout("proj%2Fsub", r#"{"which":"child"}"#);
        f.marker("claude", "proj%2Fsub", "term.json", "{}");
        let (r, raw) = f.run("proj/sub", false);
        assert_eq!((r.layout, r.sessions), (true, 1));
        let a = parse(&raw).unwrap();
        let ws = a.entries.iter().find(|e| e.kind == Kind::Workspace).unwrap();
        assert_eq!(String::from_utf8_lossy(&ws.bytes), r#"{"which":"child"}"#);
    }

    #[test]
    fn the_date_in_a_download_name_is_the_real_one() {
        // Hand-rolled civil-from-days, so it is checked against dates computed
        // elsewhere rather than against itself. These four are the ones that
        // break a naive implementation: a leap day, the day after it, a
        // century that is not a leap year, and one that is.
        for (secs, want) in [
            (0u64, "1970-01-01"),
            (951782400, "2000-02-29"),
            (951868800, "2000-03-01"),
            (1709164800, "2024-02-29"),
            (4107542400, "2100-03-01"),
            (1757750400, "2025-09-13"),
        ] {
            assert_eq!(civil_from_unix(secs), want, "for {secs}");
        }
    }

    #[test]
    fn the_conversations_opt_in_is_one_exact_string() {
        assert!(wants_conversations(Some("1")), "the one thing the UI sends");
        for off in [None, Some(""), Some("0"), Some("true"), Some("yes"), Some("on"),
                    Some("11"), Some(" 1"), Some("1 "), Some("01"), Some("TRUE")] {
            assert!(!wants_conversations(off), "{off:?} was treated as the opt-in");
        }
    }

    // ---- restore ----

    /// **The test this feature exists to pass.**
    ///
    /// Back up project A, living at one path with its conversations under one
    /// derived directory; restore it as project B at a *different* path with a
    /// *different* derived directory, and assert the transcript landed under
    /// B's.
    ///
    /// A same-name round trip — back up `proj`, restore `proj` — is the
    /// obvious test and proves nothing at all: it stays green against an
    /// implementation that records the source's absolute path and copies it
    /// straight back, which is precisely #18's third difficulty and the thing
    /// the whole no-paths format is for.
    ///
    /// Revert-checked by doing exactly that — deriving the destination from
    /// `archive.header.source` instead of from the project being restored
    /// into, which is the one-line "simplification" the `source` field invites.
    /// Three tests go red and this one is the first: the transcript lands back
    /// in the *source's* directory, so the destination never receives it.
    #[test]
    fn an_archive_restores_under_the_destinations_own_derived_directory() {
        let src = Fixture::new("rederive-src");
        let dst = Fixture::new("rederive-dst");
        src.layout("alpha", r#"{"which":"alpha"}"#);
        src.marker("claude", "alpha", "term.json", r#"{"session_id":"abc","event":"Stop"}"#);
        src.transcript("864ee734-e3ab-434e-8278-745a850b16ad", "{\"turn\":1}\n");
        let (report, raw) = src.run("alpha", true);
        assert_eq!(report.conversations, 1, "setup: the source really had one");

        let archive = parse(&raw).unwrap();
        // Different project name, different state directory, different
        // transcript directory. Nothing about the destination is shared with
        // the source, which is what makes the assertion below mean something.
        let steps = plan(&dst.state, Some(&dst.tdir), "beta", &archive, 1757750000).unwrap();
        apply(&archive, &steps).unwrap();

        let landed = dst.tdir.join("864ee734-e3ab-434e-8278-745a850b16ad.jsonl");
        assert!(landed.is_file(), "the transcript is under the destination's directory");
        assert_eq!(std::fs::read_to_string(&landed).unwrap(), "{\"turn\":1}\n");
        // And nothing was written back where it came from.
        assert!(
            !src.tdir.join("beta.jsonl").exists()
                && std::fs::read_dir(&src.tdir).unwrap().count() == 1,
            "the source directory was touched"
        );
        // The layout landed under the *destination's* key, not alpha's.
        assert_eq!(
            std::fs::read_to_string(dst.state.join("beta.json")).unwrap(),
            r#"{"which":"alpha"}"#
        );
        assert!(!dst.state.join("alpha.json").exists(), "the source key must not appear");
        assert!(dst.state.join("claude/beta/term.json").is_file(), "and so did the marker");
    }

    /// #18's third security question — "Does restore ever overwrite an
    /// existing transcript?" — answered in code. No. There is no flag.
    ///
    /// The assertion is on the existing file's **content**. Asserting that a
    /// skip was *reported* is the version that passes against a restore that
    /// reports the skip and then writes anyway through a second path.
    ///
    /// Revert-checked by deleting the exists-check and always pushing a
    /// `Write`: this test alone goes red, on the content, with the machine's
    /// two-turn conversation replaced by the archive's one-turn copy.
    #[test]
    fn a_restore_never_writes_over_a_conversation_that_is_already_here() {
        let src = Fixture::new("nooverwrite-src");
        let dst = Fixture::new("nooverwrite-dst");
        let id = "864ee734-e3ab-434e-8278-745a850b16ad";
        src.layout("alpha", "{}");
        src.transcript(id, "{\"from\":\"the archive\"}\n");
        let (_, raw) = src.run("alpha", true);
        let archive = parse(&raw).unwrap();

        // The live one. Longer than the archived copy, so a truncating write
        // is caught as well as a replacing one.
        let live = "{\"from\":\"the machine\"}\n{\"and\":\"a second turn\"}\n";
        dst.transcript(id, live);

        let steps = plan(&dst.state, Some(&dst.tdir), "alpha", &archive, 1757750000).unwrap();
        assert!(
            steps.iter().any(|s| matches!(s, Step::Keep { id: k, .. } if k == id)),
            "the plan must say it is leaving it alone: {steps:?}"
        );
        apply(&archive, &steps).unwrap();
        assert_eq!(
            std::fs::read_to_string(dst.tdir.join(format!("{id}.jsonl"))).unwrap(),
            live,
            "the conversation on this machine was modified"
        );
    }

    #[test]
    fn a_transcript_entry_roost_cannot_read_as_absent_is_kept() {
        // The last row of CLAUDE.md's table: "`Path::exists()` on a dangling
        // symlink → the destination is free → the symlink's target". `exists()`
        // collapses "not there" and "cannot look" into `false`, and here
        // `false` means "write over it".
        //
        // The symlink must **dangle**, and the first version of this test is
        // why that is spelled out: it pointed at a file that existed, so
        // `exists()` and `symlink_metadata` agreed, and swapping one for the
        // other left the test green. Revert-checked properly this time —
        // replacing the match with `if to.exists()` makes this test, and only
        // this test, fail.
        #[cfg(unix)]
        {
            let src = Fixture::new("dangling-src");
            let dst = Fixture::new("dangling-dst");
            let id = "864ee734-e3ab-434e-8278-745a850b16ad";
            src.layout("alpha", "{}");
            src.transcript(id, "archived\n");
            let (_, raw) = src.run("alpha", true);
            let archive = parse(&raw).unwrap();

            let link = dst.tdir.join(format!("{id}.jsonl"));
            let target = dst.root.join("not-here-yet.jsonl");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(!link.exists(), "setup: exists() must report false for this");
            assert!(std::fs::symlink_metadata(&link).is_ok(), "setup: but something IS there");

            let steps = plan(&dst.state, Some(&dst.tdir), "alpha", &archive, 1).unwrap();
            apply(&archive, &steps).unwrap();

            // Something was at that name and roost could not read it as a
            // transcript. The only safe reading is "leave it alone" — so the
            // symlink is still a symlink, still pointing where it did, and the
            // archived copy did not land on top of it.
            let md = std::fs::symlink_metadata(&link).expect("the entry is still there");
            assert!(md.file_type().is_symlink(), "the symlink was replaced by the restore");
            assert_eq!(std::fs::read_link(&link).unwrap(), target, "it now points elsewhere");
            assert!(!target.exists(), "the write followed the link out of the directory");
        }
    }

    #[test]
    fn the_layout_it_replaces_is_kept_beside_it() {
        // The one thing a restore is *for* replacing. Renamed, not deleted:
        // restoring onto the wrong project is then one `mv` from being undone.
        let src = Fixture::new("aside-src");
        let dst = Fixture::new("aside-dst");
        src.layout("alpha", r#"{"which":"archived"}"#);
        let (_, raw) = src.run("alpha", false);
        let archive = parse(&raw).unwrap();
        dst.layout("alpha", r#"{"which":"live"}"#);

        let steps = plan(&dst.state, None, "alpha", &archive, 1757750000).unwrap();
        apply(&archive, &steps).unwrap();
        assert_eq!(
            std::fs::read_to_string(dst.state.join("alpha.json")).unwrap(),
            r#"{"which":"archived"}"#
        );
        assert_eq!(
            std::fs::read_to_string(dst.state.join("alpha.json.before-restore-1757750000")).unwrap(),
            r#"{"which":"live"}"#,
            "the layout that was here is still recoverable"
        );
    }

    #[test]
    fn a_dry_run_plan_describes_the_restore_without_performing_it() {
        let src = Fixture::new("dryrun-src");
        let dst = Fixture::new("dryrun-dst");
        src.layout("alpha", "{}");
        src.transcript("864ee734-e3ab-434e-8278-745a850b16ad", "x\n");
        let (_, raw) = src.run("alpha", true);
        let archive = parse(&raw).unwrap();

        let steps = plan(&dst.state, Some(&dst.tdir), "beta", &archive, 1).unwrap();
        let lines: Vec<String> = steps.iter().map(Step::line).collect();
        assert!(lines.iter().any(|l| l.contains("layout →")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("conversation 864ee734… →")), "{lines:?}");
        // The whole point: building the plan wrote nothing. A plan that
        // created its destination directories would already have changed the
        // machine before the user saw the listing.
        assert!(!dst.state.join("beta.json").exists());
        assert_eq!(std::fs::read_dir(&dst.tdir).unwrap().count(), 0);
    }

    #[test]
    fn a_restored_transcript_is_not_world_readable() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let src = Fixture::new("perm-src");
            let dst = Fixture::new("perm-dst");
            let id = "864ee734-e3ab-434e-8278-745a850b16ad";
            src.layout("alpha", "{}");
            src.transcript(id, "every command it ran\n");
            let (_, raw) = src.run("alpha", true);
            let archive = parse(&raw).unwrap();
            let steps = plan(&dst.state, Some(&dst.tdir), "alpha", &archive, 1).unwrap();
            apply(&archive, &steps).unwrap();
            let m = std::fs::metadata(dst.tdir.join(format!("{id}.jsonl"))).unwrap();
            assert_eq!(m.permissions().mode() & 0o077, 0, "no group or other bits");
        }
    }
}
