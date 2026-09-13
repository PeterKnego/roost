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
}
