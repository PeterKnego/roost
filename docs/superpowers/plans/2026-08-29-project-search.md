# Project-wide search implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A double-tap of Shift opens a client-local overlay that searches the
current project's file paths, file contents and live sessions, and opening a
content hit lands on the line it was found on.

**Architecture:** A new `src/search.rs` performs a bounded, cancellable
filesystem walk with no index and no subprocess. The query arrives as a
websocket intent that is dispatched **before** the hub lock is taken, runs on a
per-connection worker thread with no lock held, and replies to the asking
connection alone. Results are rendered client-side into text nodes.

**Tech Stack:** Rust (no new crates — `serde` and `serde_json` are already
dependencies), plain JS with no framework, Deno + CDP for the browser test.

**Spec:** `docs/superpowers/specs/2026-08-29-project-search-design.md`

## Global Constraints

Copied from the spec and `CLAUDE.md`. Every task's requirements implicitly
include this section.

- **Never hold a lock across blocking I/O.** The hub mutex must be free for the
  entire duration of a walk. This project has already shipped one deadlock this
  way.
- **No panics may escape a socket or worker thread.**
- **HTTP stays GET-only apart from `POST /upload` and `POST /paste`.** Search
  adds no HTTP route at all; it is a websocket intent.
- **Absence of evidence is not evidence of absence.** A directory that cannot
  be read is counted and reported, never rendered as "no matches". A file
  skipped by *policy* (too large, binary) and a file that could not be *read*
  are different outcomes and must not share a branch.
- **Caps:** `MAX_PER_CATEGORY = 50`, `MAX_LINES_PER_FILE = 5`,
  `MAX_FILES_SCANNED = 20_000`, `DEADLINE = 1500 ms`. Constants in
  `search.rs` — **not** config keys, and never per-project.
- **Dynamic client content goes in text nodes**, never interpolated markup
  (`static/app.js:76-78`). A matched line is arbitrary file content.
- **Tests:** `cargo test -- --test-threads=1` (a bare `cargo test` hangs in this
  repo). Browser tests run against a scratch resh, never a live instance.
- **Style:** module-level `//!` doc explaining *why*; `#[cfg(test)] mod tests`
  at the bottom of the same file; comments give rationale, not mechanics.

## Deviation from the spec, and why

The spec's "Line addressing" section specifies `line: Option<u32>` on
`Tab::File`. **This plan does not do that.** Two facts found while planning:

1. `Tab::File { .. }` is constructed at **74 sites** in `src/` (`grep -c`), so
   a new field churns all of them for a value none of them care about.
2. `Tab` is *persisted* (`wsstate.rs` writes it as JSON). A line belongs to one
   act of navigation; persisting it means reopening resh tomorrow scrolls you
   to yesterday's search hit.

Instead the line travels as its own intent and its own event —
`Intent::OpenAtLine` and `Event::RevealLine` — so nothing persists, nothing
churns, and mirroring the scroll to other browsers is a deliberate choice
rather than a side effect of the layout snapshot. The dedupe the spec wanted
comes free either way: `workspace::tab_identity_eq` (`workspace.rs:193-205`)
already compares File tabs on `rel` alone.

The spec has been updated to match.

---

### Task 1: `src/search.rs` — the walk, the matchers, the caps

Self-contained: no wiring, no protocol, no client. Reviewable on its own.

**Files:**
- Create: `src/search.rs`
- Modify: `src/lib.rs` (add `pub mod search;`)
- Modify: `src/projects.rs` (expose the text-extension rule)
- Test: `src/search.rs` (`#[cfg(test)] mod tests` at the bottom)

**Interfaces:**
- Consumes: `projects::TreeFilter`, `projects::MAX_FILE_BYTES`.
- Produces: `search::run(root: &Path, q: &Query, cancelled: &dyn Fn() -> bool) -> Results`;
  `search::Query<'a> { text, filter, sessions, contents }`;
  `search::Results { files: Vec<FileHit>, lines: Vec<LineHit>, sessions: Vec<String>, outcome: Outcome, unreadable: usize }`;
  `search::Snapshot { dir: PathBuf, show_hidden_override: Option<bool>, sessions: Vec<String> }`;
  `FileHit { rel: String }`; `LineHit { rel: String, line: u32, text: String }`;
  `Outcome::{Complete, Truncated { reason: String }, Failed { msg: String }}`.

- [ ] **Step 1: Expose the text-extension rule from `projects.rs`**

`TEXT_EXTENSIONS` is private. Search must apply the *same* rule as
`read_text_file`, not a second copy of it — one rule for "is this text".

In `src/projects.rs`, directly below the `TEXT_EXTENSIONS` const (line 53):

```rust
/// Whether an extension is one we treat as text even when it sniffs binary.
/// Exposed so `search.rs` applies this rule rather than keeping a second
/// copy: two lists that disagree is how a file becomes searchable in one
/// place and invisible in another.
pub fn is_text_extension(ext: &str) -> bool {
    TEXT_EXTENSIONS.contains(&ext)
}
```

- [ ] **Step 2: Write the failing tests**

Create `src/search.rs` with only the tests (no implementation yet), so Step 3
can be watched to turn them green.

```rust
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

    fn never() -> impl Fn() -> bool { || false }

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
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --lib search:: -- --test-threads=1`
Expected: FAIL to compile — `run`, `Query`, `Results`, `Outcome` are not
defined. That is the intended failure at this step.

- [ ] **Step 4: Write the implementation**

Above the test module in `src/search.rs`:

```rust
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
/// `cancelled` is polled at every directory boundary. It is a closure rather
/// than a flag so a test can drive it deterministically, and so the caller
/// decides what supersession means.
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

    'walk: while let Some(dir) = stack.pop() {
        if cancelled() {
            r.outcome = Outcome::Truncated { reason: "superseded by a newer query".into() };
            return r;
        }
        if started.elapsed() > DEADLINE {
            truncated = Some(format!("stopped after {} ms", DEADLINE.as_millis()));
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                // The root failing is a different event from a subdirectory
                // failing: there is no partial answer to give, so say so
                // rather than returning an empty list that looks like one.
                if is_root {
                    r.outcome = Outcome::Failed { msg: format!("cannot read the project directory: {e}") };
                    return r;
                }
                r.unreadable += 1;
                continue;
            }
        };
        is_root = false;

        for entry in entries {
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

            if q.contents && r.lines.len() < MAX_PER_CATEGORY {
                match read_candidate(&path) {
                    Candidate::Unreadable => r.unreadable += 1,
                    Candidate::SkippedByPolicy => {}
                    Candidate::Text(text) => {
                        let mut in_file = 0;
                        for (i, line) in text.lines().enumerate() {
                            if !line.to_ascii_lowercase().contains(&needle) {
                                continue;
                            }
                            r.lines.push(LineHit {
                                rel: rel.clone(),
                                line: i as u32 + 1,
                                text: line.chars().take(MAX_LINE_CHARS).collect(),
                            });
                            in_file += 1;
                            if in_file >= MAX_LINES_PER_FILE || r.lines.len() >= MAX_PER_CATEGORY {
                                break;
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

    if let Some(reason) = truncated {
        r.outcome = Outcome::Truncated { reason };
    }
    r
}
```

Then add to `src/lib.rs`, in the module list, alphabetically:

```rust
pub mod search;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib search:: -- --test-threads=1`
Expected: PASS, 8 tests (7 on a non-unix target).

- [ ] **Step 6: Revert the fix and watch the key test fail**

Not a thought experiment — this is the technique `CLAUDE.md` says is the only
one that actually works. Temporarily replace the `Err(e)` arm of the
`read_dir` match with the broken version that swallows it:

```rust
            Err(_) => continue,   // BROKEN ON PURPOSE
```

Run: `cargo test --lib search::tests::an_unreadable_directory -- --test-threads=1`
Expected: FAIL with `assertion `left == right` failed: the locked directory
must be reported, not skipped` — `left: 0, right: 1`.

Then restore the real arm and re-run to confirm green. Record what you saw in
a comment on the test if the failure message differs from the above.

- [ ] **Step 7: Commit**

```bash
git add src/search.rs src/projects.rs src/lib.rs
git commit -m "search: a bounded walk that reports what it could not read"
```

---

### Task 2: Wire types

**Files:**
- Modify: `src/proto.rs` (add two `Intent` variants, two `Event` variants)
- Test: `src/proto.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `search::Results` from Task 1.
- Produces: `Intent::Search { q: String, seq: u64 }`,
  `Intent::OpenAtLine { pane: PaneId, rel: String, line: u32 }`,
  `Event::SearchResults { seq: u64, results: crate::search::Results }`,
  `Event::RevealLine { rel: String, line: u32 }`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/proto.rs`:

```rust
    #[test]
    fn a_search_carries_its_sequence_number() {
        let i = decode(r#"{"t":"Search","q":"needle","seq":7}"#).unwrap();
        match i {
            Intent::Search { q, seq } => {
                assert_eq!(q, "needle");
                assert_eq!(seq, 7);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn open_at_line_carries_a_one_based_line() {
        let i = decode(r#"{"t":"OpenAtLine","pane":2,"rel":"src/hub.rs","line":412}"#).unwrap();
        match i {
            Intent::OpenAtLine { pane, rel, line } => {
                assert_eq!(pane, MIDDLE);
                assert_eq!(rel, "src/hub.rs");
                assert_eq!(line, 412);
            }
            other => panic!("got {other:?}"),
        }
    }

    /// A File tab's JSON is what `wsstate` has already written to disk for
    /// every saved workspace on every host. Search must not change its shape:
    /// this test fails loudly if a `line` field is ever added to the tab
    /// itself, which is the design this plan deliberately avoided.
    #[test]
    fn a_file_tabs_wire_shape_is_unchanged_by_search() {
        let i = decode(r#"{"t":"OpenTab","pane":2,"tab":{"k":"File","rel":"a.rs","mode":"Edit"}}"#).unwrap();
        assert!(
            matches!(i, Intent::OpenTab { tab: Tab::File { ref rel, mode: Mode::Edit }, .. } if rel == "a.rs"),
            "got {i:?}"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib proto:: -- --test-threads=1`
Expected: FAIL to compile — no variant `Search` on `Intent`.

- [ ] **Step 3: Add the variants**

In `src/proto.rs`, inside `pub enum Intent`, after `OpenPath`:

```rust
    /// One query from one browser. `seq` is that connection's own monotonic
    /// counter: the worker abandons a query whose seq is no longer the
    /// latest, and the reply carries it back so a late answer to a query the
    /// user has already typed past is dropped rather than rendered.
    ///
    /// Deliberately an intent and not an HTTP route, but equally deliberately
    /// *not* handled like one: `wsconn` diverts it before taking the hub
    /// lock, because a walk is blocking I/O. See the routing there.
    Search { q: String, seq: u64 },
    /// Open `rel` and scroll to `line`. Separate from `OpenTab` because the
    /// line is navigation, not layout: `Tab` is persisted, and a line has no
    /// business surviving a restart. See the plan's "Deviation from the spec".
    OpenAtLine { pane: PaneId, rel: String, line: u32 },
```

Inside `pub enum Event`, after `PathRefused`:

```rust
    /// Results for one query. Sent with `send_to` and never broadcast: a
    /// query is one browser's business, and the overlay that asked is
    /// client-local by design.
    SearchResults { seq: u64, results: crate::search::Results },
    /// Scroll whichever pane holds `rel` to `line`. Broadcast rather than
    /// sent to the asker, so a second browser mirroring the tab follows it
    /// there — but carried as an event rather than as workspace state, so
    /// nothing about it persists.
    RevealLine { rel: String, line: u32 },
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib proto:: -- --test-threads=1`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/proto.rs
git commit -m "proto: Search and OpenAtLine intents, SearchResults and RevealLine events"
```

---

### Task 3: Off-lock dispatch and the per-connection worker

The task the whole design exists for. A reviewer should reject this if the hub
lock is held for one instruction longer than the snapshot.

**Files:**
- Modify: `src/hub.rs` (add `search_snapshot`)
- Modify: `src/wsconn.rs` (divert `Search` before the lock; add the worker)
- Test: `src/wsconn.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `search::{run, Query, Snapshot}` (Task 1), `Intent::Search`,
  `Event::SearchResults` (Task 2), `Hub::send_to`, `Hub::lock`.
- Produces: `Hub::search_snapshot(&self) -> search::Snapshot`;
  `wsconn::run_search(hub: &Arc<Mutex<Hub>>, id: &ConnId, q: &str, seq: u64, latest: &Arc<AtomicU64>)`;
  `wsconn::Searcher { fn submit(&self, q: String, seq: u64) }`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/wsconn.rs`:

```rust
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Builds a project of `n` small files, so a walk takes long enough to
    /// observe. Not a fixed sleep: this is the thing under test.
    fn wide_project(n: usize) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for i in 0..n {
            let sub = d.path().join(format!("d{}", i % 40));
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(sub.join(format!("f{i}.rs")), "needle in here\n").unwrap();
        }
        d
    }

    /// A query is one browser's business. With a single subscriber this test
    /// could not tell `send_to` from `broadcast` — CLAUDE.md lists that exact
    /// trap — so there are two, and the second must hear nothing.
    #[test]
    fn results_go_only_to_the_connection_that_asked() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("needle.rs"), "x\n").unwrap();

        let hub = Arc::new(Mutex::new(Hub::new("proj", d.path().to_path_buf())));
        let (asker, rx_asker) = Hub::lock(&hub).subscribe();
        let (_other, rx_other) = Hub::lock(&hub).subscribe();
        while rx_asker.try_recv().is_ok() {}
        while rx_other.try_recv().is_ok() {}

        let latest = Arc::new(AtomicU64::new(1));
        run_search(&hub, &asker, "needle", 1, &latest);

        let mut mine = vec![];
        while let Ok(m) = rx_asker.try_recv() { mine.push(m); }
        let mut theirs = vec![];
        while let Ok(m) = rx_other.try_recv() { theirs.push(m); }

        assert!(
            mine.iter().any(|m| m.contains(r#""t":"SearchResults""#) && m.contains("needle.rs")),
            "the asker must get its results, got {mine:?}"
        );
        assert!(
            !theirs.iter().any(|m| m.contains(r#""t":"SearchResults""#)),
            "another browser must not see this query's results, got {theirs:?}"
        );
    }

    /// The hard constraint, as a test. `wsconn` dispatches every other intent
    /// under the hub lock; a search must not, or every browser on the project
    /// stalls for the length of the walk.
    ///
    /// Non-vacuous by construction: it asserts the lock was taken *while the
    /// search was still running*, not merely at some point. Verify by
    /// reverting to a lock-held implementation and watching it fail.
    #[test]
    fn the_hub_lock_is_free_while_a_search_walks() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = wide_project(4000);
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", state.path().join("state"));

        let hub = Arc::new(Mutex::new(Hub::new("proj", d.path().to_path_buf())));
        let (asker, rx) = Hub::lock(&hub).subscribe();
        while rx.try_recv().is_ok() {}

        let latest = Arc::new(AtomicU64::new(1));
        let h2 = hub.clone();
        let l2 = latest.clone();
        let id2 = asker.clone();
        let walker = std::thread::spawn(move || run_search(&h2, &id2, "needle", 1, &l2));

        // Poll for a lock while the walk is in flight. The result arriving is
        // what ends the loop, so a success recorded here provably happened
        // during the walk.
        let mut locked_during = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while rx.try_recv().is_err() && std::time::Instant::now() < deadline {
            if let Ok(g) = hub.try_lock() {
                locked_during += 1;
                drop(g);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        walker.join().unwrap();

        assert!(
            locked_during > 0,
            "the hub lock was never free while the search ran — it is being held across the walk"
        );
    }

    /// A query the user has already typed past must not be answered.
    #[test]
    fn a_superseded_query_sends_nothing() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("needle.rs"), "x\n").unwrap();

        let hub = Arc::new(Mutex::new(Hub::new("proj", d.path().to_path_buf())));
        let (asker, rx) = Hub::lock(&hub).subscribe();
        while rx.try_recv().is_ok() {}

        // The connection has moved on to seq 5; the in-flight seq 1 is stale.
        let latest = Arc::new(AtomicU64::new(5));
        run_search(&hub, &asker, "needle", 1, &latest);

        let mut got = vec![];
        while let Ok(m) = rx.try_recv() { got.push(m); }
        assert!(
            !got.iter().any(|m| m.contains(r#""t":"SearchResults""#)),
            "a superseded query must not answer, got {got:?}"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib wsconn:: -- --test-threads=1`
Expected: FAIL to compile — `run_search` is not defined.

- [ ] **Step 3: Add `Hub::search_snapshot`**

In `src/hub.rs`, next to `snapshot_event`:

```rust
    /// Everything a search needs, copied out under the lock so the walk can
    /// run without it.
    ///
    /// Deliberately does *not* resolve the config here: `config::for_project`
    /// reads files, and this method's whole purpose is that its caller can
    /// drop the lock immediately. The worker resolves the settings itself,
    /// off-lock, from `dir`.
    pub fn search_snapshot(&self) -> crate::search::Snapshot {
        crate::search::Snapshot {
            dir: self.dir.clone(),
            show_hidden_override: self.ws.show_hidden,
            // `ws.live_sessions`, never `session::list_sessions`: that forks
            // a `ps` per session while holding the global session-registry
            // mutex (see `refresh_live_sessions`), which would reintroduce
            // exactly the stall this snapshot exists to avoid.
            sessions: self.ws.live_sessions.clone(),
        }
    }
```

- [ ] **Step 4: Add the worker to `wsconn.rs`**

At the top of `src/wsconn.rs`, extend the imports:

```rust
use std::sync::atomic::{AtomicU64, Ordering};
```

Then, above `pub fn handle`:

```rust
/// One query, start to finish, with the hub lock held only at the two ends.
///
/// Split out of the worker loop so tests can drive it directly — in
/// particular the one that proves the lock is free while it runs.
pub(crate) fn run_search(
    hub: &Arc<Mutex<Hub>>,
    id: &ConnId,
    q: &str,
    seq: u64,
    latest: &Arc<AtomicU64>,
) {
    if latest.load(Ordering::SeqCst) != seq {
        return; // already stale before it started
    }
    // (1) Lock, copy, unlock. The guard is bound inside the block so it is
    // dropped before anything below touches the filesystem.
    let snap = {
        let h = Hub::lock(hub);
        h.search_snapshot()
    };

    // (2) No lock held from here to the end of the walk. `for_project` reads
    // config files, which is why it is on this side of the boundary.
    let settings = crate::config::for_project(&snap.dir);
    let filter = settings.tree_filter_with(snap.show_hidden_override);
    let cancelled = || latest.load(Ordering::SeqCst) != seq;
    let query = crate::search::Query {
        text: q,
        filter,
        sessions: &snap.sessions,
        // Contents are the expensive half; paths and sessions answer from the
        // first keystroke, contents once the query is specific enough to be
        // worth reading every file for.
        contents: q.chars().count() >= 3,
    };
    let results = crate::search::run(&snap.dir, &query, &cancelled);

    // (3) Lock again, only to reply. A query the user typed past is dropped
    // here rather than rendered over what they are looking at now.
    if cancelled() {
        return;
    }
    let ev = crate::proto::Event::SearchResults { seq, results };
    Hub::lock(hub).send_to(id, &ev);
}

/// A connection's search worker: one thread, reused for every query it sends.
///
/// One thread rather than one per query, because a fast typist would
/// otherwise spawn a thread per keystroke — and `latest` means the queued
/// ones mostly have nothing left to do by the time they are picked up.
struct Searcher {
    tx: std::sync::mpsc::Sender<(String, u64)>,
    latest: Arc<AtomicU64>,
}

impl Searcher {
    fn submit(&self, q: String, seq: u64) {
        // Published before the send, so a query already in the queue sees it
        // and abandons itself rather than answering after this one.
        self.latest.store(seq, Ordering::SeqCst);
        let _ = self.tx.send((q, seq));
    }
}

fn spawn_searcher(hub: Arc<Mutex<Hub>>, id: ConnId) -> Searcher {
    let (tx, rx) = std::sync::mpsc::channel::<(String, u64)>();
    let latest = Arc::new(AtomicU64::new(0));
    let latest2 = latest.clone();
    std::thread::spawn(move || {
        while let Ok((q, seq)) = rx.recv() {
            // A panic here would silently take search away from this browser
            // for the life of the connection, with nothing in the UI to say
            // so — CLAUDE.md's "no panics may escape a socket thread", in the
            // one place where the escape would be invisible rather than loud.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_search(&hub, &id, &q, seq, &latest2);
            }));
            if r.is_err() {
                let ev = crate::proto::Event::Error { msg: "search failed".into() };
                Hub::lock(&hub).send_to(&id, &ev);
            }
        }
    });
    Searcher { tx, latest }
}
```

- [ ] **Step 5: Divert `Search` before the hub lock**

In `src/wsconn.rs`, before the `loop {` that reads messages (after `id` and
`hub` exist):

```rust
    let searcher = spawn_searcher(hub.clone(), id.clone());
```

Then replace the `Ok(Message::Text(t))` arm's opening with:

```rust
            Ok(Message::Text(t)) => {
                // Decoded *before* the hub lock, so a Search can be diverted
                // to its worker. Every other intent is handled under the lock
                // as it always was; a search is the only one that performs
                // unbounded blocking I/O, and holding this lock across that
                // would stall every other browser on this project. See
                // CLAUDE.md's "never hold a lock across blocking I/O".
                let decoded = proto::decode(&t);
                if let Ok(proto::Intent::Search { q, seq }) = decoded {
                    searcher.submit(q, seq);
                    continue;
                }
                let dirty = {
                    let mut h = Hub::lock(&hub);
                    match decoded {
                        Ok(intent) => h.handle(&id, intent),
                        Err(e) => {
                            let ev = proto::Event::Error { msg: e };
                            h.send_to(&id, &ev);
                        }
                    }
                    std::mem::take(&mut h.notices_dirty)
                };
```

The rest of the arm (the `if dirty` block) is unchanged.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --lib wsconn:: -- --test-threads=1`
Expected: PASS, including `the_hub_lock_is_free_while_a_search_walks`.

- [ ] **Step 7: Revert the fix and watch the lock test fail**

Temporarily make `run_search` hold the lock across the walk — bind the guard
for the whole function instead of only for the snapshot:

```rust
    let h = Hub::lock(hub);                       // BROKEN ON PURPOSE
    let snap = h.search_snapshot();
    // ... walk here, with `h` still alive ...
```

Run: `cargo test --lib wsconn::tests::the_hub_lock_is_free -- --test-threads=1`
Expected: FAIL with "the hub lock was never free while the search ran".

Restore, re-run, confirm green. If the broken version *passes*, the walk is
finishing too fast to observe — raise `wide_project(4000)` until it does not,
and say so in a comment on the test.

- [ ] **Step 8: Commit**

```bash
git add src/hub.rs src/wsconn.rs
git commit -m "search: run the walk off the hub lock, on a per-connection worker"
```

---

### Task 4: `OpenAtLine` and `RevealLine` in the hub

**Files:**
- Modify: `src/hub.rs` (dispatch arm + `do_open_at_line`; emit `RevealLine` from `do_open_path`)
- Modify: `src/projects.rs` (expose the trailing-line parser)
- Test: `src/hub.rs`

**Interfaces:**
- Consumes: `Intent::OpenAtLine`, `Event::RevealLine` (Task 2),
  `projects::safe_resolve`.
- Produces: `projects::trailing_line(text: &str) -> Option<u32>`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/hub.rs`:

```rust
    #[test]
    fn open_at_line_opens_the_file_and_reveals_the_line() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.rs"), "one\ntwo\nthree\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        let (_other, rx_other) = h.subscribe();
        drain(&rx);
        drain(&rx_other);

        h.handle(&c, Intent::OpenAtLine { pane: proto::MIDDLE, rel: "a.rs".into(), line: 2 });

        let mine = drain(&rx);
        assert!(mine.iter().any(|m| m.contains(r#""t":"State""#) && m.contains("a.rs")), "{mine:?}");
        assert!(
            mine.iter().any(|m| m.contains(r#""t":"RevealLine""#) && m.contains(r#""line":2"#)),
            "{mine:?}"
        );
        // Broadcast, not send_to: a second browser mirroring this tab follows
        // it to the same line. Asserted explicitly so the choice cannot flip
        // silently in either direction.
        assert!(
            drain(&rx_other).iter().any(|m| m.contains(r#""t":"RevealLine""#)),
            "a mirroring browser must be told where to scroll"
        );
    }

    #[test]
    fn open_at_line_opening_a_file_twice_does_not_open_a_second_tab() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.rs"), "one\ntwo\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(&c, Intent::OpenAtLine { pane: proto::MIDDLE, rel: "a.rs".into(), line: 1 });
        h.handle(&c, Intent::OpenAtLine { pane: proto::MIDDLE, rel: "a.rs".into(), line: 2 });

        let tabs = h.ws.panes[proto::MIDDLE as usize]
            .tabs
            .iter()
            .filter(|t| matches!(t, Tab::File { rel, .. } if rel == "a.rs"))
            .count();
        assert_eq!(tabs, 1, "a second hit in an open file must re-scroll it, not clone the tab");
    }

    /// `rel` arrives from a browser. `apply_layout` validates nothing, so the
    /// confinement check has to be here — the same reasoning `OpenPath`
    /// carries. Asserts on the *message*, not merely that something failed:
    /// an intent rejected for the wrong reason would pass an `is_err`-style
    /// check while leaving the escape open.
    #[test]
    fn open_at_line_refuses_a_path_outside_the_project() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(&c, Intent::OpenAtLine {
            pane: proto::MIDDLE,
            rel: "../../etc/passwd".into(),
            line: 1,
        });

        let got = drain(&rx);
        assert!(
            got.iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("outside project")),
            "must refuse by confinement and say so, got {got:?}"
        );
        assert!(
            !got.iter().any(|m| m.contains("passwd")),
            "nothing outside the project may reach the layout, got {got:?}"
        );
    }

    #[test]
    fn a_terminal_link_with_a_line_number_reveals_that_line() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.rs"), "one\ntwo\nthree\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(&c, Intent::OpenPath { text: "a.rs:3".into() });

        let got = drain(&rx);
        assert!(got.iter().any(|m| m.contains(r#""t":"State""#) && m.contains("a.rs")), "{got:?}");
        assert!(
            got.iter().any(|m| m.contains(r#""t":"RevealLine""#) && m.contains(r#""line":3"#)),
            "a link that named a line must land on it, got {got:?}"
        );
    }

    #[test]
    fn a_terminal_link_without_a_line_number_reveals_nothing() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.rs"), "one\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(&c, Intent::OpenPath { text: "a.rs".into() });

        assert!(
            !drain(&rx).iter().any(|m| m.contains(r#""t":"RevealLine""#)),
            "a plain path must not scroll anywhere"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib hub::tests::open_at_line hub::tests::a_terminal_link -- --test-threads=1`
Expected: FAIL to compile — no variant `OpenAtLine` handled; `trailing_line`
not defined.

- [ ] **Step 3: Expose the trailing-line parser**

In `src/projects.rs`, next to `resolve_terminal_path`:

```rust
/// The `:42` a terminal link may carry, if it has one.
///
/// Separate from `resolve_terminal_path`, which resolves and confines the
/// path and has no use for the line: this reads the same suffix for the one
/// caller that does. `a/b:c.md` is not a line number, so only a trailing run
/// of digits after the final colon counts, and only when the rest is
/// non-empty.
pub fn trailing_line(text: &str) -> Option<u32> {
    let (rest, tail) = text.rsplit_once(':')?;
    if rest.is_empty() {
        return None;
    }
    // `file.rs:42:7` — column form; the line is the first of the two.
    if let Some((rest2, mid)) = rest.rsplit_once(':') {
        if !rest2.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) && !tail.is_empty() {
            if let Ok(n) = mid.parse::<u32>() {
                return Some(n);
            }
        }
    }
    tail.parse::<u32>().ok()
}
```

- [ ] **Step 4: Handle `OpenAtLine` and extend `do_open_path`**

In `src/hub.rs`, add to the `match intent` in `handle`, next to the
`Intent::OpenPath` arm:

```rust
            Intent::OpenAtLine { pane, rel, line } => self.do_open_at_line(from, pane, rel, line),
```

Add the method next to `do_open_path`:

```rust
    /// Open `rel` and tell every browser to scroll it to `line`.
    ///
    /// The confinement check is here for the reason `do_open_path` gives:
    /// `apply_layout` validates nothing, and this `rel` came off a wire. That
    /// a search produced it is no guarantee — a client can send this intent
    /// with anything in it.
    fn do_open_at_line(&mut self, from: &ConnId, pane: crate::proto::PaneId, rel: String, line: u32) {
        if let Err(msg) = crate::projects::safe_resolve(&self.dir, &rel) {
            let ev = Event::Error { msg: format!("path outside project: {rel} ({msg})") };
            return self.send_to(from, &ev);
        }
        // Edit, not Preview: a content hit was matched against the file's
        // source, and a rendered markdown preview has no line 412 to land on.
        // `coerce_tab` still demotes anything that cannot be edited as text.
        let intent = Intent::OpenTab { pane, tab: Tab::File { rel: rel.clone(), mode: Mode::Edit } };
        self.handle(from, intent);
        let ev = Event::RevealLine { rel, line };
        self.broadcast(&ev);
    }
```

In `do_open_path`, capture the line before the existing `self.handle(from, intent)`
call and broadcast after it. Replace the tail of that function with:

```rust
        let line = crate::projects::trailing_line(&text);
        let intent = Intent::OpenTab {
            pane: crate::proto::MIDDLE,
            tab: Tab::File { rel: rel.clone(), mode: Mode::Preview },
        };
        self.handle(from, intent);
        // The line the link named. Until this existed the client stripped it
        // and flashed "line 42 — opening file", because, as app.js put it,
        // "the viewer has no line addressing to spend it on". It does now.
        if let Some(line) = line {
            let ev = Event::RevealLine { rel, line };
            self.broadcast(&ev);
        }
```

Note the `text` is moved by the `PathRefused` arm above; bind `let line =
crate::projects::trailing_line(&text);` **before** the `match` that consumes
it if the borrow checker objects.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib hub:: -- --test-threads=1`
Expected: PASS.

- [ ] **Step 6: Run the whole suite**

Run: `cargo test -- --test-threads=1`
Expected: PASS. `workspace.rs` and `wsstate.rs` hold most of the `Tab::File`
construction sites; nothing there should have changed, and a failure here
means the tab shape moved after all.

- [ ] **Step 7: Commit**

```bash
git add src/hub.rs src/projects.rs
git commit -m "hub: OpenAtLine, and terminal links land on the line they named"
```

---

### Task 5: The header control and the overlay shell

**Files:**
- Modify: `src/render.rs:1150` (the `#searchbox` div) and the SVG/format args
- Modify: `static/style.css`
- Test: `src/render.rs` (existing `mod tests`)

**Interfaces:**
- Produces: DOM ids `#searchbox` (now a `<button>`), `#searchoverlay`,
  `#searchinput`, `#searchresults`, `#searchnote` — consumed by Task 6.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/render.rs`, near the existing
`assert!(h.contains("id=\"searchbox\""))` test:

```rust
    #[test]
    fn the_header_advertises_what_search_actually_does() {
        let s = crate::wsstate::Settings::default();  // whatever the existing
        let h = workspace_page(&s);                   // test in this file uses
        assert!(h.contains("id=\"searchoverlay\""), "the overlay shell must be in the page");
        assert!(h.contains("id=\"searchinput\""), "{h}");
        assert!(h.contains("id=\"searchresults\""), "{h}");
        assert!(h.contains("id=\"searchnote\""), "{h}");
        // Symbols are out of scope. A slot that keeps promising a category
        // nobody is building is how a placeholder becomes a lie.
        assert!(!h.contains("Search files, symbols"), "the hint must not promise symbols");
        assert!(h.contains("Search files, contents, sessions"), "{h}");
        assert!(
            !h.contains("project-wide search — not implemented yet"),
            "the tooltip must stop saying search is unimplemented"
        );
    }
```

Match the existing test's way of building the page — copy the setup from
`fn ...searchbox...` already in this file rather than inventing one.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib render::tests::the_header_advertises -- --test-threads=1`
Expected: FAIL — the page still contains "symbols" and the placeholder tooltip.

- [ ] **Step 3: Replace the placeholder with a real control**

In `src/render.rs`, replace line 1150:

```html
  <button id="searchbox" title="search this project (⇧ ⇧)">{SVG_SEARCH}<span class="hintline">Search files, contents, sessions</span><kbd>⇧ ⇧</kbd></button>
```

And add the overlay shell just before `</main>`'s closing — put it immediately
after the `<main id="grid">…</main>` block, as a sibling:

```html
<div id="searchoverlay" hidden>
  <div class="searchpanel">
    <input id="searchinput" type="text" autocomplete="off" spellcheck="false" placeholder="Search files, contents, sessions">
    <div id="searchresults"></div>
    <div id="searchnote"></div>
  </div>
</div>
```

- [ ] **Step 4: Style it**

In `static/style.css`, change `#searchbox`'s `cursor: help` to
`cursor: pointer`, and append:

```css
/* The overlay. resh's first modal — deliberately minimal, so the popup pass
   docs/backlog.md wants can absorb it rather than having to undo it. */
#searchoverlay { position: fixed; inset: 0; z-index: 40; background: rgba(0,0,0,.35);
                 display: flex; align-items: flex-start; justify-content: center; }
#searchoverlay[hidden] { display: none; }
.searchpanel { margin-top: 12vh; width: min(720px, 92vw); max-height: 70vh;
               display: flex; flex-direction: column;
               background: var(--bg2); border: 1px solid var(--border);
               border-radius: 8px; overflow: hidden; }
#searchinput { border: none; border-bottom: 1px solid var(--border); background: var(--tool);
               color: var(--fg); font: inherit; padding: 10px 12px; outline: none; }
#searchresults { overflow-y: auto; }
.searchgroup { padding: 4px 12px; font-size: 11px; color: var(--muted);
               border-bottom: 1px solid var(--border); }
.searchrow { display: flex; gap: 8px; align-items: baseline; padding: 4px 12px;
             cursor: pointer; white-space: nowrap; overflow: hidden; }
.searchrow.sel { background: var(--tool); }
.searchrow .where { color: var(--muted); font-size: 11px; }
.searchrow .line { font: 12px/1.4 var(--mono); overflow: hidden; text-overflow: ellipsis; }
/* The honesty line: truncation, unreadable directories, outright failure. */
#searchnote { padding: 6px 12px; font-size: 11px; color: var(--muted);
              border-top: 1px solid var(--border); }
#searchnote:empty { display: none; }
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test --lib render:: -- --test-threads=1`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/render.rs static/style.css
git commit -m "render: the search box becomes a control, with an overlay to open"
```

---

### Task 6: The ⇧⇧ trigger, the overlay, and the results

**Files:**
- Modify: `static/app.js`

**Interfaces:**
- Consumes: `#searchoverlay`, `#searchinput`, `#searchresults`, `#searchnote`,
  `#searchbox` (Task 5); `Event::SearchResults` (Task 2); `send()`
  (`app.js:209`).
- Produces: `openSearch()`, `closeSearch()`, and an `onEvent` case for
  `SearchResults` — consumed by Task 7's `RevealLine` only indirectly.

No Rust test can reach this file; Task 8 is where it is verified.

- [ ] **Step 1: Add the trigger and the overlay**

Append to `static/app.js`:

```js
// --- project search (⇧⇧) ---------------------------------------------------
//
// Double-tap Shift, IntelliJ-style. Two properties make it safe to arm on the
// document even while a terminal has focus, which is where focus usually is:
//
//   - Shift alone emits nothing to a shell, so intercepting it steals no
//     keystroke. Any Ctrl-/Cmd- chord would have to be taken away from the
//     program running in the terminal instead.
//   - The two presses must be consecutive. Typing "HI" presses Shift twice in
//     quick succession, but the H lands between them and resets the pending
//     state, so ordinary typing cannot open this.
const SHIFT_GAP_MS = 400;
let shiftPending = 0;
let searchSeq = 0;
let searchRows = [];      // [{kind, rel, line, session}] parallel to the DOM rows
let searchSel = 0;
let searchDebounce = null;
let searchReturnFocus = null;

document.addEventListener("keydown", (e) => {
  if (e.key !== "Shift") { shiftPending = 0; return; }
  if (e.repeat) return;   // holding Shift down is one press, not many
  const now = Date.now();
  if (shiftPending && now - shiftPending < SHIFT_GAP_MS) {
    shiftPending = 0;
    openSearch();
    return;
  }
  shiftPending = now;
});

function openSearch() {
  const ov = document.getElementById("searchoverlay");
  if (!ov || !ov.hidden) return;
  // Remembered before focus moves: closing must give the terminal back, or
  // every dismissal costs the user their shell focus.
  searchReturnFocus = document.activeElement;
  ov.hidden = false;
  const input = document.getElementById("searchinput");
  input.value = "";
  renderSearch(null);
  input.focus();
}

function closeSearch() {
  const ov = document.getElementById("searchoverlay");
  if (!ov || ov.hidden) return;
  ov.hidden = true;
  searchRows = [];
  // A terminal's focus lives on its xterm textarea; .focus() on the remembered
  // element restores it without knowing which pane it was.
  try { searchReturnFocus && searchReturnFocus.focus(); } catch {}
  searchReturnFocus = null;
}

document.getElementById("searchbox")?.addEventListener("click", openSearch);

document.getElementById("searchinput")?.addEventListener("input", (e) => {
  const q = e.target.value;
  clearTimeout(searchDebounce);
  // Debounced, because every keystroke is a walk. The server drops answers to
  // queries the user has already typed past, but not sending them at all is
  // cheaper than cancelling them.
  searchDebounce = setTimeout(() => {
    if (!q) { renderSearch(null); return; }
    send({ t: "Search", q, seq: ++searchSeq });
  }, 120);
});

document.getElementById("searchoverlay")?.addEventListener("keydown", (e) => {
  if (e.key === "Escape") { e.preventDefault(); closeSearch(); return; }
  if (e.key === "ArrowDown") { e.preventDefault(); moveSearchSel(1); return; }
  if (e.key === "ArrowUp") { e.preventDefault(); moveSearchSel(-1); return; }
  if (e.key === "Enter") { e.preventDefault(); activateSearchRow(searchSel); }
});

// Clicking the backdrop closes; clicking the panel does not.
document.getElementById("searchoverlay")?.addEventListener("mousedown", (e) => {
  if (e.target.id === "searchoverlay") closeSearch();
});

function moveSearchSel(d) {
  if (!searchRows.length) return;
  searchSel = (searchSel + d + searchRows.length) % searchRows.length;
  paintSearchSel();
}

function paintSearchSel() {
  const rows = document.querySelectorAll("#searchresults .searchrow");
  rows.forEach((n, i) => n.classList.toggle("sel", i === searchSel));
  rows[searchSel]?.scrollIntoView({ block: "nearest" });
}

/// Builds one result row. Every dynamic part is a text node: a matched line is
/// arbitrary file content and a path is arbitrary filesystem content, which
/// makes these the most attacker-influenced strings this client renders. The
/// innerHTML rule at the top of this file (constant markup only) is the whole
/// defence, and it only holds if nothing here interpolates.
function searchRow(primary, secondary) {
  const row = document.createElement("div");
  row.className = "searchrow";
  const a = document.createElement("span");
  a.textContent = primary;
  row.appendChild(a);
  if (secondary !== null && secondary !== undefined) {
    const b = document.createElement("span");
    b.className = "where";
    b.textContent = secondary;
    row.appendChild(b);
  }
  return row;
}

function renderSearch(results) {
  const host = document.getElementById("searchresults");
  const note = document.getElementById("searchnote");
  if (!host) return;
  host.textContent = "";
  note.textContent = "";
  searchRows = [];
  searchSel = 0;
  if (!results) return;

  const group = (label) => {
    const g = document.createElement("div");
    g.className = "searchgroup";
    g.textContent = label;
    host.appendChild(g);
  };

  if (results.files.length) {
    group(`Files (${results.files.length})`);
    for (const f of results.files) {
      const row = searchRow(f.rel.split("/").pop(), f.rel);
      host.appendChild(row);
      searchRows.push({ kind: "file", rel: f.rel });
    }
  }
  if (results.sessions.length) {
    group(`Sessions (${results.sessions.length})`);
    for (const s of results.sessions) {
      host.appendChild(searchRow(s, "terminal"));
      searchRows.push({ kind: "session", session: s });
    }
  }
  if (results.lines.length) {
    group(`Contents (${results.lines.length})`);
    for (const l of results.lines) {
      const row = searchRow(`${l.rel}:${l.line}`, null);
      const code = document.createElement("span");
      code.className = "line";
      code.textContent = l.text.trim();   // textContent: this is file content
      row.appendChild(code);
      host.appendChild(row);
      searchRows.push({ kind: "line", rel: l.rel, line: l.line });
    }
  }

  // The honesty line. "No matches" and "I could not look everywhere" are
  // different answers, and only this element can tell them apart — which is
  // the whole reason `Results` carries an outcome instead of being a list.
  const parts = [];
  if (results.outcome.state === "Failed") parts.push(`search failed: ${results.outcome.msg}`);
  if (results.outcome.state === "Truncated") parts.push(`partial results — ${results.outcome.reason}`);
  if (results.unreadable) {
    parts.push(`${results.unreadable} ${results.unreadable === 1 ? "place" : "places"} could not be read`);
  }
  if (!parts.length && !searchRows.length) parts.push("no matches");
  note.textContent = parts.join(" · ");

  if (searchRows.length) paintSearchSel();
}

function activateSearchRow(i) {
  const r = searchRows[i];
  if (!r) return;
  closeSearch();
  if (r.kind === "session") {
    send({ t: "OpenTab", pane: 3, tab: { k: "Terminal", session: r.session } });
  } else if (r.kind === "line") {
    send({ t: "OpenAtLine", pane: 2, rel: r.rel, line: r.line });
  } else {
    send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: r.rel, mode: "Preview" } });
  }
}

document.getElementById("searchresults")?.addEventListener("click", (e) => {
  const row = e.target.closest(".searchrow");
  if (!row) return;
  const rows = [...document.querySelectorAll("#searchresults .searchrow")];
  activateSearchRow(rows.indexOf(row));
});
```

- [ ] **Step 2: Handle the event**

In `onEvent`, alongside the other event cases:

```js
  if (ev.t === "SearchResults") {
    // A late answer to a query the user has typed past must not paint over
    // what they are looking at now. The server drops most of these; this is
    // the client half of the same rule, for the ones already in flight.
    if (ev.seq !== searchSeq) return;
    renderSearch(ev.results);
    return;
  }
```

- [ ] **Step 3: Verify by hand in a browser**

`cargo test` cannot reach this file at all. Start a scratch resh (never the
live instance — a browser attaching to it clamps every terminal to the
smallest client's geometry), open a project, and check: ⇧⇧ opens; typing in a
terminal does not; Escape returns focus to the terminal; a query shows rows;
`#searchnote` says "no matches" for a query with none.

- [ ] **Step 4: Commit**

```bash
git add static/app.js
git commit -m "app: the shift-shift overlay, and results rendered as text nodes"
```

---

### Task 7: Scroll to the line

**Files:**
- Modify: `static/app.js` (a `RevealLine` case, and the termlink flash text)

**Interfaces:**
- Consumes: `Event::RevealLine` (Task 2), emitted by Task 4.

- [ ] **Step 1: Add the handler**

Append to `static/app.js`:

```js
/// Scroll whichever pane holds `rel` to `line`, and flash the row.
///
/// Three surfaces, and only two of them have lines. A code preview is a
/// single <pre class="codeview"> with no per-line elements, but `.codeview`
/// sets no white-space override, so <pre>'s default `white-space: pre`
/// applies and one source line is exactly one visual line — which is what
/// makes the arithmetic below exact. A *rendered markdown* preview has no
/// line mapping at all, so it says so rather than scrolling somewhere
/// arbitrary and looking broken.
function revealLine(rel, line) {
  for (const content of document.querySelectorAll(".pane .content")) {
    const ta = content.querySelector("textarea.editor");
    if (ta && editorRel(content) === rel) {
      const lines = ta.value.split("\n");
      const upto = lines.slice(0, Math.max(0, line - 1)).join("\n").length + (line > 1 ? 1 : 0);
      ta.focus();
      ta.setSelectionRange(upto, upto + (lines[line - 1] || "").length);
      // Measured, never assumed: line-height is set in style.css and the
      // editor inherits it through code-input's layers.
      const lh = parseFloat(getComputedStyle(ta).lineHeight) || 20;
      ta.scrollTop = Math.max(0, (line - 1) * lh - ta.clientHeight / 3);
      return;
    }
    const pre = content.querySelector("pre.codeview");
    if (pre && content.dataset.url && content.dataset.url.includes(encodeURIComponent(rel))) {
      const lh = parseFloat(getComputedStyle(pre).lineHeight) || 20;
      pre.scrollTop = Math.max(0, (line - 1) * lh - pre.clientHeight / 3);
      return;
    }
  }
}

/// The rel an editor pane is showing. The path is already in the breadcrumb
/// as a text node, which is the only place it exists client-side once the
/// textarea is mounted.
function editorRel(content) {
  const n = content.querySelector(".editwrap .path .rel");
  return n ? n.textContent : null;
}
```

In `onEvent`:

```js
  if (ev.t === "RevealLine") {
    // After a frame: the tab that this line belongs to may be mounting right
    // now, from the State event that arrived immediately before this one.
    requestAnimationFrame(() => revealLine(ev.rel, ev.line));
    return;
  }
```

- [ ] **Step 2: Retire the honest-gap flash**

At `static/app.js:1249-1253`, the comment and the flash say the viewer has no
line addressing. It does now. Replace that block with:

```js
  // The line is no longer dropped: the server sends a RevealLine alongside
  // the tab it opens (hub::do_open_path), and revealLine() scrolls there.
  const line = raw.match(/:(\d+)(?::\d+)?$/);
  if (line) termFlash(entry, `line ${line[1]}`);
```

- [ ] **Step 3: Verify by hand**

In the scratch resh: search for a string, press Enter on a content hit, and
confirm the editor opens *and* scrolls to the matched line with it selected.
Then click a `file.rs:42` link in a terminal and confirm the same.

Try a markdown file's content hit too: it opens in Edit (`do_open_at_line`
asks for Edit deliberately), so the line resolves there.

- [ ] **Step 4: Commit**

```bash
git add static/app.js
git commit -m "app: scroll to the line a search or a terminal link named"
```

---

### Task 8: The browser test

**Files:**
- Create: `tests/browser/search.mjs`
- Modify: `tests/browser/README.md` (add the file to its list)

**Interfaces:**
- Consumes: `fixture`, `freePort`, `openPage`, `profileDir`, `startBrowser`,
  `startResh`, `until` from `tests/browser/harness.mjs`.

- [ ] **Step 1: Write the test**

```js
//! Project search: the ⇧⇧ overlay, its results, and landing on a line.
//!
//! Every line of the trigger, the overlay and the scroll lives in
//! static/app.js, where `cargo test` cannot reach — the same reason
//! dotfiles.mjs and paneicons.mjs exist. The server half is covered by Rust
//! tests; all of it can be correct while the overlay never opens, the rows
//! render as markup, or the editor opens at the top of the file.
//!
//! Run: deno run -A tests/browser/search.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
await Deno.mkdir(`${fx.roots}/proj/src`, { recursive: true });
await Deno.writeTextFile(
  `${fx.roots}/proj/src/needle.rs`,
  "one\ntwo\nlet marker = 1;\nfour\n",
);
// A path and a matched line that are both markup. If either is interpolated
// rather than set as text, this file renders an element instead of characters
// — the fixture carries a real metacharacter precisely because CLAUDE.md
// records an escaping test whose fixture had none and so asserted nothing.
await Deno.writeTextFile(`${fx.roots}/proj/src/<img src=x onerror=1>.txt`, "marker here\n");

const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
const url = `http://127.0.0.1:${resh.port}/proj`;
let page;

try {
  page = await openPage(browser.port, url);
  const { evalIn } = page;
  await until(() => evalIn("ctrl && ctrl.readyState === 1 && !!state"), 30, "app");

  // --- the trigger -------------------------------------------------------
  // Two real Shift keydowns on the document, not a call to openSearch(): an
  // overlay wired to nothing is exactly the defect this file exists to catch.
  const shiftTwice = `(() => {
    const k = () => document.dispatchEvent(new KeyboardEvent("keydown", { key: "Shift", bubbles: true }));
    k(); k();
    return !document.getElementById("searchoverlay").hidden;
  })()`;
  ok(await evalIn(shiftTwice), "⇧⇧ opens the overlay");

  // A single Shift, and a Shift with a key between two Shifts, must not.
  await evalIn(`closeSearch()`);
  const notOpened = `(() => {
    const k = (key) => document.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
    k("Shift"); k("H"); k("Shift");
    return document.getElementById("searchoverlay").hidden;
  })()`;
  ok(await evalIn(notOpened), "an intervening keystroke resets the pending Shift");

  // --- results -----------------------------------------------------------
  await evalIn(shiftTwice);
  await evalIn(`(() => { const i = document.getElementById("searchinput");
    i.value = "marker"; i.dispatchEvent(new Event("input", { bubbles: true })); })()`);
  ok(
    await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length > 0`), 15, "results"),
    "a query returns rows",
  );

  // The escaping assertion. If the row had been built with innerHTML, the
  // <img> would be an element and its onerror would have fired.
  ok(
    await evalIn(`document.querySelectorAll("#searchresults img").length === 0`),
    "a path containing markup renders as text, not as an element",
  );
  ok(
    await evalIn(`[...document.querySelectorAll("#searchresults .searchrow")]
       .some((n) => n.textContent.includes("<img src=x onerror=1>"))`),
    "…and the characters are actually visible",
  );

  // --- landing on the line ----------------------------------------------
  const openedLine = `(() => {
    const rows = [...document.querySelectorAll("#searchresults .searchrow")];
    const hit = rows.find((n) => n.textContent.includes("src/needle.rs:3"));
    if (!hit) return "no content hit for needle.rs:3";
    hit.click();
    return "clicked";
  })()`;
  ok(await evalIn(openedLine) === "clicked", "a content hit is clickable");

  ok(
    await until(() => evalIn(
      `(() => { const ta = document.querySelector("textarea.editor");
         if (!ta) return false;
         const upto = ta.value.slice(0, ta.selectionStart).split("\\n").length;
         return upto === 3; })()`,
    ), 15, "line 3 selected"),
    "the editor opens with line 3 selected, not the top of the file",
  );

  // --- the honesty line --------------------------------------------------
  await evalIn(shiftTwice);
  await evalIn(`(() => { const i = document.getElementById("searchinput");
    i.value = "zzzznotpresentzzzz"; i.dispatchEvent(new Event("input", { bubbles: true })); })()`);
  ok(
    await until(() => evalIn(`document.getElementById("searchnote").textContent === "no matches"`), 15, "note"),
    "a query with no hits says so explicitly",
  );
} finally {
  try { page && page.close(); } catch {}
  await browser.stop();
  await resh.stop();
  await fx.cleanup();
}

console.log(fail ? `\n${fail} failure(s)` : "\nall ok");
Deno.exit(fail ? 1 : 0);
```

Check `harness.mjs`'s actual `startBrowser`/`startResh` return shape before
running — the teardown names above (`browser.stop()`, `resh.stop()`) must
match what the harness returns; copy the `finally` block from `dotfiles.mjs`
if they differ.

- [ ] **Step 2: Run it**

Run: `deno run -A tests/browser/search.mjs`
Expected: all ok. It skips with an actionable message if no Chromium is
present, in which case run it on the deploy host, which has one.

- [ ] **Step 3: Run the whole suite, twice, and time it**

```bash
time cargo test -- --test-threads=1
time cargo test -- --test-threads=1
```

A green suite proves less than it looks: a deadlock hangs rather than fails,
so compare the two wall-clock times. A search worker that deadlocked against
the hub would show up here as a run that takes far longer, not as a failure.

- [ ] **Step 4: Commit**

```bash
git add tests/browser/search.mjs tests/browser/README.md
git commit -m "test: browser coverage for the search overlay and line landing"
```

---

## Self-review

**Spec coverage.** Every section maps to a task: trigger and overlay → 5, 6;
categories and ranking → 1 (`score_path`), 6 (grouping); hint text → 5; engine,
filter, binary rule, symlinks, caps, three-state outcome → 1; concurrency,
snapshot, cancellation, `send_to` → 3; line addressing → 2, 4, 7 (by the
deviation recorded above); "where result rows are built" → 6; testing → every
task, plus 8.

**Not covered, deliberately:** `Tab` cycling between categories in the overlay
(the spec mentions `Tab` cycles category; the plan's overlay uses one flat list
with group headers, and ↑/↓ crosses the groups). If that matters, it is a
follow-up — it changes no server code.

**Type consistency.** `search::Results` is produced by Task 1, referenced by
`Event::SearchResults` in Task 2, constructed by `run_search` in Task 3, and
consumed field-by-field in Task 6's `renderSearch` (`files[].rel`,
`lines[].rel/.line/.text`, `sessions[]`, `outcome.state/.reason/.msg`,
`unreadable`). `Outcome`'s `#[serde(tag = "state")]` is what makes
`results.outcome.state` the discriminant the client switches on.
