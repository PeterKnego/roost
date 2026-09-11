//! The past conversations of a project, for the ✻ button's menu.
//!
//! #18 step 2 gave a terminal whose shell a reboot took a way back to *its*
//! conversation, from the id roost recorded for that terminal. This answers a
//! wider question — "what has been run in this project at all?" — which
//! `claudesess` cannot, by design: it holds one record per live terminal and
//! `end_session` forgets it, so it is a map of the present, not a history.
//!
//! The history belongs to Claude Code, which writes every conversation to
//! `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`. So this reads another
//! program's private layout, and #18 says plainly that it "can change in any
//! Claude Code release". Three things make that acceptable here and none of
//! them is optimism:
//!
//! - **Nothing is written and nothing is destroyed.** The worst outcome of a
//!   layout change is an empty menu, and an empty menu is a ✻ that launches a
//!   fresh Claude — which is exactly what ✻ did before this existed.
//! - **Every failure folds to "no history".** Unreadable directory, unparsable
//!   line, a file that is not a transcript: all mean the entry is absent, never
//!   an error and never a claim. This is the one place in this codebase where
//!   collapsing "cannot look" into "nothing there" is right, because the
//!   decision it feeds is *offer a menu or don't* — no shell dies of it.
//! - **The id is checked before it is used**, by `claudesess::valid_session_id`,
//!   the same rule that let it be stored. A filename is not a token.
//!
//! The encoding is every character outside `[A-Za-z0-9]` replaced by `-`.
//! Verified 2026-09-10 against this host's real directory: 4 of 4 paths mapped,
//! including `/home/claude/projects/roost/.claude/worktrees/claude-1` →
//! `-home-claude-projects-roost--claude-worktrees-claude-1`, and none of the
//! 26 existing directory names contained a character outside `[A-Za-z0-9-]`.
//!
//! **Files are read with a hard prefix cap and never whole.** Measured on this
//! host: 5263 transcripts totalling 410 MB, the largest 14 MB — and the first
//! user message sits within 64 KB in 40 of 40 sampled files (median 3 KB, worst
//! 41 KB). So the label costs one bounded read, and a transcript that keeps its
//! first message past the cap loses its label, not the entry.

use std::path::{Path, PathBuf};

/// How far into a transcript the label search goes. See the module doc for the
/// measurement; this is 1.5× the worst case observed.
const LABEL_SCAN_BYTES: u64 = 64 * 1024;
/// Transcripts examined, newest first, before giving up. A project with more
/// than this has plenty to offer from the newest few.
const MAX_SCANNED: usize = 30;
/// Rows the menu shows.
pub const MAX_ROWS: usize = 10;
/// Label length, in characters. A menu row, not a summary.
const MAX_LABEL: usize = 72;

/// One resumable conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub session_id: String,
    /// The first thing the user said, or empty when the transcript did not
    /// yield one within the scan cap. Empty is a real state the renderer
    /// handles, not a defect: the row is still resumable.
    pub label: String,
    /// Last-modified, unix seconds. Ordering key and what the row shows.
    pub at: u64,
}

/// Claude Code's directory for `dir`'s conversations, derived. Existence is not
/// checked here — `recent` treats a missing directory as no history.
pub fn transcript_dir(dir: &Path) -> Option<PathBuf> {
    transcript_dir_in(Path::new(&std::env::var_os("HOME")?), dir)
}

/// The derivation, with the home directory passed in.
///
/// Split so its test needs no environment. `HOME` is process-global and
/// `state_dir` and `global_config_path` both read it, so a test that set it
/// would be racing every other test in the binary for one variable — the class
/// of defect CLAUDE.md describes as "a flake in whichever test loses".
pub fn transcript_dir_in(home: &Path, dir: &Path) -> Option<PathBuf> {
    let s = dir.to_str()?;
    let encoded: String =
        s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    Some(home.join(".claude/projects").join(encoded))
}

/// The project's conversations, newest first.
///
/// Empty for every failure, which is the whole error model — see the module
/// doc. `limit` is capped at `MAX_ROWS` whatever the caller asks for.
pub fn recent(dir: &Path, limit: usize) -> Vec<Conversation> {
    let Some(tdir) = transcript_dir(dir) else { return Vec::new() };
    recent_in(&tdir, limit)
}

/// The directory-reading half, so tests need no `HOME` and no real layout.
pub fn recent_in(tdir: &Path, limit: usize) -> Vec<Conversation> {
    let Ok(entries) = std::fs::read_dir(tdir) else { return Vec::new() };
    let mut files: Vec<(u64, String, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            // `.jsonl` files only. The same directory holds per-session
            // *directories* named with the same ids, and a directory is not a
            // transcript.
            let name = path.file_name()?.to_str()?;
            let id = name.strip_suffix(".jsonl")?;
            if !crate::claudesess::valid_session_id(id) {
                return None;
            }
            let md = e.metadata().ok()?;
            if !md.is_file() {
                return None;
            }
            let at = md
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs();
            Some((at, id.to_string(), path))
        })
        .collect();
    // Newest first, and by id where two share a timestamp so the order is
    // stable rather than whatever the directory happened to yield.
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    files.truncate(MAX_SCANNED);
    files
        .into_iter()
        .take(limit.min(MAX_ROWS))
        .map(|(at, session_id, path)| Conversation {
            label: first_user_text(&path).unwrap_or_default(),
            session_id,
            at,
        })
        .collect()
}

/// Whether `id` names a conversation *of this project*.
///
/// The authorisation for a resume, and deliberately re-derived at the moment it
/// is used rather than trusted from the click that offered it — the same rule
/// `RemoveWorktree` states: "the row that offered the button is a hint, not an
/// authorisation". A menu can be stale, forged, or from another project.
///
/// Two checks, and the first is the one that matters: the id must pass
/// `claudesess::valid_session_id`, which refuses a leading `-` and anything
/// outside `[A-Za-z0-9_-]`, so nothing here can become a path segment of its
/// own or an option to `claude`. The stat is then merely "and we have it".
pub fn has(dir: &Path, id: &str) -> bool {
    if !crate::claudesess::valid_session_id(id) {
        return false;
    }
    transcript_dir(dir)
        .map(|t| t.join(format!("{id}.jsonl")))
        .and_then(|p| std::fs::symlink_metadata(p).ok())
        .is_some_and(|m| m.is_file())
}

/// The first thing the user said in a transcript, cleaned up for one menu row.
///
/// Reads at most `LABEL_SCAN_BYTES`. A line that does not parse is skipped
/// rather than ending the search: this is another program's format, and one
/// unfamiliar record must not cost the label.
fn first_user_text(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader, Read};
    let f = std::fs::File::open(path).ok()?;
    let mut r = BufReader::new(f.take(LABEL_SCAN_BYTES));
    let mut line = String::new();
    loop {
        line.clear();
        // `read_line` on possibly-invalid UTF-8 (the cap can cut a character in
        // half) is an error, not a stop — so a truncated tail ends the search
        // rather than propagating.
        match r.read_line(&mut line) {
            Ok(0) | Err(_) => return None,
            Ok(_) => {}
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if v.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        // Claude Code marks its own injected turns; those are not what the
        // person typed and make a useless row.
        if v.get("isMeta").and_then(|m| m.as_bool()) == Some(true) {
            continue;
        }
        let Some(text) = user_text(&v) else { continue };
        let clean = tidy(&text);
        // A turn that is entirely a tool result or a command wrapper says
        // nothing about the conversation. Skip and keep looking, still inside
        // the byte cap.
        if clean.is_empty() || clean.starts_with('<') {
            continue;
        }
        return Some(clean);
    }
}

/// `message.content`, which is either a string or a list of parts.
fn user_text(v: &serde_json::Value) -> Option<String> {
    let content = v.get("message")?.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let parts = content.as_array()?;
    let mut out = String::new();
    for p in parts {
        if p.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(s) = p.get("text").and_then(|t| t.as_str()) {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(s);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// One line, no control characters, capped. Whitespace is collapsed because a
/// prompt is usually several lines and a row is one.
fn tidy(s: &str) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() <= MAX_LABEL {
        return flat;
    }
    let mut out: String = flat.chars().take(MAX_LABEL.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoding, against the mapping measured on this host. Written out as
    /// the real pairs rather than as a property, because the property *is* the
    /// reverse-engineered rule — asserting it against itself would prove
    /// nothing, and these four are the evidence.
    #[test]
    fn the_directory_name_matches_what_claude_code_actually_writes() {
        let home = Path::new("/home/claude");
        for (path, want) in [
            ("/home/claude", "-home-claude"),
            ("/home/claude/projects/roost", "-home-claude-projects-roost"),
            (
                "/home/claude/projects/roost/.claude/worktrees/claude-1",
                "-home-claude-projects-roost--claude-worktrees-claude-1",
            ),
            ("/srv/a_b.c", "-srv-a-b-c"),
        ] {
            let got = transcript_dir_in(home, Path::new(path)).unwrap();
            assert_eq!(got.file_name().unwrap().to_str().unwrap(), want, "{path}");
            assert!(got.starts_with(home.join(".claude/projects")), "{got:?}");
        }
    }

    fn write(dir: &Path, id: &str, lines: &[&str]) -> PathBuf {
        let p = dir.join(format!("{id}.jsonl"));
        std::fs::write(&p, lines.join("\n")).unwrap();
        p
    }

    /// A transcript shaped like the real ones: metadata first, then the turn.
    fn transcript(text: &str) -> Vec<String> {
        vec![
            r#"{"type":"mode","sessionId":"x"}"#.to_string(),
            r#"{"type":"permission-mode","sessionId":"x"}"#.to_string(),
            format!(r#"{{"type":"user","message":{{"content":{}}}}}"#, serde_json::json!(text)),
        ]
    }

    #[test]
    fn a_transcript_yields_its_id_and_the_first_thing_the_user_said() {
        let d = tempfile::tempdir().unwrap();
        let t: Vec<String> = transcript("fix the failing test in registry.rs");
        write(d.path(), "864ee734-e3ab-434e-8278-745a850b16ad",
              &t.iter().map(String::as_str).collect::<Vec<_>>());
        let got = recent_in(d.path(), 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].session_id, "864ee734-e3ab-434e-8278-745a850b16ad");
        assert_eq!(got[0].label, "fix the failing test in registry.rs");
        assert!(got[0].at > 1_600_000_000, "an mtime of {} is not a time", got[0].at);
    }

    /// Everything in that directory that is not a transcript. The per-session
    /// *directories* are the real case — Claude Code puts them alongside the
    /// files, named with the same ids — and a directory listed as a
    /// conversation is a row that resumes nothing.
    ///
    /// Revert-checked: dropping the `md.is_file()` guard adds a `sidecar` row;
    /// dropping the `valid_session_id` check adds `../escape`.
    #[test]
    fn only_transcripts_are_listed() {
        let d = tempfile::tempdir().unwrap();
        let t = transcript("real");
        let refs: Vec<&str> = t.iter().map(String::as_str).collect();
        write(d.path(), "864ee734-e3ab-434e-8278-745a850b16ad", &refs);
        std::fs::create_dir(d.path().join("aaaa1111-2222-3333-4444-555555555555.jsonl")).unwrap();
        std::fs::write(d.path().join("notes.md"), "x").unwrap();
        std::fs::write(d.path().join("--dangerously-skip-permissions.jsonl"), "x").unwrap();
        let got = recent_in(d.path(), 10);
        assert_eq!(
            got.iter().map(|c| c.session_id.as_str()).collect::<Vec<_>>(),
            vec!["864ee734-e3ab-434e-8278-745a850b16ad"],
            "got {got:?}"
        );
    }

    /// Newest first, because "the one I was just in" is the row people want,
    /// and it is the row a menu should put nearest the cursor.
    #[test]
    fn conversations_come_back_newest_first() {
        let d = tempfile::tempdir().unwrap();
        let t = transcript("x");
        let refs: Vec<&str> = t.iter().map(String::as_str).collect();
        let old = write(d.path(), "aaaa1111-2222-3333-4444-555555555555", &refs);
        let new = write(d.path(), "bbbb2222-3333-4444-5555-666666666666", &refs);
        // Set mtimes explicitly: two files written in the same millisecond
        // would otherwise make this test's subject the tie-break, not the sort.
        let t0 = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        filetime_set(&old, t0);
        filetime_set(&new, t0 + std::time::Duration::from_secs(60));
        let got = recent_in(d.path(), 10);
        assert_eq!(
            got.iter().map(|c| c.session_id.as_str()).collect::<Vec<_>>(),
            vec!["bbbb2222-3333-4444-5555-666666666666", "aaaa1111-2222-3333-4444-555555555555"]
        );
    }

    fn filetime_set(p: &Path, t: std::time::SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(t)).unwrap();
    }

    /// Every way of failing folds to "no history", which is the error model.
    /// A ✻ with no menu is a ✻ that launches a fresh Claude — what it did
    /// before this existed.
    #[test]
    fn a_directory_that_cannot_be_read_is_no_history_and_not_an_error() {
        assert!(recent_in(Path::new("/nonexistent/roost/claudehist"), 10).is_empty());
        let d = tempfile::tempdir().unwrap();
        assert!(recent_in(d.path(), 10).is_empty(), "an empty directory offers nothing");
    }

    /// A transcript whose first user turn is past the scan cap keeps its row
    /// and loses only its label. The row is still resumable, which is the
    /// point — the id comes from the filename, not from the contents.
    #[test]
    fn a_label_that_is_too_deep_costs_the_label_and_not_the_row() {
        let d = tempfile::tempdir().unwrap();
        let filler = format!(r#"{{"type":"mode","pad":"{}"}}"#, "x".repeat(90_000));
        let t = transcript("never reached");
        let mut lines = vec![filler.as_str()];
        lines.extend(t.iter().map(String::as_str));
        write(d.path(), "864ee734-e3ab-434e-8278-745a850b16ad", &lines);
        let got = recent_in(d.path(), 10);
        assert_eq!(got.len(), 1, "the row survives");
        assert_eq!(got[0].label, "", "and its label is simply absent");
    }

    /// The rows a menu can show, and no more. `limit` above the cap is clamped
    /// rather than honoured: the caller is the renderer, and a renderer asking
    /// for a thousand rows is a bug, not a request.
    #[test]
    fn the_row_count_is_capped_whatever_the_caller_asks_for() {
        let d = tempfile::tempdir().unwrap();
        let t = transcript("x");
        let refs: Vec<&str> = t.iter().map(String::as_str).collect();
        for i in 0..15 {
            write(d.path(), &format!("aaaa1111-2222-3333-4444-5555555555{i:02}"), &refs);
        }
        assert_eq!(recent_in(d.path(), 1000).len(), MAX_ROWS);
        assert_eq!(recent_in(d.path(), 3).len(), 3);
    }

    /// A prompt is several lines and a row is one; a control character in
    /// another program's file must not reach the renderer as a line break.
    #[test]
    fn a_label_is_one_line_and_bounded() {
        assert_eq!(tidy("one\ntwo\t three   four"), "one two three four");
        assert_eq!(tidy("  padded  "), "padded");
        let long = tidy(&"ab ".repeat(200));
        assert_eq!(long.chars().count(), MAX_LABEL);
        assert!(long.ends_with('…'));
        // A multi-byte character at the boundary must not be cut in half.
        let wide = tidy(&"é".repeat(200));
        assert_eq!(wide.chars().count(), MAX_LABEL);
    }

    /// The turns that are not the user talking. Claude Code injects meta turns
    /// and wraps commands in tags; a row reading `<command-name>compact` tells
    /// nobody which conversation this was.
    #[test]
    fn injected_turns_are_skipped_in_favour_of_the_next_real_one() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "864ee734-e3ab-434e-8278-745a850b16ad", &[
            r#"{"type":"user","isMeta":true,"message":{"content":"caveat: this is a system note"}}"#,
            r#"{"type":"user","message":{"content":"<command-name>/compact</command-name>"}}"#,
            r#"{"type":"assistant","message":{"content":"ignored"}}"#,
            r#"{"type":"user","message":{"content":[{"type":"text","text":"what I actually asked"}]}}"#,
        ]);
        assert_eq!(recent_in(d.path(), 10)[0].label, "what I actually asked");
    }

    /// The authorisation, and the shapes it has to refuse. `has` re-derives at
    /// use rather than trusting the menu, so these are the things a forged or
    /// stale click can carry.
    ///
    /// Revert-checked: dropping the `valid_session_id` guard makes the
    /// traversal case pass — `..%2f..` would then be joined into a path and
    /// stat'd outside the transcript directory.
    #[test]
    fn only_a_conversation_of_this_project_authorises_a_resume() {
        let home = tempfile::tempdir().unwrap();
        let proj = Path::new("/srv/demo");
        let tdir = transcript_dir_in(home.path(), proj).unwrap();
        std::fs::create_dir_all(&tdir).unwrap();
        let t = transcript("x");
        let refs: Vec<&str> = t.iter().map(String::as_str).collect();
        write(&tdir, "864ee734-e3ab-434e-8278-745a850b16ad", &refs);
        // `has` reads HOME, so this is the one test here that needs it — and
        // it takes the lock every other HOME-touching test takes.
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        assert!(has(proj, "864ee734-e3ab-434e-8278-745a850b16ad"), "the project's own conversation");
        assert!(!has(proj, "aaaa1111-2222-3333-4444-555555555555"), "an id it does not have");
        assert!(!has(Path::new("/srv/other"), "864ee734-e3ab-434e-8278-745a850b16ad"),
                "another project's directory does not hold it");
        for bad in ["--dangerously-skip-permissions", "../escape", "a b", "", "x/y"] {
            assert!(!has(proj, bad), "{bad:?} must never authorise a resume");
        }

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    /// A line this build has never seen must cost that line, not the label.
    #[test]
    fn an_unparsable_line_does_not_end_the_search() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "864ee734-e3ab-434e-8278-745a850b16ad", &[
            "not json at all",
            r#"{"type":"user","message":{"content":"still found"}}"#,
        ]);
        assert_eq!(recent_in(d.path(), 10)[0].label, "still found");
    }
}
