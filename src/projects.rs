//! Project discovery under ROOTS and all filesystem access policy:
//! path confinement, size cap, binary sniffing.
use std::path::{Path, PathBuf};

/// Directory names that are never a project, never walked by the watcher, and
/// never offered by the picker. Mostly build and vendor output — but two
/// entries hold a whole second copy of the repository. `.git` is the obvious
/// one; `.claude` is the one that bites, because Claude Code checks worktrees
/// out at `{repo}/.claude/worktrees/{name}`, a full working tree living
/// *inside* the project directory. Without that entry the watcher spends its
/// inotify budget on a checkout nobody opened and the picker offers a worktree
/// as if it were part of its own parent, with git state belonging to a
/// different branch than the status pane beside it. A worktree is its own
/// project and is opened as one.
///
/// The file tree filters through [`TreeFilter`] instead, which hides *every*
/// dot entry by default and consults this list only for the names that do not
/// begin with a dot.
pub const SKIP_DIRS: &[&str] =
    &[".git", ".claude", "target", "node_modules", "__pycache__", ".venv"];

/// What the file tree renders and what the watcher considers a tree change —
/// one rule, so the two cannot disagree about which rows exist.
///
/// Dot entries are hidden by default and revealed together by `show_hidden`
/// (`show_hidden = true` in a config file), including `.git` and `.claude`:
/// hiding them is a decluttering default, not a safety boundary, and a user
/// who asks to see hidden files means all of them. The user's `hide` list is
/// an explicit instruction and outranks both.
#[derive(Debug, Clone, Copy, Default)]
pub struct TreeFilter<'a> {
    pub hide: &'a [String],
    pub show_hidden: bool,
}

impl TreeFilter<'_> {
    /// `name` is a single path component, never a path.
    pub fn skips(&self, name: &str) -> bool {
        if self.hide.iter().any(|h| h == name) {
            return true;
        }
        if let Some(rest) = name.strip_prefix('.') {
            // `.` and `..` never come out of `read_dir`, but `rel` segments
            // arriving from the network do reach the watcher's classifier.
            return !self.show_hidden || rest.is_empty() || rest == ".";
        }
        SKIP_DIRS.contains(&name)
    }
}

pub const RESERVED: &[&str] = &["static", "ws", "frag"];
pub const MAX_FILE_BYTES: u64 = 2_000_000;
const TEXT_EXTENSIONS: &[&str] = &[
    "rs", "toml", "md", "txt", "py", "js", "ts", "json", "yaml", "yml", "sh", "html", "css",
    "sql", "qnt", "tla", "lock", "xml", "c", "h", "cpp", "go", "java", "rb", "proto", "cfg",
    "ini", "service", "env", "gitignore", "dockerignore",
];

/// Roots to scan for projects: `RESH_ROOTS` (colon-separated) when set and
/// non-empty, otherwise the global config's `roots`. Empty when neither says
/// anything; `main` refuses to start on that rather than guessing.
///
/// There is still deliberately no compiled-in default. One machine's paths
/// used to live here, which put that host's layout into every binary and into
/// the repository. A config file is not that: it sits on the host, is not in
/// the checkout, and is the same file `allowed_origins` already lives in.
///
/// The env var wins so the unit file stays authoritative for the service, and
/// so a test or a second instance can point somewhere else for one run
/// without editing the user's config. The config entry exists for the callers
/// that are *not* the service — a caller that inherits none of the unit's
/// environment, such as a hook, and hard-coding the roots into that caller
/// duplicated the unit file in a second place that would drift.
///
/// Config side is global-only; see [`crate::config::configured_roots`] for why
/// a project file must never reach this.
pub fn roots() -> Vec<PathBuf> {
    roots_from(
        std::env::var("RESH_ROOTS").ok().as_deref(),
        &crate::config::global_config_path(),
    )
}

/// Both sources naming roots and disagreeing.
///
/// `RESH_ROOTS` wins silently, which is correct — the unit file is
/// authoritative for the service — but silence is wrong when the two were
/// meant to say the same thing. The config entry exists so callers that
/// inherit none of the unit's environment (a hook, say) resolve the same
/// projects the server does; when they drift, those callers quietly answer
/// about a different set of directories than the server serves.
///
/// Only a conflict, never a preference: one source silent is the ordinary
/// case, not a problem.
pub fn roots_conflict(env_value: Option<&str>, global: &Path) -> Option<(Vec<PathBuf>, Vec<PathBuf>)> {
    let from_env: Vec<PathBuf> = env_value
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    if from_env.is_empty() {
        return None;
    }
    let from_config = crate::config::roots_from_global(global);
    if from_config.is_empty() || from_config == from_env {
        return None;
    }
    Some((from_env, from_config))
}

/// Split from [`roots`] so the precedence can be tested without setting a
/// process-wide env var or rewriting `HOME` — both of which other tests in
/// this crate are reading concurrently.
pub fn roots_from(env_value: Option<&str>, global: &Path) -> Vec<PathBuf> {
    let from_env: Vec<PathBuf> = env_value
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    if !from_env.is_empty() {
        return from_env;
    }
    crate::config::roots_from_global(global)
}

pub struct Project {
    pub name: String,
    pub path: PathBuf,
    pub git: bool,
}

/// Ordering key shared by both listings the picker renders, so a directory's
/// children are not ordered by a different rule than the level above them.
///
/// Case-insensitive: raw `OsString` byte order puts every capitalised name
/// ahead of every lowercase one, so `Karpie` jumped to the top of the list
/// instead of sitting beside `karpie`.
///
/// Beyond ASCII this is code-point order, not alphabetical — `Ärger` sorts
/// after `zulu`. Real collation needs a locale and a Unicode table; this is a
/// dependency-free binary and a misfiled accented directory name is a cosmetic
/// cost, so the limit is accepted rather than half-solved.
fn sort_key(name: &str) -> String {
    name.to_ascii_lowercase()
}

pub fn list_projects(roots: &[PathBuf]) -> Vec<Project> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for root in roots {
        let Ok(rd) = std::fs::read_dir(root) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir()
                && !name.starts_with('.')
                && !RESERVED.contains(&name.as_str())
                && seen.insert(name.clone())
            {
                out.push(Project { git: p.join(".git").exists(), path: p, name });
            }
        }
    }
    // Sorted once over the *merged* list, not per root: the picker's top level
    // presents one alphabetical list, and sorting each root separately made the
    // concatenation order — an operator's `RESH_ROOTS` ordering, invisible in
    // the UI — the dominant sort, so a second root's `alpha` landed below the
    // first root's `zeta`. Which root a duplicate name comes from is settled
    // above by `seen`, so ordering here cannot disturb that precedence.
    out.sort_by(|a, b| sort_key(&a.name).cmp(&sort_key(&b.name)).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Resolves a project identifier — `"karpie"` or a nested `"karpie/src"` —
/// to a real, confined directory under one of `roots`. `name` is a full rel
/// path from a ROOT, joined with `/`, not just a single path component: a
/// workspace URL can now name a subdirectory (routes::route's
/// `[project, rest @ ..]` match joins the segments back into this string
/// before calling here), and the directory picker reuses this exact
/// function to confine `?at=` (see `list_dir`) — opening a workspace and
/// browsing into a directory share one security boundary.
///
/// Every segment is checked syntactically first, before any filesystem
/// access: an empty segment rules out a leading `/`, a trailing `/`, and a
/// `//` in the middle (so absolute paths are rejected here too); a
/// leading-`.` segment rules out both hidden directories and `..`
/// traversal in one check, since `..` itself starts with `.`. RESERVED is
/// checked only against the *first* segment — matching `list_projects`,
/// which likewise only ever excludes RESERVED names at the top level. A
/// real subdirectory two levels down that happens to be named e.g.
/// `static` is not this application's reserved URL prefix and must stay
/// browsable and openable.
///
/// The canonicalize-and-prefix-check loop afterward is the same discipline
/// `safe_resolve` uses: the segment checks above are necessary but not
/// sufficient, since a symlink planted partway down a project's own tree
/// (not just in the URL text) could still resolve outside the root.
pub fn resolve_project(roots: &[PathBuf], name: &str) -> Option<PathBuf> {
    let segs: Vec<&str> = name.split('/').collect();
    if RESERVED.contains(&segs[0]) {
        return None;
    }
    if segs.iter().any(|s| s.is_empty()) {
        return None;
    }
    // Dot segments stay forbidden — `.git`, `.venv`, `.config` are not
    // projects — with one exception: git itself vouching for the path as a
    // worktree. Confinement below is unchanged and still does the real work,
    // because a cloned repo could name a worktree anywhere.
    if segs.iter().any(|s| s.starts_with('.')) && !crate::worktree::is_vouched_worktree(roots, name) {
        return None;
    }
    for root in roots {
        let Ok(base) = root.canonicalize() else { continue };
        let Ok(canon) = base.join(name).canonicalize() else { continue };
        if canon.starts_with(&base) && canon.is_dir() {
            return Some(canon);
        }
    }
    None
}

/// Encodes a project identifier for use as a filesystem-adjacent storage
/// key: a state-file stem (`wsstate.rs`'s `path_for`) or a socket-path /
/// session-key component (`session.rs`'s `sock_path` and `attach`). Project
/// identifiers are now nested rel paths like `karpie/src`, and in those two
/// contexts `/` means "directory separator" or "session-key separator"
/// respectively — never project-name structure — so it must be hidden.
/// This is deliberately NOT `http::percent_encode`, which keeps `/` literal
/// because URLs want it readable; the two encoders solve opposite problems
/// for the same string. `%` is escaped too, or a literal `%2F` inside a
/// real directory name (e.g. one named `a%2Fb`) would collide with the
/// encoding of the nested project `a/b`.
///
/// Every byte outside 0x00..=0x7F (plain ASCII) is percent-encoded too, byte
/// by byte — not char by char. A name with a non-ASCII character (`café`) is
/// several UTF-8 *bytes*; naively casting each byte to `char` via `b as
/// char` reinterprets every byte >= 0x80 as its own separate Latin-1 code
/// point instead of leaving the multi-byte character it's part of intact
/// (café's `é`, two UTF-8 bytes, would become two garbled characters), so
/// the key would never decode back to the original name, `resolve_project`
/// would fail to find the real, still-existing directory, and reconcile
/// would treat a live project as deleted.
///
/// ASCII **control** bytes (`0x00..=0x1F`, plus `0x7F`) are encoded for a
/// different reason: this key becomes a path component of a dtach socket
/// path, and the only way to ask the OS who holds that socket is to read
/// `ps` output. `ps` renders a process's argv space-joined onto one line, so
/// a raw newline inside the key splits the socket path across two lines of
/// that output, and the whole-argument match in `registry` can then never
/// find it. The socket of a *running* shell reads as unheld, gets unlinked,
/// and that shell is orphaned beyond any later reap. A directory named
/// `my<newline>proj` is perfectly legal on unix and reachable as
/// `GET /my%0Aproj`, so this is not hypothetical. It is the same failure as
/// the earlier space-in-a-path bug, one delimiter along — see CLAUDE.md's
/// "Absence of evidence is not evidence of absence".
///
/// Encoding is otherwise narrow on purpose: an ordinary project name — every
/// name that predates this feature — contains none of the encoded bytes and
/// comes back unchanged, so existing state files and live sessions are
/// unaffected. That byte-for-byte stability is a hard constraint, not a nicety;
/// `existing_ascii_keys_are_unchanged_byte_for_byte` pins it.
pub fn storage_key(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        match b {
            b'/' => out.push_str("%2F"),
            b'%' => out.push_str("%25"),
            0x00..=0x1F | 0x7F => out.push_str(&format!("%{b:02X}")),
            0x20..=0x7E => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The true inverse of `storage_key`: a general percent-decoder over raw
/// bytes, not `http::percent_decode` — that is a *form* decoder (it also
/// turns `+` into a space, among other rules meant for URL query strings),
/// which is the wrong inverse here: a project literally named `gtk+` has
/// storage key `gtk+` (storage_key never touches `+`), and
/// `percent_decode("gtk+")` would wrongly turn it into `"gtk "`, making a
/// live project look like it no longer exists.
///
/// Decodes byte-for-byte (`%XX` -> that raw byte) rather than reversing only
/// the two fixed sequences `%2F`/`%25`: `storage_key` now percent-encodes
/// every non-ASCII byte too, so a name like `café` produces several distinct
/// `%XX` sequences, not just those two. The decoded bytes are re-assembled
/// with `from_utf8_lossy` rather than a strict parse, since a directory
/// listing's entry name reaching this function isn't necessarily one
/// `storage_key` ever produced — a malformed or hand-crafted `%` sequence
/// must not panic, only decode as best-effort.
pub fn decode_storage_key(key: &str) -> String {
    let bytes = key.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}



pub fn safe_resolve(project_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let canon = project_dir
        .join(rel)
        .canonicalize()
        .map_err(|e| format!("not found: {e}"))?;
    let base = project_dir.canonicalize().map_err(|e| e.to_string())?;
    if canon.starts_with(&base) {
        Ok(canon)
    } else {
        Err(format!("path outside project: {rel}"))
    }
}

/// Turns a span matched in terminal output into a project-relative path.
///
/// This is a trust boundary, not a convenience. Terminal text is chosen by
/// whatever printed it — a cloned repo's build output, a `cat`ed file — so the
/// matcher in the browser is treated as a hint and every path ends here, at
/// `safe_resolve`. That one call answers both questions worth asking: is this
/// inside the project, and is it really there. A missing file is an `Err` on
/// purpose; there is no third state to invent, and nothing here destroys
/// anything, so refusing is always the safe answer.
pub fn resolve_terminal_path(project_dir: &Path, text: &str) -> Result<String, String> {
    let bare = strip_line_suffix(text.trim());
    if bare.is_empty() {
        return Err("empty path".into());
    }

    let rel = if let Some(rest) = bare.strip_prefix("~/") {
        let home = std::env::var_os("HOME").ok_or("no home directory")?;
        abs_to_rel(project_dir, &PathBuf::from(home).join(rest))?
    } else if bare.starts_with('/') {
        abs_to_rel(project_dir, Path::new(bare))?
    } else {
        bare.trim_start_matches("./").to_string()
    };

    let abs = safe_resolve(project_dir, &rel).map_err(|e| {
        // `safe_resolve`'s own text is written for a log line, not a
        // terminal flash — the not-found branch is `format!("not found:
        // {e}")`, where `{e}` is a raw `io::Error` ("No such file or
        // directory (os error 2)"). A person clicking a false-positive link
        // (PATH_RE matches ordinary prose like "and/or" or "24/7", so this is
        // the common case, not the rare one) should see a short sentence, not
        // an errno.
        //
        // Only two shapes reach here, and they stay distinct: `safe_resolve`
        // separately reports "outside the project" (a confident, positive
        // refusal) from a canonicalize failure (anything else). The second
        // is deliberately worded "couldn't open", not "does not exist":
        // canonicalize fails the same way for a missing file as it does for
        // one that exists but can't be read (permission denied, a symlink
        // loop, ...), and CLAUDE.md's central rule is that "I could not
        // determine X" must never be folded into "X is false".
        if e.starts_with("path outside project") {
            format!("{rel} is outside this project")
        } else {
            format!("couldn't open {rel}")
        }
    })?;
    // Not `is_dir()`: it answers `false` both for "not a directory" and for
    // "could not look", and this codebase has shipped that conflation eleven
    // times. Three outcomes, matched explicitly.
    match std::fs::metadata(&abs) {
        Ok(m) if m.is_dir() => Err(format!("{rel} is a directory")),
        Ok(_) => Ok(rel),
        Err(e) => Err(format!("cannot read {rel}: {e}")),
    }
}

/// `src/main.rs:42` and `src/main.rs:42:7` both name `src/main.rs`. The browser
/// matcher deliberately swallows the suffix so the whole reference underlines;
/// this is where it comes back off.
///
/// A file whose real name ends in `:42` is therefore unreachable by this route.
/// That trade is not close: a colon in a filename is rare, a compiler citation
/// is most of what a terminal prints, and the file is still reachable from the
/// tree.
fn strip_line_suffix(text: &str) -> &str {
    let mut s = text;
    for _ in 0..2 {
        let Some((head, tail)) = s.rsplit_once(':') else { break };
        if tail.is_empty() || !tail.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        s = head;
    }
    s
}

/// An absolute path is only this project's to open if it is under this
/// project. Both sides are canonicalised before comparing, so a symlinked
/// project root still matches its own files rather than refusing them.
///
/// `pub(crate)` for `ide.rs`: `openDiff` carries absolute paths chosen by
/// whatever Claude is looking at, which is the same trust level as a path
/// scraped out of terminal output. A second copy of this over there would be
/// a second trust boundary to keep in sync, and the two would drift.
///
/// Note the precondition this carries: `abs` must already exist, because
/// `canonicalize` is what resolves the symlinks the comparison depends on. A
/// caller confining a path that does not exist yet must confine the parent —
/// see `safe_resolve_parent` for the same split.
pub(crate) fn abs_to_rel(project_dir: &Path, abs: &Path) -> Result<String, String> {
    let root = project_dir
        .canonicalize()
        .map_err(|e| format!("project root unreadable: {e}"))?;
    let abs = abs.canonicalize().map_err(|_| "no such file".to_string())?;
    abs.strip_prefix(&root)
        .map_err(|_| "path is outside this project".to_string())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Confine a path whose *target does not exist yet* (creation, rename
/// destination). `safe_resolve` canonicalizes the target and so cannot be
/// used here. Canonicalize the parent instead, confine that, then validate
/// the final component separately.
pub fn safe_resolve_parent(project_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim_start_matches('/');
    let (parent_rel, name) = match rel.rsplit_once('/') {
        Some((p, n)) => (p, n),
        None => ("", rel),
    };
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(format!("bad name: {name:?}"));
    }
    let base = project_dir.canonicalize().map_err(|e| e.to_string())?;
    let parent = if parent_rel.is_empty() {
        base.clone()
    } else {
        base.join(parent_rel).canonicalize().map_err(|e| format!("no such directory: {e}"))?
    };
    if !parent.starts_with(&base) {
        return Err(format!("path outside project: {rel}"));
    }
    Ok(parent.join(name))
}

pub fn read_text_file(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("not found: {e}"))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("file too large ({} bytes)", meta.len()));
    }
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let sniff = &data[..data.len().min(8000)];
    if sniff.contains(&0u8) && !TEXT_EXTENSIONS.contains(&ext.as_str()) {
        return Err("binary file".into());
    }
    Ok(String::from_utf8_lossy(&data).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // The whole rule in one place. Each assertion pairs the two settings so a
    // filter that ignored `show_hidden` entirely (returning a constant) fails
    // one half of every pair.
    #[test]
    fn tree_filter_hides_every_dot_entry_until_show_hidden_reveals_them() {
        let off = TreeFilter::default();
        let on = TreeFilter { show_hidden: true, ..Default::default() };
        for name in [".gitignore", ".claude", ".git", ".venv", ".hidden"] {
            assert!(off.skips(name), "{name} must be hidden by default");
            assert!(!on.skips(name), "show_hidden must reveal {name}");
        }
        // Ordinary entries are never touched by either.
        for name in ["src", "README.md", "Cargo.toml"] {
            assert!(!off.skips(name));
            assert!(!on.skips(name));
        }
    }

    // `show_hidden` is about clutter, not about build output: revealing dot
    // entries must not drag `target/` into a Rust project's tree.
    #[test]
    fn show_hidden_does_not_reveal_the_non_dot_build_dirs() {
        let on = TreeFilter { show_hidden: true, ..Default::default() };
        for name in ["target", "node_modules", "__pycache__"] {
            assert!(TreeFilter::default().skips(name));
            assert!(on.skips(name), "{name} is build output, not a hidden file");
        }
    }

    // The user's list is an explicit instruction, so it outranks `show_hidden`
    // in both directions: it hides a plain directory, and it keeps hiding a
    // dot entry that `show_hidden` would otherwise have revealed.
    #[test]
    fn the_hide_list_outranks_show_hidden() {
        let hide = vec!["dist".to_string(), ".gitignore".to_string()];
        let on = TreeFilter { hide: &hide, show_hidden: true };
        assert!(on.skips("dist"));
        assert!(on.skips(".gitignore"));
        assert!(!on.skips(".git")); // an unlisted dot entry is still revealed
        assert!(!on.skips("src"));
    }

    // `read_dir` never yields these, but a `rel` from the network reaches the
    // watcher's classifier, and "show me hidden files" is not consent to treat
    // a traversal segment as an ordinary name.
    #[test]
    fn show_hidden_never_reveals_the_traversal_segments() {
        let on = TreeFilter { show_hidden: true, ..Default::default() };
        assert!(on.skips("."));
        assert!(on.skips(".."));
    }

    fn root_fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("alpha")).unwrap();
        fs::create_dir(d.path().join("beta")).unwrap();
        fs::create_dir(d.path().join(".hidden")).unwrap();
        fs::create_dir(d.path().join("static")).unwrap(); // reserved name
        fs::create_dir_all(d.path().join("alpha/.git")).unwrap();
        fs::write(d.path().join("alpha/readme.md"), "hi").unwrap();
        d
    }

    #[test]
    fn lists_visible_unreserved_dirs() {
        let d = root_fixture();
        let ps = list_projects(&[d.path().to_path_buf()]);
        let names: Vec<_> = ps.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
        assert!(ps[0].git);
        assert!(!ps[1].git);
    }

    #[test]
    fn first_root_wins_on_duplicate_name() {
        let d1 = root_fixture();
        let d2 = tempfile::tempdir().unwrap();
        fs::create_dir(d2.path().join("alpha")).unwrap();
        let ps = list_projects(&[d1.path().to_path_buf(), d2.path().to_path_buf()]);
        assert_eq!(ps.iter().filter(|p| p.name == "alpha").count(), 1);
        assert!(ps.iter().find(|p| p.name == "alpha").unwrap().path.starts_with(d1.path()));
    }

    // Both assertions here are ones the pre-sort code failed, in different
    // ways: `Zeta` sorted ahead of `beta` under raw OsString byte order, and
    // `Alpha` — living in the second root — was appended after the whole of
    // the first root regardless of its name. Reverting either half of
    // list_projects' sort turns this red.
    #[test]
    fn top_level_is_one_alphabetical_list_across_roots_and_ignores_case() {
        let d1 = tempfile::tempdir().unwrap();
        let d2 = tempfile::tempdir().unwrap();
        fs::create_dir(d1.path().join("Zeta")).unwrap();
        fs::create_dir(d1.path().join("beta")).unwrap();
        fs::create_dir(d2.path().join("Alpha")).unwrap();
        let ps = list_projects(&[d1.path().to_path_buf(), d2.path().to_path_buf()]);
        let names: Vec<_> = ps.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "beta", "Zeta"]);
    }

    // Ordering must not become a back door around root precedence: `alpha`
    // exists under both roots, and the first root still owns it however the
    // merged list is later sorted.
    #[test]
    fn alphabetical_order_does_not_disturb_first_root_precedence() {
        let d1 = root_fixture();
        let d2 = tempfile::tempdir().unwrap();
        fs::create_dir(d2.path().join("alpha")).unwrap();
        fs::create_dir(d2.path().join("Aardvark")).unwrap(); // sorts first, different root
        let ps = list_projects(&[d1.path().to_path_buf(), d2.path().to_path_buf()]);
        let names: Vec<_> = ps.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Aardvark", "alpha", "beta"]);
        assert!(ps.iter().find(|p| p.name == "alpha").unwrap().path.starts_with(d1.path()));
    }

    // Not a test of the top-level fix — list_dir already folded case — but of
    // the invariant the two listings now share via `sort_key`: this is what
    // stops the sub-level from drifting back to byte order while the top level
    // stays folded, which is exactly the split the fix removed. Reverting
    // list_dir's key to a raw `e.file_name()` turns this red.
    #[test]
    fn resolve_rejects_bad_names() {
        let d = root_fixture();
        let roots = vec![d.path().to_path_buf()];
        assert!(resolve_project(&roots, "alpha").is_some());
        assert!(resolve_project(&roots, "nope").is_none());
        assert!(resolve_project(&roots, "a/b").is_none());
        assert!(resolve_project(&roots, "..").is_none());
        assert!(resolve_project(&roots, ".hidden").is_none());
        assert!(resolve_project(&roots, "static").is_none());
        assert!(resolve_project(&roots, "").is_none());
    }

    // A future change to resolve_project could easily regress any one of
    // these while "fixing" another — each is a distinct property this
    // security boundary must keep holding for a nested rel, not just a
    // single-segment name.
    #[test]
    fn resolve_project_accepts_nested_rel_and_keeps_every_safety_property() {
        let d = root_fixture();
        // its own fixture directory (not root_fixture's shared `alpha`),
        // so this doesn't perturb safe_resolve_parent's tests, which rely
        // on `sub` *not* existing under `alpha` to exercise their ENOENT case
        fs::create_dir_all(d.path().join("alpha/sub")).unwrap();
        fs::write(d.path().join("alpha/sub/inner.txt"), "hi").unwrap();
        let roots = vec![d.path().to_path_buf()];
        // legitimate nested rel resolves to the real, canonicalized directory
        let got = resolve_project(&roots, "alpha/sub").expect("alpha/sub exists");
        assert_eq!(got, d.path().join("alpha/sub").canonicalize().unwrap());
        // `..` traversal, anywhere in the rel, not just at the start
        assert!(resolve_project(&roots, "alpha/..").is_none());
        assert!(resolve_project(&roots, "alpha/../beta").is_none());
        assert!(resolve_project(&roots, "..").is_none());
        // absolute path (a leading `/` produces a leading empty segment)
        assert!(resolve_project(&roots, "/etc/passwd").is_none());
        // leading-dot segment, whether first or nested
        assert!(resolve_project(&roots, ".hidden/sub").is_none());
        assert!(resolve_project(&roots, "alpha/.git").is_none());
        // reserved name as the first segment only — a real nested directory
        // that happens to share a name with a reserved word, two levels
        // down, is not this application's URL prefix and stays resolvable
        fs::create_dir(d.path().join("alpha/static")).unwrap();
        assert!(resolve_project(&roots, "static/sub").is_none());
        assert!(resolve_project(&roots, "alpha/static").is_some());
    }

    #[test]
    fn safe_resolve_blocks_escapes() {
        let d = root_fixture();
        let alpha = d.path().join("alpha");
        assert!(safe_resolve(&alpha, "readme.md").is_ok());
        assert!(safe_resolve(&alpha, "../beta").is_err());
        assert!(safe_resolve(&alpha, "/etc/passwd").is_err());
        assert!(safe_resolve(&alpha, "missing.txt").is_err());
    }

    // Revert-the-fix check (CLAUDE.md: "the technique that actually works"),
    // applied for real against this working tree, one change at a time, then
    // restored. Exact panic output observed for each:
    //
    // 1. `abs.strip_prefix(&root)` arm in `abs_to_rel` replaced with
    //    `Ok(abs.to_string_lossy().into_owned())` (confinement check removed).
    //    Two tests failed, not one — `safe_resolve`'s own outside-project
    //    check still fired (the absolute path, once let through unconfined,
    //    still failed `canon.starts_with(&base)` downstream), but with a
    //    different message than this test asserts on, and the absolute-path
    //    case in the other test now returned the *un*confined absolute string:
    //      terminal_path_refuses_a_real_file_outside_the_project:
    //        panicked at src/projects.rs:699:9:
    //        expected a confinement refusal naming the reason, got "path outside project: /tmp/.tmp0xSRIq/secret.txt"
    //      terminal_path_resolves_relative_and_absolute:
    //        panicked at src/projects.rs:670:9:
    //        assertion `left == right` failed
    //          left: "/tmp/.tmpRc9oVQ/src/a.rs"
    //         right: "src/a.rs"
    // 2. `Ok(m) if m.is_dir()` arm deleted from `resolve_terminal_path`'s match.
    //      terminal_path_refuses_a_directory:
    //        panicked at src/projects.rs:713:61:
    //        called `Result::unwrap_err()` on an `Ok` value: "src"
    // 3. `strip_line_suffix` changed to `fn strip_line_suffix(text: &str) -> &str { text }`.
    //    Failed one step earlier than expected: the untouched suffix made
    //    `safe_resolve` look up a file named `src/a.rs:42`, which does not
    //    exist, so the panic is `safe_resolve`'s "not found", not an
    //    equality mismatch on the returned path:
    //      terminal_path_strips_line_and_column:
    //        panicked at src/projects.rs:675:70:
    //        called `Result::unwrap()` on an `Err` value: "not found: No such file or directory (os error 2)"
    // 4. (Fix round 1) The `if let Some(rest) = bare.strip_prefix("~/")` arm
    //    removed from `resolve_terminal_path`, so `~/...` falls into the
    //    plain-relative arm unchanged.
    //      terminal_path_resolves_a_tilde_path:
    //        panicked at src/projects.rs:707:63:
    //        called `Result::unwrap()` on an `Err` value: "not found: No such file or directory (os error 2)"
    //    (`safe_resolve` looked for a literal directory named `~`, which does
    //    not exist, and refused before reaching the assertion — same shape as
    //    revert 3 above: the missing-branch failure surfaces one layer down,
    //    inside `safe_resolve`, not at the equality check.)
    // All four restored; full `terminal_path` suite passes again (7/7).

    #[test]
    fn terminal_path_resolves_relative_and_absolute() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/a.rs"), b"fn main() {}").unwrap();

        assert_eq!(resolve_terminal_path(root.path(), "src/a.rs").unwrap(), "src/a.rs");
        assert_eq!(resolve_terminal_path(root.path(), "./src/a.rs").unwrap(), "src/a.rs");

        let abs = root.path().join("src/a.rs");
        assert_eq!(
            resolve_terminal_path(root.path(), abs.to_str().unwrap()).unwrap(),
            "src/a.rs"
        );
    }

    /// Building the fixture under the real `$HOME` (rather than mutating
    /// `HOME` itself) is deliberate: env vars are process-global and `cargo
    /// test` runs tests in parallel, so `set_var` here would corrupt
    /// whichever other test happened to read `HOME` concurrently.
    #[test]
    fn terminal_path_resolves_a_tilde_path() {
        let Some(home) = std::env::var_os("HOME") else {
            return; // no HOME on this machine: a legitimate state, not a failure
        };
        let home = PathBuf::from(home);
        match std::fs::metadata(&home) {
            Ok(m) if m.is_dir() => {}
            _ => return, // HOME missing, unreadable, or not a directory: skip, don't fail
        }

        let root = tempfile::tempdir_in(&home).unwrap();
        std::fs::write(root.path().join("a.rs"), b"fn main() {}").unwrap();

        let rel_to_home = root.path().strip_prefix(&home).unwrap().to_str().unwrap();
        let input = format!("~/{rel_to_home}/a.rs");

        assert_eq!(resolve_terminal_path(root.path(), &input).unwrap(), "a.rs");
    }

    /// Verifies the claim in `abs_to_rel`'s doc comment: a symlinked project
    /// root still matches its own files rather than refusing them, because
    /// both sides are canonicalised before the `strip_prefix` comparison.
    #[test]
    fn terminal_path_resolves_through_a_symlinked_project_root() {
        let parent = tempfile::tempdir().unwrap();
        let real = parent.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("a.rs"), b"fn main() {}").unwrap();

        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let abs = link.join("a.rs");
        assert_eq!(resolve_terminal_path(&link, abs.to_str().unwrap()).unwrap(), "a.rs");
    }

    #[test]
    fn terminal_path_strips_line_and_column() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/a.rs"), b"x").unwrap();

        assert_eq!(resolve_terminal_path(root.path(), "src/a.rs:42").unwrap(), "src/a.rs");
        assert_eq!(resolve_terminal_path(root.path(), "src/a.rs:42:7").unwrap(), "src/a.rs");
    }

    /// The escape target is a REAL file outside the project. Without it this
    /// test would fail with "no such file" before confinement was consulted
    /// at all — green, and proving nothing. CLAUDE.md lists that exact
    /// failure as the reason a symlink escape once survived review.
    #[test]
    fn terminal_path_refuses_a_real_file_outside_the_project() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        let outside = parent.path().join("secret.txt");
        std::fs::write(&outside, b"real file, really there").unwrap();

        // Absolute-path route: refused inside `abs_to_rel`, before `safe_resolve`
        // is ever called, with its own fixed wording.
        let err = resolve_terminal_path(&root, outside.to_str().unwrap()).unwrap_err();
        assert_eq!(
            err,
            "path is outside this project",
            "expected a confinement refusal naming the reason, got {err:?}"
        );

        // Relative `../` route: never touches `abs_to_rel`, so this is what
        // exercises the new mapping over `safe_resolve`'s own
        // "path outside project: {rel}" — the case the review flagged
        // (item 1) as leaking that string, and later ones as leaking a raw
        // errno for the not-found branch below.
        let err = resolve_terminal_path(&root, "../secret.txt").unwrap_err();
        assert_eq!(err, "../secret.txt is outside this project", "unexpected refusal: {err:?}");
    }

    #[test]
    fn terminal_path_refuses_a_directory() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();

        let err = resolve_terminal_path(root.path(), "src").unwrap_err();
        assert!(
            err.contains("directory"),
            "expected the refusal to say it is a directory, got {err:?}"
        );
    }

    /// The review that shipped this test's earlier `!err.is_empty()` version
    /// (item 1 of the terminal-links fix wave) is the whole reason this one
    /// asserts on exact text: the previous message was `safe_resolve`'s raw
    /// `io::Error` verbatim — "not found: No such file or directory (os
    /// error 2)" — an errno flashed at a person clicking a false-positive
    /// link, which `PATH_RE` produces often (`and/or`, `24/7`, ...). A
    /// non-empty check would still pass on that regression.
    #[test]
    fn terminal_path_refuses_a_file_that_is_not_there() {
        let root = tempfile::tempdir().unwrap();
        let err = resolve_terminal_path(root.path(), "src/gone.rs").unwrap_err();
        assert_eq!(
            err,
            "couldn't open src/gone.rs",
            "expected a readable refusal with no raw errno, got {err:?}"
        );
        // "couldn't open", not "does not exist": a canonicalize failure here
        // could equally be a permission problem, and CLAUDE.md's central
        // rule is that "I could not determine X" must never be folded into
        // "X is false".
        assert!(!err.contains("os error"), "errno leaked into the refusal: {err:?}");
    }

    #[test]
    fn read_text_file_policies() {
        let d = root_fixture();
        assert_eq!(read_text_file(&d.path().join("alpha/readme.md")).unwrap(), "hi");
        let bin = d.path().join("alpha/blob.bin");
        fs::write(&bin, b"\x00\x01\x02").unwrap();
        assert!(read_text_file(&bin).unwrap_err().contains("binary"));
    }

    /// The original assertion this replaces: reverting to a *compiled-in*
    /// fallback made the no-env cases fail with the old host paths in `left`,
    /// which was the leak — those paths were readable in the binary and in
    /// the repository. That property is still pinned by the last case here:
    /// with nothing in the environment and a config file that mentions no
    /// roots, the answer is empty rather than some guessed directory.
    #[test]
    fn roots_come_from_the_environment_first_then_the_global_config_and_never_from_the_binary() {
        let d = tempfile::tempdir().unwrap();
        let cfg = d.path().join("config.toml");
        std::fs::write(&cfg, "roots = [\"/from/config\"]\n").unwrap();

        assert_eq!(
            roots_from(Some("/one:/two"), &cfg),
            vec![PathBuf::from("/one"), PathBuf::from("/two")],
            "the unit file's env must stay authoritative for the service"
        );
        assert_eq!(
            roots_from(None, &cfg),
            vec![PathBuf::from("/from/config")],
            "with no env, the global config answers — what any caller outside the service relies on"
        );
        assert_eq!(
            roots_from(Some(""), &cfg),
            vec![PathBuf::from("/from/config")],
            "an empty RESH_ROOTS is 'unset', not 'deliberately no roots'"
        );

        // No built-in fallback, still. A config file that says nothing about
        // roots leaves the answer empty and `main` exits.
        let silent = d.path().join("silent.toml");
        std::fs::write(&silent, "allowed_origins = []\n").unwrap();
        assert!(roots_from(None, &silent).is_empty(), "a config without roots must not fall back");
        assert!(
            roots_from(None, &d.path().join("absent.toml")).is_empty(),
            "a missing config must not fall back"
        );
    }

    /// A hand-edited file invites `~/`. Left literal it would name a
    /// directory that matches nothing, and resh would come up healthy showing
    /// no projects at all — indistinguishable from every project having been
    /// deleted, which is the failure mode this codebase keeps relearning.
    #[test]
    fn a_tilde_root_expands_rather_than_naming_a_directory_that_cannot_exist() {
        let d = tempfile::tempdir().unwrap();
        let cfg = d.path().join("config.toml");
        std::fs::write(&cfg, "roots = [\"~/projects\", \"/absolute\", \"  \"]\n").unwrap();
        let home = std::env::var("HOME").expect("tests run with HOME set");
        assert_eq!(
            roots_from(None, &cfg),
            vec![PathBuf::from(&home).join("projects"), PathBuf::from("/absolute")],
            "~/ expands, absolute paths pass through, blank entries are dropped"
        );
    }

    #[test]
    fn safe_resolve_parent_allows_new_names_and_blocks_escapes() {
        let d = root_fixture();
        let alpha = d.path().join("alpha");
        // the point of this resolver: the target does not exist yet
        assert!(safe_resolve_parent(&alpha, "new.txt").is_ok());
        assert!(safe_resolve_parent(&alpha, "../escape.txt").is_err());
        assert!(safe_resolve_parent(&alpha, "/etc/newfile").is_err());
        assert!(safe_resolve_parent(&alpha, "").is_err());
        assert!(safe_resolve_parent(&alpha, "..").is_err());
        // `sub` doesn't exist under alpha, so this case fails on ENOENT
        // before ever reaching the confinement check below — it doesn't
        // prove the `..`-in-the-middle case is actually confined.
        assert!(safe_resolve_parent(&alpha, "sub/../../out.txt").is_err());
        // a missing parent directory is an error, not a silent mkdir -p
        assert!(safe_resolve_parent(&alpha, "nodir/new.txt").is_err());
    }

    #[test]
    fn safe_resolve_parent_confines_a_dot_dot_that_actually_canonicalizes() {
        let d = root_fixture();
        let alpha = d.path().join("alpha");
        // `alpha/.git` genuinely exists (see root_fixture), so
        // `.git/../..` canonicalizes cleanly to the tempdir root, one level
        // above `alpha` — this is the case that must actually hit the
        // `starts_with` confinement branch, not bail out on ENOENT first.
        let err = safe_resolve_parent(&alpha, ".git/../../out.txt").unwrap_err();
        assert!(err.contains("outside project"), "unexpected error: {err}");
    }

    #[test]
    fn storage_key_round_trips_and_leaves_plain_names_unchanged() {
        // The overwhelmingly common case — every project name that predates
        // this feature — must come back byte-for-byte, or existing state
        // files and live sessions break on upgrade.
        assert_eq!(storage_key("proj"), "proj");
        assert_eq!(storage_key("karpie-2"), "karpie-2");
        // `/` is hidden: it means "directory separator" or "session-key
        // separator" in the two places this key is used, not project structure.
        assert_eq!(storage_key("karpie/src"), "karpie%2Fsrc");
        // `%` is escaped too, or a literal `%2F` in a real (if unusual)
        // directory name would collide with the encoding of a nested project.
        assert_eq!(storage_key("a%2Fb"), "a%252Fb");
        assert_ne!(storage_key("a%2Fb"), storage_key("a/b"), "must not collide");
        // distinct inputs must never encode to the same key
        assert_ne!(storage_key("karpie/src"), storage_key("karpie-src"));
    }

    #[test]
    fn decode_storage_key_is_a_true_inverse_of_storage_key() {
        // `gtk+` is a regression case: http::percent_decode (a form
        // decoder) would turn a literal `+` into a space, making a real
        // project named `gtk+` decode to `"gtk "` and look like it no
        // longer exists. decode_storage_key must leave `+` alone.
        //
        // `café`/`café/src` are another: storage_key used to cast bytes to
        // `char` one at a time, which mangles any non-ASCII character (a
        // multi-byte UTF-8 sequence) into garbage that never decodes back —
        // the identical "looks deleted, gets killed" failure as `gtk+`, just
        // for a different reason. `résumé/notes` covers a non-ASCII byte
        // landing immediately next to an escaped `/`, so the two encodings
        // can't be mistaken for each other.
        for name in ["karpie", "a/b", "a%2Fb", "gtk+", "karpie/src", "café", "café/src", "résumé/notes"] {
            assert_eq!(decode_storage_key(&storage_key(name)), name, "round trip broke for {name:?}");
        }
        // storage_key must actually escape the non-ASCII bytes, not merely
        // happen to round-trip through some other quirk.
        assert!(storage_key("café").is_ascii(), "a storage key must never contain a raw non-ASCII byte");
        assert_ne!(
            crate::http::percent_decode("gtk+"),
            "gtk+",
            "percent_decode is a form decoder and is NOT storage_key's inverse — that's the bug this guards"
        );
        assert_eq!(decode_storage_key("gtk+"), "gtk+");
    }

    /// A control byte in a project name must not survive into the storage key,
    /// because the key becomes a component of a dtach socket path and the only
    /// way to learn who holds that socket is to parse line-oriented `ps`
    /// output. A raw newline splits one process's argv across two lines there,
    /// so the whole-argument match can never find the path, the socket of a
    /// *running* shell reads as unheld, and it gets unlinked — orphaning that
    /// shell permanently. `\n` is the one that actually breaks `ps`; the rest
    /// are escaped with it because there is no reason to leave a class of bytes
    /// half-handled, and `\x7F`/`\0` are included for the same reason.
    #[test]
    fn control_bytes_never_reach_a_storage_key() {
        for name in ["my\nproj", "a\tb", "x\r\ny", "trail\n", "\nlead", "bell\x07", "del\x7f", "nul\0x"] {
            let key = storage_key(name);
            assert!(
                !key.bytes().any(|b| b.is_ascii_control()),
                "storage_key({name:?}) = {key:?} still contains a control byte, so a live \
                 session's socket path would be unmatchable in `ps` output and get unlinked"
            );
            assert_eq!(decode_storage_key(&key), name, "round trip broke for {name:?}");
        }
        // The specific byte and case from the finding, pinned exactly.
        assert_eq!(storage_key("my\nproj"), "my%0Aproj");
    }

    /// The hard constraint from CLAUDE.md: existing top-level storage keys stay
    /// byte-for-byte identical. Real state files and live dtach sockets on the
    /// deploy host are named by this function, so widening its escape set must
    /// never rename an existing key — that would orphan both the saved workspace
    /// and the running session behind it. These are the actual key shapes in use.
    #[test]
    fn existing_ascii_keys_are_unchanged_byte_for_byte() {
        for name in [
            "karpie",
            "resh",
            "ultima_db",
            "ultima",
            "ml",
            "archive",
            "claude_code_proxy",
            "karpie-2",
            "gtk+",
            "my project",     // spaces are legal and must stay literal
            "dot.name",       // as must dots
            "a~b!c@d#e",      // and every other printable ASCII
        ] {
            assert_eq!(
                storage_key(name), name,
                "storage_key must leave plain printable ASCII untouched — {name:?} changed, \
                 which would orphan its existing state file and dtach socket"
            );
        }
        // The two deliberate exceptions, unchanged by this widening.
        assert_eq!(storage_key("karpie/src"), "karpie%2Fsrc");
        assert_eq!(storage_key("a%2Fb"), "a%252Fb");
    }

    /// A conflict is both sources speaking and disagreeing — never one being
    /// silent, which is the ordinary case on every host that sets only the
    /// unit file. Detected because `RESH_ROOTS` wins here while a caller that
    /// inherits none of the server's environment, such as a hook, silently
    /// resolves the other set.
    #[test]
    fn only_two_disagreeing_sources_are_a_conflict() {
        let d = tempfile::tempdir().unwrap();
        let cfg = d.path().join("config.toml");
        std::fs::write(&cfg, "roots = [\"/a\", \"/b\"]\n").unwrap();
        let silent = d.path().join("silent.toml");
        std::fs::write(&silent, "theme = \"dawn\"\n").unwrap();

        assert!(roots_conflict(None, &cfg).is_none(), "no env: config simply answers");
        assert!(roots_conflict(Some(""), &cfg).is_none(), "empty env is 'unset', not a conflict");
        assert!(roots_conflict(Some("/a:/b"), &silent).is_none(), "no config: env simply answers");
        assert!(roots_conflict(Some("/a:/b"), &cfg).is_none(), "agreeing sources are not a conflict");

        let got = roots_conflict(Some("/a:/different"), &cfg).expect("disagreeing sources conflict");
        assert_eq!(got.0, vec![PathBuf::from("/a"), PathBuf::from("/different")], "env side");
        assert_eq!(got.1, vec![PathBuf::from("/a"), PathBuf::from("/b")], "config side");

        // Order is part of the answer: roots are searched in order and the
        // first match wins, so the same set listed differently resolves a
        // duplicate project name to a different directory.
        assert!(roots_conflict(Some("/b:/a"), &cfg).is_some(), "reordering changes which root wins");
    }
}
