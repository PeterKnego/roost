# deadlight Stateful Projects Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make projects an explicit, persistent concept — track their dtach sessions, stop spawning shells implicitly, and let the user see what is running and close it.

**Architecture:** A process-wide registry, rebuilt at startup by scanning the state directory, socket directory and live processes (sessions outlive deadlight, so memory alone would forget them). Terminal tabs become placeholders that spawn a session only on an explicit start. A cross-project fragment feeds a header strip of open projects; a `CloseProject` intent ends a project's sessions after a confirmation that blocks on unsaved work.

**Tech Stack:** Rust 2021, tungstenite 0.24, portable-pty 0.8, serde + serde_json, notify 8.2. Runtime: `dtach` 0.9, `git`, `ps`/`pgrep`. Frontend: plain JS, vendored htmx + xterm.

**Spec:** `docs/superpowers/specs/2026-08-17-deadlight-projects-design.md`

## Global Constraints

- Bind `127.0.0.1` only. **The websocket spawns a shell — never widen the bind.**
- HTTP stays **GET-only**. `StartTerminal`, `InitGit` and `CloseProject` are state-changing and travel over the workspace websocket as intents.
- Every websocket endpoint checks `Origin` in its handshake (`origin::origin_allowed` + `config::allowed_origins`). A new socket that skips this is a Critical defect.
- Session names match `^[A-Za-z0-9_-]{1,32}$` (`session::valid_name`) — they land in a dtach socket path and a command line.
- Project storage keys are percent-encoded (`karpie%2Fsrc`); URLs keep readable slashes. Existing top-level keys must stay byte-for-byte identical.
- Caps unchanged: ≤16 sessions per project, ≤50 buffers, 1 MB scrollback, 2 MB file cap.
- `git init` runs with the project directory as cwd and takes **no** user-supplied arguments.
- Crate edition `2021`. Run `cargo test` (never `--release`) from the repo root.
- No panics may escape a socket or watcher thread.
- House style: module-level `//!` doc explaining *why*; implementation above, `#[cfg(test)] mod tests` at the bottom of the same file; comments give rationale, not mechanics.

## File Structure

| File | Responsibility |
|---|---|
| `src/session.rs` (modify) | Session enumeration with age, existence check, kill-all-for-project |
| `src/registry.rs` (new) | Startup reconciliation, orphan reaping, cross-project status |
| `src/proto.rs` (modify) | New intents/events; `live_sessions` + `is_git` in the state view |
| `src/workspace.rs` (modify) | Carry live-session names into the view |
| `src/hub.rs` (modify) | Dispatch `StartTerminal`, `InitGit`, `CloseProject` |
| `src/routes.rs` (modify) | `/frag/_projects` cross-project fragment |
| `src/render.rs` (modify) | Header strip markup; picker ●/○ markers |
| `src/lib.rs` (modify) | Call reconciliation at startup |
| `static/app.js` (modify) | Terminal placeholder, git gate, strip, close dialog |
| `static/style.css` (modify) | Placeholder, strip and dialog styling |

Tasks 1–6 are server-side and each ends green with `cargo test`. Task 7 is the client. Task 8 is docs + deploy.

---

### Task 1: `session` — enumeration, age, and kill-all

**Files:**
- Modify: `src/session.rs`

**Interfaces:**
- Produces: `session::SessionInfo { name: String, pid: u32, age_secs: u64, attached: usize }`; `session::list_sessions(project: &str) -> Vec<SessionInfo>`; `session::has_session(project: &str, name: &str) -> bool`; `session::kill_project(project: &str) -> usize` (returns how many were ended); `session::process_age_secs(pid: u32) -> Option<u64>`; `session::key_for(project: &str, name: &str) -> String`.

Sessions are keyed `{project}/{name}` in the existing `SESSIONS` map. Age comes from the OS, not from a timestamp we record, because sessions survive deadlight restarts and an in-process timestamp would reset.

- [ ] **Step 1: Write the failing tests**

Append to `src/session.rs`'s existing `mod tests`:

```rust
    #[test]
    fn key_for_is_the_map_key_shape() {
        assert_eq!(key_for("karpie", "shell"), "karpie/shell");
        assert_eq!(key_for("karpie%2Fsrc", "claude"), "karpie%2Fsrc/claude");
    }

    #[test]
    fn process_age_of_our_own_process_is_small_and_present() {
        // Our own pid is guaranteed to exist; a fresh test process is young.
        let me = std::process::id();
        let age = process_age_secs(me).expect("our own process must be readable");
        assert!(age < 60 * 60, "own process age looked wrong: {age}");
        // A pid that cannot exist yields None rather than panicking.
        assert!(process_age_secs(0).is_none() || process_age_secs(4_294_967_294).is_none());
    }

    #[test]
    fn listing_and_killing_are_scoped_to_one_project() {
        std::env::set_var("DEADLIGHT_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        // Two projects, so we can prove kill_project does not spill over.
        attach("listproj", "shell", d.path()).unwrap();
        attach("listproj", "claude", d.path()).unwrap();
        attach("otherproj", "shell", d.path()).unwrap();

        let mut names: Vec<String> = list_sessions("listproj").into_iter().map(|s| s.name).collect();
        names.sort();
        assert_eq!(names, vec!["claude", "shell"]);
        assert!(has_session("listproj", "shell"));
        assert!(!has_session("listproj", "nope"));

        let ended = kill_project("listproj");
        assert_eq!(ended, 2);
        assert!(list_sessions("listproj").is_empty(), "all of the project's sessions must go");
        assert_eq!(list_sessions("otherproj").len(), 1, "another project must be untouched");

        kill_project("otherproj");
        std::env::remove_var("DEADLIGHT_CMD");
    }
```

- [ ] **Step 2: Run, expect failure**

Run: `cargo test session`
Expected: FAIL — `key_for`, `process_age_secs`, `list_sessions`, `has_session`, `kill_project` not found.

- [ ] **Step 3: Implement above the test module**

```rust
/// The `SESSIONS` map key. Project keys are already percent-encoded, and
/// `valid_name` bars `/` in session names, so this cannot be ambiguous.
pub fn key_for(project: &str, name: &str) -> String {
    format!("{project}/{name}")
}

pub struct SessionInfo {
    pub name: String,
    pub pid: u32,
    pub age_secs: u64,
    pub attached: usize,
}

/// Elapsed seconds for a pid, via `ps -o etimes=`, which both Linux and
/// macOS support. Age is read from the OS rather than recorded in memory
/// because dtach sessions outlive deadlight — an in-process timestamp would
/// reset on every restart and report a days-old shell as brand new.
pub fn process_age_secs(pid: u32) -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "etimes=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse::<u64>().ok()
}

pub fn list_sessions(project: &str) -> Vec<SessionInfo> {
    let prefix = format!("{project}/");
    let map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let mut out: Vec<SessionInfo> = map
        .iter()
        .filter_map(|(k, s)| {
            let name = k.strip_prefix(&prefix)?;
            let pid = s.child_pid;
            Some(SessionInfo {
                name: name.to_string(),
                pid,
                age_secs: process_age_secs(pid).unwrap_or(0),
                attached: s.subs.len(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn has_session(project: &str, name: &str) -> bool {
    let map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    map.contains_key(&key_for(project, name))
}

/// Ends every session belonging to one project. This is the only way to end
/// a session from the UI; detaching a tab deliberately leaves it running.
pub fn kill_project(project: &str) -> usize {
    let prefix = format!("{project}/");
    let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let keys: Vec<String> = map.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
    let mut ended = 0;
    for k in keys {
        if let Some(mut s) = map.remove(&k) {
            let _ = s.child.kill();
            let _ = s.child.wait();
            ended += 1;
        }
    }
    ended
}
```

Add `child_pid: u32` to the `Session` struct, set from `child.process_id().unwrap_or(0)` right after `spawn_command` succeeds. `portable_pty::Child` exposes `process_id()`; if the installed version's method differs, use whatever it provides and note it in your report — the pid is only used for age lookup and a value of 0 degrades to an age of 0, never a panic.

- [ ] **Step 4: Run, expect pass**

Run: `cargo test session`
Expected: all session tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/session.rs
git commit -m "projects: session enumeration with age, existence check, kill-all"
```

---

### Task 2: `registry` — startup reconciliation and orphan reaping

**Files:**
- Create: `src/registry.rs`
- Modify: `src/lib.rs` (add `pub mod registry;` and call reconciliation in `serve`)

**Interfaces:**
- Consumes: `session::{list_sessions, process_age_secs, key_for}`, `wsstate::state_dir`, `projects::resolve_project`.
- Produces: `registry::ProjectStatus { key: String, url: String, live: usize, oldest_age_secs: u64, has_layout: bool }`; `registry::decode_key(&str) -> String`; `registry::known_projects(roots: &[PathBuf]) -> Vec<ProjectStatus>`; `registry::reconcile(roots: &[PathBuf]) -> ReapReport`; `registry::ReapReport { dead_sockets: usize, gone_projects: usize }`.

`decode_key` is the inverse of the storage-key encoding: `karpie%2Fsrc` → `karpie/src`, needed to turn a socket directory name back into a URL.

- [ ] **Step 1: Create `src/registry.rs` with tests only**

```rust
//! The project registry: what deadlight knows about, and what is running.
//!
//! Rebuilt at startup rather than accumulated in memory, because dtach
//! sessions deliberately outlive deadlight. An in-memory-only registry would
//! forget every running shell on restart — which is exactly how nine
//! orphaned sessions for deleted directories accumulated unnoticed in
//! production on 2026-08-17.
use std::path::PathBuf;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn with_state<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path());
        let out = f(d.path());
        std::env::remove_var("DEADLIGHT_STATE_DIR");
        out
    }

    #[test]
    fn decode_key_reverses_the_storage_encoding() {
        assert_eq!(decode_key("karpie"), "karpie");
        assert_eq!(decode_key("karpie%2Fsrc"), "karpie/src");
        assert_eq!(decode_key("a%2Fb%2Fc"), "a/b/c");
    }

    #[test]
    fn a_saved_layout_alone_makes_a_project_known_but_idle() {
        with_state(|state| {
            fs::create_dir_all(state).unwrap();
            fs::write(state.join("karpie.json"), "{}").unwrap();
            let roots = vec![PathBuf::from("/nonexistent-root")];
            let ps = known_projects(&roots);
            let p = ps.iter().find(|p| p.key == "karpie").expect("saved layout must be listed");
            assert!(p.has_layout);
            assert_eq!(p.live, 0, "no sessions means idle, not live");
            assert_eq!(p.url, "karpie");
        });
    }

    #[test]
    fn a_socket_with_no_live_process_is_reaped() {
        with_state(|state| {
            let sock = state.join("sock/ghost");
            fs::create_dir_all(&sock).unwrap();
            // A plain file stands in for a stale socket: no dtach process holds it.
            fs::write(sock.join("shell"), "").unwrap();
            let report = reconcile(&[PathBuf::from("/nonexistent-root")]);
            assert!(report.dead_sockets >= 1, "a socket with no process must be removed");
            assert!(!sock.join("shell").exists(), "the stale socket file must be gone");
        });
    }

    #[test]
    fn nested_project_keys_produce_slashed_urls() {
        with_state(|state| {
            fs::create_dir_all(state).unwrap();
            fs::write(state.join("karpie%2Fsrc.json"), "{}").unwrap();
            let ps = known_projects(&[PathBuf::from("/nonexistent-root")]);
            let p = ps.iter().find(|p| p.key == "karpie%2Fsrc").expect("nested project must list");
            assert_eq!(p.url, "karpie/src", "the URL keeps readable slashes");
        });
    }
}
```

- [ ] **Step 2: Add `pub mod registry;` to `src/lib.rs`, run, expect compile failure**

Run: `cargo test registry`
Expected: FAIL — `decode_key`, `known_projects`, `reconcile` not found.

- [ ] **Step 3: Implement above the test module**

```rust
pub struct ProjectStatus {
    /// Storage key, percent-encoded (`karpie%2Fsrc`).
    pub key: String,
    /// URL form, readable slashes (`karpie/src`).
    pub url: String,
    pub live: usize,
    pub oldest_age_secs: u64,
    pub has_layout: bool,
}

pub struct ReapReport {
    pub dead_sockets: usize,
    pub gone_projects: usize,
}

/// Inverse of the storage-key encoding used by `wsstate` and `session`.
pub fn decode_key(key: &str) -> String {
    crate::http::percent_decode(key)
}

fn sock_root() -> PathBuf {
    crate::wsstate::state_dir().join("sock")
}

/// True when some live process holds this socket path. `pgrep -f` matches the
/// full command line, which is where dtach carries its socket path.
fn socket_has_process(path: &std::path::Path) -> bool {
    std::process::Command::new("pgrep")
        .arg("-f")
        .arg(path.to_string_lossy().as_ref())
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

/// Removes sockets whose process is gone and sessions whose project directory
/// no longer exists. Runs at startup and on every enumeration, so orphans
/// cannot accumulate silently the way they did before this existed.
pub fn reconcile(roots: &[PathBuf]) -> ReapReport {
    let mut report = ReapReport { dead_sockets: 0, gone_projects: 0 };
    let Ok(rd) = std::fs::read_dir(sock_root()) else { return report };
    for entry in rd.flatten() {
        let key = entry.file_name().to_string_lossy().into_owned();
        let url = decode_key(&key);
        let project_gone = crate::projects::resolve_project(roots, &url).is_none();
        let Ok(inner) = std::fs::read_dir(entry.path()) else { continue };
        for sock in inner.flatten() {
            let live = socket_has_process(&sock.path());
            if !live {
                let _ = std::fs::remove_file(sock.path());
                report.dead_sockets += 1;
                eprintln!("deadlight: reaped dead socket {}", sock.path().display());
            } else if project_gone {
                let name = sock.file_name().to_string_lossy().into_owned();
                let ended = crate::session::kill_project(&key);
                let _ = std::fs::remove_file(sock.path());
                report.gone_projects += 1;
                eprintln!(
                    "deadlight: reaped session {key}/{name} — project directory is gone ({ended} in-process)"
                );
            }
        }
        // An emptied directory is noise; ignore failure when it is not empty.
        let _ = std::fs::remove_dir(entry.path());
    }
    report
}

/// Every project deadlight knows about: those with a saved layout, those with
/// live sessions, and those with both.
pub fn known_projects(roots: &[PathBuf]) -> Vec<ProjectStatus> {
    let _ = reconcile(roots);
    let mut by_key: std::collections::BTreeMap<String, ProjectStatus> = Default::default();

    if let Ok(rd) = std::fs::read_dir(crate::wsstate::state_dir()) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(key) = name.strip_suffix(".json") else { continue };
            by_key.insert(
                key.to_string(),
                ProjectStatus {
                    key: key.to_string(),
                    url: decode_key(key),
                    live: 0,
                    oldest_age_secs: 0,
                    has_layout: true,
                },
            );
        }
    }

    if let Ok(rd) = std::fs::read_dir(sock_root()) {
        for e in rd.flatten() {
            let key = e.file_name().to_string_lossy().into_owned();
            let sessions = crate::session::list_sessions(&key);
            let live = sessions.len();
            let oldest = sessions.iter().map(|s| s.age_secs).max().unwrap_or(0);
            let slot = by_key.entry(key.clone()).or_insert(ProjectStatus {
                key: key.clone(),
                url: decode_key(&key),
                live: 0,
                oldest_age_secs: 0,
                has_layout: false,
            });
            slot.live = live;
            slot.oldest_age_secs = oldest;
        }
    }

    by_key.into_values().collect()
}
```

- [ ] **Step 4: Call reconciliation at startup in `src/lib.rs`**

At the top of `serve`, before the accept loop:

```rust
    // Sessions outlive deadlight, so the registry must be rebuilt from disk
    // and live processes rather than assumed empty.
    let report = registry::reconcile(&roots);
    if report.dead_sockets > 0 || report.gone_projects > 0 {
        eprintln!(
            "deadlight: startup reap — {} dead sockets, {} sessions for missing projects",
            report.dead_sockets, report.gone_projects
        );
    }
```

- [ ] **Step 5: Run, expect pass**

Run: `cargo test registry`
Expected: 4 passed

- [ ] **Step 6: Commit**

```bash
git add src/registry.rs src/lib.rs
git commit -m "projects: registry with startup reconciliation and orphan reaping"
```

---

### Task 3: `proto` + `workspace` — live sessions and git status in the view

**Files:**
- Modify: `src/proto.rs`, `src/workspace.rs`

**Interfaces:**
- Produces: `Intent::StartTerminal { session: String }`, `Intent::InitGit`, `Intent::CloseProject`; `Event::TerminalStarted { session: String }`, `Event::GitInit { ok: bool, msg: String }`, `Event::CloseRefused { dirty: Vec<String> }`, `Event::ProjectClosed { ended: usize }`; `WorkspaceView` gains `live_sessions: Vec<String>` and `is_git: bool`.

The client needs `live_sessions` to decide whether a Terminal tab renders its placeholder or attaches: after a reload, a tab whose session is already running must attach immediately rather than asking the user to start it again.

- [ ] **Step 1: Write the failing tests**

Append to `src/proto.rs`'s `mod tests`:

```rust
    #[test]
    fn decodes_the_new_project_intents() {
        assert!(matches!(
            decode(r#"{"t":"StartTerminal","session":"shell"}"#).unwrap(),
            Intent::StartTerminal { .. }
        ));
        assert!(matches!(decode(r#"{"t":"InitGit"}"#).unwrap(), Intent::InitGit));
        assert!(matches!(decode(r#"{"t":"CloseProject"}"#).unwrap(), Intent::CloseProject));
    }

    #[test]
    fn encodes_the_new_project_events() {
        let s = encode(&Event::ProjectClosed { ended: 3 });
        assert!(s.contains(r#""t":"ProjectClosed""#) && s.contains(r#""ended":3"#));
        let s = encode(&Event::CloseRefused { dirty: vec!["a.rs".into()] });
        assert!(s.contains(r#""t":"CloseRefused""#) && s.contains("a.rs"));
        let s = encode(&Event::GitInit { ok: false, msg: "boom".into() });
        assert!(s.contains(r#""ok":false"#) && s.contains("boom"));
    }
```

Append to `src/workspace.rs`'s `mod tests`:

```rust
    #[test]
    fn view_reports_live_sessions_and_git_status() {
        let mut w = Workspace::default_layout();
        w.live_sessions = vec!["shell".into()];
        w.is_git = true;
        let v = w.view();
        assert_eq!(v.live_sessions, vec!["shell".to_string()]);
        assert!(v.is_git);
        // Default is neither: a fresh workspace has spawned nothing.
        let fresh = Workspace::default_layout().view();
        assert!(fresh.live_sessions.is_empty(), "opening a project must not imply a session");
    }
```

- [ ] **Step 2: Run, expect failure**

Run: `cargo test proto workspace`
Expected: FAIL — unknown variants and unknown fields.

- [ ] **Step 3: Implement**

In `src/proto.rs`, add to `Intent`:

```rust
    StartTerminal { session: String },
    InitGit,
    CloseProject,
```

add to `Event`:

```rust
    TerminalStarted { session: String },
    GitInit { ok: bool, msg: String },
    CloseRefused { dirty: Vec<String> },
    ProjectClosed { ended: usize },
```

and add to `WorkspaceView`:

```rust
    /// Session names currently running for this project. A Terminal tab whose
    /// name is absent renders its start placeholder instead of attaching.
    pub live_sessions: Vec<String>,
    /// Whether the project directory is a git repository — drives the
    /// initialise-git offer on the placeholder.
    pub is_git: bool,
```

In `src/workspace.rs`, add both fields to `Workspace` (defaulting to `vec![]` / `false` in `default_layout`) and copy them through in `view()`.

- [ ] **Step 4: Run, expect pass**

Run: `cargo test proto workspace`
Expected: all pass

- [ ] **Step 5: Commit**

```bash
git add src/proto.rs src/workspace.rs
git commit -m "projects: live-session and git-status fields, project intents and events"
```

---

### Task 4: `hub` — dispatch StartTerminal, InitGit, CloseProject

**Files:**
- Modify: `src/hub.rs`

**Interfaces:**
- Consumes: `session::{has_session, list_sessions, kill_project, valid_name}`, `registry`, `proto`.
- Produces: `Hub::refresh_live_sessions(&mut self)` — recomputes `ws.live_sessions` and `ws.is_git` from the registry and the filesystem; called after any intent that could change them.

`StartTerminal` does **not** spawn the PTY itself: the client opens `/ws/{project}/term/{name}`, and that attach is what spawns. `StartTerminal` exists so the server can validate the name, enforce the per-project cap, apply the git gate, and tell every mirrored client the tab has become live.

- [ ] **Step 1: Write the failing tests**

Append to `src/hub.rs`'s `mod tests`:

```rust
    #[test]
    fn close_project_is_refused_while_a_buffer_is_dirty() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.txt"), "disk\n").unwrap();
        let mut h = Hub::new("closeproj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        h.handle(&c, Intent::EditBuffer { rel: "a.txt".into(), text: "unsaved".into() });
        while rx.try_recv().is_ok() {}

        h.handle(&c, Intent::CloseProject);
        let msgs: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"CloseRefused""#) && m.contains("a.txt")),
            "unsaved work must block a close and name the file"
        );
        assert!(
            !msgs.iter().any(|m| m.contains(r#""t":"ProjectClosed""#)),
            "nothing may be ended while work is unsaved"
        );
        std::env::remove_var("DEADLIGHT_STATE_DIR");
    }

    #[test]
    fn close_project_with_clean_buffers_reports_what_it_ended() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("closeclean", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        while rx.try_recv().is_ok() {}
        h.handle(&c, Intent::CloseProject);
        let msgs: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(msgs.iter().any(|m| m.contains(r#""t":"ProjectClosed""#)));
        std::env::remove_var("DEADLIGHT_STATE_DIR");
    }

    #[test]
    fn start_terminal_rejects_an_invalid_session_name() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("startproj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        while rx.try_recv().is_ok() {}
        h.handle(&c, Intent::StartTerminal { session: "bad name;rm".into() });
        let msgs: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(msgs.iter().any(|m| m.contains(r#""t":"Error""#)));
        assert!(!msgs.iter().any(|m| m.contains(r#""t":"TerminalStarted""#)));
        std::env::remove_var("DEADLIGHT_STATE_DIR");
    }
```

- [ ] **Step 2: Run, expect failure**

Run: `cargo test hub`
Expected: FAIL — the new intents are unhandled.

- [ ] **Step 3: Implement**

Add to `Hub::handle`'s match, before the generic `apply_layout` arm:

```rust
            Intent::StartTerminal { session } => return self.do_start_terminal(from, session.clone()),
            Intent::InitGit => return self.do_init_git(from),
            Intent::CloseProject => return self.do_close_project(from),
```

and the methods:

```rust
    /// Recompute what is running. Cheap enough to call after any intent that
    /// could change it, and the single source of truth for the client's
    /// placeholder-versus-attach decision.
    pub fn refresh_live_sessions(&mut self) {
        self.ws.live_sessions =
            crate::session::list_sessions(&self.key).into_iter().map(|s| s.name).collect();
        self.ws.is_git = self.dir.join(".git").exists();
    }

    fn do_start_terminal(&mut self, from: &ConnId, session: String) {
        if !crate::session::valid_name(&session) {
            let ev = Event::Error { msg: format!("invalid session name: {session}") };
            return self.send_to(from, &ev);
        }
        let live = crate::session::list_sessions(&self.key).len();
        if !crate::session::has_session(&self.key, &session)
            && live >= crate::session::MAX_SESSIONS_PER_PROJECT
        {
            let ev = Event::Error { msg: "too many terminal sessions".into() };
            return self.send_to(from, &ev);
        }
        // The client's websocket connect is what actually spawns the PTY; this
        // is the permission check plus the notification to mirrored clients.
        self.ws.version += 1;
        self.broadcast(&Event::TerminalStarted { session });
        self.refresh_live_sessions();
        let snap = self.snapshot_event(from);
        self.broadcast(&snap);
    }

    fn do_init_git(&mut self, from: &ConnId) {
        let out = std::process::Command::new("git").arg("init").current_dir(&self.dir).output();
        let (ok, msg) = match out {
            Ok(o) if o.status.success() => (true, String::from_utf8_lossy(&o.stdout).trim().to_string()),
            Ok(o) => (false, String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => (false, e.to_string()),
        };
        self.broadcast(&Event::GitInit { ok, msg });
        self.refresh_live_sessions();
        let snap = self.snapshot_event(from);
        self.broadcast(&snap);
    }

    fn do_close_project(&mut self, from: &ConnId) {
        // Unsaved text is the one piece of state that cannot be reconstructed,
        // so a resource operation never destroys it.
        let dirty: Vec<String> =
            self.ws.buffers.iter().filter(|(_, b)| b.dirty).map(|(r, _)| r.clone()).collect();
        if !dirty.is_empty() {
            let ev = Event::CloseRefused { dirty };
            return self.send_to(from, &ev);
        }
        let ended = crate::session::kill_project(&self.key);
        self.ws.version += 1;
        self.broadcast(&Event::ProjectClosed { ended });
        self.refresh_live_sessions();
        let snap = self.snapshot_event(from);
        self.broadcast(&snap);
    }
```

`Hub` needs a `key` field (the percent-encoded storage key) alongside `project`. Set it in `Hub::new` using the same encoding `wsstate` uses, and call `refresh_live_sessions()` at the end of `Hub::new` so a freshly loaded hub reports reality.

- [ ] **Step 4: Run, expect pass**

Run: `cargo test hub`
Expected: all pass

- [ ] **Step 5: Commit**

```bash
git add src/hub.rs
git commit -m "projects: start-terminal gate, git init, and close-project dispatch"
```

---

### Task 5: `routes` + `render` — the cross-project fragment and picker markers

**Files:**
- Modify: `src/routes.rs`, `src/render.rs`

**Interfaces:**
- Consumes: `registry::known_projects`.
- Produces: `render::projects_strip(current_key: &str, projects: &[ProjectStatus]) -> String`; a `/frag/_projects?current={key}` route.

The workspace socket is deliberately per-project, so cross-project data needs its own read endpoint. `_projects` is safe as a first path segment under `/frag/` because project names are resolved separately and a leading `_` cannot collide with a directory name that `list_projects` would surface (it lists non-dot directories under ROOTS; a literal `_projects` directory would be a different route shape).

- [ ] **Step 1: Write the failing tests**

Append to `src/render.rs`'s `mod tests`:

```rust
    #[test]
    fn strip_marks_live_and_idle_projects() {
        let ps = vec![
            crate::registry::ProjectStatus {
                key: "karpie".into(), url: "karpie".into(),
                live: 2, oldest_age_secs: 8 * 3600, has_layout: true,
            },
            crate::registry::ProjectStatus {
                key: "glow".into(), url: "glow".into(),
                live: 0, oldest_age_secs: 0, has_layout: true,
            },
        ];
        let h = projects_strip("karpie", &ps);
        assert!(h.contains("target=\"dl-karpie\""), "links reuse a named browsing context");
        assert!(h.contains("href=\"/karpie\""));
        assert!(h.contains("class=\"proj live current\"") || h.contains("current"));
        assert!(h.contains("glow"));
        assert!(h.contains("2 sessions"), "the tooltip must carry the session count");
    }

    #[test]
    fn strip_escapes_project_names() {
        let ps = vec![crate::registry::ProjectStatus {
            key: "a%3Cb".into(), url: "a<b".into(),
            live: 0, oldest_age_secs: 0, has_layout: true,
        }];
        let h = projects_strip("", &ps);
        assert!(h.contains("a&lt;b"));
        assert!(!h.contains("<b\""), "a name must never break out of the markup");
    }
```

- [ ] **Step 2: Run, expect failure**

Run: `cargo test render`
Expected: FAIL — `projects_strip` not found.

- [ ] **Step 3: Implement `projects_strip` in `src/render.rs`**

```rust
/// The header strip of known projects. ● means live sessions, ○ means a saved
/// layout with nothing running — the distinction that answers "what did I
/// leave running?" without opening anything.
pub fn projects_strip(current_key: &str, projects: &[crate::registry::ProjectStatus]) -> String {
    let mut out = String::from("<span class=\"projstrip\">");
    for p in projects {
        let live = p.live > 0;
        let marker = if live { "●" } else { "○" };
        let mut cls = String::from("proj");
        if live {
            cls.push_str(" live");
        }
        if p.key == current_key {
            cls.push_str(" current");
        }
        let title = if live {
            format!("{} sessions · oldest {}", p.live, human_age(p.oldest_age_secs))
        } else {
            "saved layout, nothing running".to_string()
        };
        out.push_str(&format!(
            "<a class=\"{}\" href=\"/{}\" target=\"dl-{}\" title=\"{}\">{} {}</a>",
            cls,
            crate::http::percent_encode(&p.url),
            esc(&p.key),
            esc(&title),
            marker,
            esc(&p.url)
        ));
    }
    out.push_str("</span>");
    out
}

/// Coarse, human-readable age. Precision beyond this is noise when the
/// question is only "is this old enough that I have forgotten it?".
pub fn human_age(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3_600 {
        format!("{}h", secs / 3_600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}
```

- [ ] **Step 4: Add the route in `src/routes.rs`**

Inside `serve_frag`'s dispatch, before the project is resolved, handle the cross-project case. Add to `route`'s match, as its own arm:

```rust
        ["frag", "_projects"] => {
            let current = req.query.get("current").map(String::as_str).unwrap_or("");
            let ps = crate::registry::known_projects(roots);
            http::html(w, &render::projects_strip(current, &ps));
        }
```

Place it **before** the general `["frag", project, what @ ..]` arm so it is not shadowed.

- [ ] **Step 5: Wire the strip into the workspace header in `src/render.rs`**

In `workspace_page`, add beside the refresh control:

```html
  <span id="projstrip" hx-get="/frag/_projects?current={key}" hx-trigger="load, refresh from:body"></span>
  <button id="closeproj" title="close project — ends all its terminal sessions">✕ Close</button>
```

`{key}` is the percent-encoded storage key; add it as a `workspace_page` parameter and update the existing call site and tests.

- [ ] **Step 6: Add ●/○ markers to the picker rows**

In the picker row rendering, when an entry's rel matches a known project, append the same marker. Add a test asserting a live project's row carries `●` and an unknown directory carries neither marker.

- [ ] **Step 7: Run, expect pass**

Run: `cargo test`
Expected: all pass

- [ ] **Step 8: Commit**

```bash
git add src/render.rs src/routes.rs
git commit -m "projects: cross-project strip fragment and picker status markers"
```

---

### Task 6: Integration tests for the no-auto-spawn guarantee

**Files:**
- Modify: `tests/integration.rs`

This task exists on its own because "opening a project spawns nothing" is the behavioral promise of the whole design, and it is exactly the kind of guarantee that regresses silently.

- [ ] **Step 1: Write the tests**

```rust
#[test]
fn opening_a_project_spawns_no_terminal_session() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    std::env::set_var("DEADLIGHT_CMD", "cat");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
    let (_d, port) = fixture();

    // Fetch the workspace page and open a workspace socket — everything a
    // browser does on arrival except starting a terminal.
    let body = ureq::get(&format!("http://127.0.0.1:{port}/proj")).call().unwrap().into_string().unwrap();
    assert!(body.contains("data-project"));
    let mut ws = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    ws.send(tungstenite::Message::Text(r#"{"t":"RequestState"}"#.into())).unwrap();
    let state = read_until(&mut ws, r#""t":"State""#);
    assert!(
        state.contains(r#""live_sessions":[]"#),
        "merely opening a project must not spawn a shell; got: {state}"
    );

    let _ = ws.close(None);
    std::env::remove_var("DEADLIGHT_STATE_DIR");
}

#[test]
fn close_project_ends_sessions_and_reports_the_count() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    std::env::set_var("DEADLIGHT_CMD", "cat");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
    let (_d, port) = fixture();

    // Starting a terminal is what creates a session: connect its socket.
    let mut term = ws_connect_path(port, "/ws/proj/term/shell").unwrap();
    term.send(tungstenite::Message::Binary(b"hi\r".to_vec().into())).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));

    let mut ws = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    ws.send(tungstenite::Message::Text(r#"{"t":"CloseProject"}"#.into())).unwrap();
    let closed = read_until(&mut ws, r#""t":"ProjectClosed""#);
    assert!(closed.contains(r#""ended":1"#), "expected one session ended; got: {closed}");

    let _ = term.close(None);
    let _ = ws.close(None);
    std::env::remove_var("DEADLIGHT_STATE_DIR");
}
```

- [ ] **Step 2: Run, expect failure then pass**

Run: `cargo test --test integration`
Expected: initially FAIL on `live_sessions` being absent or non-empty; PASS once Tasks 3–4 are in.

- [ ] **Step 3: Commit**

```bash
git add tests/integration.rs
git commit -m "projects: prove opening a project spawns nothing, and close ends sessions"
```

---

### Task 7: Client — placeholder, git gate, strip, close dialog

**Files:**
- Modify: `static/app.js`, `static/style.css`

- [ ] **Step 1: Render the terminal placeholder instead of auto-connecting**

In `app.js`, `mountTab`'s `Terminal` branch currently calls `ensureTerm(session)`, which opens the websocket and therefore spawns. Gate it:

```js
  if (t.k === "Terminal") {
    const liveNow = state && state.live_sessions && state.live_sessions.includes(t.session);
    // Only attach when a session already exists. Opening a project must never
    // fork a shell — that is how nine unused sessions accumulated before this.
    if (!liveNow && !terms.has(t.session)) {
      content.appendChild(terminalPlaceholder(t.session));
      return;
    }
    const e = ensureTerm(t.session);
    content.appendChild(e.node);
    requestAnimationFrame(() => { try { e.fit.fit(); e.term.focus(); sendResize(e); } catch {} });
    return;
  }
```

```js
// A bare pane is not discoverable and a button does not match terminal muscle
// memory, so the hint is itself the control: Enter or a click both start it.
function terminalPlaceholder(session) {
  const box = document.createElement("div");
  box.className = "termstart";
  box.tabIndex = 0;
  const isGit = state && state.is_git;
  box.innerHTML = isGit
    ? `<p>Press <kbd>Enter</kbd> to start a terminal</p>`
    : `<p>Not a git repository.</p>
       <p><button class="initgit">Initialize git repo</button></p>
       <p><a class="nogit" href="#">start without git</a></p>`;
  const start = () => send({ t: "StartTerminal", session });
  if (isGit) {
    box.onclick = start;
    box.onkeydown = (e) => { if (e.key === "Enter") { e.preventDefault(); start(); } };
    requestAnimationFrame(() => box.focus());
  } else {
    box.querySelector(".initgit").onclick = () => send({ t: "InitGit" });
    box.querySelector(".nogit").onclick = (e) => { e.preventDefault(); start(); };
  }
  return box;
}
```

- [ ] **Step 2: Handle the new events**

```js
    case "TerminalStarted":
      // The server approved it; opening the socket is what spawns the PTY.
      ensureTerm(ev.session);
      render();
      break;
    case "GitInit":
      if (!ev.ok) showError({ msg: "git init failed: " + ev.msg });
      break;
    case "CloseRefused":
      showError({ msg: "Cannot close: unsaved changes in " + ev.dirty.join(", ") });
      break;
    case "ProjectClosed":
      showError({ msg: ev.ended + " terminal session(s) ended" });
      terms.forEach((e) => { try { e.sock.close(); e.term.dispose(); } catch {} });
      terms.clear();
      render();
      break;
```

- [ ] **Step 3: Wire the Close button with a confirmation that lists sessions**

```js
const closeBtn = document.getElementById("closeproj");
if (closeBtn) closeBtn.onclick = () => {
  const live = (state && state.live_sessions) || [];
  const dirty = ((state && state.buffers) || []).filter((b) => b.dirty).map((b) => b.rel);
  let msg = `Close ${PROJECT}?\n\n`;
  msg += live.length
    ? `${live.length} terminal session(s) will be ended:\n  ${live.join(", ")}\n`
    : "No terminal sessions are running.\n";
  if (dirty.length) {
    // Mirrors the server's refusal so the user is told before, not after.
    alert(msg + `\nUnsaved changes in:\n  ${dirty.join("\n  ")}\n\nSave or discard them first.`);
    return;
  }
  if (confirm(msg + "\nEnd sessions?")) send({ t: "CloseProject" });
};
```

- [ ] **Step 4: Add styling in `static/style.css`**

```css
.termstart { display:flex; flex-direction:column; align-items:center; justify-content:center;
             height:100%; gap:8px; opacity:.75; cursor:pointer; text-align:center; }
.termstart:focus { outline:1px solid var(--accent); opacity:1; }
.termstart kbd { border:1px solid var(--border); border-radius:3px; padding:0 4px; }
.projstrip { display:inline-flex; gap:8px; margin:0 8px; }
.projstrip .proj { text-decoration:none; opacity:.6; white-space:nowrap; }
.projstrip .proj.live { opacity:1; }
.projstrip .proj.current { font-weight:600; text-decoration:underline; }
```

- [ ] **Step 5: Verify in a browser**

```bash
DEADLIGHT_ROOTS="$HOME/Projects" cargo run --quiet 8444
```

- [ ] Opening a project shows the placeholder and **no** session is created (`pgrep -c dtach` unchanged).
- [ ] Enter starts a terminal; clicking the hint also starts one.
- [ ] Reloading the page with a live session attaches directly, with no placeholder.
- [ ] A non-git directory offers Initialize git repo; init succeeds and the placeholder becomes the normal start hint.
- [ ] The strip shows ● for the current project once a terminal runs, ○ for a project with only a saved layout.
- [ ] A strip link opens/reuses a tab named `dl-{key}`.
- [ ] Close asks for confirmation listing the sessions, ends them, and the strip marker becomes ○.
- [ ] Close with an unsaved buffer is refused and names the file.

- [ ] **Step 6: Commit**

```bash
git add static/app.js static/style.css
git commit -m "projects: deliberate terminal start, git gate, header strip, close dialog"
```

---

### Task 8: Docs and deploy

**Files:**
- Modify: `docs/deploy.md`

- [ ] **Step 1: Document the concepts in `docs/deploy.md`**

Add a section covering: a project is any directory opened in deadlight (normally a repo; non-repos are offered `git init` and may start anyway); sessions are tracked per project and survive restarts; the registry is rebuilt at startup and reaps sockets with no process plus sessions whose project directory is gone; terminals start only on an explicit Enter/click; Close Project ends all of a project's sessions, keeps the layout, and refuses while buffers are dirty.

- [ ] **Step 2: Full test run**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 3: Deploy**

```bash
git push origin master
ssh claude@77.42.80.36 'cd /home/claude/projects/deadlight && git pull --ff-only && cargo build --release && install -m 755 ~/.cache/cargo-target/release/deadlight ~/.local/bin/deadlight && systemctl --user restart deadlight'
```

Confirm the startup reap logged, and that no session was lost that should not have been:

```bash
ssh claude@77.42.80.36 'journalctl --user -u deadlight --since "2 min ago" --no-pager | grep -i reap; pgrep -c dtach'
```

- [ ] **Step 4: Commit**

```bash
git add docs/deploy.md
git commit -m "projects: deploy notes for the stateful-projects release"
```
