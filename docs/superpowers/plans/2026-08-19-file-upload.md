# File Upload and Terminal Image Paste Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a browser put files into a resh workspace by dropping or pasting them onto the file tree, and let an image pasted onto a terminal reach the program running there as a real image attachment.

**Architecture:** Two `POST` endpoints parse `multipart/form-data` with `multer` and stream each part to a temp file in its destination directory, then rename. No async runtime: `multer` is runtime-agnostic and is driven with `futures_executor::block_on` over an iterator-backed stream, so thread-per-connection, the hand-rolled GET parser, and both websocket paths are untouched. The hub gains nothing — mirroring already happens because `watch.rs` broadcasts `TreeChanged` for a file written by any route.

**Tech Stack:** Rust, no async runtime, thread-per-connection. New dependencies: `multer`, `futures-util`, `futures-executor`, `bytes`. Plain JS with no framework on the client; Deno for the browser test. `cargo test`, never `--release`.

**Spec:** `docs/superpowers/specs/2026-08-19-file-upload-design.md` — read it before Task 1, in particular *Spending the GET-only constraint*. The plan argues from the spec; where they disagree, the spec wins and the plan is wrong.

**Prerequisite — rebase first.** This branch predates two things on `master` that Task 7 depends on: `tests/browser/` (a Deno-driven Chromium harness) and the CLAUDE.md rule that anything touching `static/app.js` should be checked there. Start with `git rebase master` and confirm `tests/browser/harness.mjs` exists.

## Global Constraints

From `CLAUDE.md`, plus the one amendment this design makes. Not style preferences.

- **HTTP is GET-only apart from `/upload` and `/paste`.** No other route gains a method. This is the constraint this design spends, and Task 4 is where it is paid for — read the spec section before writing that code.
- **Both new endpoints check `Origin`**, including refusing a request that carries none, exactly as `wsconn.rs` and `term.rs` do. `host_allowed` still applies on top.
- **Every filesystem path is confined before use** — `projects::safe_resolve_parent` for anything being created.
- **Absence of evidence is not evidence of absence.** `Path::exists()`/`is_dir()` are banned before anything destructive; use `symlink_metadata` and treat "cannot tell" as a third outcome. `fileops::must_not_exist` already does this — reuse it, do not re-derive it.
- **Never hold a lock across blocking I/O.** No hub lock is taken anywhere in this feature; if you find yourself reaching for one, stop and re-read the spec's *Injecting the paste*.
- **No panics may escape a socket thread.**
- **Module-level `//!` doc explaining *why* the module exists**; `#[cfg(test)] mod tests` at the bottom of the same file; comments give rationale, not mechanics.
- **Limits:** 16 parts per request (constant), 100 MB aggregate (`config::max_upload_bytes`, global config only). No per-file cap — see the spec for why that was dropped rather than forgotten.
- **Tests must be able to fail.** For every negative test assert on *why* — the message, not `is_err()`. Before committing any task, revert your implementation, run the test, watch it fail, restore. That is the step that has caught vacuous tests here twice.

---

### Task 1: Let POST reach a handler

`http.rs` rejects every method but GET at the parse layer and stops reading at the blank line. This task adds the method to the parsed request and a body reader, and nothing else — no endpoint yet.

**Files:**
- Modify: `src/http.rs:6-20` (`Request` gains `method`; POST accepted)
- Modify: `src/routes.rs:19-28` (`handle` dispatches POST before `route`)
- Test: `src/http.rs` (bottom), `tests/integration.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `http::Request { method: String, .. }`, and `upload::handle_post(w, reader, req, roots)` as a stub that 404s. Tasks 4 and 5 fill it in.

- [ ] **Step 1: Write the failing tests**

In `src/http.rs`'s test module:

```rust
    #[test]
    fn parse_keeps_the_method_and_accepts_post() {
        let r = parse_str("POST /upload/proj?dir=src HTTP/1.1\r\nHost: h\r\nContent-Length: 3\r\n\r\nabc")
            .unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/upload/proj");
        assert_eq!(r.query.get("dir").map(String::as_str), Some("src"));
    }

    /// The body must still be readable after parsing: `handle` wraps the socket
    /// in a BufReader, so the first bytes of the body are frequently sitting in
    /// that buffer already. A body reader that goes back to the raw TcpStream
    /// silently loses them — the upload arrives with a hole at the front and
    /// multer reports a malformed boundary, which looks like a client bug.
    #[test]
    fn the_body_survives_header_parsing() {
        let raw = "POST /upload/proj HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\n\r\nhello";
        let mut r = std::io::BufReader::new(std::io::Cursor::new(raw.as_bytes()));
        let req = parse(&mut r).unwrap();
        assert_eq!(req.headers.get("content-length").map(String::as_str), Some("5"));
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut r, &mut rest).unwrap();
        assert_eq!(rest, "hello", "the body was consumed or lost by header parsing");
    }

    #[test]
    fn other_methods_are_still_refused() {
        let e = parse_str("DELETE /x HTTP/1.1\r\n\r\n").unwrap_err();
        assert!(e.contains("not allowed"), "unexpected message: {e}");
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --lib http`
Expected: FAIL — `Request` has no `method` field (compile error), which is the correct first failure.

- [ ] **Step 3: Carry the method through the parser**

In `src/http.rs`, add the field and replace the method check:

```rust
pub struct Request {
    /// Uppercase, as sent. GET everywhere except the two upload endpoints —
    /// see CLAUDE.md's amended GET-only constraint.
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    /// Header names lowercased.
    pub headers: HashMap<String, String>,
}
```

and in `parse`:

```rust
    let method = parts.next().ok_or("empty request")?.to_string();
    if method != "GET" && method != "POST" {
        return Err(format!("method {method} not allowed"));
    }
```

Then add `method` to the `Request` construction at the end of `parse`. The parser must **not** read the body — leaving it in the `BufReader` is what makes Task 4 possible.

- [ ] **Step 4: Dispatch POST before the router**

In `src/routes.rs`, change `handle`. Note that `route()` and every test that calls it stay untouched — POST never reaches it:

```rust
pub fn handle(stream: TcpStream, roots: &[PathBuf]) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(read_half) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read_half);
    let mut w = stream;
    match http::parse(&mut reader) {
        // POST is the upload surface and nothing else; it deliberately does not
        // reach `route`, so no existing route can be invoked with a body.
        Ok(req) if req.method == "POST" => {
            // The 10s read timeout above is an inactivity timer sized for a
            // request that arrives in one packet. A 100 MB body over a tailnet
            // hiccup exceeds it while making perfectly good progress, and the
            // upload dies mid-stream with no error the user can act on.
            let _ = w.set_read_timeout(Some(Duration::from_secs(60)));
            crate::upload::handle_post(&mut w, &mut reader, &req, roots);
        }
        Ok(req) => route(&mut w, &req, roots),
        Err(e) => http::respond(&mut w, 400, "Bad Request", "text/plain", e.as_bytes()),
    }
}
```

- [ ] **Step 5: Create the stub module**

`src/upload.rs`, registered in `src/lib.rs` as `pub mod upload;`:

```rust
//! The two POST endpoints: `/upload/{project}` and `/paste/{project}/{session}`.
//!
//! This is the only part of resh that accepts a request body, and the only
//! exception to the GET-only rule — which exists because it is why resh has no
//! CSRF surface. A `multipart/form-data` POST is a CORS *simple* request, so
//! any page the user visits can submit one cross-origin with no preflight; the
//! `Origin` check below is the whole of what stands between a hostile page and
//! an arbitrary file write. Treat it the way `wsconn.rs` treats its own.
use std::io::{BufRead, Write};
use std::path::PathBuf;

pub fn handle_post(
    w: &mut impl Write,
    _reader: &mut impl BufRead,
    req: &crate::http::Request,
    _roots: &[PathBuf],
) {
    crate::http::respond(w, 404, "Not Found", "text/plain; charset=utf-8", b"no such endpoint");
    let _ = req;
}
```

- [ ] **Step 6: Run the suite**

Run: `cargo test`
Expected: PASS. Every existing `route()` test compiles unchanged; only `Request` construction sites needed the new field.

- [ ] **Step 7: Commit**

```bash
git add src/http.rs src/routes.rs src/upload.rs src/lib.rs
git commit -m "http: parse POST and route it to an upload handler"
```

---

### Task 2: The upload limit, global-only

A configurable ceiling that a project cannot raise. `config.rs:69-72` already makes this exact argument for `allowed_origins`; this follows it rather than inventing a new rule.

**Files:**
- Modify: `src/config.rs:6-13` (`RawConfig`), and a new `max_upload_bytes()` beside `allowed_origins()`
- Test: `src/config.rs` (bottom, existing `mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `config::max_upload_bytes() -> u64` and `config::MAX_UPLOAD_PARTS: usize`, used by Tasks 4 and 5.

- [ ] **Step 1: Write the failing tests**

```rust
    /// The test that fails the moment someone "helpfully" moves this key into
    /// `Settings`. A project's `.resh/config.toml` lives inside the repository,
    /// so a cloned hostile repo could otherwise raise its own disk ceiling and
    /// turn a mis-drag into a disk-fill.
    #[test]
    fn a_project_config_cannot_raise_the_upload_limit() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", d.path());
        std::env::remove_var("RESH_MAX_UPLOAD");
        std::fs::create_dir_all(d.path().join(".config/resh")).unwrap();
        std::fs::write(d.path().join(".config/resh/config.toml"), "max_upload_bytes = 5000").unwrap();

        let proj = d.path().join("proj");
        std::fs::create_dir_all(proj.join(".resh")).unwrap();
        std::fs::write(proj.join(".resh/config.toml"), "max_upload_bytes = 999999999").unwrap();

        assert_eq!(max_upload_bytes(), 5000, "a project config must not raise the ceiling");
        // And the project file is still read for the things it *is* allowed to set.
        assert_eq!(for_project(&proj).theme, Settings::default().theme);
    }

    #[test]
    fn the_env_var_wins_and_a_missing_config_falls_back_to_the_default() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", d.path());
        std::env::remove_var("RESH_MAX_UPLOAD");
        assert_eq!(max_upload_bytes(), DEFAULT_MAX_UPLOAD);
        std::env::set_var("RESH_MAX_UPLOAD", "1234");
        assert_eq!(max_upload_bytes(), 1234);
        std::env::remove_var("RESH_MAX_UPLOAD");
    }

    /// Zero or garbage must not disable the ceiling — an unparseable value is a
    /// typo, and reading it as "no limit" turns a typo into a disk-fill.
    #[test]
    fn a_bad_value_falls_back_rather_than_disabling_the_limit() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", d.path());
        std::env::set_var("RESH_MAX_UPLOAD", "banana");
        assert_eq!(max_upload_bytes(), DEFAULT_MAX_UPLOAD);
        std::env::set_var("RESH_MAX_UPLOAD", "0");
        assert_eq!(max_upload_bytes(), DEFAULT_MAX_UPLOAD);
        std::env::remove_var("RESH_MAX_UPLOAD");
    }
```

These mutate process-global env. If the existing config tests already serialise on a lock, use it; if not, add one and take it in all three.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --lib config`
Expected: FAIL — `max_upload_bytes` does not exist.

- [ ] **Step 3: Implement**

Add to `RawConfig` (and **not** to `Settings`):

```rust
struct RawConfig {
    theme: Option<String>,
    default_tab: Option<String>,
    hide: Option<Vec<String>>,
    allowed_origins: Option<Vec<String>>,
    max_upload_bytes: Option<u64>,
}
```

and beside `allowed_origins()`:

```rust
/// 100 MB. Screenshots run 3–5 MB and a short screen recording 50–100 MB, so
/// this clears the real cases with room; anything larger is a mis-drag, and a
/// mis-drag that fills the disk breaks dtach socket creation, state writes and
/// git all at once.
pub const DEFAULT_MAX_UPLOAD: u64 = 100_000_000;

/// Not configurable, deliberately. This expresses a product decision — resh is
/// not a project transfer tool — rather than fitting a machine, and a tunable
/// would just invite the decision to be configured away.
pub const MAX_UPLOAD_PARTS: usize = 16;

/// Aggregate bytes one request may carry. Global-only, exactly like
/// [`allowed_origins`] and for the same reason: a per-project
/// `.resh/config.toml` ships inside the repository, so a cloned hostile repo
/// could otherwise raise its own disk ceiling. Deliberately **not** part of
/// [`Settings`].
pub fn max_upload_bytes() -> u64 {
    if let Ok(v) = std::env::var("RESH_MAX_UPLOAD") {
        if let Ok(n) = v.trim().parse::<u64>() {
            if n > 0 {
                return n;
            }
        }
    }
    std::fs::read_to_string(global_config_path())
        .ok()
        .and_then(|s| toml::from_str::<RawConfig>(&s).ok())
        .and_then(|r| r.max_upload_bytes)
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_UPLOAD)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib config`
Expected: PASS.

- [ ] **Step 5: Prove the isolation test can fail**

Add `max_upload_bytes` to `Settings` and have `max_upload_bytes()` read it via `for_project`. Run `cargo test --lib a_project_config_cannot_raise`, confirm it fails, then revert. This is the one test in the task whose absence would be invisible.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "config: a global-only upload ceiling, defaulting to 100 MB"
```

---

### Task 3: Streaming a part to disk

The writer both endpoints use. Streams to a temp file in the destination directory, then renames — so nothing partial is ever visible under the real name, and an abandoned upload cleans itself up.

**Files:**
- Modify: `src/fileops.rs` (add `UploadTemp` and two validators after `create_dir`)
- Test: `src/fileops.rs` (bottom, existing `mod tests`)

**Interfaces:**
- Consumes: `projects::safe_resolve_parent`, `fileops::must_not_exist`, `projects::SKIP_DIRS`.
- Produces: `fileops::UploadTemp::create(project_dir: &Path, dir_rel: &str, name: &str) -> Result<UploadTemp, String>`, `.write(&mut self, chunk: &[u8]) -> Result<(), String>`, `.commit(self) -> Result<PathBuf, String>`. Used by Tasks 4 and 5.

- [ ] **Step 1: Write the failing tests**

```rust
    fn put(project: &Path, dir: &str, name: &str, data: &[u8]) -> Result<PathBuf, String> {
        let mut t = UploadTemp::create(project, dir, name)?;
        t.write(data)?;
        t.commit()
    }

    /// Reaches the confinement check rather than failing earlier for an
    /// unrelated reason: `..` exists, so this cannot pass on ENOENT. That hole
    /// is why a symlink escape once survived review here.
    #[test]
    fn upload_refuses_a_traversal_and_says_why() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("proj");
        std::fs::create_dir(&proj).unwrap();
        let e = put(&proj, "..", "escape.png", b"x").unwrap_err();
        assert!(e.contains("path outside project"), "unexpected message: {e}");
        assert!(!d.path().join("escape.png").exists());
    }

    /// The test that keeps directory upload from arriving by accident: a
    /// separator in a part's filename is an error, never silently flattened.
    #[test]
    fn a_separator_in_the_filename_is_refused_not_flattened() {
        let d = tempfile::tempdir().unwrap();
        for name in ["sub/a.png", "sub\\a.png"] {
            let e = put(d.path(), "", name, b"x").unwrap_err();
            assert!(e.contains("invalid filename"), "unexpected message for {name}: {e}");
        }
        assert!(!d.path().join("a.png").exists(), "a flattened file was written");
    }

    /// Asserting an error alone would also pass against an implementation that
    /// truncated the file and *then* failed — the outcome this forbids.
    #[test]
    fn upload_refuses_a_collision_and_leaves_the_original_intact() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.png"), b"original").unwrap();
        let e = put(d.path(), "", "a.png", b"replacement").unwrap_err();
        assert!(e.contains("already exists"), "unexpected message: {e}");
        assert_eq!(std::fs::read(d.path().join("a.png")).unwrap(), b"original");
    }

    /// A skipped directory is not rendered in the tree, so a file written there
    /// is invisible in the UI that wrote it — and inside `.git` it can corrupt
    /// the repository. The path is legal and inside the project, which is
    /// exactly why nothing else refuses it.
    #[test]
    fn upload_refuses_a_destination_the_tree_never_shows() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let e = put(d.path(), ".git", "config", b"x").unwrap_err();
        assert!(e.contains("not visible in the tree"), "unexpected message: {e}");
        assert!(!d.path().join(".git/config").exists());
    }

    /// The complement, and the test that stops the rule being written as
    /// "refuse a leading dot": the tree hides a fixed list of *directories*,
    /// not dotfiles, so `.gitignore` is visible and uploading it is honest.
    #[test]
    fn upload_allows_an_ordinary_dotfile() {
        let d = tempfile::tempdir().unwrap();
        assert!(put(d.path(), "", ".gitignore", b"target\n").is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn upload_refuses_when_it_cannot_tell_whether_the_target_exists() {
        use std::os::unix::fs::PermissionsExt;
        if is_root() {
            return; // mode bits do not apply to root; the fixture cannot enter its own precondition
        }
        let d = tempfile::tempdir().unwrap();
        let locked = d.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let r = put(d.path(), "locked", "a.png", b"x");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let e = r.unwrap_err();
        assert!(
            e.contains("no such directory") || e.contains("cannot check"),
            "an unreadable parent must be refused as unknown, not treated as absent: {e}"
        );
    }

    #[test]
    fn a_part_written_in_chunks_lands_whole_and_leaves_no_temp() {
        let d = tempfile::tempdir().unwrap();
        let mut t = UploadTemp::create(d.path(), "", "a.bin").unwrap();
        t.write(&[0x89, 0x50]).unwrap();
        t.write(&[0x4e, 0x47]).unwrap();
        let p = t.commit().unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), [0x89, 0x50, 0x4e, 0x47]);
        assert!(leftover_temps(d.path()).is_empty());
    }

    /// An abandoned upload — a cap breach, a dropped connection — must not
    /// leave anything behind. Dropping without committing is the common path,
    /// not an edge case.
    #[test]
    fn an_abandoned_part_removes_its_temp_file() {
        let d = tempfile::tempdir().unwrap();
        {
            let mut t = UploadTemp::create(d.path(), "", "a.bin").unwrap();
            t.write(b"partial").unwrap();
        }
        assert!(leftover_temps(d.path()).is_empty(), "a dropped upload left its temp behind");
        assert!(!d.path().join("a.bin").exists(), "an uncommitted upload became visible");
    }

    fn leftover_temps(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("resh.tmp"))
            .collect()
    }

    fn is_root() -> bool {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim() == "0")
            .unwrap_or(false)
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --lib fileops`
Expected: FAIL — `UploadTemp` does not exist.

- [ ] **Step 3: Implement**

```rust
/// Rejects what `safe_resolve_parent` does not: it validates the final
/// component for traversal, but a part's filename comes from the browser and is
/// attacker-influenced. Separators are refused rather than flattened — that is
/// how directory upload stays a non-goal instead of arriving by accident.
fn valid_upload_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(format!("invalid filename: {name:?}"));
    }
    if name.len() > 255 {
        return Err(format!("invalid filename: {} bytes is too long", name.len()));
    }
    if name.contains('/') || name.contains('\\') || name.chars().any(|c| c.is_control()) {
        return Err(format!("invalid filename: {name:?}"));
    }
    Ok(())
}

/// Refuses destinations the file tree never renders. Not a path-safety rule —
/// these paths are legal and inside the project — but a visibility one: a file
/// written into `.git` or `.claude` cannot be seen, opened or deleted from the
/// UI that wrote it, and the next upload of the same name is refused as
/// "already exists" against a file the user has no way to find. Inside `.git`
/// it is worse than confusing, since a write into an object or ref directory
/// can corrupt the repository.
///
/// Keyed on `SKIP_DIRS`, not on a leading dot: the tree hides a fixed list of
/// directories, so `.gitignore` is visible and uploading one is honest.
fn visible_in_tree(rel: &str) -> Result<(), String> {
    for segment in rel.split('/').filter(|s| !s.is_empty()) {
        if crate::projects::SKIP_DIRS.contains(&segment) {
            return Err(format!("{segment} is not visible in the tree; refusing to upload into it"));
        }
    }
    Ok(())
}

/// A part being streamed to disk. Writes to a temp file in the *destination*
/// directory so the final step is a rename on the same filesystem — atomic, so
/// a reader never sees a partial file under the real name — and removes the
/// temp on drop, so an abandoned upload leaves nothing behind.
pub struct UploadTemp {
    tmp: Option<PathBuf>,
    dest: PathBuf,
    rel: String,
    file: std::fs::File,
}

impl UploadTemp {
    pub fn create(project_dir: &Path, dir_rel: &str, name: &str) -> Result<Self, String> {
        valid_upload_name(name)?;
        let rel = if dir_rel.is_empty() { name.to_string() } else { format!("{dir_rel}/{name}") };
        visible_in_tree(&rel)?;
        let dest = safe_resolve_parent(project_dir, &rel)?;
        // Checked here so an upload that cannot land is refused before its bytes
        // are accepted, and again in `commit` because a streamed part takes long
        // enough for the answer to change.
        must_not_exist(&dest, &rel)?;
        let parent = dest.parent().ok_or("no parent directory")?;
        // Pid-unique: two resh processes writing the same destination must not
        // collide on the temp name.
        let tmp = parent.join(format!(".{name}.{}.resh.tmp", std::process::id()));
        let file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        Ok(UploadTemp { tmp: Some(tmp), dest, rel, file })
    }

    pub fn write(&mut self, chunk: &[u8]) -> Result<(), String> {
        use std::io::Write;
        self.file.write_all(chunk).map_err(|e| e.to_string())
    }

    pub fn commit(mut self) -> Result<PathBuf, String> {
        use std::io::Write;
        self.file.flush().map_err(|e| e.to_string())?;
        must_not_exist(&self.dest, &self.rel)?;
        let tmp = self.tmp.take().ok_or("upload already committed")?;
        std::fs::rename(&tmp, &self.dest).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })?;
        Ok(self.dest.clone())
    }
}

impl Drop for UploadTemp {
    fn drop(&mut self) {
        if let Some(tmp) = self.tmp.take() {
            let _ = std::fs::remove_file(tmp);
        }
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Prove two tests can fail**

Remove the `must_not_exist` call in `create`, run `cargo test --lib upload_refuses_a_collision`, confirm it fails on the *bytes* assertion, restore. Then remove the `Drop` impl, run `cargo test --lib an_abandoned_part`, confirm it fails, restore.

- [ ] **Step 6: Commit**

```bash
git add src/fileops.rs
git commit -m "fileops: stream an upload to a temp file, then rename"
```

---

### Task 4: `POST /upload/{project}`

The endpoint, the `Origin` check, and the caps. This is the task that spends the GET-only constraint — read the spec's *Spending the GET-only constraint* before starting.

**Files:**
- Modify: `Cargo.toml` (add `multer`, `futures-util`, `futures-executor`, `bytes`)
- Modify: `src/upload.rs` (replace the stub)
- Test: `tests/integration.rs`

**Interfaces:**
- Consumes: `config::max_upload_bytes`, `config::MAX_UPLOAD_PARTS`, `fileops::UploadTemp`, `origin::origin_allowed`, `projects::resolve` (whatever `route` already uses to turn a project segment into a directory — reuse it, do not re-resolve).
- Produces: `POST /upload/{project}?dir=<rel>` returning `{"results":[{"name":…,"ok":…,"error":…}]}`.

- [ ] **Step 1: Write the failing tests**

In `tests/integration.rs`. A tiny multipart builder keeps these readable:

```rust
fn multipart(parts: &[(&str, &[u8])]) -> (String, Vec<u8>) {
    let boundary = "----reshtestboundary";
    let mut body = Vec::new();
    for (name, data) in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// Raw socket rather than a client library: these tests must control the
/// Origin header exactly, including omitting it.
fn post(port: u16, path: &str, origin: Option<&str>, ctype: &str, body: &[u8]) -> (u16, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(20))).unwrap();
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: {ctype}\r\n\
         Content-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(o) = origin {
        head.push_str(&format!("Origin: {o}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut resp = Vec::new();
    let _ = s.read_to_end(&mut resp);
    let text = String::from_utf8_lossy(&resp).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, text)
}

/// The check the whole GET-only amendment rests on, so it gets the treatment
/// `ws_rejects_foreign_and_missing_origin` already gives the socket — and it
/// asserts the *file* was not written, not merely the status, because a 403
/// returned after the write would still be a drive-by write.
#[test]
fn upload_refuses_a_foreign_or_absent_origin_without_writing() {
    let (d, port) = fixture();
    let (ct, body) = multipart(&[("evil.txt", b"x")]);

    let (s1, _) = post(port, "/upload/proj", Some("https://evil.example.com"), &ct, &body);
    assert_eq!(s1, 403, "a foreign origin must not reach the upload endpoint");

    let (s2, _) = post(port, "/upload/proj", None, &ct, &body);
    assert_eq!(s2, 403, "a request with no Origin must be refused");

    assert!(
        !d.path().join("proj/evil.txt").exists(),
        "a refused upload must not have written the file"
    );
}

#[test]
fn upload_writes_every_part_and_reports_per_file() {
    let (d, port) = fixture();
    let origin = format!("http://127.0.0.1:{port}");
    std::fs::write(d.path().join("proj/taken.txt"), b"original").unwrap();
    let (ct, body) = multipart(&[("a.txt", b"AAA"), ("taken.txt", b"BBB"), ("c.txt", b"CCC")]);

    let (status, resp) = post(port, "/upload/proj", Some(&origin), &ct, &body);
    assert_eq!(status, 200, "a partial failure is still a well-formed request");

    assert_eq!(std::fs::read(d.path().join("proj/a.txt")).unwrap(), b"AAA");
    assert_eq!(std::fs::read(d.path().join("proj/c.txt")).unwrap(), b"CCC");
    assert_eq!(
        std::fs::read(d.path().join("proj/taken.txt")).unwrap(),
        b"original",
        "the colliding part must not have overwritten anything"
    );
    assert!(resp.contains("taken.txt") && resp.contains("already exists"), "response: {resp}");
    // The neighbours must be reported as successes, or a caller cannot tell
    // which of the three failed.
    assert!(resp.contains("\"name\":\"a.txt\",\"ok\":true"), "response: {resp}");
}

#[test]
fn upload_refuses_more_parts_than_the_limit() {
    let (d, port) = fixture();
    let origin = format!("http://127.0.0.1:{port}");
    let names: Vec<String> = (0..20).map(|i| format!("f{i}.txt")).collect();
    let parts: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), b"x" as &[u8])).collect();
    let (ct, body) = multipart(&parts);

    let (status, resp) = post(port, "/upload/proj", Some(&origin), &ct, &body);
    assert_eq!(status, 413);
    assert!(resp.contains("too many files"), "the parts cap must name itself: {resp}");
}

/// A different cap with a different message. Two tests that both pass because
/// the same limit fired would say nothing about the other.
#[test]
fn upload_refuses_a_body_past_the_aggregate_limit() {
    let (d, port) = fixture();
    std::env::set_var("RESH_MAX_UPLOAD", "4096");
    let origin = format!("http://127.0.0.1:{port}");
    let big = vec![b'x'; 8192];
    let (ct, body) = multipart(&[("big.bin", &big)]);

    let (status, resp) = post(port, "/upload/proj", Some(&origin), &ct, &body);
    std::env::remove_var("RESH_MAX_UPLOAD");
    assert_eq!(status, 413);
    assert!(resp.contains("too large"), "the size cap must name itself: {resp}");
    assert!(!d.path().join("proj/big.bin").exists());
    let leftovers: Vec<_> = std::fs::read_dir(d.path().join("proj"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("resh.tmp"))
        .collect();
    assert!(leftovers.is_empty(), "a cap breach left a partial file: {leftovers:?}");
}

#[test]
fn upload_refuses_a_hidden_destination() {
    let (d, port) = fixture();
    let origin = format!("http://127.0.0.1:{port}");
    std::fs::create_dir_all(d.path().join("proj/.git")).unwrap();
    let (ct, body) = multipart(&[("config", b"[core]")]);
    let (status, resp) = post(port, "/upload/proj?dir=.git", Some(&origin), &ct, &body);
    assert_eq!(status, 200);
    assert!(resp.contains("not visible in the tree"), "response: {resp}");
    assert!(!d.path().join("proj/.git/config").exists());
}

/// The new arm must not be reachable without a body.
#[test]
fn get_on_the_upload_path_is_still_refused() {
    let (_d, port) = fixture();
    let (status, _, _) = get_full(port, "/upload/proj");
    assert_ne!(status, 200, "GET must not reach the upload endpoint");
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --test integration upload`
Expected: FAIL — the stub 404s.

- [ ] **Step 3: Add the dependencies**

```toml
multer = "3.1"
futures-util = { version = "0.3", default-features = false }
futures-executor = "0.3"
bytes = "1"
```

`multer`'s `tokio-io` feature stays off — that is what keeps this runtime-free.

- [ ] **Step 4: Implement the endpoint**

Replace the stub in `src/upload.rs`. The comments carry the two non-obvious constraints:

```rust
pub fn handle_post(
    w: &mut impl Write,
    reader: &mut impl BufRead,
    req: &crate::http::Request,
    roots: &[PathBuf],
) {
    // Same DNS-rebinding gate the GET router applies.
    if !crate::origin::host_allowed(
        req.headers.get("host").map(String::as_str),
        req.headers.get("x-forwarded-host").map(String::as_str),
        &crate::config::allowed_origins(),
    ) {
        return crate::http::respond(w, 403, "Forbidden", "text/plain", b"host not allowed");
    }
    // The whole of what stands between a hostile page and an arbitrary file
    // write. A missing Origin is refused, not defaulted — browsers always send
    // one cross-origin, and this endpoint exists for the browser.
    let origin = req.headers.get("origin").map(String::as_str);
    if !crate::origin::origin_allowed(origin, &crate::config::allowed_origins()) {
        eprintln!("resh: rejected upload origin={origin:?} (set allowed_origins)");
        return crate::http::respond(w, 403, "Forbidden", "text/plain", b"origin not allowed");
    }

    let segs: Vec<&str> = req.path.split('/').filter(|s| !s.is_empty()).collect();
    match segs.as_slice() {
        ["upload", project @ ..] if !project.is_empty() => {
            do_upload(w, reader, req, roots, &project.join("/"))
        }
        // Task 5 adds the /paste arm here.
        _ => crate::http::respond(w, 404, "Not Found", "text/plain", b"no such endpoint"),
    }
}

enum Halt {
    TooManyParts,
    TooLarge,
    Malformed(String),
}

fn do_upload(
    w: &mut impl Write,
    reader: &mut impl BufRead,
    req: &crate::http::Request,
    roots: &[PathBuf],
    project: &str,
) {
    let Some(dir) = crate::projects::resolve(roots, project) else {
        return crate::http::respond(w, 404, "Not Found", "text/plain", b"no such project");
    };
    let sub = req.query.get("dir").cloned().unwrap_or_default();
    let ctype = req.headers.get("content-type").cloned().unwrap_or_default();
    let Ok(boundary) = multer::parse_boundary(&ctype) else {
        return crate::http::respond(w, 400, "Bad Request", "text/plain", b"expected multipart/form-data");
    };
    let len: u64 = req.headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let cap = crate::config::max_upload_bytes();

    match receive(reader, len, &boundary, &dir, &sub, cap) {
        Ok(results) => {
            let body = serde_json::json!({ "results": results }).to_string();
            crate::http::respond(w, 200, "OK", "application/json", body.as_bytes());
        }
        Err(Halt::TooManyParts) => {
            let msg = format!("too many files in one upload (limit {})", crate::config::MAX_UPLOAD_PARTS);
            crate::http::respond(w, 413, "Payload Too Large", "text/plain", msg.as_bytes())
        }
        Err(Halt::TooLarge) => {
            let msg = format!("upload too large (limit {cap} bytes)");
            crate::http::respond(w, 413, "Payload Too Large", "text/plain", msg.as_bytes())
        }
        Err(Halt::Malformed(e)) => {
            crate::http::respond(w, 400, "Bad Request", "text/plain", e.as_bytes())
        }
    }
}

fn receive(
    reader: &mut impl BufRead,
    len: u64,
    boundary: &str,
    dir: &Path,
    sub: &str,
    cap: u64,
) -> Result<Vec<serde_json::Value>, Halt> {
    use futures_util::StreamExt;
    use std::io::Read;

    // `take(len)` rather than reading to EOF: the client sends the body and then
    // waits for a response, so reading to EOF would block until it gives up.
    let mut body = reader.take(len);
    // Blocking reads inside a stream are fine here: `block_on` below drives this
    // on the connection's own thread, which has nothing else to do.
    let chunks = std::iter::from_fn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        match body.read(&mut buf) {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some(Ok::<_, std::io::Error>(bytes::Bytes::from(buf)))
            }
            Err(e) => Some(Err(e)),
        }
    });
    let mut mp = multer::Multipart::new(futures_util::stream::iter(chunks), boundary);

    futures_executor::block_on(async move {
        let mut results = Vec::new();
        let mut parts = 0usize;
        let mut total: u64 = 0;

        while let Some(mut field) = mp.next_field().await.map_err(|e| Halt::Malformed(e.to_string()))? {
            parts += 1;
            if parts > crate::config::MAX_UPLOAD_PARTS {
                return Err(Halt::TooManyParts);
            }
            let name = field.file_name().unwrap_or_default().to_string();

            let mut sink = match crate::fileops::UploadTemp::create(dir, sub, &name) {
                Ok(t) => Some(t),
                Err(e) => {
                    results.push(serde_json::json!({"name": name, "ok": false, "error": e}));
                    None
                }
            };
            // A rejected part is still drained. multer is a single pass over one
            // stream: abandoning a field mid-way leaves the parser positioned
            // inside it, and every later part is lost or misread.
            let mut failed: Option<String> = None;
            while let Some(chunk) = field.chunk().await.map_err(|e| Halt::Malformed(e.to_string()))? {
                total += chunk.len() as u64;
                if total > cap {
                    return Err(Halt::TooLarge); // UploadTemp's Drop removes the partial file
                }
                if let Some(t) = sink.as_mut() {
                    if let Err(e) = t.write(&chunk) {
                        failed = Some(e);
                        sink = None;
                    }
                }
            }
            if let Some(t) = sink {
                match t.commit() {
                    Ok(_) => results.push(serde_json::json!({"name": name, "ok": true})),
                    Err(e) => results.push(serde_json::json!({"name": name, "ok": false, "error": e})),
                }
            } else if let Some(e) = failed {
                results.push(serde_json::json!({"name": name, "ok": false, "error": e}));
            }
        }
        Ok(results)
    })
}
```

If `projects::resolve` is not the actual helper `route()` uses to turn a project segment into a directory, use whichever one it does — do not add a second resolution path.

- [ ] **Step 5: Run the tests**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Prove the Origin test can fail**

Delete the `origin_allowed` block, run `cargo test --test integration upload_refuses_a_foreign`, and confirm it fails on the *file exists* assertion as well as the status. Restore. Of every revert-and-watch step in this plan, this is the one to actually perform: it is the check the GET-only amendment is traded against.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock src/upload.rs
git commit -m "upload: POST /upload, streamed to disk under two caps"
```

---

### Task 5: `POST /paste/{project}/{session}`

Same receiving machinery, different destination and one side effect: the path is injected into the session's PTY.

**Files:**
- Create: `src/paste.rs`
- Modify: `src/lib.rs` (`pub mod paste;`), `src/upload.rs` (the `/paste` arm)
- Test: `src/paste.rs` (bottom), `tests/integration.rs`

**Interfaces:**
- Consumes: `wsstate::state_dir`, `projects::storage_key`, `fileops::UploadTemp`, `session::{valid_name, has_session, key_for, write_input}`.
- Produces: `paste::extension_of(&[u8]) -> Option<&'static str>`, `paste::scratch_dir(project: &str) -> PathBuf`.

- [ ] **Step 1: Write the failing tests**

In `src/paste.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];

    #[test]
    fn sniffs_each_accepted_format() {
        assert_eq!(extension_of(PNG), Some("png"));
        assert_eq!(extension_of(&[0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0]), Some("jpg"));
        assert_eq!(extension_of(b"GIF89a__"), Some("gif"));
        assert_eq!(extension_of(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
    }

    /// A BMP is a real image the *clipboard* route accepts, and is still refused
    /// here: the receiving side's filename regex does not cover `.bmp`, so
    /// writing one would produce a file that silently arrives as text.
    #[test]
    fn refuses_formats_the_receiver_cannot_read_from_a_path() {
        assert_eq!(extension_of(b"BM\0\0\0\0\0\0"), None);
        assert_eq!(extension_of(b"not an image at all"), None);
        assert_eq!(extension_of(&[]), None);
    }

    /// A nested project's `/` must not become a separator — the same reason
    /// wsstate keys by storage_key rather than the raw project string.
    #[test]
    fn a_nested_project_key_is_encoded_not_split() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path());
        let p = scratch_dir("karpie/src");
        let leaf = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(!leaf.contains('/'), "project key leaked a separator: {leaf}");
        assert_eq!(leaf, crate::projects::storage_key("karpie/src"));
        assert!(p.starts_with(d.path()), "scratch must live under the state dir, not the project");
    }
}
```

And in `tests/integration.rs`:

```rust
/// With RESH_CMD=cat the PTY echoes what is written to it, so the terminal
/// socket is a direct view of the injected bytes. Asserting the markers — not
/// merely that the session survived — is the point: CLAUDE.md records a test
/// whose subject was a call it never actually verified.
#[test]
fn a_pasted_image_injects_a_bracketed_path_into_the_pty() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("RESH_CMD", "cat");
    let state = tempfile::tempdir().unwrap();
    std::env::set_var("RESH_STATE_DIR", state.path());
    let (_d, port) = fixture();
    let origin = format!("http://127.0.0.1:{port}");

    let mut term = ws_connect(port, Some("http://127.0.0.1:8444")).unwrap();
    let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
    let (ct, body) = multipart(&[("clip.png", &png)]);
    let (status, _) = post(port, "/paste/proj/shell", Some(&origin), &ct, &body);
    assert_eq!(status, 200);

    let mut seen = String::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline && !seen.contains("\u{1b}[201~") {
        match term.read() {
            Ok(tungstenite::Message::Binary(b)) => seen.push_str(&String::from_utf8_lossy(&b)),
            Ok(_) => {}
            Err(e) => panic!("terminal socket died: {e}"),
        }
    }
    assert!(seen.contains("\u{1b}[200~"), "missing opening marker: {seen:?}");
    assert!(seen.contains("\u{1b}[201~"), "missing closing marker: {seen:?}");
    assert!(seen.contains(".png"), "the injected path must carry an image extension: {seen:?}");
    assert!(
        seen.contains(&state.path().join("pasted").to_string_lossy().to_string()),
        "the path must be absolute and under the state dir: {seen:?}"
    );
}

#[test]
fn a_paste_of_a_non_image_is_refused() {
    let (_d, port) = fixture();
    let origin = format!("http://127.0.0.1:{port}");
    let (ct, body) = multipart(&[("clip.png", b"BM not really a png")]);
    let (status, resp) = post(port, "/paste/proj/shell", Some(&origin), &ct, &body);
    assert_eq!(status, 400);
    assert!(resp.contains("PNG"), "the error must name what is accepted: {resp}");
}

#[test]
fn a_paste_onto_a_dead_session_is_an_error_not_a_silent_success() {
    let (_d, port) = fixture();
    let origin = format!("http://127.0.0.1:{port}");
    let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
    let (ct, body) = multipart(&[("clip.png", &png)]);
    let (status, resp) = post(port, "/paste/proj/nosuch", Some(&origin), &ct, &body);
    assert_eq!(status, 404);
    assert!(resp.contains("no such session"), "unexpected error: {resp}");
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test paste`
Expected: FAIL — module and endpoint missing.

- [ ] **Step 3: Write `src/paste.rs`**

```rust
//! Scratch storage for images pasted onto a terminal.
//!
//! These files live deliberately *outside* the project. resh already keeps its
//! own state out of the working tree so that using it never shows up in `git
//! status`; a paste directory inside the repo would undo that on the first
//! screenshot.
//!
//! The extension is not cosmetic. The program receiving the paste decides
//! whether a pasted path is an image by looking at the *filename*, not the
//! bytes, so a correct PNG written as `.dat` silently arrives as text. That is
//! why this sniffs the content and refuses anything it cannot name correctly,
//! rather than trusting a MIME type from the browser.
use std::path::PathBuf;

/// The formats the receiver recognises from a path. BMP is deliberately absent:
/// the clipboard route accepts it, the path route does not.
pub fn extension_of(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("png");
    }
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("jpg");
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some("gif");
    }
    if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        return Some("webp");
    }
    None
}

pub fn scratch_dir(project: &str) -> PathBuf {
    crate::wsstate::state_dir().join("pasted").join(crate::projects::storage_key(project))
}

/// A name no existing file holds. A counter rather than a bare timestamp: two
/// pastes in the same second would collide, and `UploadTemp` refuses a
/// collision rather than resolving it, so the second paste would appear to do
/// nothing.
pub fn free_name(dir: &std::path::Path, ext: &str) -> Result<String, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for n in 0..1000 {
        let name = if n == 0 { format!("{stamp}.{ext}") } else { format!("{stamp}-{n}.{ext}") };
        if dir.join(&name).symlink_metadata().is_err() {
            return Ok(name);
        }
    }
    Err("too many pasted images in the same second".into())
}
```

- [ ] **Step 4: Add the `/paste` arm**

In `src/upload.rs`'s `handle_post` match, before the catch-all:

```rust
        ["paste", rest @ ..] if rest.len() >= 2 => {
            let (session, project) = rest.split_last().expect("rest.len() >= 2");
            do_paste(w, reader, req, roots, &project.join("/"), session)
        }
```

and the handler. Note where the PTY write happens — on this thread, with no lock held anywhere, because the hub is not involved:

```rust
fn do_paste(
    w: &mut impl Write,
    reader: &mut impl BufRead,
    req: &crate::http::Request,
    roots: &[PathBuf],
    project: &str,
    session: &str,
) {
    if !crate::session::valid_name(session) {
        return crate::http::respond(w, 400, "Bad Request", "text/plain", b"invalid session name");
    }
    if crate::projects::resolve(roots, project).is_none() {
        return crate::http::respond(w, 404, "Not Found", "text/plain", b"no such project");
    }
    // Checked before the bytes are accepted: writing markers into a dead PTY is
    // not destructive, but reporting success for a paste nobody will see is
    // worse than an error.
    if !crate::session::has_session(project, session) {
        let msg = format!("no such session: {session}");
        return crate::http::respond(w, 404, "Not Found", "text/plain", msg.as_bytes());
    }

    let dir = crate::paste::scratch_dir(project);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        let msg = format!("cannot create paste directory: {e}");
        return crate::http::respond(w, 500, "Internal Server Error", "text/plain", msg.as_bytes());
    }

    match receive_image(reader, req, &dir) {
        Ok(path) => {
            // Bracketed-paste markers are load-bearing: the same path arriving
            // as raw characters is inserted as literal text instead of being
            // read as an image. See the spec's evidence appendix.
            let mut payload = Vec::with_capacity(path.as_os_str().len() + 12);
            payload.extend_from_slice(b"\x1b[200~");
            payload.extend_from_slice(path.to_string_lossy().as_bytes());
            payload.extend_from_slice(b"\x1b[201~");
            let key = crate::session::key_for(project, session);
            match crate::session::write_input(&key, &payload) {
                Ok(()) => crate::http::respond(w, 200, "OK", "application/json", b"{\"ok\":true}"),
                Err(e) => {
                    let msg = format!("paste failed: {e}");
                    crate::http::respond(w, 500, "Internal Server Error", "text/plain", msg.as_bytes())
                }
            }
        }
        Err(e) => crate::http::respond(w, 400, "Bad Request", "text/plain", e.as_bytes()),
    }
}
```

`receive_image` mirrors `receive`, with one difference: it sniffs the **first chunk** to choose the extension — which keeps it streaming rather than buffering the image to inspect it — then creates the `UploadTemp` under `paste::free_name` and writes that first chunk followed by the rest. It refuses when `extension_of` returns `None`, with a message naming PNG, JPEG, GIF and WebP.

- [ ] **Step 5: Run the tests**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Prove the marker assertions can fail**

Remove the two marker `extend_from_slice` lines so only the bare path is written, run `cargo test --test integration a_pasted_image`, and confirm it fails on a *marker* assertion rather than passing because the session survived. Restore.

- [ ] **Step 7: Commit**

```bash
git add src/paste.rs src/upload.rs src/lib.rs tests/integration.rs
git commit -m "paste: POST an image and inject its path into the terminal"
```

---

### Task 6: The client

**Files:**
- Modify: `static/app.js` (a new section near the divider-drag handlers, around line 697)

**Interfaces:**
- Consumes: the two endpoints; the existing `send`, `showError`, `PROJECT` and `SESSION_RE` globals.
- Produces: nothing further.

- [ ] **Step 1: Write it**

```js
// Uploads: files dropped or pasted onto the tree, and images pasted onto a
// terminal. Delegated at document level, not bound per row: the tree is an htmx
// fragment replaced wholesale on TreeChanged, and an upload triggers exactly
// that — per-row listeners would not survive their own first success.
const MAX_UPLOAD_PARTS = 16; // must match config::MAX_UPLOAD_PARTS

// The destination for a drop: the nearest row with a data-rel. A directory row
// contributes itself, a file row its parent. Null means the drop was not on the
// tree at all, which is what keeps the destination unambiguous.
function dropDir(target) {
  const el = target && target.closest && target.closest("[data-rel]");
  if (!el) return null;
  const rel = el.dataset.rel;
  if (el.tagName === "DETAILS") return rel;
  return rel.includes("/") ? rel.slice(0, rel.lastIndexOf("/")) : "";
}

function focusedSession() {
  const host = document.activeElement && document.activeElement.closest(".termhost");
  return host ? host.dataset.session : null;
}

// File.size and FileList.length come from the OS and are readable before a byte
// is sent, so the caps can be checked at drop time. This is a courtesy, not the
// enforcement — the server applies both caps while streaming regardless.
function tooMuch(files) {
  if (files.length > MAX_UPLOAD_PARTS) {
    return `${files.length} files at once (limit ${MAX_UPLOAD_PARTS}) — use git or scp to move a project`;
  }
  return null;
}

function postFiles(url, files, label) {
  const form = new FormData();
  for (const f of files) form.append("file", f, f.name);
  const xhr = new XMLHttpRequest();
  xhr.open("POST", url);
  // XHR rather than fetch: fetch exposes no upload progress, and a 100 MB send
  // with no feedback reads as a hang.
  xhr.upload.onprogress = (e) => {
    if (e.lengthComputable) setUploadProgress(label, e.loaded / e.total);
  };
  xhr.onload = () => {
    setUploadProgress(label, null);
    if (xhr.status !== 200) return showError(`${label}: ${xhr.responseText || xhr.status}`);
    let body = {};
    try { body = JSON.parse(xhr.responseText); } catch { return; }
    for (const r of body.results || []) if (!r.ok) showError(`${r.name}: ${r.error}`);
  };
  xhr.onerror = () => { setUploadProgress(label, null); showError(`${label}: upload failed`); };
  xhr.send(form);
}

function uploadFiles(files, dir) {
  const refusal = tooMuch(files);
  if (refusal) return showError(refusal);
  const q = dir ? `?dir=${dir.split("/").map(encodeURIComponent).join("/")}` : "";
  postFiles(`/upload/${PROJECT}${q}`, files, `upload to ${dir || "project root"}`);
}

// A dropped directory arrives as a zero-length entry that fails on read, so a
// size check would send a mystery empty part and surface a confusing server
// error. webkitGetAsEntry is the reliable test; directories are a non-goal, so
// say so at the drop, by name.
function droppedDirectories(dt) {
  const dirs = [];
  for (const item of dt.items || []) {
    const entry = item.webkitGetAsEntry && item.webkitGetAsEntry();
    if (entry && entry.isDirectory) dirs.push(entry.name);
  }
  return dirs;
}

// Without preventDefault on dragover the browser navigates to the dropped file
// instead of delivering a drop event.
document.addEventListener("dragover", (e) => {
  if (dropDir(e.target) !== null) e.preventDefault();
});

document.addEventListener("drop", (e) => {
  const dir = dropDir(e.target);
  if (dir === null || !e.dataTransfer) return;
  e.preventDefault();
  const dirs = droppedDirectories(e.dataTransfer);
  if (dirs.length) {
    return showError(`folders are not uploaded (${dirs.join(", ")}) — use git or scp for a directory`);
  }
  if (e.dataTransfer.files.length) uploadFiles(e.dataTransfer.files, dir);
});

document.addEventListener("paste", (e) => {
  const files = e.clipboardData && e.clipboardData.files;
  if (!files || !files.length) return;
  const session = focusedSession();
  if (session) {
    const img = [...files].find((f) => f.type.startsWith("image/"));
    if (!img) return; // fall through to xterm's own text handling
    e.preventDefault();
    postFiles(`/paste/${PROJECT}/${session}`, [img], "paste");
    return;
  }
  const dir = dropDir(document.activeElement) ?? dropDir(e.target);
  if (dir === null) return;
  e.preventDefault();
  uploadFiles(files, dir);
});
```

`setUploadProgress(label, fraction)` renders into the existing status area — reuse whatever `showError` writes to, adding a determinate bar when `fraction` is a number and clearing on `null`.

- [ ] **Step 2: Check it in a real browser**

Not optional and not substitutable — `CLAUDE.md` records four defects a green suite could not have caught. Against the dev instance (`http://127.0.0.1:8555`, or `https://resh.<tailnet>.ts.net:8445`):

1. Drag a real file from a desktop file manager onto a **file** row; it lands in that file's directory and the tree refreshes with no reload.
2. Drag one onto a **directory** row; it lands inside.
3. Drop one **outside the tree**; nothing happens and the browser does not navigate away.
4. Drop a file whose name already exists; a visible error names it, and the original is unchanged on disk.
5. Drop a **folder**; the error names the folder rather than failing obscurely.
6. Drop 20 files; refused at the drop, instantly, without uploading.
7. Copy a file in the OS file manager, click a tree row, paste; it uploads.
8. Copy a screenshot, focus a terminal running `claude`, paste; it arrives as an image attachment, not a path.
9. Two browsers on the same project: an upload in one appears in the other's tree.

- [ ] **Step 3: Commit**

```bash
git add static/app.js
git commit -m "upload: drop and paste files from the browser"
```

---

### Task 7: The browser test, and the docs

`CLAUDE.md` says anything touching `static/app.js` should be checked in `tests/browser/`, since no Rust test can reach that file.

**Files:**
- Create: `tests/browser/upload.mjs`
- Modify: `README.md`, `CLAUDE.md`, `docs/backlog.md`

**Interfaces:**
- Consumes: `tests/browser/harness.mjs` (`fixture`, `freePort`, `openPage`, `profileDir`, `startBrowser`, `startResh`, `until`).

- [ ] **Step 1: Write the browser test**

Read `tests/browser/README.md` first — especially its four traps that make a browser test pass while asserting nothing — then follow `reconnect.mjs`'s shape:

```js
//! Do dropped and pasted files actually reach the filesystem?
//!
//! No Rust test can reach static/app.js, and the integration tests post their
//! own multipart by hand — so the client's own path (FormData, the XHR, the
//! caps checked at drop time, the folder refusal) is untested without this.
//!
//! Run: deno run -A tests/browser/upload.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until }
  from "./harness.mjs";
```

Four assertions, each pinning something the Rust tests cannot:

1. **A real upload lands.** Build a `File` in the page, call `uploadFiles([file], "")`, then `until()` the file exists on disk with the right bytes — this exercises `FormData`, the XHR, and the endpoint together.
2. **The part cap refuses before sending.** Call `uploadFiles` with 20 files and assert *no request was made* — hook `XMLHttpRequest.prototype.send` to count calls. Asserting only the error message would pass against a client that uploaded 20 files and then complained.
3. **A folder is refused by name.** Synthesise a `DataTransfer` whose item reports `isDirectory`, dispatch a drop on a tree row, assert the error names it and no request went out.
4. **The tree refreshes on its own** after an upload, via the watcher's `TreeChanged` — assert the new filename appears in the DOM without a reload.

- [ ] **Step 2: Run it**

Run: `deno run -A tests/browser/upload.mjs`
Expected: every assertion `ok`. It skips cleanly when no browser is present.

- [ ] **Step 3: Run the whole suite on the Linux host**

Per the dev/prod substitution table:

```bash
ssh <host> 'cd ~/projects/resh && cargo test && deno run -A tests/browser/upload.mjs'
```

- [ ] **Step 4: Amend the two constraints in CLAUDE.md**

The GET-only bullet is now false as written. Replace it with:

```markdown
- **HTTP is GET-only apart from `POST /upload` and `POST /paste`**, which check
  `Origin` exactly as the websocket handshakes do (a request with no `Origin`
  is refused too). Everything else is a websocket intent. These two endpoints
  are the whole CSRF surface — keep it that way.
```

And the caps line:

```markdown
- Caps: ≤16 sessions per project, ≤50 open buffers, 1 MB scrollback, 2 MB file
  cap for reads and for buffer writes; uploads are bounded per *request* —
  16 parts and `max_upload_bytes` (default 100 MB, global config only, never
  per-project).
```

- [ ] **Step 5: Update the README**

After the "All state lives on the server" paragraph:

```markdown
**Files go in through the browser.** Drag files from the desktop onto the file
tree, or copy and paste them there, and they land in that directory. Paste a
screenshot onto a terminal and it reaches the program running there as an
image, not as a path. Folders are refused on purpose — `git`, `rsync` and `scp`
are what move a project.
```

- [ ] **Step 6: Close the backlog items**

Replace the three upload/paste entries at `docs/backlog.md:24-32` with one line recording the outcome. Leave the tab-reordering entry alone — it is unrelated:

```markdown
- Drag-n-drop upload, copy-paste of file content, and pasting images into the
  claude terminal — **shipped**, see
  `docs/superpowers/specs/2026-08-19-file-upload-design.md`. Directories,
  archive extraction, download/drag-out and a host clipboard bridge are the
  recorded non-goals.
```

- [ ] **Step 7: Commit**

```bash
git add tests/browser/upload.mjs README.md CLAUDE.md docs/backlog.md
git commit -m "upload: a browser test, and the docs this makes stale"
```

---

## Self-Review

**Spec coverage.** Every section maps to a task: the POST transport and its `Origin` check are Tasks 1 and 4; the GET-only amendment is Task 4's code and Task 7's CLAUDE.md edit; the two caps are Tasks 2 and 4; streaming, confinement, the visibility rule and collision handling are Task 3; the scratch location, sniffing and injection are Task 5; the client's delegation, pre-flight checks and folder refusal are Task 6; the browser test and docs are Task 7. The spec's testing list is distributed across the tasks, each beside the code it covers.

**Three open questions remain open**, with a working default in the code and one named function to change: collisions refuse (`UploadTemp::create`), scratch files are never pruned (`paste::free_name`'s directory), and the part count is 16 (`config::MAX_UPLOAD_PARTS`).

**Type consistency.** `UploadTemp::create(&Path, &str, &str) -> Result<UploadTemp, String>` with `.write(&[u8])` and `.commit() -> Result<PathBuf, String>` is defined in Task 3 and used in Tasks 4 and 5. `config::max_upload_bytes() -> u64` and `config::MAX_UPLOAD_PARTS: usize` are defined in Task 2 and used in Task 4. `paste::extension_of(&[u8]) -> Option<&'static str>`, `paste::scratch_dir(&str) -> PathBuf` and `paste::free_name(&Path, &str) -> Result<String, String>` are defined in Task 5 and used there. `http::Request.method: String` is added in Task 1 and read in Task 1's dispatch only.

**Two things left to the implementer, both compile errors rather than silent bugs.** Task 4 assumes `projects::resolve(roots, project)` is the helper `route()` already uses to turn a project segment into a directory — if it is named differently, use the existing one rather than adding a second resolution path. And Task 5's `receive_image` is described rather than written out, because it is `receive` with the first-chunk sniff substituted; writing it twice would invite the two copies to drift.
