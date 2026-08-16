# deadlight v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the untested Python v1 with a Rust single-binary per-project workspace: persistent zellij terminal over one websocket + stateless server-rendered viewer (tree/file/markdown/diff) via htmx.

**Architecture:** One `TcpListener` on 127.0.0.1:8444, thread per connection. Each connection is peeked: `GET /ws/` → tungstenite handshake → PTY running `zellij attach --create {project}`; anything else → hand-rolled GET-only HTTP parser → routed to server-rendered HTML pages/fragments. No async runtime, no client state, no writes over HTTP.

**Tech Stack:** Rust 2021 (rustc 1.96), tungstenite 0.24, portable-pty 0.8, pulldown-cmark 0.13, toml 0.8 + serde 1. Dev: ureq 2, tempfile 3. Frontend: vendored htmx 2.0.4, @xterm/xterm 5.5.0 (+fit addon 0.10.0), existing vendored highlight.js + github-markdown-css.

**Spec:** `docs/superpowers/specs/2026-08-16-deadlight-v2-design.md`

## Global Constraints

- Bind `127.0.0.1` only. Port 8444 default, overridable as argv[1]. **Never bind 0.0.0.0 — the websocket is a shell.**
- `ROOTS = ["/home/claude/ultima", "/home/claude/projects"]`; reserved top-level names `static`, `ws`, `frag`; duplicate project name → first root wins.
- `SKIP_DIRS = [".git", "target", "node_modules", "__pycache__", ".venv"]`; per-project `hide` appends to it.
- File cap 2,000,000 bytes; binary sniff = NUL in first 8000 bytes and extension not in the known-text list.
- Config re-read on **every** request; malformed config → defaults + visible warning, never a crash.
- No `pushState`/`hx-push-url` anywhere. Hash mirror uses `replaceState` only.
- Crate edition `2021` (keeps `std::env::set_var` safe for tests).
- All commands run from the repo root `/home/claude/projects/deadlight`. Run `cargo test` (never `cargo test --release`).
- The old Python v1 files (`server.py`, `static/index.html`, `static/app.js`, `static/vendor/marked.min.js`) stay untouched until Task 9 removes them.

---

### Task 1: Cargo scaffold + `http` module (request parsing, responses, percent codec)

**Files:**
- Create: `Cargo.toml`, `.gitignore`, `src/main.rs`, `src/lib.rs`, `src/http.rs`

**Interfaces:**
- Produces: `http::Request { path: String, query: HashMap<String,String> }`; `http::parse<R: BufRead>(&mut R) -> Result<Request, String>`; `http::respond(w, status: u16, reason: &str, ctype: &str, body: &[u8])`; `http::html(w, &str)`; `http::not_found(w, &str)`; `http::percent_decode(&str) -> String`; `http::percent_encode(&str) -> String` (keeps `/` literal). All `w: &mut impl Write`.

- [ ] **Step 1: Create the scaffold**

`Cargo.toml`:

```toml
[package]
name = "deadlight"
version = "0.2.0"
edition = "2021"

[dependencies]
tungstenite = "0.24"
portable-pty = "0.8"
pulldown-cmark = "0.13"
toml = "0.8"
serde = { version = "1", features = ["derive"] }

[dev-dependencies]
ureq = "2"
tempfile = "3"

[profile.release]
strip = true
```

`.gitignore`:

```
/target
```

`src/lib.rs`:

```rust
pub mod http;
```

`src/main.rs`:

```rust
fn main() {
    println!("deadlight: not wired yet");
}
```

- [ ] **Step 2: Write `src/http.rs` with tests only (no implementation yet)**

```rust
//! Minimal HTTP/1.1 layer: GET-only request parsing, response writing,
//! percent encoding/decoding. Hand-rolled deliberately — see spec §Stack.
use std::collections::HashMap;
use std::io::{BufRead, Write};

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse_str(raw: &str) -> Result<Request, String> {
        parse(&mut Cursor::new(raw.as_bytes()))
    }

    #[test]
    fn parses_path_and_query() {
        let r = parse_str("GET /frag/proj/file?path=src%2Fmain.rs&x=a+b HTTP/1.1\r\nHost: h\r\n\r\n").unwrap();
        assert_eq!(r.path, "/frag/proj/file");
        assert_eq!(r.query["path"], "src/main.rs");
        assert_eq!(r.query["x"], "a b");
    }

    #[test]
    fn parses_bare_path() {
        let r = parse_str("GET /alpha HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(r.path, "/alpha");
        assert!(r.query.is_empty());
    }

    #[test]
    fn rejects_non_get() {
        assert!(parse_str("POST / HTTP/1.1\r\n\r\n").is_err());
        assert!(parse_str("").is_err());
    }

    #[test]
    fn percent_roundtrip() {
        assert_eq!(percent_decode("a%20b%2Fc+d"), "a b/c d");
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
        assert_eq!(percent_encode("src/main file.rs"), "src/main%20file.rs");
        assert_eq!(percent_encode("a&b?c"), "a%26b%3Fc");
    }

    #[test]
    fn respond_writes_status_and_headers() {
        let mut out = Vec::new();
        respond(&mut out, 404, "Not Found", "text/plain", b"nope");
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(s.contains("Content-Length: 4\r\n"));
        assert!(s.ends_with("\r\n\r\nnope"));
    }
}
```

- [ ] **Step 3: Run tests, expect compile failure**

Run: `cargo test http`
Expected: FAIL — `cannot find struct/fn 'Request'/'parse'` etc.

- [ ] **Step 4: Add the implementation above the test module in `src/http.rs`**

```rust
pub struct Request {
    pub path: String,
    pub query: HashMap<String, String>,
}

pub fn parse<R: BufRead>(r: &mut R) -> Result<Request, String> {
    let mut line = String::new();
    r.read_line(&mut line).map_err(|e| e.to_string())?;
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or("empty request")?;
    let target = parts.next().ok_or("no path")?.to_string();
    if method != "GET" {
        return Err(format!("method {method} not allowed"));
    }
    loop {
        // drain headers; we need none of them for plain GETs
        let mut h = String::new();
        let n = r.read_line(&mut h).map_err(|e| e.to_string())?;
        if n == 0 || h == "\r\n" || h == "\n" {
            break;
        }
    }
    let (path, query_str) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let mut query = HashMap::new();
    for pair in query_str.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(percent_decode(k), percent_decode(v));
    }
    Ok(Request { path: percent_decode(&path), query })
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match std::str::from_utf8(&b[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn respond(w: &mut impl Write, status: u16, reason: &str, ctype: &str, body: &[u8]) {
    let _ = write!(
        w,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = w.write_all(body);
    let _ = w.flush();
}

pub fn html(w: &mut impl Write, body: &str) {
    respond(w, 200, "OK", "text/html; charset=utf-8", body.as_bytes());
}

pub fn not_found(w: &mut impl Write, msg: &str) {
    respond(w, 404, "Not Found", "text/plain; charset=utf-8", msg.as_bytes());
}
```

- [ ] **Step 5: Run tests, expect pass**

Run: `cargo test http`
Expected: 5 passed

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock .gitignore src/
git commit -m "v2: cargo scaffold + hand-rolled GET parser/response layer"
```

---

### Task 2: `config` module — TOML settings cascade

**Files:**
- Create: `src/config.rs`
- Modify: `src/lib.rs` (add `pub mod config;`)

**Interfaces:**
- Produces: `config::Settings { theme: String, default_tab: String, hide: Vec<String>, warning: Option<String> }` (Default: `"dark"`, `"terminal"`, `[]`, `None`); `config::load(paths: &[&Path]) -> Settings` (later paths override earlier, per-key); `config::for_project(project_dir: &Path) -> Settings` (global `~/.config/deadlight/config.toml` then `{project}/.deadlight/config.toml`).

- [ ] **Step 1: Create `src/config.rs` with tests only**

```rust
//! Settings cascade: global ~/.config/deadlight/config.toml, then
//! {project}/.deadlight/config.toml. Re-read on every request — never cached.
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn defaults_when_no_files() {
        let d = tempfile::tempdir().unwrap();
        let s = load(&[&d.path().join("none.toml")]);
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn project_overrides_global_per_key() {
        let d = tempfile::tempdir().unwrap();
        let g = d.path().join("global.toml");
        let p = d.path().join("project.toml");
        fs::write(&g, "theme = \"light\"\nhide = [\"dist\"]").unwrap();
        fs::write(&p, "theme = \"gruvbox\"").unwrap();
        let s = load(&[&g, &p]);
        assert_eq!(s.theme, "gruvbox"); // project wins
        assert_eq!(s.hide, vec!["dist"]); // untouched key survives from global
        assert_eq!(s.default_tab, "terminal"); // default fills the rest
        assert!(s.warning.is_none());
    }

    #[test]
    fn malformed_file_warns_and_keeps_defaults() {
        let d = tempfile::tempdir().unwrap();
        let bad = d.path().join("bad.toml");
        fs::write(&bad, "theme = [unclosed").unwrap();
        let s = load(&[&bad]);
        assert_eq!(s.theme, "dark");
        assert!(s.warning.is_some());
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("c.toml");
        fs::write(&f, "future_key = 1\ntheme = \"light\"").unwrap();
        let s = load(&[&f]);
        assert_eq!(s.theme, "light");
        assert!(s.warning.is_none());
    }
}
```

- [ ] **Step 2: Add `pub mod config;` to `src/lib.rs`, run tests, expect compile failure**

Run: `cargo test config`
Expected: FAIL — `Settings`/`load` not found

- [ ] **Step 3: Add the implementation above the test module**

```rust
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawConfig {
    theme: Option<String>,
    default_tab: Option<String>,
    hide: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub theme: String,
    pub default_tab: String,
    pub hide: Vec<String>,
    pub warning: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: "dark".into(),
            default_tab: "terminal".into(),
            hide: vec![],
            warning: None,
        }
    }
}

pub fn global_config_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".config/deadlight/config.toml")
}

pub fn load(paths: &[&Path]) -> Settings {
    let mut s = Settings::default();
    let mut warnings = Vec::new();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue; // missing file is normal, not a warning
        };
        match toml::from_str::<RawConfig>(&text) {
            Ok(raw) => {
                if let Some(v) = raw.theme {
                    s.theme = v;
                }
                if let Some(v) = raw.default_tab {
                    s.default_tab = v;
                }
                if let Some(v) = raw.hide {
                    s.hide = v;
                }
            }
            Err(e) => warnings.push(format!("{}: {}", path.display(), e.message())),
        }
    }
    if !warnings.is_empty() {
        s.warning = Some(warnings.join("; "));
    }
    s
}

pub fn for_project(project_dir: &Path) -> Settings {
    load(&[
        &global_config_path(),
        &project_dir.join(".deadlight/config.toml"),
    ])
}
```

- [ ] **Step 4: Run tests, expect pass**

Run: `cargo test config`
Expected: 4 passed

- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/lib.rs
git commit -m "v2: TOML settings cascade (global -> .deadlight, warn on malformed)"
```

---

### Task 3: `projects` module — roots, resolution, path safety, file reading policy

**Files:**
- Create: `src/projects.rs`
- Modify: `src/lib.rs` (add `pub mod projects;`)

**Interfaces:**
- Produces: `projects::SKIP_DIRS: &[&str]`; `projects::RESERVED: &[&str]` = `["static","ws","frag"]`; `projects::roots() -> Vec<PathBuf>` (the two hard-coded ROOTS); `projects::Project { name: String, path: PathBuf, git: bool }`; `projects::list_projects(roots: &[PathBuf]) -> Vec<Project>`; `projects::resolve_project(roots: &[PathBuf], name: &str) -> Option<PathBuf>`; `projects::safe_resolve(project_dir: &Path, rel: &str) -> Result<PathBuf, String>`; `projects::read_text_file(path: &Path) -> Result<String, String>` (Err is a human-readable hint).

- [ ] **Step 1: Create `src/projects.rs` with tests only**

```rust
//! Project discovery under ROOTS and all filesystem access policy:
//! path confinement, size cap, binary sniffing.
use std::path::{Path, PathBuf};

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
}
```

- [ ] **Step 2: Add `pub mod projects;` to `src/lib.rs`, run tests, expect compile failure**

Run: `cargo test projects`
Expected: FAIL — missing items

- [ ] **Step 3: Add the implementation above the test module**

```rust
pub const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "__pycache__", ".venv"];
pub const RESERVED: &[&str] = &["static", "ws", "frag"];
const MAX_FILE_BYTES: u64 = 2_000_000;
const TEXT_EXTENSIONS: &[&str] = &[
    "rs", "toml", "md", "txt", "py", "js", "ts", "json", "yaml", "yml", "sh", "html", "css",
    "sql", "qnt", "tla", "lock", "xml", "c", "h", "cpp", "go", "java", "rb", "proto", "cfg",
    "ini", "service", "env", "gitignore", "dockerignore",
];

pub fn roots() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/home/claude/ultima"),
        PathBuf::from("/home/claude/projects"),
    ]
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

pub fn resolve_project(roots: &[PathBuf], name: &str) -> Option<PathBuf> {
    if name.is_empty()
        || name.contains('/')
        || name.starts_with('.')
        || RESERVED.contains(&name)
    {
        return None;
    }
    roots.iter().map(|r| r.join(name)).find(|p| p.is_dir())
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
```

- [ ] **Step 4: Run tests, expect pass**

Run: `cargo test projects`
Expected: 5 passed

- [ ] **Step 5: Commit**

```bash
git add src/projects.rs src/lib.rs
git commit -m "v2: project discovery, path confinement, file read policy"
```

---

### Task 4: `gitio` module — status parsing and diffs via the git binary

**Files:**
- Create: `src/gitio.rs`
- Modify: `src/lib.rs` (add `pub mod gitio;`)

**Interfaces:**
- Consumes: `projects::safe_resolve`.
- Produces: `gitio::Change { xy: String, path: String }`; `gitio::Status { branch: String, changes: Vec<Change> }`; `gitio::parse_status(porcelain: &str) -> Status` (pure); `gitio::status(repo: &Path) -> Result<Status, String>`; `gitio::diff(repo: &Path, path: Option<&str>) -> Result<String, String>` (untracked files render as an all-new synthetic diff; the untracked read goes through `safe_resolve`).

- [ ] **Step 1: Create `src/gitio.rs` with tests only**

```rust
//! Git working-tree state via the git binary. Porcelain v2 parsing includes
//! rename lines ("2 ..."), which v1 flagged as its most suspect code.
use std::path::Path;
use std::process::Command;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn repo_fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = Command::new("git").arg("-C").arg(d.path()).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        fs::write(d.path().join("a.txt"), "one\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        d
    }

    #[test]
    fn parses_ordinary_and_untracked_lines() {
        let p = "# branch.head main\n\
                 1 .M N... 100644 100644 100644 abc def a.txt\n\
                 ? b.txt\n";
        let st = parse_status(p);
        assert_eq!(st.branch, "main");
        assert_eq!(st.changes.len(), 2);
        assert_eq!(st.changes[0].xy, ".M");
        assert_eq!(st.changes[0].path, "a.txt");
        assert_eq!(st.changes[1].xy, "??");
        assert_eq!(st.changes[1].path, "b.txt");
    }

    #[test]
    fn parses_rename_lines() {
        let p = "# branch.head main\n\
                 2 R. N... 100644 100644 100644 abc def R100 new.txt\told.txt\n";
        let st = parse_status(p);
        assert_eq!(st.changes.len(), 1);
        assert_eq!(st.changes[0].xy, "R.");
        assert_eq!(st.changes[0].path, "new.txt");
    }

    #[test]
    fn status_against_real_repo() {
        let d = repo_fixture();
        fs::write(d.path().join("a.txt"), "two\n").unwrap();
        fs::write(d.path().join("b.txt"), "bee\n").unwrap();
        let st = status(d.path()).unwrap();
        assert_eq!(st.branch, "main");
        assert_eq!(st.changes.len(), 2);
    }

    #[test]
    fn diff_tracked_untracked_and_escape() {
        let d = repo_fixture();
        fs::write(d.path().join("a.txt"), "two\n").unwrap();
        fs::write(d.path().join("b.txt"), "bee\n").unwrap();
        let full = diff(d.path(), None).unwrap();
        assert!(full.contains("-one"));
        assert!(full.contains("+two"));
        let untracked = diff(d.path(), Some("b.txt")).unwrap();
        assert!(untracked.contains("+++ b/b.txt"));
        assert!(untracked.contains("+bee"));
        assert!(diff(d.path(), Some("../../etc/passwd")).is_err());
    }

    #[test]
    fn status_errors_outside_a_repo() {
        let d = tempfile::tempdir().unwrap();
        assert!(status(d.path()).is_err());
    }
}
```

- [ ] **Step 2: Add `pub mod gitio;` to `src/lib.rs`, run tests, expect compile failure**

Run: `cargo test gitio`
Expected: FAIL — missing items

- [ ] **Step 3: Add the implementation above the test module**

```rust
pub struct Change {
    pub xy: String,
    pub path: String,
}

pub struct Status {
    pub branch: String,
    pub changes: Vec<Change>,
}

fn run_git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    let code = out.status.code().unwrap_or(-1);
    if code != 0 && code != 1 {
        // git diff exits 1 when differences exist
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn parse_status(porcelain: &str) -> Status {
    let mut branch = String::new();
    let mut changes = Vec::new();
    for line in porcelain.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            branch = rest.to_string();
        } else if line.starts_with("1 ") {
            // "1 XY sub mH mI mW hH hI path"
            let parts: Vec<&str> = line.splitn(9, ' ').collect();
            if parts.len() == 9 {
                changes.push(Change { xy: parts[1].into(), path: parts[8].into() });
            }
        } else if line.starts_with("2 ") {
            // "2 XY sub mH mI mW hH hI Xscore path\torigPath"
            let parts: Vec<&str> = line.splitn(10, ' ').collect();
            if parts.len() == 10 {
                let path = parts[9].split('\t').next().unwrap_or("");
                changes.push(Change { xy: parts[1].into(), path: path.into() });
            }
        } else if let Some(rest) = line.strip_prefix("? ") {
            changes.push(Change { xy: "??".into(), path: rest.to_string() });
        }
    }
    Status { branch, changes }
}

pub fn status(repo: &Path) -> Result<Status, String> {
    if !repo.join(".git").exists() {
        return Err("not a git repository".into());
    }
    run_git(repo, &["status", "--porcelain=v2", "-b"]).map(|s| parse_status(&s))
}

pub fn diff(repo: &Path, path: Option<&str>) -> Result<String, String> {
    match path {
        None => run_git(repo, &["diff", "HEAD"]),
        Some(p) => {
            let tracked = Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["ls-files", "--error-unmatch", p])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if tracked {
                run_git(repo, &["diff", "HEAD", "--", p])
            } else {
                // untracked: synthesize an all-new diff; confine the read
                let abs = crate::projects::safe_resolve(repo, p)?;
                let body = std::fs::read_to_string(&abs).unwrap_or_default();
                let lines: Vec<&str> = body.lines().collect();
                let mut d = format!("--- /dev/null\n+++ b/{p}\n@@ -0,0 +1,{} @@\n", lines.len());
                for l in &lines {
                    d.push('+');
                    d.push_str(l);
                    d.push('\n');
                }
                Ok(d)
            }
        }
    }
}
```

- [ ] **Step 4: Run tests, expect pass**

Run: `cargo test gitio`
Expected: 5 passed

- [ ] **Step 5: Commit**

```bash
git add src/gitio.rs src/lib.rs
git commit -m "v2: git status/diff via git binary, porcelain v2 incl renames"
```

---

### Task 5: `render` module — all HTML generation

**Files:**
- Create: `src/render.rs`
- Modify: `src/lib.rs` (add `pub mod render;`)

**Interfaces:**
- Consumes: `config::Settings`, `gitio::Status`, `projects::{Project, SKIP_DIRS}`, `http::percent_encode`.
- Produces: `render::esc(&str) -> String`; `render::hint(&str) -> String`; `render::diff_html(&str) -> String`; `render::markdown_html(&str) -> String`; `render::file_fragment(rel: &str, content: &str) -> String`; `render::tree_fragment(project: &str, dir: &Path, open: &str, hide: &[String]) -> String`; `render::changes_fragment(project: &str, st: &Status) -> String`; `render::status_fragment(st: &Status) -> String`; `render::index_page(&[Project]) -> String`; `render::workspace_page(project: &str, s: &Settings, has_theme_css: bool) -> String`.

- [ ] **Step 1: Create `src/render.rs` with tests only**

```rust
//! All HTML generation. Plain string building, no template engine.
//! Fragments target htmx swap sites; pages are full documents.
use crate::config::Settings;
use crate::gitio::Status;
use crate::projects::Project;
use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn esc_escapes_html() {
        assert_eq!(esc("a<b>&\"c\""), "a&lt;b&gt;&amp;&quot;c&quot;");
    }

    #[test]
    fn diff_lines_are_classified() {
        let d = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old <\n+new\n ctx";
        let h = diff_html(d);
        assert!(h.contains("dl meta"));
        assert!(h.contains("dl hunk"));
        assert!(h.contains("dl del"));
        assert!(h.contains("dl add"));
        assert!(h.contains("dl ctx"));
        assert!(h.contains("-old &lt;")); // escaped
    }

    #[test]
    fn markdown_renders_wrapped() {
        let h = markdown_html("# Hi\n\n- a\n");
        assert!(h.starts_with("<article class=\"markdown-body\">"));
        assert!(h.contains("<h1>Hi</h1>"));
        assert!(h.contains("<li>a</li>"));
    }

    #[test]
    fn file_fragment_md_vs_code() {
        let md = file_fragment("readme.md", "# T");
        assert!(md.contains("markdown-body"));
        let code = file_fragment("main.rs", "fn x() -> Vec<u8> {}");
        assert!(code.contains("language-rs"));
        assert!(code.contains("Vec&lt;u8&gt;")); // escaped, hljs runs client-side
    }

    #[test]
    fn tree_marks_open_path_and_skips_hidden() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("src/sub")).unwrap();
        fs::create_dir(d.path().join("target")).unwrap();
        fs::create_dir(d.path().join("dist")).unwrap();
        fs::write(d.path().join("src/main.rs"), "").unwrap();
        fs::write(d.path().join("src/sub/x.rs"), "").unwrap();
        fs::write(d.path().join("README.md"), "").unwrap();
        let h = tree_fragment("proj", d.path(), "src/main.rs", &["dist".to_string()]);
        assert!(h.contains("<details open><summary>src</summary>"));
        assert!(h.contains("<details><summary>sub</summary>")); // not on open path
        assert!(h.contains("class=\"file sel\""));
        assert!(h.contains("hx-get=\"/frag/proj/file?path=src/main.rs\""));
        assert!(h.contains("README.md"));
        assert!(!h.contains("target"));
        assert!(!h.contains("dist"));
    }

    #[test]
    fn changes_and_status_fragments() {
        let st = Status {
            branch: "main".into(),
            changes: vec![crate::gitio::Change { xy: ".M".into(), path: "a.txt".into() }],
        };
        let c = changes_fragment("proj", &st);
        assert!(c.contains("full diff"));
        assert!(c.contains("class=\"xy\""));
        assert!(c.contains("hx-get=\"/frag/proj/diff?path=a.txt\""));
        let s = status_fragment(&st);
        assert!(s.contains("main"));
        assert!(s.contains("(1)"));
        let clean = changes_fragment("proj", &Status { branch: "main".into(), changes: vec![] });
        assert!(clean.contains("working tree clean"));
    }

    #[test]
    fn workspace_page_wires_everything() {
        let s = Settings { theme: "gruvbox".into(), ..Settings::default() };
        let h = workspace_page("proj", &s, true);
        assert!(h.contains("/static/themes/gruvbox.css"));
        assert!(h.contains("/frag/proj/theme.css")); // has_theme_css
        assert!(h.contains("data-project=\"proj\""));
        assert!(h.contains("data-default-tab=\"terminal\""));
        assert!(h.contains("htmx.min.js"));
        assert!(h.contains("id=\"term\""));
        let no_custom = workspace_page("proj", &s, false);
        assert!(!no_custom.contains("theme.css\">"));
    }

    #[test]
    fn index_page_lists_projects() {
        let ps = vec![Project { name: "alpha".into(), path: "/tmp/alpha".into(), git: true }];
        let h = index_page(&ps);
        assert!(h.contains("href=\"/alpha\""));
        assert!(h.contains("/tmp/alpha"));
    }
}
```

- [ ] **Step 2: Add `pub mod render;` to `src/lib.rs`, run tests, expect compile failure**

Run: `cargo test render`
Expected: FAIL — missing items

- [ ] **Step 3: Add the implementation above the test module**

```rust
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn hint(msg: &str) -> String {
    format!("<div class=\"hint\">{}</div>", esc(msg))
}

pub fn diff_html(diff: &str) -> String {
    diff.lines()
        .map(|l| {
            let cls = if l.starts_with("+++") || l.starts_with("---") || l.starts_with("diff ") {
                "meta"
            } else if l.starts_with("@@") {
                "hunk"
            } else if l.starts_with('+') {
                "add"
            } else if l.starts_with('-') {
                "del"
            } else {
                "ctx"
            };
            let body = if l.is_empty() { " ".to_string() } else { esc(l) };
            format!("<div class=\"dl {cls}\">{body}</div>")
        })
        .collect()
}

pub fn markdown_html(md: &str) -> String {
    use pulldown_cmark::{html, Options, Parser};
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut out = String::new();
    html::push_html(&mut out, Parser::new_ext(md, opts));
    format!("<article class=\"markdown-body\">{out}</article>")
}

pub fn file_fragment(rel: &str, content: &str) -> String {
    let ext = rel.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if ext == "md" || ext == "markdown" {
        format!("<div class=\"path\">{}</div>{}", esc(rel), markdown_html(content))
    } else {
        format!(
            "<div class=\"path\">{}</div><pre class=\"codeview\"><code class=\"language-{}\">{}</code></pre>",
            esc(rel),
            esc(&ext),
            esc(content)
        )
    }
}

pub fn tree_fragment(project: &str, dir: &Path, open: &str, hide: &[String]) -> String {
    let mut budget = 4000usize;
    let mut out = String::from("<ul class=\"tree\">");
    tree_level(project, dir, "", open, hide, &mut budget, &mut out);
    out.push_str("</ul>");
    if budget == 0 {
        out.push_str("<div class=\"hint\">tree truncated (too many entries)</div>");
    }
    out
}

fn tree_level(
    project: &str,
    dir: &Path,
    rel: &str,
    open: &str,
    hide: &[String],
    budget: &mut usize,
    out: &mut String,
) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| (e.path().is_file(), e.file_name().to_ascii_lowercase()));
    for e in entries {
        if *budget == 0 {
            return;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if crate::projects::SKIP_DIRS.contains(&name.as_str()) || hide.iter().any(|h| h == &name)
        {
            continue;
        }
        *budget -= 1;
        let erel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if e.path().is_dir() {
            let is_open = open == erel || open.starts_with(&format!("{erel}/"));
            out.push_str(&format!(
                "<li><details{}><summary>{}</summary><ul>",
                if is_open { " open" } else { "" },
                esc(&name)
            ));
            tree_level(project, &e.path(), &erel, open, hide, budget, out);
            out.push_str("</ul></details></li>");
        } else {
            let sel = if open == erel { " sel" } else { "" };
            out.push_str(&format!(
                "<li><a class=\"file{sel}\" data-rel=\"{}\" hx-get=\"/frag/{}/file?path={}\" hx-target=\"#content\">{}</a></li>",
                esc(&erel),
                project,
                crate::http::percent_encode(&erel),
                esc(&name)
            ));
        }
    }
}

pub fn changes_fragment(project: &str, st: &Status) -> String {
    if st.changes.is_empty() {
        return hint("working tree clean");
    }
    let mut out = format!(
        "<ul class=\"changes\"><li><a class=\"file\" data-rel=\"\" hx-get=\"/frag/{project}/diff\" hx-target=\"#content\"><b>— full diff —</b></a></li>"
    );
    for c in &st.changes {
        out.push_str(&format!(
            "<li><a class=\"file\" data-rel=\"{}\" hx-get=\"/frag/{}/diff?path={}\" hx-target=\"#content\"><span class=\"xy\">{}</span> {}</a></li>",
            esc(&c.path),
            project,
            crate::http::percent_encode(&c.path),
            esc(&c.xy),
            esc(&c.path)
        ));
    }
    out.push_str("</ul>");
    out
}

pub fn status_fragment(st: &Status) -> String {
    format!(
        "<span id=\"branch\">{}</span><span id=\"badge\">{}</span>",
        if st.branch.is_empty() { String::new() } else { format!("⎇ {}", esc(&st.branch)) },
        if st.changes.is_empty() { String::new() } else { format!("({})", st.changes.len()) }
    )
}

pub fn index_page(projects: &[Project]) -> String {
    let rows: String = projects
        .iter()
        .map(|p| {
            format!(
                "<li><a href=\"/{0}\">{0}</a><span class=\"path\">{1}{2}</span></li>",
                esc(&p.name),
                esc(&p.path.to_string_lossy()),
                if p.git { " ⎇" } else { "" }
            )
        })
        .collect();
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>deadlight</title>\
         <link rel=\"stylesheet\" href=\"/static/themes/dark.css\">\
         <link rel=\"stylesheet\" href=\"/static/style.css\">\
         </head><body><header><span class=\"proj\">deadlight</span></header>\
         <main><ul class=\"projects\">{rows}</ul></main></body></html>"
    )
}

pub fn workspace_page(project: &str, s: &Settings, has_theme_css: bool) -> String {
    let warn = s
        .warning
        .as_deref()
        .map(|w| format!("<span class=\"warn\" title=\"{}\">⚠ config</span>", esc(w)))
        .unwrap_or_default();
    let theme_css = if has_theme_css {
        format!("<link rel=\"stylesheet\" href=\"/frag/{project}/theme.css\">")
    } else {
        String::new()
    };
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{project} — deadlight</title>
<link rel="stylesheet" href="/static/vendor/xterm.css">
<link rel="stylesheet" href="/static/vendor/hljs-github-dark.min.css">
<link rel="stylesheet" href="/static/vendor/github-markdown.min.css">
<link rel="stylesheet" href="/static/themes/{theme}.css">
<link rel="stylesheet" href="/static/style.css">
{theme_css}
<script src="/static/vendor/htmx.min.js"></script>
<script src="/static/vendor/xterm.js"></script>
<script src="/static/vendor/xterm-addon-fit.js"></script>
<script src="/static/vendor/highlight.min.js"></script>
</head><body data-project="{project}" data-default-tab="{tab}">
<header>
  <a class="home" href="/">◆</a><span class="proj">{project}</span>
  <nav><button id="tab-terminal">Terminal</button><button id="tab-files">Files</button><button id="tab-changes">Changes</button></nav>
  <span id="gitinfo" hx-get="/frag/{project}/status" hx-trigger="load, refresh from:body"></span>
  {warn}
  <button id="refresh" title="refresh (r)">⟳</button>
</header>
<main>
  <section id="term-pane"><div id="term"></div><div id="term-overlay" class="hidden">disconnected — reconnecting…</div></section>
  <section id="viewer" class="hidden"><nav id="sidebar"></nav><div id="content"></div></section>
</main>
<script src="/static/app.js"></script>
</body></html>"#,
        theme = esc(&s.theme),
        tab = esc(&s.default_tab)
    )
}
```

- [ ] **Step 4: Run tests, expect pass**

Run: `cargo test render`
Expected: 8 passed

- [ ] **Step 5: Commit**

```bash
git add src/render.rs src/lib.rs
git commit -m "v2: server-side HTML rendering (pages, tree, file, changes, diff)"
```

---

### Task 6: `routes` module + `serve()` dispatcher + real `main`

**Files:**
- Create: `src/routes.rs`, `tests/integration.rs`
- Modify: `src/lib.rs` (add `pub mod routes;` and `serve`), `src/main.rs` (real main)

**Interfaces:**
- Consumes: everything from Tasks 1–5.
- Produces: `routes::handle(stream: TcpStream, roots: &[PathBuf])`; `routes::STATIC_DIR: &str` (compile-time `{repo}/static`); `deadlight::serve(listener: TcpListener, roots: Vec<PathBuf>)` — accept loop, peeks for `GET /ws/` (Task 7 wires `term::handle_ws`; until then ws connections are dropped).
- Fragment endpoints return HTTP 200 with a `hint` div on errors (htmx does not swap 4xx responses); unknown pages/projects return 404.

- [ ] **Step 1: Write `tests/integration.rs` (fails to compile until routes exist)**

```rust
use std::net::TcpListener;
use std::path::PathBuf;

fn start(roots: Vec<PathBuf>) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || deadlight::serve(listener, roots));
    port
}

fn fixture() -> (tempfile::TempDir, u16) {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("proj")).unwrap();
    std::fs::write(d.path().join("proj/hello.md"), "# Hello\n").unwrap();
    std::fs::create_dir(d.path().join("proj/.deadlight")).unwrap();
    std::fs::write(d.path().join("proj/.deadlight/config.toml"), "theme = \"light\"\n").unwrap();
    let port = start(vec![d.path().to_path_buf()]);
    (d, port)
}

#[test]
fn index_lists_projects() {
    let (_d, port) = fixture();
    let body = ureq::get(&format!("http://127.0.0.1:{port}/"))
        .call().unwrap().into_string().unwrap();
    assert!(body.contains("proj"));
}

#[test]
fn workspace_page_applies_project_settings() {
    let (_d, port) = fixture();
    let body = ureq::get(&format!("http://127.0.0.1:{port}/proj"))
        .call().unwrap().into_string().unwrap();
    assert!(body.contains("/static/themes/light.css")); // .deadlight config read per request
    assert!(body.contains("data-project=\"proj\""));
}

#[test]
fn fragments_render_and_errors_become_hints() {
    let (_d, port) = fixture();
    let base = format!("http://127.0.0.1:{port}");
    let tree = ureq::get(&format!("{base}/frag/proj/tree")).call().unwrap().into_string().unwrap();
    assert!(tree.contains("hello.md"));
    let file = ureq::get(&format!("{base}/frag/proj/file?path=hello.md"))
        .call().unwrap().into_string().unwrap();
    assert!(file.contains("<h1>Hello</h1>"));
    // escape attempt: 200 + hint, and definitely no file content
    let esc = ureq::get(&format!("{base}/frag/proj/file?path=../../../etc/passwd"))
        .call().unwrap().into_string().unwrap();
    assert!(esc.contains("class=\"hint\""));
    assert!(!esc.contains("root:"));
}

#[test]
fn unknown_pages_are_404() {
    let (_d, port) = fixture();
    assert!(ureq::get(&format!("http://127.0.0.1:{port}/no-such-project")).call().is_err());
    assert!(ureq::get(&format!("http://127.0.0.1:{port}/frag/proj/nope")).call().is_err());
}

#[test]
fn static_assets_served_with_type() {
    let (_d, port) = fixture();
    let resp = ureq::get(&format!("http://127.0.0.1:{port}/static/vendor/highlight.min.js"))
        .call().unwrap();
    assert!(resp.content_type().starts_with("text/javascript"));
}
```

- [ ] **Step 2: Run, expect compile failure**

Run: `cargo test --test integration`
Expected: FAIL — `deadlight::serve` not found

- [ ] **Step 3: Create `src/routes.rs`**

```rust
//! HTTP request routing. URL surface (spec §URLs):
//!   /                    index page
//!   /{project}           workspace page
//!   /static/*            assets
//!   /frag/{project}/*    htmx fragments
//! Fragment errors render as 200 + hint (htmx ignores 4xx bodies).
use crate::{config, gitio, http, projects, render};
use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

pub fn handle(stream: TcpStream, roots: &[PathBuf]) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(read_half) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read_half);
    let mut w = stream;
    match http::parse(&mut reader) {
        Ok(req) => route(&mut w, &req, roots),
        Err(e) => http::respond(&mut w, 400, "Bad Request", "text/plain", e.as_bytes()),
    }
}

fn route(w: &mut impl Write, req: &http::Request, roots: &[PathBuf]) {
    let segs: Vec<&str> = req.path.split('/').filter(|s| !s.is_empty()).collect();
    match segs.as_slice() {
        [] => http::html(w, &render::index_page(&projects::list_projects(roots))),
        ["static", rest @ ..] => serve_static(w, &rest.join("/")),
        ["frag", project, what @ ..] => serve_frag(w, req, roots, project, what),
        [project] => serve_workspace(w, roots, project),
        _ => http::not_found(w, "no such page"),
    }
}

fn serve_workspace(w: &mut impl Write, roots: &[PathBuf], project: &str) {
    let Some(dir) = projects::resolve_project(roots, project) else {
        return http::not_found(w, "no such project");
    };
    let settings = config::for_project(&dir);
    let has_theme_css = dir.join(".deadlight/theme.css").is_file();
    http::html(w, &render::workspace_page(project, &settings, has_theme_css));
}

fn serve_static(w: &mut impl Write, rel: &str) {
    let base = Path::new(STATIC_DIR);
    let (Ok(f), Ok(basec)) = (base.join(rel).canonicalize(), base.canonicalize()) else {
        return http::not_found(w, "no such asset");
    };
    if !f.starts_with(&basec) || !f.is_file() {
        return http::not_found(w, "no such asset");
    }
    let ctype = match f.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    };
    match std::fs::read(&f) {
        Ok(body) => http::respond(w, 200, "OK", ctype, &body),
        Err(_) => http::not_found(w, "unreadable"),
    }
}

fn serve_frag(
    w: &mut impl Write,
    req: &http::Request,
    roots: &[PathBuf],
    project: &str,
    what: &[&str],
) {
    let Some(dir) = projects::resolve_project(roots, project) else {
        return http::not_found(w, "no such project");
    };
    let settings = config::for_project(&dir);
    match what {
        ["tree"] => {
            let open = req.query.get("open").map(String::as_str).unwrap_or("");
            http::html(w, &render::tree_fragment(project, &dir, open, &settings.hide));
        }
        ["file"] => match req.query.get("path") {
            None => http::html(w, &render::hint("missing path")),
            Some(rel) => match projects::safe_resolve(&dir, rel)
                .and_then(|p| projects::read_text_file(&p))
            {
                Ok(content) => http::html(w, &render::file_fragment(rel, &content)),
                Err(e) => http::html(w, &render::hint(&e)),
            },
        },
        ["changes"] => match gitio::status(&dir) {
            Ok(st) => http::html(w, &render::changes_fragment(project, &st)),
            Err(e) => http::html(w, &render::hint(&e)),
        },
        ["status"] => {
            let st = gitio::status(&dir)
                .unwrap_or(gitio::Status { branch: String::new(), changes: vec![] });
            http::html(w, &render::status_fragment(&st));
        }
        ["diff"] => {
            let path = req.query.get("path").map(String::as_str);
            match gitio::diff(&dir, path) {
                Ok(d) if d.trim().is_empty() => http::html(w, &render::hint("no diff")),
                Ok(d) => http::html(
                    w,
                    &format!(
                        "<div class=\"path\">{}</div><div class=\"diffview\">{}</div>",
                        render::esc(path.unwrap_or("all changes")),
                        render::diff_html(&d)
                    ),
                ),
                Err(e) => http::html(w, &render::hint(&e)),
            }
        }
        ["theme.css"] => match std::fs::read(dir.join(".deadlight/theme.css")) {
            Ok(css) => http::respond(w, 200, "OK", "text/css; charset=utf-8", &css),
            Err(_) => http::not_found(w, "no theme.css"),
        },
        _ => http::not_found(w, "no such fragment"),
    }
}
```

- [ ] **Step 4: Rewrite `src/lib.rs` with `serve()`**

```rust
pub mod config;
pub mod gitio;
pub mod http;
pub mod projects;
pub mod render;
pub mod routes;

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

pub fn serve(listener: TcpListener, roots: Vec<PathBuf>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let roots = roots.clone();
        std::thread::spawn(move || {
            if is_ws(&stream) {
                // Task 7 replaces this drop with term::handle_ws(stream, &roots)
                drop(stream);
            } else {
                routes::handle(stream, &roots);
            }
        });
    }
}

/// Peek the first bytes without consuming them: websocket requests go to
/// tungstenite with the request intact; everything else to the HTTP parser.
fn is_ws(stream: &TcpStream) -> bool {
    let mut buf = [0u8; 8];
    for _ in 0..50 {
        match stream.peek(&mut buf) {
            Ok(n) if n >= 8 => return &buf[..8] == b"GET /ws/",
            Ok(_) => std::thread::sleep(Duration::from_millis(2)),
            Err(_) => return false,
        }
    }
    false
}
```

- [ ] **Step 5: Rewrite `src/main.rs`**

```rust
fn main() {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(8444);
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind 127.0.0.1");
    eprintln!("deadlight listening on http://127.0.0.1:{port}");
    deadlight::serve(listener, deadlight::projects::roots());
}
```

- [ ] **Step 6: Run all tests, expect pass**

Run: `cargo test`
Expected: all unit tests + 5 integration tests pass

- [ ] **Step 7: Commit**

```bash
git add src/ tests/
git commit -m "v2: routing, static serving, accept loop with ws peek-dispatch"
```

---

### Task 7: `term` module — websocket terminal wrapping zellij

**Files:**
- Create: `src/term.rs`
- Modify: `src/lib.rs` (add `pub mod term;`, replace the `drop(stream)` placeholder with `term::handle_ws(stream, &roots)`)
- Test: append to `tests/integration.rs`

**Interfaces:**
- Consumes: `projects::resolve_project`.
- Produces: `term::handle_ws(stream: TcpStream, roots: &[PathBuf])`. Protocol: browser→server Binary frames = raw terminal input; Text frames `resize:{cols}x{rows}` = PTY resize; server→browser Binary frames = raw PTY output. Command: `zellij attach --create {project}`, overridable via env `DEADLIGHT_CMD` (whitespace-split; used by tests to run `cat`).

- [ ] **Step 1: Append the ws test to `tests/integration.rs`**

```rust
#[test]
fn terminal_ws_echoes_through_pty() {
    std::env::set_var("DEADLIGHT_CMD", "cat");
    let (_d, port) = fixture();
    let (mut ws, _resp) =
        tungstenite::connect(format!("ws://127.0.0.1:{port}/ws/proj")).unwrap();
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    }
    ws.send(tungstenite::Message::Text("resize:100x30".into())).unwrap();
    ws.send(tungstenite::Message::Binary(b"hello\r".to_vec())).unwrap();
    let mut seen = String::new();
    for _ in 0..100 {
        match ws.read() {
            Ok(tungstenite::Message::Binary(b)) => seen.push_str(&String::from_utf8_lossy(&b)),
            Ok(_) => {}
            Err(_) => break,
        }
        if seen.contains("hello") {
            break;
        }
    }
    assert!(seen.contains("hello"), "PTY echo not received; got: {seen:?}");
    let _ = ws.close(None);
}
```

Note: `tungstenite` is a main dependency, so tests can use it directly. `DEADLIGHT_CMD` is process-global; only this test sets it, and `cat` ignores the project argument entirely.

- [ ] **Step 2: Run, expect failure**

Run: `cargo test --test integration terminal_ws`
Expected: FAIL — connect refused/handshake error (serve drops ws streams)

- [ ] **Step 3: Create `src/term.rs`**

```rust
//! Terminal websocket: bridges a browser tab to a PTY running
//! `zellij attach --create {project}`. One connection = one zellij client;
//! zellij owns all session state. Two pump directions over one TcpStream:
//! tungstenite over try_clone'd halves (frames are independent per direction).
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use tungstenite::handshake::server::{Request as WsRequest, Response as WsResponse};
use tungstenite::protocol::Role;
use tungstenite::{accept_hdr, Message, WebSocket};

pub fn handle_ws(stream: TcpStream, roots: &[PathBuf]) {
    let mut path = String::new();
    let accepted = accept_hdr(stream, |req: &WsRequest, resp: WsResponse| {
        path = req.uri().path().to_string();
        Ok(resp)
    });
    let Ok(mut ws_read) = accepted else { return };

    let project = match path.strip_prefix("/ws/") {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => {
            let _ = ws_read.close(None);
            return;
        }
    };
    let Some(dir) = crate::projects::resolve_project(roots, &project) else {
        let _ = ws_read.close(None);
        return;
    };

    let cmd: Vec<String> = match std::env::var("DEADLIGHT_CMD") {
        Ok(c) => c.split_whitespace().map(String::from).collect(),
        Err(_) => vec!["zellij".into(), "attach".into(), "--create".into(), project.clone()],
    };

    let pty = native_pty_system();
    let Ok(pair) = pty.openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
    else {
        return;
    };
    let mut cb = CommandBuilder::new(&cmd[0]);
    cb.args(&cmd[1..]);
    cb.cwd(&dir);
    cb.env("TERM", "xterm-256color");
    let Ok(mut child) = pair.slave.spawn_command(cb) else { return };
    drop(pair.slave);
    let Ok(mut pty_reader) = pair.master.try_clone_reader() else { return };
    let Ok(mut pty_writer) = pair.master.take_writer() else { return };
    let master = pair.master;

    let Ok(write_half) = ws_read.get_ref().try_clone() else { return };
    let mut ws_write: WebSocket<TcpStream> =
        WebSocket::from_raw_socket(write_half, Role::Server, None);

    // PTY -> browser
    let out_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if ws_write.send(Message::Binary(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = ws_write.close(None);
    });

    // browser -> PTY (this thread)
    loop {
        match ws_read.read() {
            Ok(Message::Binary(b)) => {
                if pty_writer.write_all(&b).is_err() {
                    break;
                }
            }
            Ok(Message::Text(t)) => {
                if let Some(sz) = t.strip_prefix("resize:") {
                    if let Some((c, r)) = sz.split_once('x') {
                        if let (Ok(cols), Ok(rows)) = (c.parse(), r.parse()) {
                            let _ = master.resize(PtySize {
                                rows,
                                cols,
                                pixel_width: 0,
                                pixel_height: 0,
                            });
                        }
                    }
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    // browser gone: kill our zellij client (detach); the session survives
    let _ = child.kill();
    let _ = child.wait();
    let _ = out_thread.join();
}
```

- [ ] **Step 4: Wire it into `src/lib.rs`**

Add `pub mod term;` and replace the placeholder branch in `serve`:

```rust
            if is_ws(&stream) {
                term::handle_ws(stream, &roots);
            } else {
                routes::handle(stream, &roots);
            }
```

- [ ] **Step 5: Run all tests, expect pass**

Run: `cargo test`
Expected: all pass, including `terminal_ws_echoes_through_pty`

- [ ] **Step 6: Commit**

```bash
git add src/term.rs src/lib.rs tests/integration.rs
git commit -m "v2: websocket terminal bridging PTY to zellij attach"
```

---

### Task 8: Frontend — vendored assets, app.js, style.css, themes

**Files:**
- Create: `static/vendor/htmx.min.js`, `static/vendor/xterm.js`, `static/vendor/xterm.css`, `static/vendor/xterm-addon-fit.js` (downloads), `static/style.css`, `static/themes/{dark,light,gruvbox,solarized-dark}.css`
- Modify: `static/app.js` (full overwrite of the v1 file)

**Interfaces:**
- Consumes: the DOM produced by `render::workspace_page` (ids `term-pane`, `term`, `term-overlay`, `viewer`, `sidebar`, `content`, `gitinfo`, `refresh`, `tab-*`; `body[data-project]`, `body[data-default-tab]`; sidebar links carry `data-rel`), the fragment endpoints from Task 6, the ws protocol from Task 7 (`resize:{cols}x{rows}` text frames, binary I/O).
- Produces: hash mirror format `#<terminal|files|changes>[/<relpath>]` via `replaceState` only.

- [ ] **Step 1: Vendor the new libraries (pinned versions)**

```bash
cd /home/claude/projects/deadlight/static/vendor
curl -fsSLo htmx.min.js https://unpkg.com/htmx.org@2.0.4/dist/htmx.min.js
curl -fsSLo xterm.js https://cdn.jsdelivr.net/npm/@xterm/xterm@5.5.0/lib/xterm.js
curl -fsSLo xterm.css https://cdn.jsdelivr.net/npm/@xterm/xterm@5.5.0/css/xterm.css
curl -fsSLo xterm-addon-fit.js https://cdn.jsdelivr.net/npm/@xterm/addon-fit@0.10.0/lib/addon-fit.js
grep -l "Terminal" xterm.js && grep -l "FitAddon" xterm-addon-fit.js && echo OK
```

Expected: `OK`. These UMD builds expose globals `Terminal` and `FitAddon` (class at `FitAddon.FitAddon`). If a download 404s, use the same path on `unpkg.com` instead of jsdelivr. If the globals differ (grep shows an export name like `exports.Terminal` only), check the file head for the UMD wrapper's global assignment and adjust `app.js` accordingly — verify in the browser in Task 10.

- [ ] **Step 2: Write `static/style.css`**

```css
* { box-sizing: border-box; margin: 0; }
html, body { height: 100%; }
body { background: var(--bg); color: var(--fg); font: 14px/1.5 system-ui, sans-serif; display: flex; flex-direction: column; }
.hidden { display: none !important; }
header { display: flex; align-items: center; gap: 12px; padding: 6px 12px; background: var(--bg2); border-bottom: 1px solid var(--border); flex: none; }
header .home { color: var(--accent); text-decoration: none; font-size: 16px; }
header .proj { font-weight: 600; }
header nav { display: flex; gap: 4px; }
header button { background: none; border: 1px solid transparent; border-radius: 6px; color: var(--muted); padding: 3px 10px; cursor: pointer; font: inherit; }
header button.active { color: var(--fg); background: var(--bg); border-color: var(--border); }
header button:hover { color: var(--fg); }
#gitinfo { color: var(--muted); }
#gitinfo #badge { color: var(--accent); margin-left: 4px; }
.warn { color: #d29922; cursor: help; }
#refresh { margin-left: auto; }
main { flex: 1; min-height: 0; display: flex; }
#term-pane { position: relative; flex: 1; background: var(--bg); }
#term { position: absolute; inset: 4px; }
#term-overlay { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; color: var(--muted); background: var(--bg2); opacity: 0.9; z-index: 5; }
#viewer { flex: 1; display: grid; grid-template-columns: 280px 1fr; min-height: 0; }
#sidebar { border-right: 1px solid var(--border); overflow: auto; padding: 8px; background: var(--bg2); }
#content { overflow: auto; }
#sidebar ul { list-style: none; padding-left: 14px; }
#sidebar > ul { padding-left: 0; }
#sidebar a { color: var(--fg); text-decoration: none; display: block; padding: 1px 4px; border-radius: 4px; white-space: nowrap; cursor: pointer; }
#sidebar a:hover { background: var(--sel-bg); }
#sidebar a.sel { background: var(--sel-bg); color: var(--accent); }
#sidebar summary { cursor: pointer; color: var(--muted); padding: 1px 4px; white-space: nowrap; }
#sidebar summary:hover { color: var(--fg); }
.xy { display: inline-block; min-width: 2.2em; color: var(--hunk); font-family: ui-monospace, monospace; font-size: 12px; }
.hint { color: var(--muted); padding: 16px; }
.path { position: sticky; top: 0; background: var(--bg2); border-bottom: 1px solid var(--border); padding: 6px 12px; font-family: ui-monospace, monospace; font-size: 12px; color: var(--muted); }
.codeview { padding: 12px; font: 13px/1.5 ui-monospace, monospace; overflow-x: auto; }
.codeview code { background: none; }
.diffview { font: 12.5px/1.45 ui-monospace, monospace; padding: 8px 0; }
.dl { padding: 0 12px; white-space: pre-wrap; }
.dl.add { background: var(--add-bg); color: var(--add-fg); }
.dl.del { background: var(--del-bg); color: var(--del-fg); }
.dl.hunk { color: var(--hunk); padding-top: 6px; }
.dl.meta { color: var(--muted); }
.markdown-body { max-width: 900px; margin: 0 auto; padding: 24px; }
ul.projects { list-style: none; padding: 24px; max-width: 700px; margin: 0 auto; width: 100%; }
ul.projects li { display: flex; justify-content: space-between; gap: 12px; padding: 6px 8px; border-bottom: 1px solid var(--border); }
ul.projects a { color: var(--accent); text-decoration: none; font-weight: 600; }
ul.projects .path { position: static; background: none; border: none; padding: 0; }
```

- [ ] **Step 3: Write the four theme files**

`static/themes/dark.css`:

```css
:root {
  --bg: #0d1117; --bg2: #161b22; --fg: #c9d1d9; --muted: #8b949e;
  --accent: #58a6ff; --border: #30363d; --sel-bg: #1f6feb33;
  --add-bg: #12261e; --add-fg: #3fb950; --del-bg: #2d1214; --del-fg: #f85149;
  --hunk: #a371f7;
}
```

`static/themes/light.css`:

```css
:root {
  --bg: #ffffff; --bg2: #f6f8fa; --fg: #1f2328; --muted: #656d76;
  --accent: #0969da; --border: #d0d7de; --sel-bg: #0969da22;
  --add-bg: #dafbe1; --add-fg: #116329; --del-bg: #ffebe9; --del-fg: #cf222e;
  --hunk: #8250df;
}
```

`static/themes/gruvbox.css`:

```css
:root {
  --bg: #282828; --bg2: #3c3836; --fg: #ebdbb2; --muted: #a89984;
  --accent: #83a598; --border: #504945; --sel-bg: #83a59833;
  --add-bg: #2a3222; --add-fg: #b8bb26; --del-bg: #3c2526; --del-fg: #fb4934;
  --hunk: #d3869b;
}
```

`static/themes/solarized-dark.css`:

```css
:root {
  --bg: #002b36; --bg2: #073642; --fg: #93a1a1; --muted: #586e75;
  --accent: #268bd2; --border: #0e4a59; --sel-bg: #268bd233;
  --add-bg: #073a2e; --add-fg: #859900; --del-bg: #3a1a20; --del-fg: #dc322f;
  --hunk: #6c71c4;
}
```

- [ ] **Step 4: Overwrite `static/app.js`**

```javascript
/* deadlight glue: terminal wiring, tabs, hash mirror, refresh.
   No pushState anywhere — the Back button must never traverse app flow. */
const PROJECT = document.body.dataset.project;
htmx.config.historyCacheSize = 0;

/* ---- in-page view state (mirrored to the hash, never to history) ---- */
let mode = document.body.dataset.defaultTab || "terminal";
let openRel = null;
const m = location.hash.match(/^#(terminal|files|changes)(?:\/(.*))?$/);
if (m) { mode = m[1]; openRel = m[2] ? decodeURIComponent(m[2]) : null; }

function mirror() {
  // encodeURI keeps "/" literal so the hash stays readable: #files/src/main.rs
  history.replaceState(null, "", "#" + mode + (openRel ? "/" + encodeURI(openRel) : ""));
}

/* ---- tabs ---- */
const termPane = document.getElementById("term-pane");
const viewer = document.getElementById("viewer");
function setMode(next, rel) {
  mode = next;
  openRel = rel ?? null;
  termPane.classList.toggle("hidden", mode !== "terminal");
  viewer.classList.toggle("hidden", mode === "terminal");
  for (const t of ["terminal", "files", "changes"])
    document.getElementById("tab-" + t).classList.toggle("active", mode === t);
  if (mode === "files") {
    htmx.ajax("GET", "/frag/" + PROJECT + "/tree" + (openRel ? "?open=" + encodeURIComponent(openRel) : ""), "#sidebar");
    if (openRel) htmx.ajax("GET", "/frag/" + PROJECT + "/file?path=" + encodeURIComponent(openRel), "#content");
    else document.getElementById("content").innerHTML = "";
  } else if (mode === "changes") {
    htmx.ajax("GET", "/frag/" + PROJECT + "/changes", "#sidebar");
    if (openRel) htmx.ajax("GET", "/frag/" + PROJECT + "/diff?path=" + encodeURIComponent(openRel), "#content");
    else document.getElementById("content").innerHTML = "";
  } else {
    fit();
    term.focus();
  }
  mirror();
}
document.getElementById("tab-terminal").onclick = () => setMode("terminal");
document.getElementById("tab-files").onclick = () => setMode("files", openRel);
document.getElementById("tab-changes").onclick = () => setMode("changes", openRel);

/* track the open file/diff for the hash + selection highlight */
document.body.addEventListener("click", (e) => {
  const a = e.target.closest("a[data-rel]");
  if (!a) return;
  openRel = a.dataset.rel || null;
  document.querySelectorAll("#sidebar a.sel").forEach((x) => x.classList.remove("sel"));
  if (a.dataset.rel) a.classList.add("sel");
  mirror();
});

/* highlight code after htmx swaps */
document.body.addEventListener("htmx:afterSwap", (e) => {
  e.target.querySelectorAll("pre.codeview code").forEach((b) => hljs.highlightElement(b));
});

/* refresh re-fetches the current panes; the terminal is never touched */
function refresh() {
  document.body.dispatchEvent(new Event("refresh")); // #gitinfo hx-trigger listens
  if (mode !== "terminal") setMode(mode, openRel);
}
document.getElementById("refresh").onclick = refresh;
document.addEventListener("keydown", (e) => {
  if (e.key === "r" && !e.metaKey && !e.ctrlKey && mode !== "terminal") refresh();
});

/* ---- terminal ---- */
const css = getComputedStyle(document.documentElement);
const term = new Terminal({
  fontSize: 14,
  theme: {
    background: css.getPropertyValue("--bg").trim(),
    foreground: css.getPropertyValue("--fg").trim(),
  },
});
const fitAddon = new FitAddon.FitAddon();
term.loadAddon(fitAddon);
term.open(document.getElementById("term"));
let ws = null;
let retry = 250;
function fit() {
  try { fitAddon.fit(); } catch {}
  if (ws && ws.readyState === 1) ws.send("resize:" + term.cols + "x" + term.rows);
}
function connect() {
  ws = new WebSocket(
    (location.protocol === "https:" ? "wss://" : "ws://") + location.host + "/ws/" + PROJECT
  );
  ws.binaryType = "arraybuffer";
  ws.onopen = () => {
    retry = 250;
    document.getElementById("term-overlay").classList.add("hidden");
    fit();
    if (mode === "terminal") term.focus();
  };
  ws.onmessage = (e) => term.write(new Uint8Array(e.data));
  ws.onclose = () => {
    document.getElementById("term-overlay").classList.remove("hidden");
    setTimeout(connect, retry);
    retry = Math.min(retry * 2, 5000);
  };
}
term.onData((d) => {
  if (ws && ws.readyState === 1) ws.send(new TextEncoder().encode(d));
});
new ResizeObserver(() => { if (mode === "terminal") fit(); }).observe(termPane);
window.addEventListener("focus", () => {
  if (ws && ws.readyState !== 1) { try { ws.close(); } catch {} }
});

setMode(mode, openRel);
connect();
```

- [ ] **Step 5: Smoke-test over HTTP**

```bash
cd /home/claude/projects/deadlight
cargo run -- 8446 & SRV=$!
sleep 2
curl -sf http://127.0.0.1:8446/static/style.css | head -1
curl -sf http://127.0.0.1:8446/static/themes/dark.css | head -1
curl -sf http://127.0.0.1:8446/static/vendor/htmx.min.js >/dev/null && echo htmx-ok
curl -sf http://127.0.0.1:8446/deadlight | grep -o 'data-project="deadlight"'
kill $SRV
```

Expected: CSS first lines, `htmx-ok`, `data-project="deadlight"`.

- [ ] **Step 6: Commit**

```bash
git add static/
git commit -m "v2: frontend — htmx/xterm vendored, app.js glue, layout css, 4 themes"
```

---

### Task 9: Remove v1, update HANDOFF.md

**Files:**
- Delete: `server.py`, `static/index.html`, `static/vendor/marked.min.js`
- Modify: `HANDOFF.md` (full overwrite)

- [ ] **Step 1: Delete superseded files**

```bash
cd /home/claude/projects/deadlight
git rm server.py static/index.html static/vendor/marked.min.js
```

(`static/app.js` was already overwritten in Task 8.)

- [ ] **Step 2: Overwrite `HANDOFF.md`**

```markdown
# deadlight — handoff

Per-project remote workspace: persistent zellij terminal + stateless
read-only viewer (tree, git changes, markdown, code) in one Rust binary.

- **Design spec:** `docs/superpowers/specs/2026-08-16-deadlight-v2-design.md`
- **Implementation plan:** `docs/superpowers/plans/2026-08-16-deadlight-v2.md`
- Run: `cargo run` (127.0.0.1:8444), tests: `cargo test`.
- Deployed via systemd user unit `deadlight.service`, exposed via
  `tailscale serve --bg --https=8444 8444`.
- URLs: `/` (index), `/{project}` (workspace). Everything else is plumbing
  (`/static`, `/ws/{project}`, `/frag/{project}/...`).
- Settings: `~/.config/deadlight/config.toml` then
  `{project}/.deadlight/config.toml` (theme, default_tab, hide) — re-read
  every request; edit the file, hit refresh.
- The v1 Python implementation was replaced wholesale on 2026-08-16
  (see git history and the spec's History note).
- code-server stays running as fallback — don't restart it casually
  (Peter's live Claude sessions run under its extension host).
```

- [ ] **Step 3: Run tests, confirm nothing referenced the deleted files**

Run: `cargo test`
Expected: all pass

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "v2: remove Python v1, point HANDOFF at spec/plan"
```

---

### Task 10: Deploy — systemd unit, tailscale, browser verification

**Files:**
- Create: `~/.config/systemd/user/deadlight.service` (outside the repo)

- [ ] **Step 1: Release build**

```bash
cd /home/claude/projects/deadlight
cargo build --release
```

Expected: `target/release/deadlight` exists.

- [ ] **Step 2: Write the unit file**

`~/.config/systemd/user/deadlight.service`:

```ini
[Unit]
Description=deadlight — project workspace (viewer + zellij terminal)

[Service]
ExecStart=/home/claude/projects/deadlight/target/release/deadlight
Restart=always
RestartSec=2

[Install]
WantedBy=default.target
```

- [ ] **Step 3: Enable and start**

```bash
systemctl --user daemon-reload
systemctl --user enable --now deadlight
sleep 1
curl -sf http://127.0.0.1:8444/ | head -c 200
```

Note: `daemon-reload` was blocked by the permission classifier in a previous session — if blocked, ask Peter to run `! systemctl --user daemon-reload && systemctl --user enable --now deadlight`.

- [ ] **Step 4: Expose via tailscale (keep existing 8082/8443 serves)**

```bash
tailscale serve --bg --https=8444 8444
tailscale serve status
```

Expected: status lists 8444→8444 alongside the existing 8082 and 8443 entries. URL: `https://ubuntu-16gb-hel1-2.tail66d083.ts.net:8444`.

- [ ] **Step 5: Manual browser checklist (Peter, or via tailscale from any device)**

- [ ] `/` lists projects; clicking one opens `/{project}` on the Terminal tab with a live zellij session (type, see output).
- [ ] Files tab: tree renders, dirs expand natively, opening `HANDOFF.md` renders markdown, opening `src/main.rs` shows highlighted code, hash shows `#files/src/main.rs`.
- [ ] Reload the page: terminal reattaches (zellij repaints), the open file is restored from the hash.
- [ ] Duplicate the tab: both tabs mirror the same zellij session; viewer panes are independent.
- [ ] Changes tab: touch a file in the repo, press `r` — badge and list update; diff renders colorized; terminal untouched.
- [ ] Back button from inside a project goes to the previous page (not through app flow); the terminal was not disturbed by any in-app clicks.
- [ ] Edit `.deadlight/config.toml` → `theme = "gruvbox"`, reload: theme applies. Write garbage to it, reload: page still works, ⚠ config shows in header. Revert.
- [ ] Close the laptop lid / drop the network briefly: overlay appears, then auto-reconnects.

- [ ] **Step 6: Commit anything pending and report**

```bash
git status
```

Expected: clean tree. Report checklist results to Peter, including anything that failed.
