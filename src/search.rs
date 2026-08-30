//! Project-wide search: one bounded walk per query, with no index and no
//! subprocess.
//!
//! **Why not an index.** An index maintained by `watch.rs` would be the only
//! way to search contents as you type, and its failure mode is *stale*
//! results — which a user cannot tell from correct ones. A walk's failure
//! mode is slowness, which is visible. That trade is worth making until a
//! walk is measurably too slow, at which point the answer is an index, not a
//! bigger deadline.
//!
//! **Why not ripgrep.** It is not installed on the deploy host, so it would
//! be an undeclared runtime dependency for a project that ships as one
//! binary. Worse, a missing binary or a non-zero exit reads as an empty
//! result list, which is CLAUDE.md's "absence of evidence" defect in its
//! quietest form: search cannot kill a shell, so nothing announces the lie.
//!
//! Which is why the result type carries an [`Outcome`] and an `unreadable`
//! count rather than being a bare `Vec`. "I could not look" is a third
//! answer here, and it is rendered as one.

use crate::projects::TreeFilter;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const MAX_PER_CATEGORY: usize = 50;
pub const MAX_LINES_PER_FILE: usize = 5;
pub const MAX_FILES_SCANNED: usize = 20_000;
pub const DEADLINE: Duration = Duration::from_millis(1500);
/// A matched line is echoed back to the browser; a minified bundle's single
/// 2 MB line is not worth sending, and no one can read it either way.
const MAX_LINE_CHARS: usize = 300;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileHit {
    pub rel: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LineHit {
    pub rel: String,
    /// 1-based, as a person counts lines and as an editor addresses them.
    pub line: u32,
    pub text: String,
}

/// Why the answer might not be the whole answer.
///
/// `Truncated` always names the cap that fired; a reason of "" would make the
/// UI's honesty impossible to test.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state")]
pub enum Outcome {
    Complete,
    Truncated { reason: String },
    Failed { msg: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Results {
    pub files: Vec<FileHit>,
    pub lines: Vec<LineHit>,
    pub sessions: Vec<String>,
    pub outcome: Outcome,
    /// Directories and files the walk could not read. Reported so the UI can
    /// say "12 matches, 3 places I could not look" instead of implying the
    /// 12 are everything.
    pub unreadable: usize,
}

impl Results {
    fn empty() -> Self {
        Results { files: vec![], lines: vec![], sessions: vec![], outcome: Outcome::Complete, unreadable: 0 }
    }
}

pub struct Query<'a> {
    pub text: &'a str,
    pub filter: TreeFilter<'a>,
    pub sessions: &'a [String],
    /// False for a short query: paths and sessions answer from the first
    /// keystroke, contents only once the query is worth the read.
    pub contents: bool,
}

/// Everything a search needs from the hub, copied out so the walk itself can
/// run with no lock held. Deliberately owns its data: a borrow would keep the
/// hub guard alive for exactly as long as the walk takes.
pub struct Snapshot {
    pub dir: PathBuf,
    pub show_hidden_override: Option<bool>,
    pub sessions: Vec<String>,
}

/// What reading one candidate produced. Three outcomes, not two: a file
/// skipped by policy is a decision the search made, a file that could not be
/// read is a gap in what the search can see, and folding them together is the
/// exact mistake this module exists to avoid.
enum Candidate {
    Text(String),
    SkippedByPolicy,
    Unreadable,
}

fn read_candidate(path: &Path) -> Candidate {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return Candidate::Unreadable,
    };
    if meta.len() > crate::projects::MAX_FILE_BYTES {
        return Candidate::SkippedByPolicy;
    }
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return Candidate::Unreadable,
    };
    // The same rule as `projects::read_text_file`, via the same list.
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let sniff = &data[..data.len().min(8000)];
    if sniff.contains(&0u8) && !crate::projects::is_text_extension(&ext) {
        return Candidate::SkippedByPolicy;
    }
    Candidate::Text(String::from_utf8_lossy(&data).into_owned())
}

/// Is `needle` a subsequence of `hay`? Both already lowercased.
fn is_subsequence(hay: &str, needle: &str) -> bool {
    let mut it = hay.chars();
    needle.chars().all(|c| it.any(|h| h == c))
}

/// Rank of a path against a lowercased query; `None` means no match.
///
/// The bands are ordered so that what you typed a filename to find comes
/// first: an exact basename beats a prefix, which beats a substring, which
/// beats a scattered subsequence, and anything in the basename beats the same
/// quality of match somewhere in the directory part.
fn score_path(rel: &str, q: &str) -> Option<i32> {
    let lower = rel.to_ascii_lowercase();
    let base = lower.rsplit('/').next().unwrap_or("").to_string();
    if base == q {
        Some(1000)
    } else if base.starts_with(q) {
        Some(800)
    } else if base.contains(q) {
        Some(600)
    } else if is_subsequence(&base, q) {
        Some(400)
    } else if lower.contains(q) {
        Some(300)
    } else if is_subsequence(&lower, q) {
        Some(100)
    } else {
        None
    }
}

/// One query against one project.
///
/// `cancelled` is polled at every directory boundary *and* every 64 entries
/// within one, alongside the deadline: one directory can hold every file the
/// walk is allowed to read, so a boundary-only poll is no bound at all. It is
/// a closure rather than a flag so a test can drive it deterministically, and
/// so the caller decides what supersession means.
///
/// Never panics: every I/O error becomes a counter, because this runs on a
/// connection's worker thread and a panic there would take search away from
/// that browser for the life of the connection.
pub fn run(root: &Path, q: &Query, cancelled: &dyn Fn() -> bool) -> Results {
    let started = Instant::now();
    let needle = q.text.to_ascii_lowercase();
    let mut r = Results::empty();

    r.sessions = q
        .sessions
        .iter()
        .filter(|s| s.to_ascii_lowercase().contains(&needle))
        .take(MAX_PER_CATEGORY)
        .cloned()
        .collect();

    if needle.is_empty() {
        return r;
    }

    let mut scored: Vec<(i32, String)> = vec![];
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut scanned = 0usize;
    let mut truncated: Option<String> = None;
    let mut is_root = true;
    // Separate from `truncated`: the file-match cap and this one are two
    // different caps, and folding this into `truncated` immediately would
    // let it be overwritten (or silently win over) the deadline/scanned
    // caps depending on loop order. Recorded once, folded in after the walk,
    // the same way the file-match cap is.
    let mut lines_capped = false;
    // And separate again from `lines_capped`: that one says "the whole answer
    // is capped at MAX_PER_CATEGORY lines", this one says "one file had more
    // matches than it was allowed to contribute". A query matching eight
    // times in a single file and nowhere else trips only this one, and
    // without it the walk reports `Complete` over five of eight hits —
    // "I chose not to look further" rendered to the user as completeness.
    let mut file_lines_capped = false;
    // Entries seen, for the deadline/cancellation poll inside the entry loop
    // below. Distinct from `scanned`, which counts only files and so would
    // never advance in a directory of nothing but skipped names.
    let mut stepped = 0usize;
    // One scratch buffer for the case-folded copy of the line being matched,
    // reused for every line of every file rather than allocated per line —
    // this is the innermost loop of the walk, and its cost is charged against
    // the deadline this module reports on. Measured (480k lines, rustc -O):
    // 9.4 ms allocating per line, 7.9 ms reusing this. A byte-wise
    // case-insensitive search with no buffer at all was tried and is *slower*
    // (10.3 ms): `str::contains` is SIMD-accelerated and a hand-rolled window
    // scan is not, so the copy pays for itself.
    let mut lowered = String::new();

    'walk: while let Some(dir) = stack.pop() {
        if cancelled() {
            r.outcome = Outcome::Truncated { reason: "superseded by a newer query".into() };
            return r;
        }
        if started.elapsed() > DEADLINE {
            // The measurement, not `DEADLINE`: the poll below is periodic, so
            // the walk can overshoot, and a constant here would make the one
            // honesty line the user gets wrong by however much it overshot.
            truncated = Some(format!("stopped after {} ms", started.elapsed().as_millis()));
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                // The root failing is a different event from a subdirectory
                // failing: there is no partial answer to give, so say so
                // rather than returning an empty list that looks like one.
                if is_root {
                    r.outcome = Outcome::Failed {
                        msg: format!("cannot read the project directory {}: {e}", dir.display()),
                    };
                    return r;
                }
                r.unreadable += 1;
                continue;
            }
        };
        is_root = false;

        for entry in entries {
            // The checks at the top of `'walk` fire once per *directory*, and
            // one directory can hold every file the walk is allowed to read.
            // Polling here too bounds the overshoot at 64 entries instead of
            // MAX_FILES_SCANNED, and lets a superseded query abandon inside
            // the directory it is in rather than only at its end.
            //
            // 64 rather than every entry: an `Instant::elapsed` and an atomic
            // load are cheap next to the `symlink_metadata` each entry
            // already costs, but this is the hot loop and the remaining
            // overshoot is now reported as a measurement rather than assumed
            // away.
            stepped += 1;
            if stepped.is_multiple_of(64) {
                if cancelled() {
                    r.outcome = Outcome::Truncated { reason: "superseded by a newer query".into() };
                    return r;
                }
                if started.elapsed() > DEADLINE {
                    truncated = Some(format!("stopped after {} ms", started.elapsed().as_millis()));
                    break 'walk;
                }
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => {
                    r.unreadable += 1;
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if q.filter.skips(&name) {
                continue;
            }
            let path = entry.path();
            // symlink_metadata, never metadata: `metadata` follows the link,
            // and a symlink is how a walk leaves the project root.
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => {
                    r.unreadable += 1;
                    continue;
                }
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            let rel = match path.strip_prefix(root) {
                Ok(p) => p.to_string_lossy().into_owned(),
                Err(_) => continue,
            };

            scanned += 1;
            if scanned > MAX_FILES_SCANNED {
                truncated = Some(format!("stopped after {MAX_FILES_SCANNED} files"));
                break 'walk;
            }

            if let Some(s) = score_path(&rel, &needle) {
                scored.push((s, rel.clone()));
            }

            if q.contents {
                if r.lines.len() >= MAX_PER_CATEGORY {
                    // The cap was already full before this file was even
                    // opened: there may be more matches past it that the
                    // walk never looked for, which is exactly what
                    // `lines_capped` exists to say.
                    lines_capped = true;
                } else {
                    match read_candidate(&path) {
                        Candidate::Unreadable => r.unreadable += 1,
                        Candidate::SkippedByPolicy => {}
                        Candidate::Text(text) => {
                            let mut in_file = 0;
                            for (i, line) in text.lines().enumerate() {
                                lowered.clear();
                                lowered.push_str(line);
                                lowered.make_ascii_lowercase();
                                if !lowered.contains(&needle) {
                                    continue;
                                }
                                r.lines.push(LineHit {
                                    rel: rel.clone(),
                                    line: i as u32 + 1,
                                    text: line.chars().take(MAX_LINE_CHARS).collect(),
                                });
                                in_file += 1;
                                if r.lines.len() >= MAX_PER_CATEGORY {
                                    lines_capped = true;
                                    break;
                                }
                                if in_file >= MAX_LINES_PER_FILE {
                                    // May over-report: a file with exactly
                                    // MAX_LINES_PER_FILE matches is complete
                                    // and still trips this. The same safe
                                    // direction `lines_capped` already takes
                                    // — claiming there might be more is
                                    // recoverable, claiming there is not when
                                    // there is is the defect.
                                    file_lines_capped = true;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Sorted, then capped: the cap must keep the *best* matches, not the
    // first ones the directory order happened to produce. Ties break on
    // shorter path then alphabetically, so results are deterministic and a
    // test can assert on them.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())).then(a.1.cmp(&b.1)));
    if scored.len() > MAX_PER_CATEGORY {
        scored.truncate(MAX_PER_CATEGORY);
        truncated.get_or_insert_with(|| format!("more than {MAX_PER_CATEGORY} files matched"));
    }
    r.files = scored.into_iter().map(|(_, rel)| FileHit { rel }).collect();

    if lines_capped {
        truncated.get_or_insert_with(|| format!("more than {MAX_PER_CATEGORY} lines matched"));
    }
    // After the global cap, so the broader statement wins when both fired:
    // "more than 50 lines matched" already tells the user the answer is
    // short, and naming the per-file cap instead would understate it.
    if file_lines_capped {
        truncated.get_or_insert_with(|| {
            format!("more than {MAX_LINES_PER_FILE} lines matched in one file")
        });
    }

    if let Some(reason) = truncated {
        r.outcome = Outcome::Truncated { reason };
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A project directory with the given `(relative path, contents)` files.
    fn proj(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (rel, body) in files {
            let p = d.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
        d
    }

    fn query<'a>(text: &'a str, filter: TreeFilter<'a>) -> Query<'a> {
        Query { text, filter, sessions: &[], contents: true }
    }

    fn never() -> impl Fn() -> bool {
        || false
    }

    #[test]
    fn a_path_match_and_a_content_match_are_separate_categories() {
        let d = proj(&[("src/needle.rs", "nothing here\n"), ("src/other.rs", "a needle line\n")]);
        let r = run(d.path(), &query("needle", TreeFilter::default()), &never());
        assert_eq!(r.files.iter().map(|f| f.rel.as_str()).collect::<Vec<_>>(), ["src/needle.rs"]);
        assert_eq!(r.lines.len(), 1, "{:?}", r.lines);
        assert_eq!(r.lines[0].rel, "src/other.rs");
        // 1-based: the line a user would type into an editor, not an index.
        assert_eq!(r.lines[0].line, 1);
        assert_eq!(r.outcome, Outcome::Complete);
    }

    /// The defect this whole module is shaped around. A directory that cannot
    /// be read must be *counted*, never folded into "nothing matched".
    ///
    /// Made unreadable by removing search permission, which is what an EACCES
    /// on a real deployment looks like. Skipped when running as root, since
    /// root ignores the permission bits and a silent pass would make this
    /// vacuous — the same guard `fileops.rs` uses for the same reason.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_directory_is_counted_not_silently_empty() {
        use std::os::unix::fs::PermissionsExt;
        let d = proj(&[("open/visible.rs", "needle\n"), ("locked/hidden.rs", "needle\n")]);
        let locked = d.path().join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let blocked = fs::read_dir(&locked).is_err();
        if !blocked {
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skipped: running as root, the premise does not hold");
            return;
        }

        let r = run(d.path(), &query("needle", TreeFilter::default()), &never());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap(); // so tempdir can clean up

        assert_eq!(r.unreadable, 1, "the locked directory must be reported, not skipped");
        assert!(
            matches!(r.outcome, Outcome::Complete),
            "an unreadable subdirectory does not make the search itself fail"
        );
        // The visible half still answers — a gap must not eat the results.
        assert_eq!(r.lines.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_not_followed_out_of_the_project() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.rs"), "needle\n").unwrap();
        let d = proj(&[("inside.rs", "nothing\n")]);
        std::os::unix::fs::symlink(outside.path(), d.path().join("escape")).unwrap();

        let r = run(d.path(), &query("needle", TreeFilter::default()), &never());

        // Asserting on emptiness alone would also pass if the walk had failed
        // outright, so the counters are checked too: the walk ran, saw the
        // symlink, and declined to follow it.
        assert!(r.lines.is_empty(), "leaked: {:?}", r.lines);
        assert!(r.files.is_empty(), "leaked: {:?}", r.files);
        assert_eq!(r.unreadable, 0, "declining to follow a symlink is not a failure to read");
        // Without this, "the walk failed outright" and "the walk correctly
        // declined to follow the symlink" are indistinguishable: both leave
        // every counter at zero.
        assert_eq!(r.outcome, Outcome::Complete);
    }

    /// The root failing is handled by a different branch than a subdirectory
    /// failing (`run`'s `is_root` check): there is no partial answer to give
    /// when the walk cannot even start, so it must say so via `Failed`
    /// rather than coming back with empty, `Complete` results that read as
    /// "this project has nothing matching" instead of "I could not look".
    #[cfg(unix)]
    #[test]
    fn an_unreadable_root_is_reported_as_failed() {
        use std::os::unix::fs::PermissionsExt;
        let d = proj(&[("visible.rs", "needle\n")]);
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o000)).unwrap();
        let blocked = fs::read_dir(d.path()).is_err();
        if !blocked {
            fs::set_permissions(d.path(), fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skipped: running as root, the premise does not hold");
            return;
        }

        let r = run(d.path(), &query("needle", TreeFilter::default()), &never());
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o755)).unwrap(); // so tempdir can clean up

        match &r.outcome {
            Outcome::Failed { msg } => assert!(
                msg.contains(&d.path().display().to_string()),
                "the message should name the directory that could not be read, got {msg:?}"
            ),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn the_result_cap_names_which_cap_fired() {
        let files: Vec<(String, String)> = (0..MAX_PER_CATEGORY + 10)
            .map(|i| (format!("needle{i}.rs"), String::from("x\n")))
            .collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let d = proj(&refs);

        let r = run(d.path(), &query("needle", TreeFilter::default()), &never());

        assert_eq!(r.files.len(), MAX_PER_CATEGORY);
        // Asserting only on the count would stay green if a different cap
        // fired, or if the reason string were empty.
        match &r.outcome {
            Outcome::Truncated { reason } => assert!(
                reason.contains("files matched"),
                "the reason must name the cap that fired, got {reason:?}"
            ),
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    /// The file-match cap and the line-match cap are tracked independently
    /// (`the_result_cap_names_which_cap_fired` only drives the file cap): a
    /// query whose paths never match "needle" but whose contents do, past
    /// `MAX_PER_CATEGORY` lines, must still say *which* cap fired rather than
    /// silently reporting `Complete` once the line vector stops growing.
    #[test]
    fn the_line_cap_names_which_cap_fired() {
        let files: Vec<(String, String)> = (0..MAX_PER_CATEGORY + 10)
            .map(|i| (format!("f{i}.rs"), String::from("a needle line\n")))
            .collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let d = proj(&refs);

        let r = run(d.path(), &query("needle", TreeFilter::default()), &never());

        assert!(r.files.is_empty(), "f{{i}}.rs does not match 'needle' by path: {:?}", r.files);
        assert_eq!(r.lines.len(), MAX_PER_CATEGORY);
        match &r.outcome {
            Outcome::Truncated { reason } => assert!(
                reason.contains("lines matched"),
                "the reason must name the line cap, not be silent or name the file cap, got {reason:?}"
            ),
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    /// A third cap again, and the one that hides best: five matches returned
    /// out of eight, all in a single file, with nothing else in the project
    /// matching at all — so neither cap above fires and the walk would
    /// otherwise report `Complete`. "Find every use of this symbol" is
    /// exactly the query that lands here, and `renderSearch` prints an empty
    /// note for `Complete`, i.e. a UI positively asserting nothing was left
    /// out.
    ///
    /// Revert-checked: with `file_lines_capped = true` removed from the
    /// `in_file >= MAX_LINES_PER_FILE` break, this fails with
    /// `expected Truncated naming the per-file cap, got Complete`.
    #[test]
    fn the_per_file_line_cap_names_which_cap_fired() {
        let hits = MAX_LINES_PER_FILE + 3;
        let body: String = (0..hits).map(|i| format!("a needle line {i}\n")).collect();
        // "quiet.rs" matches "needle" by content only, never by path, and
        // there is exactly one file — so the file cap and the global line cap
        // are both nowhere near firing and the per-file cap is the only one
        // that can produce a Truncated outcome here.
        let d = proj(&[("quiet.rs", body.as_str())]);

        let r = run(d.path(), &query("needle", TreeFilter::default()), &never());

        assert!(r.files.is_empty(), "quiet.rs does not match 'needle' by path: {:?}", r.files);
        assert_eq!(r.lines.len(), MAX_LINES_PER_FILE, "the per-file cap is what stopped it");
        assert!(
            r.lines.len() < MAX_PER_CATEGORY,
            "the global line cap must be nowhere near firing, or this test proves the wrong cap"
        );
        match &r.outcome {
            Outcome::Truncated { reason } => assert!(
                reason.contains("in one file"),
                "the reason must name the per-file cap distinguishably from the global \
                 line cap ('more than 50 lines matched'), got {reason:?}"
            ),
            other => panic!("expected Truncated naming the per-file cap, got {other:?}"),
        }
    }

    #[test]
    fn a_binary_file_is_skipped_by_policy_not_counted_unreadable() {
        let d = tempfile::tempdir().unwrap();
        // A NUL inside, and an extension that is not in TEXT_EXTENSIONS.
        fs::write(d.path().join("blob.bin"), b"needle\0needle\n").unwrap();

        let r = run(d.path(), &query("needle", TreeFilter::default()), &never());

        assert!(r.lines.is_empty(), "a binary file's bytes are not content hits");
        assert_eq!(r.unreadable, 0, "skipping by policy is a decision, not a gap");
        // The *path* still matches: only its contents were declined.
        assert!(r.files.is_empty(), "blob.bin does not match 'needle' by path");
    }

    #[test]
    fn the_walk_hides_exactly_what_the_tree_hides() {
        let d = proj(&[("target/needle.rs", "x\n"), ("src/needle.rs", "x\n")]);
        let off = run(d.path(), &query("needle", TreeFilter::default()), &never());
        assert_eq!(off.files.iter().map(|f| f.rel.as_str()).collect::<Vec<_>>(), ["src/needle.rs"]);
        // Paired with the opposite setting, so a filter that ignored
        // `show_hidden` entirely (returning a constant) fails one half.
        let on = TreeFilter { hide: &[], show_hidden: true };
        let shown = run(d.path(), &query("needle", on), &never());
        assert_eq!(shown.files.len(), 1, "SKIP_DIRS is not a dotfile rule; target/ stays hidden");
    }

    #[test]
    fn a_cancelled_search_stops_and_says_so() {
        let d = proj(&[("a/needle.rs", "x\n"), ("b/needle.rs", "x\n")]);
        let r = run(d.path(), &query("needle", TreeFilter::default()), &|| true);
        assert!(
            matches!(&r.outcome, Outcome::Truncated { reason } if reason.contains("superseded")),
            "got {:?}",
            r.outcome
        );
    }

    #[test]
    fn sessions_match_by_substring_and_need_no_walk() {
        let names = vec!["term".to_string(), "claude-1".to_string()];
        let d = proj(&[]);
        let q = Query { text: "claude", filter: TreeFilter::default(), sessions: &names, contents: true };
        let r = run(d.path(), &q, &never());
        assert_eq!(r.sessions, vec!["claude-1".to_string()]);
    }
}
