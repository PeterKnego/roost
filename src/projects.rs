//! Project discovery under ROOTS and all filesystem access policy:
//! path confinement, size cap, binary sniffing.
use std::path::{Path, PathBuf};

pub const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "__pycache__", ".venv"];
pub const RESERVED: &[&str] = &["static", "ws", "frag"];
const MAX_FILE_BYTES: u64 = 2_000_000;
const TEXT_EXTENSIONS: &[&str] = &[
    "rs", "toml", "md", "txt", "py", "js", "ts", "json", "yaml", "yml", "sh", "html", "css",
    "sql", "qnt", "tla", "lock", "xml", "c", "h", "cpp", "go", "java", "rb", "proto", "cfg",
    "ini", "service", "env", "gitignore", "dockerignore",
];

/// The deploy host's roots. Used unless `DEADLIGHT_ROOTS` overrides them.
pub fn default_roots() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/home/claude/ultima"),
        PathBuf::from("/home/claude/projects"),
    ]
}

/// Roots to scan for projects: `DEADLIGHT_ROOTS` (colon-separated) when set
/// and non-empty, otherwise [`default_roots`]. Lets the binary run on a
/// machine that isn't the deploy host.
pub fn roots() -> Vec<PathBuf> {
    let from_env: Vec<PathBuf> = std::env::var("DEADLIGHT_ROOTS")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    if from_env.is_empty() {
        default_roots()
    } else {
        from_env
    }
}

pub struct Project {
    pub name: String,
    pub path: PathBuf,
    pub git: bool,
}

pub fn list_projects(roots: &[PathBuf]) -> Vec<Project> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for root in roots {
        let Ok(rd) = std::fs::read_dir(root) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
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
    if segs.iter().any(|s| s.is_empty() || s.starts_with('.')) {
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
/// would treat a live project as deleted. Encoding is otherwise narrow on
/// purpose: an ordinary ASCII project name — every name that predates this
/// feature — contains none of the encoded bytes and comes back unchanged,
/// so existing state files and live sessions are unaffected.
pub fn storage_key(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        match b {
            b'/' => out.push_str("%2F"),
            b'%' => out.push_str("%25"),
            0x00..=0x7F => out.push(b as char),
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

/// One row in the directory picker (see `render::index_page`): either a
/// browsable/openable directory or a greyed-out, unselectable file.
pub struct Entry {
    pub name: String,
    /// Full rel path from the ROOTS, e.g. `"karpie/src"` — used both as the
    /// next `?at=` value (browsing) and as the workspace path (opening).
    pub rel: String,
    pub is_dir: bool,
    /// Only ever true for directories; a useful cue when choosing where to open.
    pub git: bool,
}

/// Lists the picker's rows for `at`: the merged top level (both ROOTS'
/// children, directories only — unchanged from the pre-picker index) when
/// `at` is empty, or one directory's immediate children otherwise. `None`
/// means `at` is not a legitimate, confined directory — refused the same
/// way `resolve_project` refuses to open it as a workspace, which is
/// exactly the function this reuses to resolve it.
pub fn list_dir(roots: &[PathBuf], at: &str) -> Option<Vec<Entry>> {
    if at.is_empty() {
        return Some(
            list_projects(roots)
                .into_iter()
                .map(|p| Entry { rel: p.name.clone(), name: p.name, is_dir: true, git: p.git })
                .collect(),
        );
    }
    let dir = resolve_project(roots, at)?;
    let rd = std::fs::read_dir(&dir).ok()?;
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| (e.path().is_file(), e.file_name().to_ascii_lowercase()));
    let mut out = Vec::new();
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        // Same hidden-entry convention as list_projects: dotfiles (which
        // includes each directory's own .git) never appear as rows.
        if name.starts_with('.') {
            continue;
        }
        // Skip build/vendor directories (target, node_modules, __pycache__).
        // The picker chooses a workspace, and these are never that; hiding
        // them here keeps the picker consistent with the file tree's SKIP_DIRS.
        if SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        let p = e.path();
        let is_dir = p.is_dir();
        let git = is_dir && p.join(".git").exists();
        out.push(Entry { rel: format!("{at}/{name}"), name, is_dir, git });
    }
    Some(out)
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

    #[test]
    fn read_text_file_policies() {
        let d = root_fixture();
        assert_eq!(read_text_file(&d.path().join("alpha/readme.md")).unwrap(), "hi");
        let bin = d.path().join("alpha/blob.bin");
        fs::write(&bin, b"\x00\x01\x02").unwrap();
        assert!(read_text_file(&bin).unwrap_err().contains("binary"));
    }

    #[test]
    fn roots_env_overrides_defaults() {
        std::env::set_var("DEADLIGHT_ROOTS", "/one:/two");
        assert_eq!(roots(), vec![PathBuf::from("/one"), PathBuf::from("/two")]);
        // empty or unset falls back to the built-in roots
        std::env::set_var("DEADLIGHT_ROOTS", "");
        assert_eq!(roots(), default_roots());
        std::env::remove_var("DEADLIGHT_ROOTS");
        assert_eq!(roots(), default_roots());
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

    #[test]
    fn list_dir_at_empty_is_the_merged_top_level() {
        let d = root_fixture();
        let entries = list_dir(&[d.path().to_path_buf()], "").unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]); // same set/order as list_projects
        assert!(entries.iter().all(|e| e.is_dir), "top level is directories only");
        assert!(entries.iter().find(|e| e.name == "alpha").unwrap().git);
    }

    #[test]
    fn list_dir_at_a_directory_shows_its_children_marked_distinctly() {
        let d = root_fixture();
        fs::create_dir_all(d.path().join("alpha/sub/.git")).unwrap();
        let roots = vec![d.path().to_path_buf()];
        let entries = list_dir(&roots, "alpha").unwrap();
        let file = entries.iter().find(|e| e.name == "readme.md").unwrap();
        assert!(!file.is_dir, "files are listed but must be marked non-directory");
        assert!(!file.git);
        let dir = entries.iter().find(|e| e.name == "sub").unwrap();
        assert!(dir.is_dir);
        assert!(dir.git, "sub's own .git marks it, independent of alpha's");
        assert_eq!(dir.rel, "alpha/sub"); // rel is the full path from the ROOTS
        // .git itself and other dotfiles never appear as rows
        assert!(!entries.iter().any(|e| e.name == ".git"));
    }

    #[test]
    fn list_dir_refuses_an_at_outside_the_roots() {
        let d = root_fixture();
        let roots = vec![d.path().join("alpha")]; // root is alpha itself
        // "beta" is a real directory, but it's a sibling of the root, not
        // under it — the same escape list_dir must refuse that
        // resolve_project already refuses for opening a workspace.
        assert!(list_dir(&roots, "../beta").is_none());
        assert!(list_dir(&roots, "nonexistent").is_none());
    }

    #[test]
    fn list_dir_skips_build_and_vendor_dirs() {
        let d = root_fixture();
        let roots = vec![d.path().to_path_buf()];
        // Create a subdirectory with normal dirs, build dirs, and a file.
        fs::create_dir(d.path().join("alpha/src")).unwrap();
        fs::create_dir(d.path().join("alpha/target")).unwrap();
        fs::create_dir(d.path().join("alpha/node_modules")).unwrap();
        fs::create_dir(d.path().join("alpha/__pycache__")).unwrap();
        fs::write(d.path().join("alpha/cargo.toml"), "").unwrap();
        // list_dir should return src and cargo.toml, but not the build/vendor dirs.
        let entries = list_dir(&roots, "alpha").unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"src"), "normal dirs should be listed");
        assert!(names.contains(&"cargo.toml"), "files should be listed");
        assert!(!names.contains(&"target"), "target should be skipped");
        assert!(!names.contains(&"node_modules"), "node_modules should be skipped");
        assert!(!names.contains(&"__pycache__"), "__pycache__ should be skipped");
    }
}
