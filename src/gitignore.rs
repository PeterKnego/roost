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
//! - A file containing **any** negation (`!`), or one that exists and cannot
//!   be read, makes its whole subtree **opaque**: nothing there is ignored,
//!   by it or by any *ancestor*. Suppressing ancestors is the point, and it
//!   follows from git's precedence — a deeper `.gitignore` outranks a
//!   shallower one, so a `!` here exists precisely to cancel a parent's rule.
//!   Dropping the `!` and keeping the parent's rule is the one mistake that
//!   over-ignores, and it shipped once: `parse` returned an empty list for
//!   both "no rules" and "no usable rules", so a `!dist/` in `.gitignore`
//!   vanished while `.git/info/exclude`'s `dist/` survived in the same merged
//!   scope. Descendants are deliberately unaffected — they outrank the
//!   bailing file too.
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
    /// This directory's ignore file could not be reasoned about — it carried
    /// a negation, or could not be read — so **nothing** is ignored at or
    /// below it by any *ancestor* rule.
    ///
    /// Suppressing ancestors is the whole point, and it follows from git's
    /// precedence: a deeper `.gitignore` outranks a shallower one, so a `!`
    /// here can re-include something a parent excluded. Dropping the `!` and
    /// keeping the parent's rule is exactly the over-ignore this module is
    /// shaped to avoid. Descendants are *not* suppressed: they outrank this
    /// file too, and were parsed on their own terms.
    opaque: bool,
    parent: Option<Rc<Ignore>>,
}

impl Ignore {
    /// The chain for the search root: its `.gitignore` and, if present, the
    /// repository-local `.git/info/exclude`, which is not committed and so is
    /// where a developer puts their own machine's build output.
    pub fn for_root(root: &Path) -> Rc<Ignore> {
        // Both files, and either one bailing poisons the pair. They are
        // merged into one scope here, so honouring one while discarding the
        // other is how `!dist/` in `.gitignore` — which git lets re-include
        // what `.git/info/exclude` excluded — turned into a directory roost
        // skipped and git does not.
        let own = parse_file(&root.join(".gitignore"));
        let excl = read_exclude(root);
        let (Some(mut rules), Some(more)) = (own, excl) else {
            return Rc::new(Ignore { scope: None, opaque: true, parent: None });
        };
        rules.extend(more);
        rules.truncate(MAX_RULES);
        Rc::new(Ignore {
            scope: (!rules.is_empty()).then(|| Scope { base: String::new(), rules }),
            opaque: false,
            parent: None,
        })
    }

    /// A chain that ignores nothing, for a walk that has ignoring switched
    /// off. Cheaper and clearer than an `Option<Rc<Ignore>>` threaded through
    /// the walk, and it cannot accidentally start matching.
    pub fn none() -> Rc<Ignore> {
        Rc::new(Ignore { scope: None, opaque: false, parent: None })
    }

    /// The chain for a subdirectory about to be walked. `rel` is its path
    /// relative to the search root.
    pub fn enter(self: &Rc<Self>, dir: &Path, rel: &str) -> Rc<Ignore> {
        let Some(rules) = parse_file(&dir.join(".gitignore")) else {
            // A `!` in here can re-include what an ancestor excluded, and
            // this file outranks every ancestor. Since it cannot be
            // evaluated, nothing below may be ignored on an ancestor's word.
            return Rc::new(Ignore { scope: None, opaque: true, parent: Some(Rc::clone(self)) });
        };
        if rules.is_empty() {
            // Nothing of its own: share the parent's chain rather than
            // lengthening it. Most directories take this path, which is what
            // keeps a deep walk's per-candidate cost proportional to the
            // number of ignore files rather than to the depth.
            return Rc::clone(self);
        }
        Rc::new(Ignore {
            scope: Some(Scope { base: rel.to_string(), rules }),
            opaque: false,
            parent: Some(Rc::clone(self)),
        })
    }

    /// Whether git would ignore the directory at `rel` (relative to the
    /// search root, `/`-separated).
    pub fn skips_dir(&self, rel: &str) -> bool {
        let mut node = Some(self);
        while let Some(n) = node {
            // This node's own rules first: they outrank everything above,
            // so a scope that matches wins even when an ancestor is opaque.
            if let Some(s) = &n.scope {
                if s.matches(rel) {
                    return true;
                }
            }
            // Then the stop. An unevaluable file here means no ancestor may
            // speak for anything at or below it.
            if n.opaque {
                return false;
            }
            node = n.parent.as_deref();
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

/// `.git/info/exclude`, when there is such a file to read.
///
/// `.git` is probed first, and that probe is the whole point. In a **git
/// worktree or a submodule, `.git` is a regular file** holding a `gitdir:`
/// pointer — so `stat(".git/info/exclude")` fails with `ENOTDIR`, which is
/// not `NotFound`. Reading that as "cannot tell" made the entire root opaque
/// and switched gitignore filtering off for the whole tree, silently,
/// reinstating the 20 000-file budget exhaustion this module exists to
/// prevent. roost treats worktrees as a first-class feature, so that is not
/// an exotic layout.
///
/// A `.git` that is a file, or absent, is a *definite* answer that no
/// `<root>/.git/info/exclude` exists — the same kind of answer `NotFound` is,
/// and it must not be folded into "unknown". Probing the parent says so
/// portably, without `ErrorKind::NotADirectory`, which needs a newer
/// toolchain than this crate asks for (`search.rs` already refuses a method
/// for that reason).
fn read_exclude(root: &Path) -> Option<Vec<Rule>> {
    match std::fs::symlink_metadata(root.join(".git")) {
        Ok(m) if m.is_dir() => parse_file(&root.join(".git").join("info").join("exclude")),
        // A worktree's or submodule's `.git` file: no exclude to read.
        Ok(_) => Some(Vec::new()),
        // Not a repository at all. Also a definite answer.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(Vec::new()),
        // Something is wrong with `.git` itself — unreadable, a broken mount.
        // Whether an exclude file exists, and whether it negates, is unknown.
        Err(_) => None,
    }
}

/// Reads one ignore file into rules.
///
/// Three answers, not two. `Some(rules)` is what the file says; `Some(empty)`
/// is a file that is *definitely* not there, or is not an ignore file at all,
/// and contributes nothing; `None` is a file that exists and might say
/// something this cannot read — which makes its subtree opaque, because
/// "cannot tell whether it negates" may not be folded into "it does not".
fn parse_file(path: &Path) -> Option<Vec<Rule>> {
    // `metadata`, which follows symlinks, rather than `symlink_metadata`.
    // This is project content and git reads it the same way — and refusing to
    // follow became disproportionate once a refusal meant *opacity*: a
    // monorepo that symlinks one shared `.gitignore` into `web/` would have
    // switched off every ancestor rule for that whole subtree. The target is
    // still bounded, and it still only ever decides which directories to skip.
    match std::fs::metadata(path) {
        // Positively absent: nothing to say, which is not the same as being
        // unable to say. The ordinary case for almost every directory, and it
        // must not disable anything.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        // Present and usable.
        Ok(m) if m.is_file() && m.len() <= MAX_BYTES => {}
        // A directory or a fifo where the file should be is not an ignore
        // file to git either, so it contributes nothing rather than
        // suppressing every ancestor rule below it.
        Ok(m) if !m.is_file() => return Some(Vec::new()),
        // A regular file too large to parse, or an error that is not
        // NotFound. git *would* read this one, so whether it holds a negation
        // is genuinely unknown.
        _ => return None,
    }
    let Ok(text) = std::fs::read_to_string(path) else { return None };
    parse(&text)
}

fn parse(text: &str) -> Option<Vec<Rule>> {
    // The negation scan runs over the whole file, before any cap. It used to
    // share the loop below, so a file with `MAX_RULES` patterns ahead of its
    // `!` hit `break` and never saw it — and the remaining patterns were then
    // honoured alone. The shape that reaches is exactly the one this module
    // is built to refuse: `*` kept, `!src/` dropped, the entire project
    // invisible to search, reported as one skipped directory.
    if text.lines().any(|l| l.trim_end().starts_with('!')) {
        return None;
    }
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
            //
            // `None`, not an empty list. The two are different answers — "no
            // rules" and "no usable rules" — and folding them together is
            // what let an ancestor's rule survive a `!` that was there to
            // cancel it.
            return None;
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
    Some(out)
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
        Scope { base: base.into(), rules: parse(text).expect("fixture must not bail") }
    }

    fn rules(text: &str) -> Vec<Rule> {
        parse(text).expect("fixture must not bail")
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
    fn the_rule_cap_cannot_hide_a_negation_further_down_the_file() {
        // The `!` guard used to sit inside the same loop as the rule cap, so
        // a file with `MAX_RULES` patterns before its negation hit `break`
        // and never saw it. The remaining patterns were then honoured alone —
        // and the shape that reaches is exactly the one the module header
        // names as the thing that must never happen: `*` kept, `!src/`
        // dropped, so the entire project becomes invisible to search and the
        // note calls it "1 gitignored directory not searched".
        let mut text = String::new();
        for i in 0..MAX_RULES {
            text.push_str(&format!("junk{i}/\n"));
        }
        text.push_str("*\n!src/\n");
        assert!(
            parse(&text).is_none(),
            "a negation past the rule cap must still poison the file"
        );
    }

    #[test]
    fn a_negation_anywhere_disables_the_whole_file() {
        // The one rule that cannot be dropped on its own. Honouring `*` while
        // discarding `!src/` would hide the entire project — over-ignoring is
        // the failure this module is shaped to avoid, so a file with any `!`
        // in it contributes nothing.
        // `None`, not an empty list: "no usable rules" is a different answer
        // from "no rules", and the caller has to be able to tell, or an
        // ancestor's rule survives a `!` that existed to cancel it.
        assert!(parse("*\n!src/\n").is_none(), "the classic allowlist shape");
        assert!(parse("build/\n!build/keep/\n").is_none(), "and a narrow one");
        assert!(!rules("build/\n").is_empty(), "the same file without it still works");
        assert_eq!(parse("# just a comment\n").as_deref(), Some(&[][..]), "empty is not the same as bailed");
    }

    #[test]
    fn an_undecidable_pattern_is_dropped_and_its_neighbours_survive() {
        // Dropping one pattern under-ignores, which is the safe direction:
        // the walk descends exactly as it does today.
        let r = rules("a/**/b\nlogs[0-9]\nesc\\ ape\ndist\n");
        assert_eq!(r.len(), 1, "only the decidable one: {r:?}");
        assert_eq!(r[0].segs, vec!["dist".to_string()]);
    }

    #[test]
    fn comments_and_blank_lines_are_not_patterns() {
        assert!(rules("# a comment\n\n   \n").is_empty());
        // But a `#` inside a pattern is a literal, not a comment.
        let r = rules("we#ird\n");
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
        assert!(rules("").is_empty());
        // Absent is the ordinary state of almost every directory, and it is
        // *determinable* — NotFound is an answer. It must read as "nothing to
        // say", never as "cannot say", or entering any directory without a
        // `.gitignore` would switch ignoring off for everything below it.
        assert_eq!(
            parse_file(Path::new("/nonexistent/.gitignore")).as_deref(),
            Some(&[][..]),
            "a missing file has nothing to say, and that is not the same as being unable to say"
        );
    }

    #[test]
    fn a_worktree_or_submodule_root_still_ignores_anything() {
        // In a git worktree and in a submodule, `.git` is a regular *file*
        // holding a `gitdir:` pointer. `stat(".git/info/exclude")` then fails
        // with ENOTDIR, which is not `NotFound` — and reading that as "cannot
        // tell" made the whole root opaque, switching gitignore filtering off
        // for the entire tree. Silently: nothing fails, search just quietly
        // walks `node_modules` again and the 20 000-file budget goes back to
        // being exhausted by build output.
        //
        // roost creates worktrees itself (`.claude/worktrees/`), so this is
        // the normal layout for a large share of the trees it searches.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".gitignore"), b"dist\n").unwrap();
        std::fs::write(d.path().join(".git"), b"gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
        assert!(
            Ignore::for_root(d.path()).skips_dir("dist"),
            "a `.git` file is a definite 'no exclude file', not an unknown"
        );
    }

    #[test]
    fn a_repository_with_no_git_at_all_still_ignores_anything() {
        // The other definite absence, and the ordinary case for a plain
        // directory that is not a checkout.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".gitignore"), b"dist\n").unwrap();
        assert!(Ignore::for_root(d.path()).skips_dir("dist"));
    }

    #[test]
    fn a_symlinked_ignore_file_is_read_rather_than_blanking_its_subtree() {
        // Sharing one `.gitignore` across a monorepo by symlink is ordinary,
        // and git reads it. Refusing to follow it cost nothing while a
        // refusal meant "no rules"; once a refusal meant *opacity* it
        // switched off every ancestor rule for that whole subtree.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("shared-ignore"), b"build\n").unwrap();
        std::fs::create_dir_all(d.path().join("web")).unwrap();
        std::os::unix::fs::symlink(
            d.path().join("shared-ignore"),
            d.path().join("web/.gitignore"),
        )
        .unwrap();
        std::fs::write(d.path().join(".gitignore"), b"dist\n").unwrap();

        let root = Ignore::for_root(d.path());
        let web = root.enter(&d.path().join("web"), "web");
        assert!(web.skips_dir("web/build"), "the symlinked file's own rule applies");
        assert!(web.skips_dir("dist"), "and the root's rule is not suppressed under it");
    }

    #[test]
    fn a_directory_where_the_ignore_file_belongs_contributes_nothing() {
        // Not an ignore file to git either, so it has nothing to say — which
        // is different from having something unreadable to say.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".gitignore"), b"dist\n").unwrap();
        std::fs::create_dir_all(d.path().join("web/.gitignore")).unwrap();
        let root = Ignore::for_root(d.path());
        let web = root.enter(&d.path().join("web"), "web");
        assert!(web.skips_dir("dist"), "an ancestor rule survives it");
    }

    #[test]
    fn a_negation_cannot_be_cancelled_by_the_other_file_in_the_same_scope() {
        // The reported defect. `for_root` merges `.gitignore` and
        // `.git/info/exclude` into one scope, and `parse` used to bail to an
        // empty list — so `.gitignore`'s `!dist/`, which in git re-includes
        // what `exclude` excluded, vanished while `exclude`'s `dist/`
        // survived. roost then skipped a directory git does not ignore, which
        // is the over-ignore direction this whole module is shaped against.
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".git/info")).unwrap();
        std::fs::write(d.path().join(".git/info/exclude"), b"dist/\n").unwrap();

        // Control first, so the assertion below cannot pass by the rule never
        // having worked.
        let before = Ignore::for_root(d.path());
        assert!(before.skips_dir("dist"), "setup: exclude alone really does ignore dist");

        std::fs::write(d.path().join(".gitignore"), b"!dist/\n").unwrap();
        let after = Ignore::for_root(d.path());
        assert!(
            !after.skips_dir("dist"),
            "a negation in either file poisons the pair — git does not ignore this"
        );
    }

    #[test]
    fn a_bailing_child_suppresses_its_ancestors_but_not_its_descendants() {
        // Precedence is what makes this the right shape: a deeper
        // `.gitignore` outranks a shallower one, so a `!` in the child can
        // re-include what the root excluded — and therefore the root may not
        // speak for anything at or below that child. Descendants are
        // untouched: they outrank the bailing file too.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".gitignore"), b"dist\n").unwrap();
        std::fs::create_dir_all(d.path().join("web/inner")).unwrap();
        std::fs::write(d.path().join("web/.gitignore"), b"*\n!keep/\n").unwrap();
        std::fs::write(d.path().join("web/inner/.gitignore"), b"build\n").unwrap();

        let root = Ignore::for_root(d.path());
        assert!(root.skips_dir("dist"), "setup: the root rule works where nothing bails");

        let web = root.enter(&d.path().join("web"), "web");
        assert!(
            !web.skips_dir("web/dist"),
            "the root's rule must not reach past a file it cannot be reconciled with"
        );

        let inner = web.enter(&d.path().join("web/inner"), "web/inner");
        assert!(
            inner.skips_dir("web/inner/build"),
            "but a descendant's own rule still applies — it outranks the bailing file"
        );
        assert!(
            !inner.skips_dir("web/inner/dist"),
            "while the root's still does not"
        );
    }

    #[test]
    fn an_ignore_file_that_exists_and_cannot_be_parsed_is_opaque() {
        // "Could not read it" says nothing about whether it holds a
        // negation, and CLAUDE.md's rule is that a failed check may never be
        // folded into a definite answer. Degrading to "ignore nothing here"
        // is the safe direction — it is the behaviour before this module
        // existed.
        //
        // Oversized, not a directory and not a mode-000 file. A directory is
        // now (correctly) "not an ignore file at all", which is a definite
        // answer rather than an unknown — the first version of this test used
        // one and started passing for the wrong reason the moment that
        // distinction was drawn. A mode-000 file would be readable anyway
        // when the suite runs as root, which this project has hit before.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".gitignore"), b"dist\n").unwrap();
        std::fs::create_dir_all(d.path().join("web")).unwrap();
        let mut huge = String::from("build\n");
        while huge.len() <= MAX_BYTES as usize {
            huge.push_str("filler\n");
        }
        std::fs::write(d.path().join("web/.gitignore"), huge.as_bytes()).unwrap();

        let root = Ignore::for_root(d.path());
        assert!(root.skips_dir("dist"), "setup: the root rule works");
        let web = root.enter(&d.path().join("web"), "web");
        assert!(
            !web.skips_dir("web/dist"),
            "a file git would read but this cannot suspends ignoring below it"
        );
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
