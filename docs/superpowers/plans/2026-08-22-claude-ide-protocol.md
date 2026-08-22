# Claude Code IDE Protocol — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make resh an IDE as far as Claude Code is concerned — Claude's edit
prompts render as a diff tab in the browser, the tree and editor can insert
`@src/hub.rs#L12-40` into Claude's prompt box, and the editor selection can be
shared as ambient context.

**Architecture:** One loopback WebSocket server per project, on an OS-assigned
ephemeral port, speaking JSON-RPC 2.0 (MCP). It advertises itself in
`~/.claude/ide/<port>.lock` with a fresh 128-bit token, and `session.rs` puts
that port in `CLAUDE_CODE_SSE_PORT` so a `claude` started in a resh terminal
connects without any path matching. The socket authenticates by token and
**rejects any handshake carrying an `Origin`** — the inverse of the workspace
socket's rule, because the client is a Bun process, not a browser. Claude's
`openDiff` never blocks resh's read loop: the request is parked in a pending
registry and answered later, when a human clicks, over the same connection's
writer channel.

**Tech Stack:** Rust (no async, no runtime, thread per connection), tungstenite
0.24, serde_json, `/dev/urandom` for tokens (no new dependency), plain JS with
no framework, Deno + Chromium for browser tests.

**Spec:** `docs/superpowers/specs/2026-08-22-claude-ide-protocol-design.md`

## Where to run this

**Implement in the primary checkout `/home/claude/projects/resh`, not in a
worktree.** This host points every cargo workspace at one shared `target-dir`,
and `build.rs` bakes *absolute* asset paths into its generated table. A `cargo
build` from a second checkout rewrites that table with the other checkout's
paths and leaves the shared binary built from the other tree — while reporting
`Fresh resh` and letting the browser tests go on passing against the wrong
source. Recover with `cargo clean -p resh` and confirm with the `grep` in
CLAUDE.md's *Verify, don't trust*.

## Global Constraints

Copied from CLAUDE.md and the spec. Every task's requirements include these.

- **Bind `127.0.0.1` only.** The IDE listener gets no "all interfaces" setting.
  JetBrains has one; resh's security boundary is the loopback bind.
- **HTTP stays GET-only** apart from the two existing POSTs. This feature adds
  **no HTTP surface**: it is a second websocket listener on its own port.
- **The IDE socket's auth rule is the inverse of the workspace socket's.**
  `origin.rs` rejects a handshake with no `Origin`; the IDE socket rejects one
  that *has* an `Origin`, and authenticates by constant-time token equality.
  Both rules are correct. Do not "reconcile" them — that is CVE-2025-52882.
- **`executeCode` is never implemented.** It is model-visible arbitrary code
  execution reachable from this socket. Answer JSON-RPC error `-32601`.
- **Every filesystem path is confined** before use: `projects::safe_resolve`.
  An absolute path from the wire is a hint, never a boundary.
- **Never hold a lock across blocking I/O.** A pending proposal is open for
  minutes. No hub lock, no registry lock, may be held across it.
- **No panics may escape a socket thread.** Every new path returns `Result`.
- **"I could not determine X" is never folded into "X is false."** This applies
  three times here: reading `/proc/<pid>/cwd`, reading the lock directory, and
  deciding whether a lock file is ours. Only the first of those has a
  destructive branch, and it must refuse rather than guess.
- **resh removes only lock files it wrote.** It never enumerates
  `~/.claude/ide/` and never deletes a file it did not create — a real
  IntelliJ's registration lives in that directory.
- **`cargo test`, never `cargo test --release`.**
- **Module-level `//!` doc; `#[cfg(test)] mod tests` at the bottom of the same
  file.** Comments give rationale, not mechanics.
- **Every new test gets the revert-the-fix check**: apply the broken version,
  run it, read the failure, restore — and record the failure mode in the test's
  own comment. A test that cannot fail is the dominant defect class here.
- **Unit tests take their directory as a parameter, never via an env var.**
  `RESH_STATE_DIR`-style env mutation is process-global; two of these tests
  running concurrently would each see the other's directory, and CLAUDE.md
  already records a "~1-in-8 flake" that was one test reaping another's state.
  Every function below has an `_in(dir, …)` form that tests call directly.

## File Structure

| File | Responsibility |
|---|---|
| `src/idelock.rs` (create) | Token generation and the lock file: write atomically, remove only our own. Knows nothing about sockets. |
| `src/idecwd.rs` (create) | `pid → cwd` with three outcomes. Knows nothing about resh. |
| `src/ide.rs` (create) | The listener, the handshake, JSON-RPC dispatch, the per-project connection registry, and the pending-proposal registry. |
| `src/lib.rs` (modify) | Declare the three new modules. |
| `src/session.rs:191-198` (modify) | Inject `CLAUDE_CODE_SSE_PORT`. |
| `src/hub.rs` (modify) | Start/stop the listener with the project; route the new intents. |
| `src/proto.rs` (modify) | `Tab::Proposal`, `Intent::MentionPath`, `Intent::AnswerProposal`, `Event::Proposal`. |
| `src/workspace.rs` (modify) | Drop `Tab::Proposal` when loading a persisted layout. |
| `static/app.js`, `static/style.css` (modify) | The proposal tab UI and the mention keybinding. |
| `tests/browser/ide.mjs` (create) | Real Chromium against a real resh with a real `claude`. |

---

### Task 1: Token and lock file

**Files:**
- Create: `src/idelock.rs`
- Modify: `src/lib.rs` (add `pub mod idelock;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub fn ide_dir() -> PathBuf`
  - `pub fn new_token() -> Result<String, String>` — 32 lowercase hex chars
  - `pub struct Lock` with `pub fn path(&self) -> &Path`, and a `Drop` that
    removes exactly the file it wrote
  - `pub fn write_in(dir: &Path, port: u16, token: &str, workspace: &Path) -> Result<Lock, String>`
  - `pub fn write(port: u16, token: &str, workspace: &Path) -> Result<Lock, String>`

- [ ] **Step 1: Write the failing tests**

Add to `src/idelock.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_thirty_two_hex_chars_and_not_a_constant() {
        let a = new_token().expect("/dev/urandom must be readable");
        let b = new_token().unwrap();
        assert_eq!(a.len(), 32, "128 bits, hex-encoded");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // Reverting to a fixed token passes every other test in this file;
        // only this assertion fails.
        assert_ne!(a, b, "two tokens must differ or the CSPRNG is not being read");
    }

    #[test]
    fn the_lock_file_carries_what_the_cli_reads_out_of_it() {
        let d = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let lock = write_in(d.path(), 5599, "cafe", ws.path()).unwrap();
        assert_eq!(lock.path().file_name().unwrap(), "5599.lock", "the CLI parses the port out of the filename");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(lock.path()).unwrap()).unwrap();
        assert_eq!(v["pid"], serde_json::json!(std::process::id()));
        assert_eq!(v["transport"], "ws");
        assert_eq!(v["authToken"], "cafe");
        assert_eq!(v["ideName"], "resh");
        assert_eq!(v["workspaceFolders"], serde_json::json!([ws.path().to_str().unwrap()]));
    }

    #[test]
    fn writing_leaves_no_temp_file_behind_and_replaces_an_existing_one() {
        let d = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("5599.lock"), "stale garbage").unwrap();
        let lock = write_in(d.path(), 5599, "cafe", ws.path()).unwrap();
        let names: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["5599.lock".to_string()], "a temp file must not survive the write");
        assert!(std::fs::read_to_string(lock.path()).unwrap().contains("cafe"));
    }

    #[test]
    fn the_temp_name_is_unique_per_process_so_two_resh_instances_cannot_collide() {
        // Two processes writing the same port is impossible (the OS assigned
        // it), but two processes writing *different* ports into one directory
        // is the normal case, and a shared temp name would let one truncate
        // the other's in-flight write.
        let d = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        assert!(temp_name(5599).contains(&std::process::id().to_string()));
        assert_ne!(temp_name(5599), temp_name(5600));
        let _ = write_in(d.path(), 5599, "cafe", ws.path()).unwrap();
    }

    #[test]
    fn dropping_removes_our_lock_and_leaves_a_strangers_alone() {
        let d = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        // A real IntelliJ's registration, in the directory we share with it.
        let foreign = d.path().join("4711.lock");
        std::fs::write(&foreign, "{}").unwrap();
        {
            let _lock = write_in(d.path(), 5599, "cafe", ws.path()).unwrap();
            assert!(d.path().join("5599.lock").exists());
        }
        assert!(!d.path().join("5599.lock").exists(), "our own lock must go on drop");
        // Reverting `Drop` to a directory sweep passes the line above and
        // fails this one. That sweep is the defect this test exists for.
        assert!(foreign.exists(), "resh must never delete a lock file it did not write");
    }

    #[test]
    fn a_missing_directory_is_created_rather_than_failing_the_project_open() {
        let d = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let nested = d.path().join("claude/ide");
        let lock = write_in(&nested, 5599, "cafe", ws.path()).unwrap();
        assert!(lock.path().exists());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test idelock`
Expected: FAIL — `could not find idelock in the crate root`.

- [ ] **Step 3: Write the implementation**

Create `src/idelock.rs`:

```rust
//! The lock file that tells Claude Code this project has an IDE socket.
//!
//! Claude Code discovers IDEs by scanning `~/.claude/ide/*.lock`; the filename
//! is the port and the contents carry the token that authenticates the
//! socket. Two properties are load-bearing and neither is obvious.
//!
//! The write is atomic because the CLI *deletes* any lock file it cannot
//! parse. A half-written file therefore does not degrade the integration, it
//! silently unregisters it — so a reader must never see a partial one.
//!
//! Removal only ever touches the path this process wrote. The directory is
//! shared with every other IDE on the host: a sweep of "stale-looking"
//! entries would unlink a live IntelliJ's registration the moment a check
//! failed, which is exactly the class of defect CLAUDE.md's table is about.
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};

/// `$CLAUDE_CONFIG_DIR/ide` when set — the CLI honours the same override, so
/// a user who relocated their Claude config still finds us.
pub fn ide_dir() -> PathBuf {
    if let Ok(d) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d).join("ide");
        }
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude/ide")
}

/// 128 bits from the OS CSPRNG, hex-encoded — the same shape the CLI's own
/// extensions use. `/dev/urandom` rather than a dependency: this is the only
/// randomness resh needs, and the deploy target is Linux.
pub fn new_token() -> Result<String, String> {
    let mut f = std::fs::File::open("/dev/urandom").map_err(|e| format!("no CSPRNG: {e}"))?;
    let mut buf = [0u8; 16];
    f.read_exact(&mut buf).map_err(|e| format!("short read from /dev/urandom: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn temp_name(port: u16) -> String {
    // Pid-unique: two resh instances share this directory, and a shared temp
    // name lets one truncate the other's in-flight write.
    format!(".{}.{}.resh.tmp", port, std::process::id())
}

pub struct Lock {
    path: PathBuf,
}

impl Lock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Exactly this path. Never a scan of the directory.
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn write_in(dir: &Path, port: u16, token: &str, workspace: &Path) -> Result<Lock, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let body = serde_json::json!({
        "pid": std::process::id(),
        "workspaceFolders": [workspace.to_string_lossy()],
        "ideName": "resh",
        "transport": "ws",
        "authToken": token,
    })
    .to_string();
    let tmp = dir.join(temp_name(port));
    let path = dir.join(format!("{port}.lock"));
    let mut f = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })?;
    Ok(Lock { path })
}

pub fn write(port: u16, token: &str, workspace: &Path) -> Result<Lock, String> {
    write_in(&ide_dir(), port, token, workspace)
}
```

Add `pub mod idelock;` to `src/lib.rs` beside the other module declarations.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test idelock`
Expected: 6 passed.

- [ ] **Step 5: Do the revert-the-fix check on the two tests that matter**

Apply each break, run, read the failure, restore:

1. In `new_token`, return `Ok("00".repeat(16))`. Expected: only
   `a_token_is_thirty_two_hex_chars_and_not_a_constant` fails, on `assert_ne!`.
2. In `Drop`, replace the `remove_file` with a loop that removes every
   `*.lock` in the parent directory. Expected:
   `dropping_removes_our_lock_and_leaves_a_strangers_alone` fails on the
   `foreign.exists()` assertion, and nothing else does.

Record both failure modes in the tests' own comments (they are already written
above; confirm the text matches what you actually saw).

- [ ] **Step 6: Commit**

```bash
git add src/idelock.rs src/lib.rs
git commit -m "ide: a lock file a reader can only ever see whole"
```

---

### Task 2: Which directory Claude is in

**Files:**
- Create: `src/idecwd.rs`
- Modify: `src/lib.rs` (add `pub mod idecwd;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum Cwd { At(PathBuf), Gone, Unknown }`
  - `pub fn cwd_of_in(proc_root: &Path, pid: u32) -> Cwd`
  - `pub fn cwd_of(pid: u32) -> Cwd`

The protocol hands over a pid and nothing else — `ide_connected` carries
`{pid}`, and MCP's `initialize` carries a name and version. The directory has
to come from the kernel.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_own_pid_resolves_to_our_own_directory() {
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        match cwd_of(std::process::id()) {
            Cwd::At(p) => assert_eq!(p.canonicalize().unwrap(), here),
            other => panic!("expected At, got {other:?}"),
        }
    }

    #[test]
    fn a_pid_that_cannot_exist_is_gone_not_unknown() {
        // The distinction is the whole point: Gone drops the connection,
        // Unknown must not.
        assert!(matches!(cwd_of(u32::MAX), Cwd::Gone));
    }

    #[test]
    fn an_unreadable_proc_is_unknown_not_gone() {
        // /proc/<pid> exists but its cwd entry cannot be read. Folding this
        // into Gone is how a live Claude gets disconnected because a check
        // failed. Reverting the Unknown branch to Gone fails only this test.
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("1234")).unwrap();
        assert!(matches!(cwd_of_in(d.path(), 1234), Cwd::Unknown));
    }

    #[test]
    fn a_missing_proc_entry_is_gone() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(cwd_of_in(d.path(), 1234), Cwd::Gone));
    }

    #[test]
    fn a_missing_proc_filesystem_is_unknown() {
        // Not Linux, or a container without /proc. resh cannot tell, so it
        // must not claim the process is gone.
        let d = tempfile::tempdir().unwrap();
        let absent = d.path().join("no-proc-here");
        assert!(matches!(cwd_of_in(&absent, 1234), Cwd::Unknown));
    }

    #[test]
    fn a_dangling_cwd_symlink_still_reports_the_path() {
        // A process whose directory was deleted under it. readlink answers
        // regardless of whether the target exists, and that answer is the
        // truth about the process — resolving it would turn a valid answer
        // into a wrong one.
        let d = tempfile::tempdir().unwrap();
        let pdir = d.path().join("1234");
        std::fs::create_dir(&pdir).unwrap();
        std::os::unix::fs::symlink("/tmp/deleted-under-it", pdir.join("cwd")).unwrap();
        match cwd_of_in(d.path(), 1234) {
            Cwd::At(p) => assert_eq!(p, PathBuf::from("/tmp/deleted-under-it")),
            other => panic!("expected At, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test idecwd`
Expected: FAIL — `could not find idecwd in the crate root`.

- [ ] **Step 3: Write the implementation**

Create `src/idecwd.rs`:

```rust
//! Claude's working directory, from its pid.
//!
//! The IDE protocol never sends a path. On connect the CLI sends exactly
//! `ide_connected {pid}`, and MCP's `initialize` adds a client name and
//! version — nothing that says where the process is. So resh asks the kernel.
//!
//! This matters for worktrees, which is the case that makes the question
//! worth asking at all: `worktree.rs` records that the dominant worktree
//! location is `{repo}/.claude/worktrees/{name}`, a directory Claude Code
//! creates for itself. resh knows the directory it *spawned* a shell in
//! (`session.rs`), but that is where the session started, not where Claude is
//! now. Every absolute path in an `openDiff` is relative to the latter.
//!
//! Three outcomes, not two. "I could not read /proc/<pid>/cwd" is not "the
//! process is gone", and only the second may drop a connection.
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum Cwd {
    At(PathBuf),
    /// Positive evidence the process no longer exists.
    Gone,
    /// resh cannot tell. Never a reason to destroy anything.
    Unknown,
}

pub fn cwd_of_in(proc_root: &Path, pid: u32) -> Cwd {
    // read_link, not canonicalize: a process whose directory was deleted
    // under it still has a truthful cwd, and resolving would discard it.
    let pdir = proc_root.join(pid.to_string());
    match std::fs::read_link(pdir.join("cwd")) {
        Ok(p) => Cwd::At(p),
        Err(_) => {
            // The link is unreadable. Which of the two reasons applies is
            // decided by what is definitely present, never by the same
            // failure that just happened.
            match std::fs::symlink_metadata(&pdir) {
                Ok(_) => Cwd::Unknown, // the process is there; we just cannot look
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Distinguish "no such process" from "no /proc at all".
                    match std::fs::symlink_metadata(proc_root) {
                        Ok(_) => Cwd::Gone,
                        Err(_) => Cwd::Unknown,
                    }
                }
                Err(_) => Cwd::Unknown,
            }
        }
    }
}

pub fn cwd_of(pid: u32) -> Cwd {
    cwd_of_in(Path::new("/proc"), pid)
}
```

Add `pub mod idecwd;` to `src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test idecwd`
Expected: 6 passed.

- [ ] **Step 5: Do the revert-the-fix check**

Replace the `Ok(_) => Cwd::Unknown` arm with `Ok(_) => Cwd::Gone`. Run
`cargo test idecwd`. Expected: only `an_unreadable_proc_is_unknown_not_gone`
fails. Restore.

- [ ] **Step 6: Commit**

```bash
git add src/idecwd.rs src/lib.rs
git commit -m "ide: a pid resh cannot read is not a pid that is gone"
```

---

### Task 3: The listener, the handshake, and the inverted Origin rule

**Files:**
- Create: `src/ide.rs`
- Modify: `src/lib.rs` (add `pub mod ide;`)

This task ships the entire security surface and nothing else. It is reviewed on
its own for that reason.

**Interfaces:**
- Consumes: `idelock::{new_token, write, Lock}`.
- Produces:
  - `pub struct Ide { pub port: u16, pub token: String }`
  - `pub fn start_in(dir: &Path, project: &str, workspace: PathBuf) -> Result<Arc<Ide>, String>`
  - `pub fn start(project: &str, workspace: PathBuf) -> Result<Arc<Ide>, String>`
  - `pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tungstenite::client::IntoClientRequest;

    /// Verified against tungstenite 0.24: a built `http::Request` keeps its
    /// custom headers through `into_client_request`, and the handshake key,
    /// version and Host are added by the client afterwards.
    fn connect(port: u16, token: Option<&str>, origin: Option<&str>) -> Result<(), String> {
        let mut b = tungstenite::http::Request::builder()
            .uri(format!("ws://127.0.0.1:{port}/"))
            .header("Sec-WebSocket-Protocol", "mcp");
        if let Some(t) = token {
            b = b.header("X-Claude-Code-Ide-Authorization", t);
        }
        if let Some(o) = origin {
            b = b.header("Origin", o);
        }
        let req = b.body(()).unwrap().into_client_request().unwrap();
        tungstenite::connect(req).map(|_| ()).map_err(|e| e.to_string())
    }

    fn started() -> (tempfile::TempDir, tempfile::TempDir, Arc<Ide>) {
        let lockdir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let ide = start_in(lockdir.path(), "proj", ws.path().to_path_buf()).unwrap();
        (lockdir, ws, ide)
    }

    #[test]
    fn the_right_token_and_no_origin_connects() {
        let (_l, _w, ide) = started();
        connect(ide.port, Some(&ide.token), None).expect("the CLI's own shape must be accepted");
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let (_l, _w, ide) = started();
        let err = connect(ide.port, Some(&"0".repeat(32)), None)
            .expect_err("a guessed token must not connect");
        assert!(err.contains("403"), "expected an HTTP 403, got: {err}");
    }

    #[test]
    fn a_missing_token_is_refused() {
        let (_l, _w, ide) = started();
        let err = connect(ide.port, None, None).expect_err("no token, no socket");
        assert!(err.contains("403"), "expected an HTTP 403, got: {err}");
    }

    #[test]
    fn a_handshake_carrying_an_origin_is_refused_even_with_the_right_token() {
        // This is CVE-2025-52882 in one assertion. A browser is the only
        // thing that sends Origin, WebSocket handshakes bypass the
        // same-origin policy, and this socket can read files. A page that
        // somehow learned the token still must not get in.
        //
        // Note this is the *inverse* of origin.rs's rule for the workspace
        // socket, which refuses a handshake with no Origin. Both are right.
        let (_l, _w, ide) = started();
        let err = connect(ide.port, Some(&ide.token), Some("https://evil.example"))
            .expect_err("a browser must never reach this socket");
        assert!(err.contains("403"), "expected an HTTP 403, got: {err}");
    }

    #[test]
    fn a_loopback_origin_is_refused_too() {
        // The workspace socket allows loopback origins. This one does not:
        // resh's own page has no business here either, and allowing it would
        // reopen the hole for anything that can forge an origin.
        let (_l, _w, ide) = started();
        assert!(connect(ide.port, Some(&ide.token), Some("http://127.0.0.1:8444")).is_err());
    }

    #[test]
    fn starting_advertises_the_port_it_actually_bound() {
        let (lockdir, ws, ide) = started();
        let f = lockdir.path().join(format!("{}.lock", ide.port));
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap();
        assert_eq!(v["authToken"], ide.token.as_str());
        assert_eq!(v["workspaceFolders"], serde_json::json!([ws.path().to_str().unwrap()]));
        assert_ne!(ide.port, 0, "an OS-assigned port must be read back after bind");
    }

    #[test]
    fn constant_time_compare_answers_correctly_including_on_length() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(!ct_eq(b"", b"a"));
        assert!(ct_eq(b"", b""));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test ide::`
Expected: FAIL — `could not find ide in the crate root`.

- [ ] **Step 3: Write the implementation**

Create `src/ide.rs`:

```rust
//! The socket Claude Code connects to, and the reason its rules differ from
//! every other socket in this codebase.
//!
//! resh is the *server* here. The extension model has the IDE listening and
//! `claude` connecting out to it, which is what lets the integration work for
//! a Claude attached to a dtach session resh did not spawn.
//!
//! The client is a Bun process, not a browser, so it sends no `Origin` — it
//! sends a token from the lock file. `origin.rs` refuses a handshake with no
//! Origin because "every browser sends one, so its absence means a non-browser
//! client, which has no business here." On this socket that reasoning runs
//! backwards: a browser is the only thing that sends one, and a browser has no
//! business here. Both sockets are right; the rules are opposites.
//!
//! That is not a stylistic point. Claude Code's own extensions shipped this
//! socket unauthenticated and Origin-blind through version 1.0.23, and because
//! WebSocket handshakes bypass the same-origin policy, any web page could scan
//! localhost, connect, and read files — CVE-2025-52882, fixed in 1.0.24 by the
//! lock-file token this module implements.
use crate::idelock;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tungstenite::handshake::server::{Request as WsRequest, Response as WsResponse};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::accept_hdr_with_config;

/// An `openDiff` carries a whole file, capped elsewhere at 2 MB; this is the
/// coarse backstop against an oversized frame being buffered at all.
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

pub struct Ide {
    pub port: u16,
    pub token: String,
    /// Removed on drop, and only ever the path we wrote.
    _lock: idelock::Lock,
}

/// Length is not secret — the token is a fixed 32 hex chars — but the bytes
/// are, so the comparison must not stop at the first difference.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn start_in(dir: &Path, project: &str, workspace: PathBuf) -> Result<Arc<Ide>, String> {
    // Port 0: the OS picks, and the lock file must advertise what was actually
    // bound, not what was asked for.
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let token = idelock::new_token()?;
    let lock = idelock::write_in(dir, port, &token, &workspace)?;
    let ide = Arc::new(Ide { port, token: token.clone(), _lock: lock });
    let project = project.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let token = token.clone();
            let project = project.clone();
            let workspace = workspace.clone();
            std::thread::spawn(move || {
                // A panic here must not take the process down with it: this
                // thread is fed attacker-influenced bytes.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    serve_conn(stream, &token, &project, &workspace);
                }));
            });
        }
    });
    Ok(ide)
}

pub fn start(project: &str, workspace: PathBuf) -> Result<Arc<Ide>, String> {
    start_in(&idelock::ide_dir(), project, workspace)
}

fn serve_conn(stream: TcpStream, token: &str, _project: &str, _workspace: &Path) {
    let config = WebSocketConfig { max_message_size: Some(MAX_FRAME_BYTES), ..Default::default() };
    let accepted = accept_hdr_with_config(
        stream,
        |req: &WsRequest, mut resp: WsResponse| {
            let deny = |why: &str| {
                eprintln!("resh: rejected ide ws handshake ({why})");
                tungstenite::http::Response::builder()
                    .status(403)
                    .body(Some("forbidden".to_string()))
                    .expect("static 403 response")
            };
            // See the module doc: on this socket an Origin is disqualifying.
            if req.headers().get("origin").is_some() {
                return Err(deny("carries an Origin, so it is a browser"));
            }
            let got = req
                .headers()
                .get("x-claude-code-ide-authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !ct_eq(got.as_bytes(), token.as_bytes()) {
                return Err(deny("token mismatch"));
            }
            // The CLI asks for the `mcp` subprotocol; a server that does not
            // echo it back is not guaranteed to be accepted by the client.
            resp.headers_mut().insert(
                "sec-websocket-protocol",
                tungstenite::http::HeaderValue::from_static("mcp"),
            );
            Ok(resp)
        },
        Some(config),
    );
    let Ok(mut ws) = accepted else { return };
    // Task 4 replaces this with the JSON-RPC loop.
    while ws.read().is_ok() {}
}
```

Add `pub mod ide;` to `src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test ide::`
Expected: 7 passed.

- [ ] **Step 5: Do the revert-the-fix check on the inversion**

Delete the `if req.headers().get("origin").is_some()` block. Run
`cargo test ide::`. Expected: only
`a_handshake_carrying_an_origin_is_refused_even_with_the_right_token` and
`a_loopback_origin_is_refused_too` fail. Restore.

Then delete the `ct_eq` token check. Expected: `a_wrong_token_is_refused` and
`a_missing_token_is_refused` fail. Restore. Confirm you saw four distinct
failures across the two breaks — if a break produces no failure, the test is
not reaching the code.

- [ ] **Step 6: Commit**

```bash
git add src/ide.rs src/lib.rs
git commit -m "ide: the socket a browser must never reach"
```

---

### Task 4: JSON-RPC, `ide_connected`, and the tool list

**Files:**
- Modify: `src/ide.rs` (replace the `while ws.read()` stub with a dispatch loop)

**Interfaces:**
- Consumes: `idecwd::{cwd_of, Cwd}`, `ide::start_in` from Task 3.
- Produces:
  - `fn dispatch(msg: &serde_json::Value, conn: &mut Conn) -> Option<serde_json::Value>`
  - `pub struct Conn { pub cwd: Option<PathBuf>, pub workspace: PathBuf }`

End state: `claude` runs `/ide`, says `Connected to resh.`, and
`mcp__ide__getDiagnostics` appears in `/mcp` while `executeCode` does not.

- [ ] **Step 1: Write the failing tests**

```rust
    fn rpc(id: i64, method: &str, params: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    #[test]
    fn initialize_answers_with_resh_as_the_server_name() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx);
        let out = dispatch(&rpc(1, "initialize", serde_json::json!({})), &mut c).unwrap();
        assert_eq!(out["id"], 1);
        assert_eq!(out["result"]["serverInfo"]["name"], "resh");
        assert!(out["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn the_tool_list_offers_diagnostics_and_never_offers_code_execution() {
        // executeCode is one of only two tools the CLI makes visible to the
        // model, and it is arbitrary code execution reachable from this
        // socket. Adding it to the list is the defect this asserts against.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx);
        let out = dispatch(&rpc(2, "tools/list", serde_json::json!({})), &mut c).unwrap();
        let names: Vec<String> = out["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["getDiagnostics".to_string()]);
    }

    #[test]
    fn calling_execute_code_is_a_method_error_not_an_empty_success() {
        // An empty success would read to Claude as "ran, produced nothing".
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx);
        let out = dispatch(
            &rpc(3, "tools/call", serde_json::json!({"name": "executeCode", "arguments": {"code": "1"}})),
            &mut c,
        )
        .unwrap();
        assert_eq!(out["error"]["code"], -32601);
        assert!(
            out["error"]["message"].as_str().unwrap().contains("executeCode"),
            "the refusal must name what was refused: {}", out["error"]["message"]
        );
        assert!(out.get("result").is_none());
    }

    #[test]
    fn diagnostics_answers_empty_rather_than_failing() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx);
        let out = dispatch(
            &rpc(4, "tools/call", serde_json::json!({"name": "getDiagnostics", "arguments": {}})),
            &mut c,
        )
        .unwrap();
        assert_eq!(out["result"]["content"][0]["type"], "text");
        assert_eq!(out["result"]["content"][0]["text"], "[]");
    }

    #[test]
    fn ide_connected_resolves_the_senders_directory_and_is_not_answered() {
        // A notification has no id, so a reply would be a protocol error.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", std::env::current_dir().unwrap(), tx);
        let note = serde_json::json!({
            "jsonrpc": "2.0", "method": "ide_connected",
            "params": {"pid": std::process::id()}
        });
        assert!(dispatch(&note, &mut c).is_none(), "notifications get no response");
        assert_eq!(
            c.cwd.as_ref().unwrap().canonicalize().unwrap(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
    }

    #[test]
    fn ide_connected_from_an_unreadable_pid_leaves_the_connection_usable() {
        // Cwd::Unknown must not disconnect. Folding it into "gone" would kill
        // a live Claude because a check failed.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx);
        let note = serde_json::json!({
            "jsonrpc": "2.0", "method": "ide_connected", "params": {"pid": u32::MAX}
        });
        assert!(dispatch(&note, &mut c).is_none());
        assert!(c.cwd.is_none(), "no directory was learned");
        assert!(!c.closed, "but the connection stays open");
    }

    #[test]
    fn an_unknown_method_is_a_method_error_carrying_the_request_id() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Conn::new("t", PathBuf::from("/tmp"), tx);
        let out = dispatch(&rpc(9, "nonsense/method", serde_json::json!({})), &mut c).unwrap();
        assert_eq!(out["id"], 9);
        assert_eq!(out["error"]["code"], -32601);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test ide::`
Expected: FAIL — `cannot find function dispatch`, `cannot find type Conn`.

- [ ] **Step 3: Write the implementation**

Add to `src/ide.rs`, above the test module:

```rust
use crate::idecwd::{self, Cwd};

pub struct Conn {
    /// Claude's working directory, learned from `ide_connected`'s pid. `None`
    /// until it connects, or when resh could not read it — those are different
    /// situations with the same representation here only because both mean
    /// "do not trust a path against it yet".
    pub cwd: Option<PathBuf>,
    pub workspace: PathBuf,
    pub project: String,
    /// This connection's writer channel. Unused until Task 6 gives the
    /// connection a writer thread, and carried from the start because the
    /// connection owns its identity and its output from the moment it exists.
    pub reply: std::sync::mpsc::Sender<String>,
    pub closed: bool,
}

impl Conn {
    pub fn new(project: &str, workspace: PathBuf, reply: std::sync::mpsc::Sender<String>) -> Self {
        Conn { cwd: None, workspace, project: project.to_string(), reply, closed: false }
    }
}

fn err(id: &serde_json::Value, code: i64, message: String) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn ok(id: &serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn text_result(s: &str) -> serde_json::Value {
    serde_json::json!({"content": [{"type": "text", "text": s}]})
}

fn dispatch(msg: &serde_json::Value, conn: &mut Conn) -> Option<serde_json::Value> {
    let method = msg["method"].as_str().unwrap_or("");
    let id = msg.get("id").cloned();

    // A message with no id is a notification: answering one is a protocol
    // error, not a harmless extra.
    let Some(id) = id else {
        if method == "ide_connected" {
            let pid = msg["params"]["pid"].as_u64().unwrap_or(0) as u32;
            match idecwd::cwd_of(pid) {
                Cwd::At(p) => conn.cwd = Some(p),
                // Gone and Unknown both leave cwd unset, and neither closes
                // the connection here: the socket itself is the evidence that
                // something is on the other end, and it is more trustworthy
                // than a /proc lookup that just failed.
                Cwd::Gone | Cwd::Unknown => {}
            }
        }
        return None;
    };

    match method {
        "initialize" => Some(ok(
            &id,
            serde_json::json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "resh", "version": env!("CARGO_PKG_VERSION")},
            }),
        )),
        "tools/list" => Some(ok(
            &id,
            serde_json::json!({"tools": [{
                "name": "getDiagnostics",
                "description": "Get language diagnostics from the editor",
                "inputSchema": {
                    "type": "object",
                    "properties": {"uri": {"type": "string"}},
                },
            }]}),
        )),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            match name {
                // resh has no language server. An empty list is the honest
                // answer and is what Claude sees when nothing is wrong — so
                // if a `cargo check` bridge ever lands, it lands here.
                "getDiagnostics" => Some(ok(&id, text_result("[]"))),
                other => Some(err(&id, -32601, format!("resh does not implement {other}"))),
            }
        }
        "ping" => Some(ok(&id, serde_json::json!({}))),
        other => Some(err(&id, -32601, format!("unknown method {other}"))),
    }
}
```

Replace the stub loop in `serve_conn` with:

```rust
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let mut conn = Conn::new(project, workspace.to_path_buf(), reply_tx);
    let _ = reply_rx; // drained by the writer thread from Task 6 onward
    loop {
        let Ok(msg) = ws.read() else { break };
        let text = match msg {
            tungstenite::Message::Text(t) => t,
            tungstenite::Message::Close(_) => break,
            _ => continue,
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        if let Some(reply) = dispatch(&v, &mut conn) {
            if ws.send(tungstenite::Message::Text(reply.to_string())).is_err() {
                break;
            }
        }
        if conn.closed {
            break;
        }
    }
```

Rename `_project`/`_workspace` to `project`/`workspace` where now used.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test ide::`
Expected: 14 passed (7 from Task 3, 7 here).

- [ ] **Step 5: Do the revert-the-fix check**

Add `"executeCode"` to the `tools/list` array. Run. Expected: only
`the_tool_list_offers_diagnostics_and_never_offers_code_execution` fails.
Restore.

- [ ] **Step 6: Verify against a real `claude`, not a mock**

This is the substitution trap in this feature: every test above sends what
*this plan* says the CLI sends, and the plan was derived from minified code.

```bash
cargo build
# In one terminal, from a project directory:
CLAUDE_CODE_SSE_PORT=<port from the lock file> claude
# then, inside claude:
/ide
```

Expected: `Connected to resh.` Then `/mcp` lists an `ide` server, and
`mcp__ide__getDiagnostics` is callable while `executeCode` is absent.

Record the actual output in the commit message. If it does not connect, the
mismatch is real and the tests above are wrong — fix the tests, not just the
code.

- [ ] **Step 7: Commit**

```bash
git add src/ide.rs
git commit -m "ide: answer the handshake Claude actually sends"
```

---

### Task 5: Lock file lifecycle and `CLAUDE_CODE_SSE_PORT`

**Files:**
- Modify: `src/hub.rs:134-` (`for_project`), and the `CloseProject` path
- Modify: `src/session.rs:191-198`
- Modify: `src/ide.rs` (add the per-project registry)

**Interfaces:**
- Consumes: `ide::start`.
- Produces:
  - `pub fn for_project(project: &str, workspace: PathBuf) -> Option<Arc<Ide>>` in `ide.rs`

**A trap this task must avoid:** the registry is process-global, so two tests
that both use the project name `"alpha"` will hand each other a listener and
pass or fail depending on order — the exact shape of the flake CLAUDE.md
records. **Give every test in this task a project name unique to it**
(`"reuse-alpha"`, `"twoports-alpha"`, `"stop-alpha"`), and do not reach for a
shared teardown.
  - `pub fn port_for(project: &str) -> Option<u16>`
  - `pub fn stop(project: &str)`

- [ ] **Step 1: Write the failing tests**

```rust
    /// Each test gets its own lock directory, so tests that run concurrently
    /// cannot see each other's lock files. Deliberately not an env var:
    /// `CLAUDE_CONFIG_DIR` is process-global, and CLAUDE.md records a
    /// "~1-in-8 flake" that was one test reaping another's state.
    fn dirs() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
    }

    #[test]
    fn one_listener_per_project_not_one_per_connection() {
        let (dir, ws, _) = dirs();
        let a = for_project_in(dir.path(), "alpha", ws.path().to_path_buf()).unwrap();
        let again = for_project_in(dir.path(), "alpha", ws.path().to_path_buf()).unwrap();
        assert_eq!(a.port, again.port, "a second call must reuse the listener");
    }

    #[test]
    fn two_projects_get_two_ports_and_two_lock_files() {
        let (dir, wa, wb) = dirs();
        // One listener listing both roots would let a claude in beta be
        // handed alpha's socket: the CLI takes the first workspaceFolder that
        // contains its cwd.
        let a = for_project_in(dir.path(), "alpha", wa.path().to_path_buf()).unwrap();
        let b = for_project_in(dir.path(), "beta", wb.path().to_path_buf()).unwrap();
        assert_ne!(a.port, b.port);
        assert!(dir.path().join(format!("{}.lock", a.port)).exists());
        assert!(dir.path().join(format!("{}.lock", b.port)).exists());
    }

    #[test]
    fn stopping_a_project_removes_its_lock_and_leaves_the_others() {
        let (dir, wa, wb) = dirs();
        let a = for_project_in(dir.path(), "alpha", wa.path().to_path_buf()).unwrap();
        let b = for_project_in(dir.path(), "beta", wb.path().to_path_buf()).unwrap();
        stop("alpha");
        assert!(!dir.path().join(format!("{}.lock", a.port)).exists());
        assert!(dir.path().join(format!("{}.lock", b.port)).exists(), "closing one project must not unregister another");
    }

    #[test]
    fn a_failure_to_write_the_lock_file_does_not_fail_the_project() {
        let (dir, ws, _) = dirs();
        // IDE integration is a convenience. A read-only home directory must
        // degrade it, never stop a project opening.
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, "").unwrap();
        assert!(for_project_in(&blocked, "alpha", ws.path().to_path_buf()).is_none());
    }
```

And in `src/session.rs`'s test module:

```rust
    #[test]
    fn a_spawned_shell_is_told_which_port_to_connect_to() {
        // Without this a claude in a resh terminal has to path-match, which
        // is exactly the comparison that goes wrong for worktrees.
        // Reverting the cb.env line leaves this the only failing test.
        let env = session_env("alpha", "main", Some(5599));
        assert_eq!(env.get("CLAUDE_CODE_SSE_PORT").map(String::as_str), Some("5599"));
        assert_eq!(env.get("RESH_PROJECT").map(String::as_str), Some("alpha"));
    }

    #[test]
    fn a_project_without_a_listener_gets_no_port_variable() {
        let env = session_env("alpha", "main", None);
        assert!(!env.contains_key("CLAUDE_CODE_SSE_PORT"), "an empty value would be worse than absence");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test ide:: session::`
Expected: FAIL — `cannot find function for_project_in`, `cannot find function session_env`.

- [ ] **Step 3: Write the implementation**

In `src/ide.rs`:

```rust
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<Ide>>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, Arc<Ide>>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn for_project_in(dir: &Path, project: &str, workspace: PathBuf) -> Option<Arc<Ide>> {
    let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = map.get(project) {
        return Some(existing.clone());
    }
    match start_in(dir, project, workspace) {
        Ok(ide) => {
            map.insert(project.to_string(), ide.clone());
            Some(ide)
        }
        Err(e) => {
            // Degraded, never fatal: a project must still open.
            eprintln!("resh: IDE integration unavailable for {project}: {e}");
            None
        }
    }
}

pub fn for_project(project: &str, workspace: PathBuf) -> Option<Arc<Ide>> {
    for_project_in(&idelock::ide_dir(), project, workspace)
}

pub fn port_for(project: &str) -> Option<u16> {
    let map = registry().lock().unwrap_or_else(|e| e.into_inner());
    map.get(project).map(|i| i.port)
}

/// Dropping the `Arc` drops the `Lock`, which removes exactly the file it
/// wrote. Nothing here scans the directory.
pub fn stop(project: &str) {
    let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
    map.remove(project);
}
```

In `src/session.rs`, factor the environment out so it is testable without
spawning a PTY, then use it:

```rust
/// The environment a resh shell is spawned with. Split out from the spawn so
/// it can be asserted on without a PTY.
fn session_env(project: &str, name: &str, ide_port: Option<u16>) -> std::collections::HashMap<String, String> {
    let mut env = std::collections::HashMap::new();
    env.insert("TERM".into(), "xterm-256color".into());
    env.insert("RESH_NOTIFY".into(), "1".into());
    env.insert("RESH_PROJECT".into(), project.to_string());
    env.insert("RESH_SESSION".into(), name.to_string());
    // Claude Code matches a lock file by port before it tries to match by
    // path, so this makes a claude started here connect without any path
    // comparison at all — which sidesteps every worktree, symlink and
    // canonicalisation question in one line.
    if let Some(p) = ide_port {
        env.insert("CLAUDE_CODE_SSE_PORT".into(), p.to_string());
    }
    env
}
```

and replace the four `cb.env(...)` calls at `src/session.rs:191-198` with:

```rust
        for (k, v) in session_env(project, name, crate::ide::port_for(project)) {
            cb.env(k, v);
        }
```

In `src/hub.rs`, inside `for_project`'s `or_insert_with`, after the hub is
built, start the listener:

```rust
                // Started here rather than on first connection: the lock file
                // must exist before a terminal is spawned, since the spawn
                // reads the port out of the registry.
                crate::ide::for_project(project, dir.clone());
```

and in `do_close_project`, alongside the session teardown:

```rust
        crate::ide::stop(&self.project);
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test ide:: session::`
Expected: all pass.

- [ ] **Step 5: Do the revert-the-fix check**

Remove the `if let Some(p) = ide_port` block. Run `cargo test session::`.
Expected: only `a_spawned_shell_is_told_which_port_to_connect_to` fails.
Restore.

Then make `stop` clear the whole map instead of removing one key. Expected:
`stopping_a_project_removes_its_lock_and_leaves_the_others` fails on the
second assertion. Restore.

- [ ] **Step 6: Verify against a real `claude` with no `/ide`**

```bash
cargo build
# start resh, open a project, open a terminal tab, and run:
claude
```

Expected: it connects on its own, with no `/ide`, because the port was in the
environment. Confirm with `/mcp` showing the `ide` server. Note in the commit
whether you saw it auto-connect — that is the whole point of this task.

- [ ] **Step 7: Commit**

```bash
git add src/ide.rs src/hub.rs src/session.rs
git commit -m "ide: a shell resh spawns already knows where to connect"
```

---

### Task 6: `at_mentioned`

**Files:**
- Modify: `src/ide.rs` (connection registry + outbound notification)
- Modify: `src/proto.rs` (`Intent::MentionPath`)
- Modify: `src/hub.rs` (route it)

This is `docs/backlog.md:20`, the first item under "First things to do", done
without pasting into a terminal.

**Interfaces:**
- Consumes: the per-project registry from Task 5.
- Produces:
  - `pub fn mention(project: &str, abs: &Path, lines: Option<(u32, u32)>) -> Result<(), String>`
  - `Intent::MentionPath { rel: String, line_start: Option<u32>, line_end: Option<u32> }`

- [ ] **Step 1: Write the test harness the next two tasks also use**

A fake Claude: a real WebSocket client on a real socket, whose received frames
land in a channel the test can read. Real rather than a stub, because the
handshake and the framing are part of what is being tested.

```rust
    /// Test-only: the send half of each fake client's socket, so a test can
    /// speak as Claude without owning the socket itself.
    static SENDERS: OnceLock<Mutex<HashMap<String, std::sync::mpsc::Sender<String>>>> =
        OnceLock::new();

    fn senders() -> &'static Mutex<HashMap<String, std::sync::mpsc::Sender<String>>> {
        SENDERS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Connects a client the way the CLI does and returns (frames it
    /// receives, the live Ide). Holding the returned `Arc<Ide>` matters: it
    /// owns the lock file, and dropping it unregisters the project.
    fn connected_fake_client_for(project: &str) -> (std::sync::mpsc::Receiver<String>, Arc<Ide>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let ide = for_project_in(dir.path(), project, ws.path().to_path_buf()).unwrap();
        let req = tungstenite::http::Request::builder()
            .uri(format!("ws://127.0.0.1:{}/", ide.port))
            .header("Sec-WebSocket-Protocol", "mcp")
            .header("X-Claude-Code-Ide-Authorization", &ide.token)
            .body(())
            .unwrap()
            .into_client_request()
            .unwrap();
        let (mut sock, _) = tungstenite::connect(req).expect("the fake client must connect");
        let (tx, rx) = std::sync::mpsc::channel();
        // The test drives sends through this channel too, so one thread owns
        // the socket and the test never races itself.
        let (send_tx, send_rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || loop {
            while let Ok(out) = send_rx.try_recv() {
                if sock.send(tungstenite::Message::Text(out)).is_err() {
                    return;
                }
            }
            match sock.read() {
                Ok(tungstenite::Message::Text(t)) => {
                    if tx.send(t).is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(_) => return,
            }
        });
        senders().lock().unwrap().insert(project.to_string(), send_tx);
        (rx, ide, dir)
    }
```

Two callers-side helpers, used by Task 7:

```rust
    /// Sends a raw JSON-RPC message as Claude would.
    fn send_from_claude(project: &str, msg: &serde_json::Value) {
        senders().lock().unwrap()[project].send(msg.to_string()).unwrap();
    }

    /// Sends an `openDiff` and returns the pending id resh assigned, read off
    /// the `Event::Proposal` that reaches the project's browsers.
    fn open_diff_from_claude(project: &str, path: &str, new_text: &str, tab: &str) -> String {
        send_from_claude(project, &rpc(100, "tools/call", serde_json::json!({
            "name": "openDiff",
            "arguments": {
                "old_file_path": path, "new_file_path": path,
                "new_file_contents": new_text, "tab_name": tab,
            }
        })));
        pending_id_for_tab(project, tab).expect("openDiff must register a pending proposal")
    }
```

`pending_id_for_tab` is a `#[cfg(test)]` accessor on the pending registry added
in Task 7. Until then, the Task 6 tests use only `connected_fake_client_for`.

- [ ] **Step 2: Write the failing tests**

Every test names its own project, for the reason given in Task 5.

```rust
    #[test]
    fn a_mention_is_the_notification_the_cli_expects() {
        let (rx, ide, _d) = connected_fake_client_for("mention-shape");
        mention("alpha", Path::new("/w/src/hub.rs"), Some((12, 40))).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(v["method"], "at_mentioned");
        assert_eq!(v["params"]["filePath"], "/w/src/hub.rs");
        assert_eq!(v["params"]["lineStart"], 12);
        assert_eq!(v["params"]["lineEnd"], 40);
        assert!(v.get("id").is_none(), "a notification must carry no id or the CLI will wait for a reply it never gets");
        let _ = ide;
    }

    #[test]
    fn a_whole_file_mention_carries_no_line_numbers() {
        let (rx, _ide, _d) = connected_fake_client_for("mention-wholefile");
        mention("alpha", Path::new("/w/README.md"), None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert!(v["params"].get("lineStart").is_none());
    }

    #[test]
    fn mentioning_with_no_claude_connected_is_an_error_not_a_panic() {
        // The tree's keybinding is always available; Claude is not. This must
        // surface as a refusal the UI can show, never as a socket-thread panic.
        let err = mention("nobody-here", Path::new("/w/x.rs"), None).unwrap_err();
        assert!(err.contains("no Claude"), "the message must say what is missing: {err}");
    }

    #[test]
    fn a_mention_reaches_every_connected_claude_not_just_the_first() {
        // Two terminals, two claudes, one project. Sending to only the first
        // is indistinguishable from sending to all with a single subscriber —
        // which is exactly the trap CLAUDE.md records.
        let (rx1, _a, _d1) = connected_fake_client_for("mention-fanout");
        let (rx2, _b, _d2) = connected_fake_client_for("mention-fanout");
        mention("alpha", Path::new("/w/x.rs"), None).unwrap();
        assert!(rx1.recv_timeout(std::time::Duration::from_secs(2)).is_ok());
        assert!(rx2.recv_timeout(std::time::Duration::from_secs(2)).is_ok());
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test ide::`
Expected: FAIL — `cannot find function mention`.

- [ ] **Step 4: Write the implementation**

Give each connection a writer channel, as `wsconn.rs` and `term.rs` already do,
and register it under the project:

```rust
/// Live Claude connections per project. A `Sender` per connection, drained by
/// that connection's writer thread — the notification path must never block
/// on a socket write, and must never hold this lock while doing one.
static CONNS: OnceLock<Mutex<HashMap<String, Vec<std::sync::mpsc::Sender<String>>>>> =
    OnceLock::new();

fn conns() -> &'static Mutex<HashMap<String, Vec<std::sync::mpsc::Sender<String>>>> {
    CONNS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn mention(project: &str, abs: &Path, lines: Option<(u32, u32)>) -> Result<(), String> {
    let mut params = serde_json::json!({"filePath": abs.to_string_lossy()});
    if let Some((a, b)) = lines {
        params["lineStart"] = serde_json::json!(a);
        params["lineEnd"] = serde_json::json!(b);
    }
    // No id: this is a notification. An id would make the CLI wait for a
    // response that never comes.
    let msg = serde_json::json!({"jsonrpc": "2.0", "method": "at_mentioned", "params": params})
        .to_string();
    // Cloned out under the lock, sent outside it.
    let targets: Vec<_> = {
        let map = conns().lock().unwrap_or_else(|e| e.into_inner());
        map.get(project).cloned().unwrap_or_default()
    };
    if targets.is_empty() {
        return Err("no Claude is connected to this project".into());
    }
    for t in &targets {
        let _ = t.send(msg.clone());
    }
    Ok(())
}
```

In `serve_conn`, after a successful handshake, create the channel, spawn the
writer thread, register the sender, and deregister on exit (with a guard, so a
panic still unregisters — `wsconn.rs`'s `UnsubGuard` is the pattern).

In `src/proto.rs`, add to `Intent`:

```rust
    /// A file or selection the user wants Claude to look at. Resolved
    /// server-side and sent as `at_mentioned`, not pasted into a terminal:
    /// a paste lands in whatever state the terminal is in and competes with
    /// whatever Claude is doing at that instant.
    MentionPath { rel: String, line_start: Option<u32>, line_end: Option<u32> },
```

In `src/hub.rs`'s `handle`, route it through `safe_resolve` before it reaches
`ide::mention`, and report failure with `send_to` (not `broadcast`) — only the
browser that pressed the key should see the refusal.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test ide:: hub::`
Expected: all pass.

- [ ] **Step 6: Do the revert-the-fix check**

Change `for t in &targets` to send only to `targets.first()`. Run. Expected:
only `a_mention_reaches_every_connected_claude_not_just_the_first` fails, and
only on `rx2`. Restore.

- [ ] **Step 7: Commit**

```bash
git add src/ide.rs src/proto.rs src/hub.rs
git commit -m "ide: point at a file without pasting into a terminal"
```

---

### Task 7: `openDiff`, `close_tab`, and the proposal tab

**Files:**
- Modify: `src/ide.rs` (pending registry, `openDiff`, `close_tab`)
- Modify: `src/proto.rs` (`Tab::Proposal`, `Intent::AnswerProposal`, `Event::Proposal`)
- Modify: `src/hub.rs` (open the tab, answer the pending request)
- Modify: `src/workspace.rs` (drop `Tab::Proposal` on load)

The largest task here. Everything in it is Rust-testable; the browser half is
Task 8.

**Interfaces:**
- Consumes: `mention`'s connection registry.
- Produces:
  - `pub enum Answer { Accepted, AcceptedEdited(String), Rejected }`
  - `pub fn answer(project: &str, id: &str, a: Answer) -> Result<(), String>`
  - `Intent::AnswerProposal { id: String, accept: bool, text: Option<String> }`
  - `Event::Proposal { id: String, rel: String, old_text: String, new_text: String }`
  - `pub(crate) const MAX_PENDING: usize = 16`
  - `#[cfg(test)] fn pending_id_for_tab(project: &str, tab: &str) -> Option<String>`

  In `hub.rs`, three functions this task also creates — they exist so `ide.rs`
  never takes a hub lock itself:
  - `pub fn has_dirty_buffer(project: &str, rel: &str) -> bool`
  - `pub fn open_proposal(project: &str, id: &str, rel: &str, old_text: &str, new_text: &str)`
  - `pub fn close_proposal(project: &str, id: &str)`

  In `projects.rs`, `abs_to_rel` changes from private (`src/projects.rs:412`)
  to `pub(crate)`. It already does this exact job for `resolve_terminal_path`;
  a second copy would be a second trust boundary to keep in sync.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn accepting_unchanged_answers_tab_closed() {
        let proj = "diff-1";
        let (rx, _c, _d) = connected_fake_client_for(proj);
        let pending = open_diff_from_claude(proj, "/w/a.rs", "new contents", "proposal-1");
        answer(proj, &pending, Answer::Accepted).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(v["result"]["content"][0]["text"], "TAB_CLOSED");
    }

    #[test]
    fn accepting_an_edited_proposal_answers_file_saved_and_the_edited_text() {
        // The second element is how "the user changed my proposal before
        // accepting" reaches Claude. Dropping it makes Claude write the text
        // the user rejected.
        let proj = "diff-2";
        let (rx, _c, _d) = connected_fake_client_for(proj);
        let pending = open_diff_from_claude(proj, "/w/a.rs", "claude's version", "proposal-1");
        answer(proj, &pending, Answer::AcceptedEdited("the human's version".into())).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(v["result"]["content"][0]["text"], "FILE_SAVED");
        assert_eq!(v["result"]["content"][1]["text"], "the human's version");
    }

    #[test]
    fn rejecting_answers_diff_rejected_and_is_not_the_same_as_accepting() {
        let proj = "diff-3";
        let (rx, _c, _d) = connected_fake_client_for(proj);
        let pending = open_diff_from_claude(proj, "/w/a.rs", "x", "proposal-1");
        answer(proj, &pending, Answer::Rejected).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(v["result"]["content"][0]["text"], "DIFF_REJECTED");
    }

    #[test]
    fn resh_never_writes_the_file_itself() {
        // The CLI applies the edit with the content this answer returns
        // (`updatedInput`). A write here would be a second write, and would
        // leave hub.self_writes hashing content that never reached disk.
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.rs");
        std::fs::write(&f, "original").unwrap();
        let proj = "diff-4";
        let (_rx, _c, _d) = connected_fake_client_for(proj);
        let pending = open_diff_from_claude(proj, f.to_str().unwrap(), "proposed", "p1");
        answer(proj, &pending, Answer::Accepted).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "original");
    }

    #[test]
    fn the_read_loop_is_not_blocked_while_a_proposal_is_pending() {
        // If openDiff blocked the read loop, Claude could not send close_tab
        // to withdraw it, and a user who walked away would wedge the socket.
        let proj = "diff-5";
        let (rx, _c, _d) = connected_fake_client_for(proj);
        let _pending = open_diff_from_claude(proj, "/w/a.rs", "x", "p1");
        send_from_claude(proj, &rpc(77, "ping", serde_json::json!({})));
        let v: serde_json::Value =
            serde_json::from_str(&rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap()).unwrap();
        assert_eq!(v["id"], 77, "a later request must be answered while a proposal is open");
    }

    #[test]
    fn a_second_answer_to_one_proposal_is_refused() {
        // Two browsers mirror this state; both can click.
        let proj = "diff-6";
        let (_rx, _c, _d) = connected_fake_client_for(proj);
        let pending = open_diff_from_claude(proj, "/w/a.rs", "x", "p1");
        answer(proj, &pending, Answer::Accepted).unwrap();
        assert!(answer(proj, &pending, Answer::Rejected).is_err(), "first answer wins");
    }

    #[test]
    fn pending_proposals_are_capped_and_the_overflow_is_refused_not_queued() {
        // The spec's cap, in the spirit of the existing <=16-session and
        // <=50-buffer caps. Queueing would let a Claude in a loop hold
        // unbounded content in resh's memory; DIFF_REJECTED is a refusal
        // Claude already knows how to handle.
        let proj = "diff-cap";
        let (rx, _c, _d) = connected_fake_client_for(proj);
        for i in 0..MAX_PENDING {
            open_diff_from_claude(proj, "/w/a.rs", "x", &format!("tab-{i}"));
        }
        send_from_claude(proj, &rpc(999, "tools/call", serde_json::json!({
            "name": "openDiff",
            "arguments": {
                "old_file_path": "/w/a.rs", "new_file_path": "/w/a.rs",
                "new_file_contents": "one too many", "tab_name": "overflow",
            }
        })));
        let v = loop {
            let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
            if v["id"] == 999 { break v; }
        };
        assert_eq!(v["result"]["content"][0]["text"], "DIFF_REJECTED",
            "over the cap, answer immediately rather than parking the request");
    }

    #[test]
    fn a_path_outside_the_project_is_refused_without_opening_a_tab() {
        // Create a real file at the escape target, or this errors with ENOENT
        // before reaching the confinement check and proves nothing.
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("secret.txt");
        std::fs::write(&victim, "not yours").unwrap();
        let proj = "diff-7";
        let (rx, _c, _d) = connected_fake_client_for(proj);
        open_diff_from_claude(proj, victim.to_str().unwrap(), "x", "p1");
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert!(v["error"].is_object(), "expected a refusal, got {v}");
    }

    #[test]
    fn close_tab_withdraws_a_pending_proposal() {
        let proj = "diff-8";
        let (_rx, _c, _d) = connected_fake_client_for(proj);
        let pending = open_diff_from_claude(proj, "/w/a.rs", "x", "proposal-1");
        send_from_claude(proj, &rpc(5, "tools/call", serde_json::json!({
            "name": "close_tab", "arguments": {"tab_name": "proposal-1"}
        })));
        assert!(answer(proj, &pending, Answer::Accepted).is_err(), "a withdrawn proposal cannot be answered");
    }

    #[test]
    fn a_proposal_tab_does_not_survive_a_restart() {
        // Its counterparty is a socket that died with the process. Restoring
        // the tab would render a proposal nobody can answer.
        let mut w = Workspace::default();
        w.panes[proto::MIDDLE].tabs.push(Tab::Proposal { id: "p1".into() });
        w.panes[proto::MIDDLE].tabs.push(Tab::Tree);
        let reloaded = workspace::drop_dead_tabs(w);
        assert_eq!(reloaded.panes[proto::MIDDLE].tabs, vec![Tab::Tree]);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test ide:: workspace::`
Expected: FAIL — `cannot find function answer`, `no variant Proposal`.

- [ ] **Step 3: Write the implementation**

In `src/ide.rs`, a pending registry keyed by an id resh generates (not
`tab_name`, which Claude chooses and could collide across connections):

```rust
pub enum Answer {
    Accepted,
    AcceptedEdited(String),
    Rejected,
}

struct Pending {
    project: String,
    rpc_id: serde_json::Value,
    tab_name: String,
    reply: std::sync::mpsc::Sender<String>,
}

static PENDING: OnceLock<Mutex<HashMap<String, Pending>>> = OnceLock::new();

pub fn answer(project: &str, id: &str, a: Answer) -> Result<(), String> {
    // Removed, not read: the removal *is* the "first answer wins" rule, and
    // doing it under the lock means two browsers cannot both win.
    let p = {
        let mut map = pending().lock().unwrap_or_else(|e| e.into_inner());
        match map.get(id) {
            Some(p) if p.project == project => map.remove(id).expect("just matched"),
            Some(_) => return Err("proposal belongs to another project".into()),
            None => return Err("no such proposal — it was already answered or withdrawn".into()),
        }
    };
    let content = match a {
        Answer::Accepted => serde_json::json!([{"type": "text", "text": "TAB_CLOSED"}]),
        Answer::Rejected => serde_json::json!([{"type": "text", "text": "DIFF_REJECTED"}]),
        Answer::AcceptedEdited(text) => serde_json::json!([
            {"type": "text", "text": "FILE_SAVED"},
            {"type": "text", "text": text},
        ]),
    };
    let msg = serde_json::json!({
        "jsonrpc": "2.0", "id": p.rpc_id, "result": {"content": content}
    })
    .to_string();
    // Outside the lock: this is a channel send feeding a socket write.
    p.reply.send(msg).map_err(|_| "Claude disconnected before answering".to_string())
}
```

In `dispatch`, `tools/call` gains `openDiff` and `close_tab`:

```rust
/// Enough for any real review session. Over it, refuse rather than queue: a
/// queued proposal holds a whole file in memory and Claude is already
/// required to handle DIFF_REJECTED.
pub(crate) const MAX_PENDING: usize = 16;

/// A map key within one process — never persisted, never security-relevant.
/// A counter rather than randomness so test assertions are deterministic; a
/// collision after a restart is impossible because pending proposals do not
/// survive one.
fn new_pending_id(project: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{project}-{n}")
}

fn open_diff(
    id: &serde_json::Value,
    args: &serde_json::Value,
    conn: &Conn,
) -> Option<serde_json::Value> {
    let path = args["old_file_path"].as_str().unwrap_or("");
    let new_text = args["new_file_contents"].as_str().unwrap_or("").to_string();
    let tab_name = args["tab_name"].as_str().unwrap_or("proposal").to_string();

    // The path is absolute and comes off the wire, so it is a hint. Confine it
    // against the directory Claude is actually in when we could read it
    // (worktrees), and against the project either way.
    let base = conn.cwd.clone().unwrap_or_else(|| conn.workspace.clone());
    let rel = match crate::projects::abs_to_rel(&conn.workspace, std::path::Path::new(path)) {
        Ok(r) => r,
        Err(e) => return Some(err(id, -32602, format!("path outside project: {e}"))),
    };
    if crate::projects::safe_resolve(&base, &rel).is_err()
        && crate::projects::safe_resolve_parent(&base, &rel).is_err()
    {
        // Both fail only when the path escapes; safe_resolve alone would also
        // fail for a file Claude is about to create, which is legitimate.
        return Some(err(id, -32602, format!("path outside project: {rel}")));
    }

    // Missing is not an error: an openDiff for a file that does not exist yet
    // is Claude creating one, and the left-hand side is simply empty.
    let old_text = std::fs::read_to_string(&base.join(&rel)).unwrap_or_default();

    // resh may be holding unsaved edits to this very file. Accepting would
    // discard them silently, which is the one thing the whole conflict-guard
    // exists to prevent.
    if crate::hub::has_dirty_buffer(&conn.project, &rel) {
        return Some(err(id, -32002, format!("{rel} has unsaved changes in resh")));
    }

    let pid = new_pending_id(&conn.project);
    {
        let mut map = pending().lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= MAX_PENDING {
            return Some(ok(id, serde_json::json!({
                "content": [{"type": "text", "text": "DIFF_REJECTED"}]
            })));
        }
        map.insert(pid.clone(), Pending {
            project: conn.project.clone(),
            rpc_id: id.clone(),
            tab_name,
            reply: conn.reply.clone(),
        });
    }

    // Outside the lock: this opens a tab and mirrors it to every browser.
    crate::hub::open_proposal(&conn.project, &pid, &rel, &old_text, &new_text);

    // No reply yet, and crucially the read loop is not blocked: Claude must
    // still be able to send close_tab, and a user who walked away must not
    // wedge the socket.
    None
}

fn close_tab(id: &serde_json::Value, args: &serde_json::Value, conn: &Conn) -> Option<serde_json::Value> {
    let name = args["tab_name"].as_str().unwrap_or("");
    let withdrawn: Vec<String> = {
        let mut map = pending().lock().unwrap_or_else(|e| e.into_inner());
        let ids: Vec<String> = map
            .iter()
            .filter(|(_, p)| p.project == conn.project && p.tab_name == name)
            .map(|(k, _)| k.clone())
            .collect();
        for k in &ids {
            map.remove(k);
        }
        ids
    };
    for k in &withdrawn {
        crate::hub::close_proposal(&conn.project, k);
    }
    Some(ok(id, serde_json::json!({"content": [{"type": "text", "text": "TAB_CLOSED"}]})))
}
```

Wire both into the `tools/call` match arm added in Task 4, alongside
`getDiagnostics`. `Conn` gains two fields: `project: String` and
`reply: std::sync::mpsc::Sender<String>` (the writer channel from Task 6).

`projects::abs_to_rel` is currently private; make it `pub(crate)`. It already
does exactly this job for `resolve_terminal_path`, and duplicating it would be
a second trust boundary to keep in sync.

In `src/proto.rs`:

```rust
    /// A change Claude has proposed and nobody has answered yet. Deliberately
    /// not `Tab::Diff`, which is a git diff of a tracked path: a proposal
    /// compares disk against content that has never been written, and it is
    /// keyed by a pending request rather than by a file.
    Proposal { id: String },
```

In `src/workspace.rs`, add `pub fn drop_dead_tabs(w: Workspace) -> Workspace`
and call it on the load path — a proposal's counterparty is a socket that died
with the process.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test ide:: workspace:: hub::`
Expected: all pass.

- [ ] **Step 5: Do the revert-the-fix check on the three that matter**

1. Make `Answer::AcceptedEdited` emit only the `FILE_SAVED` element. Expected:
   only `accepting_an_edited_proposal_answers_file_saved_and_the_edited_text`
   fails, on the second element. Restore.
2. Make `answer` read without removing. Expected: only
   `a_second_answer_to_one_proposal_is_refused` fails. Restore.
3. Make `openDiff` block on a channel receive instead of returning `None`.
   Expected: `the_read_loop_is_not_blocked_while_a_proposal_is_pending` fails
   on the `recv_timeout`. Restore. **Do this one** — it is the design decision
   the whole task rests on, and a test that passes either way is worthless.

- [ ] **Step 6: Time the suite, do not just count failures**

A pending proposal holds a channel and touches three locks. A deadlock hangs
rather than fails.

Run: `time cargo test`
Expected: total wall time within a few seconds of the pre-task baseline.
Record both numbers in the commit message. A run that takes noticeably longer
is a lock-ordering problem, not a slow machine.

- [ ] **Step 7: Commit**

```bash
git add src/ide.rs src/proto.rs src/hub.rs src/workspace.rs
git commit -m "ide: Claude's proposal is a diff, not eighty columns of ascii"
```

---

### Task 8: The browser half

**Files:**
- Modify: `static/app.js`, `static/style.css`
- Create: `tests/browser/ide.mjs`

No Rust test reaches `static/app.js`. CLAUDE.md's table records that "no
browser" once hid the fact that saving was completely broken.

- [ ] **Step 1: Write the browser test first**

Create `tests/browser/ide.mjs`, modelled on `reconnect.mjs`. Read
`tests/browser/README.md` first — especially the four traps that make a browser
test pass while asserting nothing. It must:

1. Start a real resh with a real project, and skip (not fail) when no Chromium
   is present, as the existing scripts do.
2. Drive a real `claude` in a terminal tab and have it propose an edit.
3. Assert a proposal tab appears **with the changed hunk visible** — assert on
   the rendered text of a specific changed line, not on the tab existing. A
   tab with an empty body passes the weaker assertion.
4. Click Accept and assert the file on disk changes **and** that the change was
   written by Claude, not by resh — check the content matches what Claude
   proposed.
5. Click Reject on a second proposal and assert the file is unchanged.
6. Select a range in an editor tab, press the mention key, and assert the
   terminal shows `@`-reference text arriving in Claude's prompt.

- [ ] **Step 2: Run it to verify it fails**

Run: `deno run -A tests/browser/ide.mjs`
Expected: FAIL — no proposal tab appears.

- [ ] **Step 3: Implement the UI**

In `static/app.js`, beside the existing tab renderers:

```js
// A proposal renders through the same hunk view as the save-conflict banner:
// the divergence is the thing to read, and showing both files whole is what
// textdiff.rs exists to avoid.
function renderProposal(el, tab) {
  const p = state.proposals[tab.id];
  if (!p) { el.textContent = 'This proposal was withdrawn.'; return; }
  el.innerHTML = '';
  el.appendChild(renderHunks(p.old_text, p.new_text));
  const bar = document.createElement('div');
  bar.className = 'proposal-actions';
  const edited = () => {
    const box = el.querySelector('.proposal-edit');
    return box && box.value !== p.new_text ? box.value : null;
  };
  const answer = (accept) => send({
    t: 'AnswerProposal', id: tab.id, accept, text: accept ? edited() : null,
  });
  bar.append(
    button('Accept', () => answer(true)),
    button('Reject', () => answer(false)),
  );
  el.appendChild(bar);
}

// Alt+K, matching the extensions' own binding. The selection's line range
// travels; the text does not (that is Task 9, and it is opt-in).
document.addEventListener('keydown', (e) => {
  if (!e.altKey || e.key.toLowerCase() !== 'k') return;
  const t = activeTab();
  if (!t) return;
  e.preventDefault();
  const sel = editorSelection(t);
  send({
    t: 'MentionPath',
    rel: t.rel,
    line_start: sel ? sel.startLine : null,
    line_end: sel ? sel.endLine : null,
  });
});
```

In `static/style.css`, `.proposal-actions` gets the same treatment as the
save-conflict banner's buttons — reuse the existing class rather than adding a
parallel one.

- [ ] **Step 4: Run the browser test to verify it passes**

Run: `deno run -A tests/browser/ide.mjs`
Expected: PASS.

- [ ] **Step 5: Run the whole suite on the Linux host**

Run: `cargo test` locally, then `ssh` to the deploy host and run `cargo test`
there. CLAUDE.md records defects that only appeared on the real host
(inotify vs FSEvents, the dtach socket directory).

- [ ] **Step 6: Commit**

```bash
git add static/app.js static/style.css tests/browser/ide.mjs
git commit -m "ide: accept or reject where you can actually read it"
```

---

### Task 9: `selection_changed`, opt-in

**Files:**
- Modify: `src/ide.rs`, `src/proto.rs`, `src/hub.rs`, `static/app.js`
- Modify: `docs/deploy.md` (document the config key)

**Interfaces:**
- Consumes: `notify_all`, factored out of Task 6's `mention`.
- Produces:
  - `pub fn selection_changed(project: &str, project_dir: &Path, abs: &Path, text: &str, start: (u32,u32), end: (u32,u32)) -> Result<(), String>`
  - `Settings::share_selection: bool` (default `false`)
  - `Intent::ShareSelection { rel: String, text: String, start_line: u32, start_col: u32, end_line: u32, end_col: u32 }`

This ships file contents to Claude without an explicit user action, which is a
change in resh's posture. Claude Code's own answer is `Read` deny rules; resh
has no permission system to hang that off, so it is off unless a project opts
in and visible whenever it is on.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn selection_sharing_is_off_unless_a_project_opts_in() {
        // The struct is `Settings`, not `Config`. The default must be off. A test that only checks the on-path would
        // pass with the default flipped.
        let cfg = Settings::default();
        assert!(!cfg.share_selection, "a highlighted line of .env must not leave the host by default");
    }

    #[test]
    fn a_shared_selection_is_the_notification_the_cli_expects() {
        let proj = "diff-9";
        let (rx, _c, _d) = connected_fake_client_for(proj);
        selection_changed("alpha", d.path(), Path::new("/w/a.rs"), "let x = 1;", (10, 4), (10, 14)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(v["method"], "selection_changed");
        assert_eq!(v["params"]["text"], "let x = 1;");
        assert_eq!(v["params"]["selection"]["start"]["line"], 10);
        assert_eq!(v["params"]["selection"]["start"]["character"], 4);
        assert_eq!(v["params"]["selection"]["end"]["character"], 14);
    }

    #[test]
    fn nothing_is_sent_when_the_project_has_not_opted_in() {
        let (rx, _c, _d) = connected_fake_client_without_optin();
        assert!(selection_changed("alpha", d.path(), Path::new("/w/a.rs"), "secret", (1, 0), (1, 6)).is_err());
        assert!(rx.try_recv().is_err(), "the socket must stay silent");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test ide:: config::`
Expected: FAIL — `no field share_selection`.

- [ ] **Step 3: Implement**

In `src/config.rs`, add the key to the existing `Settings` struct (beside
`autosave`, `src/config.rs:29`) with an explicit `false` default. Per-project
is allowed here, unlike `max_upload_bytes`: sharing your own selection with
your own Claude cannot raise a ceiling on anything.

```rust
    /// Off unless a project asks for it. This ships file contents to Claude
    /// with no explicit user action, and resh has no permission system to
    /// scope it the way Claude Code's own `Read` deny rules do.
    #[serde(default)]
    pub share_selection: bool,
```

In `src/ide.rs`:

```rust
pub fn selection_changed(
    project: &str,
    project_dir: &Path,
    abs: &Path,
    text: &str,
    start: (u32, u32),
    end: (u32, u32),
) -> Result<(), String> {
    // `config::for_project` takes the project *directory* (src/config.rs:191),
    // which the hub already holds — not the project name.
    if !crate::config::for_project(project_dir).share_selection {
        return Err("selection sharing is off for this project".into());
    }
    notify_all(project, &serde_json::json!({
        "jsonrpc": "2.0",
        "method": "selection_changed",
        "params": {
            "filePath": abs.to_string_lossy(),
            "text": text,
            "selection": {
                "start": {"line": start.0, "character": start.1},
                "end":   {"line": end.0,   "character": end.1},
            },
        },
    }))
}
```

where `notify_all` is `mention`'s send loop, factored out.

In `static/app.js`, debounce on `selectionchange` (in the browser, not in
Rust — a debounce in the socket thread would hold state per connection for no
reason), and render the indicator in the pane header:

```js
const shareSelection = debounce(() => {
  if (!state.config.share_selection) return;
  const sel = editorSelection(activeTab());
  if (!sel) return;
  send({ t: 'ShareSelection', rel: sel.rel, text: sel.text,
         start_line: sel.startLine, start_col: sel.startCol,
         end_line: sel.endLine, end_col: sel.endCol });
}, 200);
```

The pane header shows `⧉ sharing selection` whenever the key is on, for the
same reason the header shows which projects have shells running: this is the
README's "visible and deliberate" stance, and silent exfiltration of a
highlighted line of `.env` is precisely what it is for.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test ide:: config::`
Expected: all pass.

- [ ] **Step 5: Do the revert-the-fix check**

Flip the default to `true`. Expected: only
`selection_sharing_is_off_unless_a_project_opts_in` fails. Restore.

- [ ] **Step 6: Commit**

```bash
git add src/ide.rs src/proto.rs src/hub.rs src/config.rs static/app.js docs/deploy.md
git commit -m "ide: the selection goes to Claude only when you said it could"
```

---

## After the last task

- [ ] Add the Origin inversion to CLAUDE.md's hard constraints, beside the
      existing "Every websocket checks `Origin`" bullet. Without it, the next
      reader reconciles the two sockets and reintroduces CVE-2025-52882.
- [ ] Update README.md's feature list.
- [ ] Move `docs/backlog.md:20` (the `@reference` item) to the shipped section
      with a pointer to the spec, as the file-upload entry does.
- [ ] Deploy, then confirm the *running* binary changed — `cargo build` alone
      updates neither path the service uses (`docs/deploy.md`).
