//! Enough of `.gitignore` to keep the search walk out of build output.
//!
//! Search walks the filesystem; git reads its index. Nothing reconciles the
//! two, so anything gitignored is invisible to `git status` and fully visible
//! to the walk. The measured failure: a 1.1 GB Chromium profile under
//! `tests/browser/tmp/` — gitignored, so nothing ever complained about it —
//! consumed the whole 20 000-file budget, and a search of this repository came
//! back truncated before it reached `src/`.
//!
//! # The failure direction is chosen, not accidental
//!
//! This is a deliberately partial implementation of a specification with
//! negations, character classes, `**`, escapes and precedence rules. Getting
//! any of that subtly wrong hides a file the user was looking for, and a
//! search that silently omits real source is far worse than one that includes
//! build output — the second is the bug being fixed, the first is a new and
//! invisible one.
//!
//! So every unsupported construct fails towards **not ignoring**:
//!
//! - A pattern this module cannot evaluate (`**`, a `[class]`, a backslash
//!   escape) contributes no rule. The walk descends, exactly as it does
//!   today.
//! - A file containing **any** negation (`!`) contributes *nothing at all*.
//!   A negation can only ever make a broader pattern narrower, so dropping it
//!   while keeping its neighbours is the one mistake that over-ignores:
//!   `*` plus `!src/` would become "ignore everything". Dropping the whole
//!   file is the only safe reading of a file with a `!` in it.
//!
//! # Directories only
//!
//! Only directory names are tested. That is where the measured cost is — the
//! budget is exhausted by *descending* into something huge — and it is the
//! half with the least room to hurt: skipping a directory git already ignores
//! removes generated output, where skipping files would start deciding which
//! of a project's own files are worth showing.
//!
//! # What is not read
//!
//! `core.excludesFile`, the per-user global ignore list. Finding it means
//! `git config`, and this module's whole reason for existing is that search
//! spawns no subprocess (see `search.rs`'s header on ripgrep). Repository
//! `.gitignore` files at every level, and `.git/info/exclude`, are read.

use std::path::Path;
use std::rc::Rc;

/// One ignore file's worth of rules is enough for any real project; a file
/// with more than this is not one this is meant to serve.
const MAX_RULES: usize = 1000;
/// Read bound. `.gitignore` files are small; a huge one is either not an
/// ignore file or not worth the walk it would save.
const MAX_BYTES: u64 = 256 * 1024;

/// A single pattern, reduced to what this module can decide.
#[derive(Debug, Clone, PartialEq)]
struct Rule {
    /// Segments, already split on `/`.
    segs: Vec<String>,
    /// The pattern named a path rather than a name, so it is fixed to the
    /// directory holding the ignore file. `/build` and `a/b` are both
    /// anchored; `build` is not. This is git's rule: a pattern containing a
    /// `/` anywhere but at the end is anchored.
    anchored: bool,
}

/// The rules from one ignore file, and where they apply from.
#[derive(Debug, Clone, PartialEq)]
struct Scope {
    /// Directory holding the file, relative to the search root, `/`-separated
    /// and without a trailing slash. Empty for the root itself.
    base: String,
    rules: Vec<Rule>,
}

/// A directory's inherited rules: its own scope, and its parent's chain.
///
/// A linked list rather than a flat `Vec`, because the walk is a DFS over an
/// explicit stack — every pending directory has to carry its own inheritance,
/// and sharing it by `Rc` makes that O(1) per entry instead of a copy of
/// every ancestor's rules.
pub struct Ignore {
    scope: Option<Scope>,
    parent: Option<Rc<Ignore>>,
}

impl Ignore {
    /// The chain for the search root: its `.gitignore` and, if present, the
    /// repository-local `.git/info/exclude`, which is not committed and so is
    /// where a developer puts their own machine's build output.
    pub fn for_root(root: &Path) -> Rc<Ignore> {
        let mut rules = parse_file(&root.join(".gitignore"));
        rules.extend(parse_file(&root.join(".git").join("info").join("exclude")));
        rules.truncate(MAX_RULES);
        Rc::new(Ignore {
            scope: (!rules.is_empty()).then(|| Scope { base: String::new(), rules }),
            parent: None,
        })
    }

    /// A chain that ignores nothing, for a walk that has ignoring switched
    /// off. Cheaper and clearer than an `Option<Rc<Ignore>>` threaded through
    /// the walk, and it cannot accidentally start matching.
    pub fn none() -> Rc<Ignore> {
        Rc::new(Ignore { scope: None, parent: None })
    }

    /// The chain for a subdirectory about to be walked. `rel` is its path
    /// relative to the search root.
    pub fn enter(self: &Rc<Self>, dir: &Path, rel: &str) -> Rc<Ignore> {
        let rules = parse_file(&dir.join(".gitignore"));
        if rules.is_empty() {
            // Nothing of its own: share the parent's chain rather than
            // lengthening it. Most directories take this path, which is what
            // keeps a deep walk's per-candidate cost proportional to the
            // number of ignore files rather than to the depth.
            return Rc::clone(self);
        }
        Rc::new(Ignore {
            scope: Some(Scope { base: rel.to_string(), rules }),
            parent: Some(Rc::clone(self)),
        })
    }

    /// Whether git would ignore the directory at `rel` (relative to the
    /// search root, `/`-separated).
    pub fn skips_dir(&self, rel: &str) -> bool {
        let mut node = Some(self);
        let mut owned;
        while let Some(n) = node {
            if let Some(s) = &n.scope {
                if s.matches(rel) {
                    return true;
                }
            }
            owned = n.parent.as_deref();
            node = owned;
        }
        false
    }
}

impl Scope {
    fn matches(&self, rel: &str) -> bool {
        // The path as this scope sees it: everything below the directory
        // holding the ignore file.
        let within = if self.base.is_empty() {
            rel
        } else if let Some(rest) = rel.strip_prefix(&self.base) {
            match rest.strip_prefix('/') {
                Some(r) => r,
                // `rel` merely starts with the same *bytes* as `base`
                // ("srcfoo" vs "src"), which is not the same directory.
                None => return false,
            }
        } else {
            return false;
        };
        if within.is_empty() {
            return false;
        }
        let segs: Vec<&str> = within.split('/').collect();
        self.rules.iter().any(|r| {
            if r.anchored {
                // Fixed to this scope's directory: the pattern's segments
                // must be the start of the path. A prefix rather than an
                // exact match because git ignores everything beneath an
                // ignored directory, and this is asked about descendants
                // whenever a walk reaches one without having been offered
                // the ancestor.
                r.segs.len() <= segs.len()
                    && r.segs.iter().zip(&segs).all(|(p, s)| glob(p, s))
            } else {
                // Free-floating: git matches a `/`-less pattern against the
                // basename at any depth below the ignore file.
                let p = &r.segs[0];
                segs.iter().any(|s| glob(p, s))
            }
        })
    }
}

/// Reads one ignore file into rules. Every failure — missing, unreadable,
/// oversized — is an empty list, which means "ignore nothing" and leaves the
/// walk behaving exactly as it does without this module.
fn parse_file(path: &Path) -> Vec<Rule> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && m.len() <= MAX_BYTES => {}
        _ => return Vec::new(),
    }
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    parse(&text)
}

fn parse(text: &str) -> Vec<Rule> {
    let mut out = Vec::new();
    for raw in text.lines() {
        // Trailing spaces are not part of a pattern unless escaped; escapes
        // are unsupported, so a line ending in `\ ` is dropped below anyway.
        let line = raw.trim_end();
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('!') {
            // The one construct that cannot be dropped in isolation: it
            // narrows something else, so honouring its neighbours without it
            // over-ignores. See the module header.
            return Vec::new();
        }
        if line.contains('\\') || line.contains('[') || line.contains("**") {
            continue; // cannot decide it; do not ignore on a guess
        }
        let anchored = line.trim_end_matches('/').contains('/') || line.starts_with('/');
        let segs: Vec<String> = line
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if segs.is_empty() {
            continue;
        }
        if !anchored && segs.len() != 1 {
            continue; // unreachable given `anchored`, but not worth assuming
        }
        out.push(Rule { segs, anchored });
        if out.len() >= MAX_RULES {
            break;
        }
    }
    out
}

/// `*` and `?` against a single path segment. No character classes and no
/// `**`: both are refused at parse time, so anything reaching here is
/// decidable.
///
/// The greedy two-pointer form, not recursion: a pattern is attacker-adjacent
/// (it comes out of a file in the project being searched) and a backtracking
/// matcher on `a*a*a*a*…` is exponential. This is O(pattern × segment).
fn glob(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(base: &str, text: &str) -> Scope {
        Scope { base: base.into(), rules: parse(text) }
    }

    #[test]
    fn the_pattern_that_motivated_this_is_matched() {
        // roost's own `.gitignore`, verbatim. The 1.1 GB Chromium profile
        // that exhausted the file budget lived under the fourth line, and a
        // first draft of this module bailed on any pattern with an internal
        // `/` — which would have shipped a fix that misses its own reported
        // case. Every entry here is anchored and multi-segment.
        let s = scope("", "/target\n/.idea/\n/.superpowers/\n/.claude/worktrees/\n/tests/browser/tmp/\n");
        assert!(s.matches("tests/browser/tmp"), "the directory itself");
        assert!(s.matches("tests/browser/tmp/profile"), "and everything under it");
        assert!(s.matches(".claude/worktrees"));
        assert!(s.matches("target"));
        assert!(!s.matches("tests"), "but not the path on the way to it");
        assert!(!s.matches("tests/browser"), "nor its parent");
        assert!(!s.matches("src"), "and nothing else");
    }

    #[test]
    fn an_anchored_pattern_does_not_match_the_same_name_deeper() {
        // `/build` is the project's build directory, not every `build`
        // anywhere in the tree. Getting this wrong hides `src/build/`, which
        // is source.
        let s = scope("", "/build\n");
        assert!(s.matches("build"));
        assert!(!s.matches("src/build"));
        assert!(!s.matches("a/b/build"));
    }

    #[test]
    fn a_bare_name_matches_at_every_depth() {
        let s = scope("", "node_modules\n");
        assert!(s.matches("node_modules"));
        assert!(s.matches("packages/ui/node_modules"));
    }

    #[test]
    fn a_prefix_of_a_name_is_not_the_name() {
        // Segment comparison, not `starts_with`. `/build` must not take
        // `build2`, and `src` must not take `srcfoo`.
        let s = scope("", "/build\nnode_modules\n");
        assert!(!s.matches("build2"));
        assert!(!s.matches("node_modules_old"));
        let nested = scope("src", "tmp\n");
        assert!(!nested.matches("srcfoo/tmp"), "base must match on a segment boundary");
        assert!(nested.matches("src/tmp"));
    }

    #[test]
    fn a_nested_ignore_file_applies_only_below_itself() {
        let s = scope("tests/browser", "tmp/\n");
        assert!(s.matches("tests/browser/tmp"));
        assert!(s.matches("tests/browser/a/tmp"), "unanchored, so any depth below");
        assert!(!s.matches("tmp"), "not above itself");
        assert!(!s.matches("src/tmp"), "nor in a sibling");
    }

    #[test]
    fn a_negation_anywhere_disables_the_whole_file() {
        // The one rule that cannot be dropped on its own. Honouring `*` while
        // discarding `!src/` would hide the entire project — over-ignoring is
        // the failure this module is shaped to avoid, so a file with any `!`
        // in it contributes nothing.
        assert!(parse("*\n!src/\n").is_empty(), "the classic allowlist shape");
        assert!(parse("build/\n!build/keep/\n").is_empty(), "and a narrow one");
        assert!(!parse("build/\n").is_empty(), "the same file without it still works");
    }

    #[test]
    fn an_undecidable_pattern_is_dropped_and_its_neighbours_survive() {
        // Dropping one pattern under-ignores, which is the safe direction:
        // the walk descends exactly as it does today.
        let r = parse("a/**/b\nlogs[0-9]\nesc\\ ape\ndist\n");
        assert_eq!(r.len(), 1, "only the decidable one: {r:?}");
        assert_eq!(r[0].segs, vec!["dist".to_string()]);
    }

    #[test]
    fn comments_and_blank_lines_are_not_patterns() {
        assert!(parse("# a comment\n\n   \n").is_empty());
        // But a `#` inside a pattern is a literal, not a comment.
        let r = parse("we#ird\n");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn a_glob_matches_a_segment_and_never_crosses_one() {
        assert!(glob("*.egg-info", "roost.egg-info"));
        assert!(glob("build-*", "build-1"));
        assert!(glob("?ist", "dist"));
        assert!(!glob("?ist", "dist2"));
        assert!(glob("*", "anything"));
        // A `*` in a pattern segment cannot swallow a path separator,
        // because matching is done one segment at a time.
        let s = scope("", "a*c\n");
        assert!(!s.matches("a/b/c"), "a glob must not span directories");
        assert!(s.matches("abc"));
    }

    #[test]
    fn a_pathological_glob_does_not_take_exponential_time() {
        // The pattern comes out of a file in the project being searched, and
        // this runs inside a walk with a 1500 ms deadline. A backtracking
        // matcher answers this in geological time; the two-pointer form is
        // linear in the product.
        let pat = "a*a*a*a*a*a*a*a*a*a*a*a*b";
        let s = "a".repeat(200);
        let t0 = std::time::Instant::now();
        assert!(!glob(pat, &s));
        assert!(t0.elapsed() < std::time::Duration::from_millis(200), "took {:?}", t0.elapsed());
    }

    #[test]
    fn an_empty_or_absent_file_ignores_nothing() {
        assert!(parse("").is_empty());
        assert!(parse_file(Path::new("/nonexistent/.gitignore")).is_empty());
    }

    #[test]
    fn the_chain_applies_every_ancestors_rules() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".gitignore"), b"/target\n").unwrap();
        std::fs::create_dir_all(d.path().join("web")).unwrap();
        std::fs::write(d.path().join("web/.gitignore"), b"dist\n").unwrap();

        let root = Ignore::for_root(d.path());
        assert!(root.skips_dir("target"));
        assert!(!root.skips_dir("web/dist"), "the nested file has not been read yet");

        let web = root.enter(&d.path().join("web"), "web");
        assert!(web.skips_dir("web/dist"), "its own rule");
        assert!(web.skips_dir("target"), "and it still inherits the root's");
        assert!(!web.skips_dir("web/src"));
    }

    #[test]
    fn a_directory_with_no_ignore_file_shares_its_parents_chain() {
        // Not just an optimisation: it is what keeps a deep tree's
        // per-candidate cost proportional to the number of ignore files
        // rather than to the depth.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".gitignore"), b"/target\n").unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        let root = Ignore::for_root(d.path());
        let src = root.enter(&d.path().join("src"), "src");
        assert!(Rc::ptr_eq(&root, &src), "no new link for a directory with no rules");
    }

    #[test]
    fn the_repository_local_exclude_file_is_read_too() {
        // Not committed, so it is where a developer puts their own machine's
        // build output — the case a shared `.gitignore` never covers.
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".git/info")).unwrap();
        std::fs::write(d.path().join(".git/info/exclude"), b"scratch\n").unwrap();
        let root = Ignore::for_root(d.path());
        assert!(root.skips_dir("scratch"));
        assert!(root.skips_dir("a/b/scratch"));
    }
}
