# deadlight v3 IDE-Style Workspace Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn deadlight from a one-pane tab-switcher into a four-pane workspace with universal tabs, all state server-owned and live-mirrored to every browser, editing with a conflict guard, filesystem watching, and deadlight-owned PTYs replacing zellij.

**Architecture:** A per-project `Hub` owns a `Workspace` (panes, tabs, buffers) plus a subscriber list. Browsers open `/ws/{project}/_workspace` (JSON intents up, events down) and one `/ws/{project}/term/{name}` per terminal tab (raw bytes). Every mutation broadcasts to all subscribers, so two browsers mirror like two zellij clients. HTTP stays GET-only — all writes travel over the websocket. Terminal sessions live in a server-side registry spawning `dtach`, so they survive a deadlight restart.

**Tech Stack:** Rust 2021, tungstenite 0.24, portable-pty 0.8, serde 1 + serde_json 1, notify 8.2.0 + notify-debouncer-full 0.7.0, pulldown-cmark 0.13, toml 0.8. Dev: ureq 2, tempfile 3. Runtime: `dtach` 0.9. Frontend: vendored htmx 2.0.4, xterm 5.5.0 + fit addon, highlight.js.

**Spec:** `docs/superpowers/specs/2026-08-16-deadlight-v3-workspace-design.md`

## Global Constraints

- Bind `127.0.0.1` only. **The websocket spawns a shell — never widen the bind.**
- Origin/Host allowlisting already shipped (`src/origin.rs`, commit `b7f8a39`). Every new socket endpoint goes through the same `accept_hdr` check. Do not add a socket that bypasses it.
- HTTP stays **GET-only**. No POST, no request-body parsing. All writes go over the workspace socket.
- `allowed_origins` is readable from `DEADLIGHT_ORIGINS` or global config **only** — never from `{project}/.deadlight/config.toml`.
- Every filesystem path resolves through a confinement check before use: `projects::safe_resolve` for existing targets, `projects::safe_resolve_parent` (Task 4) for creation and rename destinations.
- Session names match `^[A-Za-z0-9_-]{1,32}$`. They land in a socket path and a command line.
- Caps: ≤16 terminal sessions per project, ≤50 open buffers, 1 MB scrollback ring per session, 2 MB file cap for reads *and* writes.
- State lives in `$DEADLIGHT_STATE_DIR` (default `~/.local/state/deadlight/`), mode `0600` files / `0700` dirs. Never inside a project.
- Crate edition stays `2021` (keeps `std::env::set_var` safe in tests).
- Run `cargo test` (never `--release`). All commands from the repo root.
- Debounce intervals must be constructor parameters, not constants, so tests set them to zero.
- Panics must never escape a socket thread; malformed input produces an `Error` event.

## File Structure

| File | Responsibility |
|---|---|
| `src/proto.rs` (new) | Wire types: `Intent`, `Event`, serde tagging, decode robustness |
| `src/workspace.rs` (new) | `Workspace`/`Pane`/`Tab`/`Buffer` + **pure** intent transitions |
| `src/wsstate.rs` (new) | State-dir resolution, persist/load, corruption handling |
| `src/fileops.rs` (new) | Parent-canonicalizing resolver, create/delete/rename, atomic save, conflict detection |
| `src/hub.rs` (new) | Per-project registry: workspace + subscribers + broadcast + intent dispatch |
| `src/wsconn.rs` (new) | `/ws/{project}/_workspace` connection handler |
| `src/session.rs` (new) | Terminal session registry: PTY, scrollback ring, subscribers, dtach spawn |
| `src/watch.rs` (new) | Pure path classifier + notify wiring + self-write suppression |
| `src/term.rs` (modify) | Reduced to the per-connection byte pump against `session::Registry` |
| `src/projects.rs` (modify) | Add `safe_resolve_parent` |
| `src/routes.rs` (modify) | Route `/ws/{project}/_workspace` and `/ws/{project}/term/{name}` |
| `src/lib.rs` (modify) | Module wiring, hub registry, ws dispatch |
| `static/app.js` (rewrite) | Pane/tab chrome from mirrored state, terminal node pooling |
| `static/style.css` (rewrite) | Four-pane grid, dividers, tab strips |

Tasks 1–8 are server-side and each ends green with `cargo test`. Task 9–10 are the client. Task 11 is deployment.

---

### Task 1: `proto` — wire types

**Files:**
- Create: `src/proto.rs`
- Modify: `src/lib.rs` (add `pub mod proto;`), `Cargo.toml` (add `serde_json = "1"`)

**Interfaces:**
- Produces: `proto::PaneId` (`u8` newtype-ish alias with consts), `proto::Mode {Preview, Edit}`, `proto::Tab` enum, `proto::Intent` enum, `proto::Event` enum, `proto::decode(&str) -> Result<Intent, String>`, `proto::encode(&Event) -> String`.

- [ ] **Step 1: Add the dependency**

In `Cargo.toml` under `[dependencies]`:

```toml
serde_json = "1"
```

- [ ] **Step 2: Write `src/proto.rs` with tests only**

```rust
//! Wire types for the workspace socket. Intents travel up, events down.
//! Externally tagged on "t" so the JSON reads like the spec's examples.
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_open_tab() {
        let i = decode(r#"{"t":"OpenTab","pane":2,"tab":{"k":"File","rel":"src/main.rs","mode":"Preview"}}"#).unwrap();
        match i {
            Intent::OpenTab { pane, tab } => {
                assert_eq!(pane, 2);
                assert_eq!(tab, Tab::File { rel: "src/main.rs".into(), mode: Mode::Preview });
            }
            other => panic!("wrong intent: {other:?}"),
        }
    }

    #[test]
    fn decodes_move_and_terminal_tabs() {
        let i = decode(r#"{"t":"MoveTab","from":2,"idx":0,"to":3,"at":1}"#).unwrap();
        assert!(matches!(i, Intent::MoveTab { from: 2, idx: 0, to: 3, at: 1 }));
        let i = decode(r#"{"t":"OpenTab","pane":3,"tab":{"k":"Terminal","session":"shell"}}"#).unwrap();
        assert!(matches!(i, Intent::OpenTab { tab: Tab::Terminal { .. }, .. }));
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        assert!(decode("not json").is_err());
        assert!(decode(r#"{"t":"NoSuchIntent"}"#).is_err());
        assert!(decode(r#"{"t":"MoveTab","from":2}"#).is_err()); // missing fields
        assert!(decode("").is_err());
    }

    #[test]
    fn encodes_events_with_tag() {
        let s = encode(&Event::Error { msg: "bad".into() });
        assert!(s.contains(r#""t":"Error""#));
        assert!(s.contains(r#""msg":"bad""#));
        let s = encode(&Event::TreeChanged);
        assert_eq!(s, r#"{"t":"TreeChanged"}"#);
    }

    #[test]
    fn diff_tab_none_is_the_full_diff_entry() {
        let i = decode(r#"{"t":"OpenTab","pane":2,"tab":{"k":"Diff","rel":null}}"#).unwrap();
        assert!(matches!(i, Intent::OpenTab { tab: Tab::Diff { rel: None }, .. }));
    }
}
```

- [ ] **Step 3: Run, expect compile failure**

Run: `cargo test proto`
Expected: FAIL — `decode`, `Intent`, `Tab` not found.

- [ ] **Step 4: Add the implementation above the test module**

```rust
pub type PaneId = u8;
pub const LEFT_TOP: PaneId = 0;
pub const LEFT_BOTTOM: PaneId = 1;
pub const MIDDLE: PaneId = 2;
pub const RIGHT: PaneId = 3;
pub const PANE_COUNT: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Preview,
    Edit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k")]
pub enum Tab {
    Tree,
    Changes,
    File { rel: String, mode: Mode },
    Diff { rel: Option<String> },
    Terminal { session: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sizes {
    pub left_w: u32,
    pub right_w: u32,
    pub left_split: u32,
}

impl Default for Sizes {
    fn default() -> Self {
        Sizes { left_w: 260, right_w: 520, left_split: 60 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "t")]
pub enum Intent {
    OpenTab { pane: PaneId, tab: Tab },
    CloseTab { pane: PaneId, idx: usize },
    ActivateTab { pane: PaneId, idx: usize },
    MoveTab { from: PaneId, idx: usize, to: PaneId, at: usize },
    Resize { sizes: Sizes },
    SetMode { rel: String, mode: Mode },
    EditBuffer { rel: String, text: String },
    SaveBuffer { rel: String, force: bool },
    CloseBuffer { rel: String },
    CreateFile { rel: String },
    CreateDir { rel: String },
    DeleteFile { rel: String },
    RenamePath { from: String, to: String },
    RequestState,
}

/// Snapshot sent as `Event::State`. Deliberately carries buffer *metadata*
/// only — text moves in `BufferText`, or every keystroke would rebroadcast
/// every open buffer to every client.
#[derive(Debug, Clone, Serialize)]
pub struct BufferMeta {
    pub rel: String,
    pub dirty: bool,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PaneView {
    pub tabs: Vec<Tab>,
    pub active: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceView {
    pub sizes: Sizes,
    pub panes: Vec<PaneView>,
    pub buffers: Vec<BufferMeta>,
    pub watch_degraded: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t")]
pub enum Event {
    State { version: u64, origin: String, ws: WorkspaceView },
    BufferText { rel: String, text: String, origin: String },
    BufferStale { rel: String },
    SaveOk { rel: String },
    SaveConflict { rel: String, diff_html: String },
    FileChanged { rel: String },
    TreeChanged,
    StatusChanged,
    Error { msg: String },
}

pub fn decode(s: &str) -> Result<Intent, String> {
    serde_json::from_str(s).map_err(|e| e.to_string())
}

pub fn encode(e: &Event) -> String {
    serde_json::to_string(e).unwrap_or_else(|_| r#"{"t":"Error","msg":"encode failed"}"#.into())
}
```

- [ ] **Step 5: Run tests, expect pass**

Run: `cargo test proto`
Expected: 5 passed

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/proto.rs src/lib.rs
git commit -m "v3: workspace wire protocol types"
```

---

### Task 2: `workspace` — state and pure transitions

**Files:**
- Create: `src/workspace.rs`
- Modify: `src/lib.rs` (add `pub mod workspace;`)

**Interfaces:**
- Consumes: `proto::{Intent, Tab, Mode, Sizes, PaneId, PANE_COUNT}`.
- Produces: `workspace::Buffer { text, base_mtime, base_hash, dirty, stale }`, `workspace::Pane { tabs, active }`, `workspace::Workspace { version, sizes, panes, buffers, watch_degraded }`, `workspace::Workspace::default_layout() -> Workspace`, `workspace::apply_layout(&mut Workspace, &Intent) -> Result<bool, String>` (Ok(true) = state changed), `workspace::Workspace::view() -> proto::WorkspaceView`, `workspace::Workspace::find_tab(&Tab) -> Option<(PaneId, usize)>`.

Only the pure, non-I/O intents are handled here. `SaveBuffer`, `CreateFile`, `CreateDir`, `DeleteFile`, `RenamePath` are dispatched by the hub (Task 5) to `fileops` (Task 4).

- [ ] **Step 1: Write `src/workspace.rs` with tests only**

```rust
//! Workspace state and its pure transitions. No I/O lives here — that is
//! exactly what makes the transition table cheap to test.
use crate::proto::{self, Intent, Mode, PaneId, Sizes, Tab, PANE_COUNT};
use std::collections::HashMap;
use std::time::SystemTime;

#[cfg(test)]
mod tests {
    use super::*;

    fn file(rel: &str) -> Tab {
        Tab::File { rel: rel.into(), mode: Mode::Preview }
    }

    #[test]
    fn default_layout_matches_the_spec() {
        let w = Workspace::default_layout();
        assert_eq!(w.panes.len(), PANE_COUNT);
        assert_eq!(w.panes[proto::LEFT_TOP as usize].tabs, vec![Tab::Tree]);
        assert_eq!(w.panes[proto::LEFT_BOTTOM as usize].tabs, vec![Tab::Changes]);
        assert!(w.panes[proto::MIDDLE as usize].tabs.is_empty());
        assert_eq!(
            w.panes[proto::RIGHT as usize].tabs,
            vec![Tab::Terminal { session: "shell".into() }]
        );
    }

    #[test]
    fn open_tab_appends_and_activates() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("a.rs") }).unwrap();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("b.rs") }).unwrap();
        let p = &w.panes[proto::MIDDLE as usize];
        assert_eq!(p.tabs.len(), 2);
        assert_eq!(p.active, 1);
    }

    #[test]
    fn opening_an_already_open_tab_focuses_it_instead_of_duplicating() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("a.rs") }).unwrap();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("b.rs") }).unwrap();
        // reopening a.rs, even targeting a different pane, focuses the existing tab
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::RIGHT, tab: file("a.rs") }).unwrap();
        assert_eq!(w.panes[proto::MIDDLE as usize].tabs.len(), 2);
        assert_eq!(w.panes[proto::MIDDLE as usize].active, 0);
        assert_eq!(w.panes[proto::RIGHT as usize].tabs.len(), 1); // unchanged
    }

    #[test]
    fn two_terminals_with_different_names_coexist() {
        let mut w = Workspace::default_layout();
        let t = Tab::Terminal { session: "claude".into() };
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::RIGHT, tab: t }).unwrap();
        assert_eq!(w.panes[proto::RIGHT as usize].tabs.len(), 2);
    }

    #[test]
    fn closing_the_active_tab_clamps_the_index() {
        let mut w = Workspace::default_layout();
        for n in ["a.rs", "b.rs", "c.rs"] {
            apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file(n) }).unwrap();
        }
        assert_eq!(w.panes[proto::MIDDLE as usize].active, 2);
        apply_layout(&mut w, &Intent::CloseTab { pane: proto::MIDDLE, idx: 2 }).unwrap();
        assert_eq!(w.panes[proto::MIDDLE as usize].active, 1, "active must not dangle past the end");
        apply_layout(&mut w, &Intent::CloseTab { pane: proto::MIDDLE, idx: 0 }).unwrap();
        assert_eq!(w.panes[proto::MIDDLE as usize].tabs.len(), 1);
        assert_eq!(w.panes[proto::MIDDLE as usize].active, 0);
    }

    #[test]
    fn move_tab_between_panes_preserves_the_tab() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("a.rs") }).unwrap();
        apply_layout(
            &mut w,
            &Intent::MoveTab { from: proto::MIDDLE, idx: 0, to: proto::RIGHT, at: 0 },
        )
        .unwrap();
        assert!(w.panes[proto::MIDDLE as usize].tabs.is_empty());
        assert_eq!(w.panes[proto::RIGHT as usize].tabs[0], file("a.rs"));
    }

    #[test]
    fn out_of_range_intents_error_rather_than_panic() {
        let mut w = Workspace::default_layout();
        assert!(apply_layout(&mut w, &Intent::CloseTab { pane: 99, idx: 0 }).is_err());
        assert!(apply_layout(&mut w, &Intent::CloseTab { pane: proto::MIDDLE, idx: 7 }).is_err());
        assert!(apply_layout(
            &mut w,
            &Intent::MoveTab { from: proto::MIDDLE, idx: 0, to: proto::RIGHT, at: 0 }
        )
        .is_err()); // middle is empty
        assert!(apply_layout(&mut w, &Intent::ActivateTab { pane: proto::MIDDLE, idx: 3 }).is_err());
    }

    #[test]
    fn set_mode_rewrites_the_matching_file_tab() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("a.rs") }).unwrap();
        apply_layout(&mut w, &Intent::SetMode { rel: "a.rs".into(), mode: Mode::Edit }).unwrap();
        assert_eq!(
            w.panes[proto::MIDDLE as usize].tabs[0],
            Tab::File { rel: "a.rs".into(), mode: Mode::Edit }
        );
    }

    #[test]
    fn edit_buffer_marks_dirty_and_caps_buffer_count() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::EditBuffer { rel: "a.rs".into(), text: "hi".into() }).unwrap();
        assert!(w.buffers["a.rs"].dirty);
        assert_eq!(w.buffers["a.rs"].text, "hi");
        for i in 0..MAX_BUFFERS + 5 {
            let _ = apply_layout(
                &mut w,
                &Intent::EditBuffer { rel: format!("f{i}.rs"), text: "x".into() },
            );
        }
        assert!(w.buffers.len() <= MAX_BUFFERS, "buffer count must stay capped");
    }

    #[test]
    fn view_exposes_metadata_without_text() {
        let mut w = Workspace::default_layout();
        apply_layout(&mut w, &Intent::EditBuffer { rel: "a.rs".into(), text: "secret".into() })
            .unwrap();
        let v = w.view();
        assert_eq!(v.buffers.len(), 1);
        assert_eq!(v.buffers[0].rel, "a.rs");
        assert!(v.buffers[0].dirty);
        let json = serde_json::to_string(&v).unwrap();
        assert!(!json.contains("secret"), "State must never carry buffer text");
    }
}
```

- [ ] **Step 2: Add `pub mod workspace;` to `src/lib.rs`, run, expect compile failure**

Run: `cargo test workspace`
Expected: FAIL — `Workspace`, `apply_layout` not found.

- [ ] **Step 3: Add the implementation above the test module**

```rust
pub const MAX_BUFFERS: usize = 50;

#[derive(Debug, Clone)]
pub struct Buffer {
    pub text: String,
    pub base_mtime: Option<SystemTime>,
    pub base_hash: u64,
    pub dirty: bool,
    pub stale: bool,
}

impl Default for Buffer {
    fn default() -> Self {
        Buffer { text: String::new(), base_mtime: None, base_hash: 0, dirty: false, stale: false }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Pane {
    pub tabs: Vec<Tab>,
    pub active: usize,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub version: u64,
    pub sizes: Sizes,
    pub panes: Vec<Pane>,
    pub buffers: HashMap<String, Buffer>,
    pub watch_degraded: bool,
}

/// Stable content hash used as the conflict guard. FNV-1a: no dependency,
/// and collision risk on a save-conflict check is not a security boundary.
pub fn hash_text(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

impl Workspace {
    pub fn default_layout() -> Workspace {
        let mut panes = vec![Pane::default(); PANE_COUNT];
        panes[proto::LEFT_TOP as usize].tabs = vec![Tab::Tree];
        panes[proto::LEFT_BOTTOM as usize].tabs = vec![Tab::Changes];
        panes[proto::RIGHT as usize].tabs = vec![Tab::Terminal { session: "shell".into() }];
        Workspace {
            version: 0,
            sizes: Sizes::default(),
            panes,
            buffers: HashMap::new(),
            watch_degraded: false,
        }
    }

    pub fn find_tab(&self, want: &Tab) -> Option<(PaneId, usize)> {
        for (pi, p) in self.panes.iter().enumerate() {
            if let Some(ti) = p.tabs.iter().position(|t| tab_identity_eq(t, want)) {
                return Some((pi as PaneId, ti));
            }
        }
        None
    }

    pub fn view(&self) -> proto::WorkspaceView {
        let mut buffers: Vec<proto::BufferMeta> = self
            .buffers
            .iter()
            .map(|(rel, b)| proto::BufferMeta {
                rel: rel.clone(),
                dirty: b.dirty,
                stale: b.stale,
            })
            .collect();
        buffers.sort_by(|a, b| a.rel.cmp(&b.rel)); // deterministic for tests and diffing
        proto::WorkspaceView {
            sizes: self.sizes,
            panes: self
                .panes
                .iter()
                .map(|p| proto::PaneView { tabs: p.tabs.clone(), active: p.active })
                .collect(),
            buffers,
            watch_degraded: self.watch_degraded,
        }
    }
}

/// Two tabs are "the same tab" when they address the same thing. A File tab
/// differing only in Mode is still the same file, so switching to Edit must
/// not open a second tab.
fn tab_identity_eq(a: &Tab, b: &Tab) -> bool {
    match (a, b) {
        (Tab::File { rel: x, .. }, Tab::File { rel: y, .. }) => x == y,
        (Tab::Diff { rel: x }, Tab::Diff { rel: y }) => x == y,
        (Tab::Terminal { session: x }, Tab::Terminal { session: y }) => x == y,
        (Tab::Tree, Tab::Tree) | (Tab::Changes, Tab::Changes) => true,
        _ => false,
    }
}

fn pane_mut(w: &mut Workspace, id: PaneId) -> Result<&mut Pane, String> {
    w.panes.get_mut(id as usize).ok_or_else(|| format!("no pane {id}"))
}

/// Apply a pure (no-I/O) intent. `Ok(true)` means state changed and the hub
/// should bump the version and broadcast. I/O intents are the hub's job.
pub fn apply_layout(w: &mut Workspace, intent: &Intent) -> Result<bool, String> {
    match intent {
        Intent::OpenTab { pane, tab } => {
            if let Some((pi, ti)) = w.find_tab(tab) {
                pane_mut(w, pi)?.active = ti;
                return Ok(true);
            }
            let p = pane_mut(w, *pane)?;
            p.tabs.push(tab.clone());
            p.active = p.tabs.len() - 1;
            Ok(true)
        }
        Intent::CloseTab { pane, idx } => {
            let p = pane_mut(w, *pane)?;
            if *idx >= p.tabs.len() {
                return Err(format!("no tab {idx}"));
            }
            p.tabs.remove(*idx);
            p.active = p.active.min(p.tabs.len().saturating_sub(1));
            Ok(true)
        }
        Intent::ActivateTab { pane, idx } => {
            let p = pane_mut(w, *pane)?;
            if *idx >= p.tabs.len() {
                return Err(format!("no tab {idx}"));
            }
            p.active = *idx;
            Ok(true)
        }
        Intent::MoveTab { from, idx, to, at } => {
            let src = pane_mut(w, *from)?;
            if *idx >= src.tabs.len() {
                return Err(format!("no tab {idx}"));
            }
            let tab = src.tabs.remove(*idx);
            src.active = src.active.min(src.tabs.len().saturating_sub(1));
            let dst = pane_mut(w, *to)?;
            let at = (*at).min(dst.tabs.len());
            dst.tabs.insert(at, tab);
            dst.active = at;
            Ok(true)
        }
        Intent::Resize { sizes } => {
            w.sizes = *sizes;
            Ok(true)
        }
        Intent::SetMode { rel, mode } => {
            let mut hit = false;
            for p in w.panes.iter_mut() {
                for t in p.tabs.iter_mut() {
                    if let Tab::File { rel: r, mode: m } = t {
                        if r == rel {
                            *m = *mode;
                            hit = true;
                        }
                    }
                }
            }
            if hit {
                Ok(true)
            } else {
                Err(format!("no file tab for {rel}"))
            }
        }
        Intent::EditBuffer { rel, text } => {
            if !w.buffers.contains_key(rel) && w.buffers.len() >= MAX_BUFFERS {
                return Err("too many open buffers".into());
            }
            let b = w.buffers.entry(rel.clone()).or_default();
            b.text = text.clone();
            b.dirty = true;
            Ok(true)
        }
        Intent::CloseBuffer { rel } => {
            w.buffers.remove(rel);
            Ok(true)
        }
        // I/O intents are dispatched by the hub, not here.
        _ => Ok(false),
    }
}
```

- [ ] **Step 4: Run tests, expect pass**

Run: `cargo test workspace`
Expected: 10 passed

- [ ] **Step 5: Commit**

```bash
git add src/workspace.rs src/lib.rs
git commit -m "v3: workspace state and pure intent transitions"
```

---

### Task 3: `wsstate` — persistence

**Files:**
- Create: `src/wsstate.rs`
- Modify: `src/lib.rs` (add `pub mod wsstate;`)

**Interfaces:**
- Consumes: `workspace::{Workspace, Buffer, Pane}`, `proto::{Tab, Sizes}`.
- Produces: `wsstate::state_dir() -> PathBuf` (honours `DEADLIGHT_STATE_DIR`), `wsstate::save(project: &str, w: &Workspace) -> Result<(), String>`, `wsstate::load(project: &str) -> (Workspace, Option<String>)` (warning on corruption, never a panic).

- [ ] **Step 1: Write `src/wsstate.rs` with tests only**

```rust
//! Workspace persistence. Lives in $DEADLIGHT_STATE_DIR, never inside a
//! project — following zellij, so pane drags never show up in git status.
use crate::proto::{Sizes, Tab};
use crate::workspace::{Buffer, Pane, Workspace};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{self, Mode};

    fn with_state_dir<T>(f: impl FnOnce() -> T) -> T {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path());
        let out = f();
        std::env::remove_var("DEADLIGHT_STATE_DIR");
        out
    }

    #[test]
    fn round_trips_layout_and_buffers() {
        with_state_dir(|| {
            let mut w = Workspace::default_layout();
            w.sizes = Sizes { left_w: 111, right_w: 222, left_split: 33 };
            w.panes[proto::MIDDLE as usize].tabs =
                vec![Tab::File { rel: "a.rs".into(), mode: Mode::Edit }];
            w.buffers.insert(
                "a.rs".into(),
                Buffer { text: "unsaved".into(), dirty: true, ..Buffer::default() },
            );
            save("proj", &w).unwrap();

            let (got, warn) = load("proj");
            assert!(warn.is_none());
            assert_eq!(got.sizes.left_w, 111);
            assert_eq!(got.panes[proto::MIDDLE as usize].tabs.len(), 1);
            assert_eq!(got.buffers["a.rs"].text, "unsaved", "unsaved text is crash-safe");
            assert!(got.buffers["a.rs"].dirty);
        });
    }

    #[test]
    fn missing_file_yields_defaults_without_warning() {
        with_state_dir(|| {
            let (w, warn) = load("never-saved");
            assert!(warn.is_none());
            assert_eq!(w.panes[proto::LEFT_TOP as usize].tabs, vec![Tab::Tree]);
        });
    }

    #[test]
    fn corrupt_file_yields_defaults_with_a_warning() {
        with_state_dir(|| {
            std::fs::create_dir_all(state_dir()).unwrap();
            std::fs::write(state_dir().join("broken.json"), "{ not json").unwrap();
            let (w, warn) = load("broken");
            assert!(warn.is_some(), "corruption must be visible, not silent");
            assert_eq!(w.panes.len(), proto::PANE_COUNT);
        });
    }

    #[test]
    fn state_file_is_not_world_readable() {
        with_state_dir(|| {
            save("proj", &Workspace::default_layout()).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let m = std::fs::metadata(state_dir().join("proj.json")).unwrap();
                assert_eq!(m.permissions().mode() & 0o077, 0, "buffer text may be secret");
            }
        });
    }
}
```

- [ ] **Step 2: Add `pub mod wsstate;` to `src/lib.rs`, run, expect compile failure**

Run: `cargo test wsstate`
Expected: FAIL — `save`, `load`, `state_dir` not found.

- [ ] **Step 3: Add the implementation above the test module**

```rust
#[derive(Serialize, Deserialize)]
struct PaneDisk {
    tabs: Vec<Tab>,
    active: usize,
}

#[derive(Serialize, Deserialize)]
struct BufferDisk {
    text: String,
    dirty: bool,
}

#[derive(Serialize, Deserialize)]
struct Disk {
    sizes: Sizes,
    panes: Vec<PaneDisk>,
    buffers: std::collections::HashMap<String, BufferDisk>,
}

pub fn state_dir() -> PathBuf {
    if let Ok(d) = std::env::var("DEADLIGHT_STATE_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state/deadlight")
}

fn path_for(project: &str) -> PathBuf {
    state_dir().join(format!("{project}.json"))
}

pub fn save(project: &str, w: &Workspace) -> Result<(), String> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let disk = Disk {
        sizes: w.sizes,
        panes: w
            .panes
            .iter()
            .map(|p| PaneDisk { tabs: p.tabs.clone(), active: p.active })
            .collect(),
        buffers: w
            .buffers
            .iter()
            .map(|(k, b)| (k.clone(), BufferDisk { text: b.text.clone(), dirty: b.dirty }))
            .collect(),
    };
    let json = serde_json::to_string(&disk).map_err(|e| e.to_string())?;
    let tmp = path_for(project).with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path_for(project)).map_err(|e| e.to_string())
}

pub fn load(project: &str) -> (Workspace, Option<String>) {
    let mut w = Workspace::default_layout();
    let Ok(text) = std::fs::read_to_string(path_for(project)) else {
        return (w, None); // never saved is normal, not a warning
    };
    match serde_json::from_str::<Disk>(&text) {
        Ok(d) => {
            w.sizes = d.sizes;
            if d.panes.len() == w.panes.len() {
                w.panes = d
                    .panes
                    .into_iter()
                    .map(|p| {
                        let active = p.active.min(p.tabs.len().saturating_sub(1));
                        Pane { tabs: p.tabs, active }
                    })
                    .collect();
            }
            for (k, b) in d.buffers {
                w.buffers.insert(
                    k,
                    Buffer {
                        base_hash: crate::workspace::hash_text(&b.text),
                        text: b.text,
                        dirty: b.dirty,
                        ..Buffer::default()
                    },
                );
            }
            (w, None)
        }
        Err(e) => (w, Some(format!("workspace state unreadable: {e}"))),
    }
}
```

- [ ] **Step 4: Run tests, expect pass**

Run: `cargo test wsstate`
Expected: 4 passed

- [ ] **Step 5: Commit**

```bash
git add src/wsstate.rs src/lib.rs
git commit -m "v3: workspace persistence with corruption tolerance"
```

---

### Task 4: `fileops` — creation resolver, file operations, atomic save

**Files:**
- Create: `src/fileops.rs`
- Modify: `src/projects.rs` (add `safe_resolve_parent`), `src/lib.rs`

**Interfaces:**
- Consumes: `projects::safe_resolve`.
- Produces: `projects::safe_resolve_parent(project_dir: &Path, rel: &str) -> Result<PathBuf, String>`; `fileops::SaveOutcome { Written, Conflict { disk_text: String } }`; `fileops::save(project_dir, rel, text, base_hash, force) -> Result<SaveOutcome, String>`; `fileops::create_file(project_dir, rel)`, `create_dir`, `delete(project_dir, rel)`, `rename(project_dir, from, to)` — all `Result<PathBuf, String>`.

- [ ] **Step 1: Add tests for `safe_resolve_parent` to `src/projects.rs`**

Append inside the existing `mod tests`:

```rust
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
        assert!(safe_resolve_parent(&alpha, "sub/../../out.txt").is_err());
        // a missing parent directory is an error, not a silent mkdir -p
        assert!(safe_resolve_parent(&alpha, "nodir/new.txt").is_err());
    }
```

- [ ] **Step 2: Run, expect failure**

Run: `cargo test projects::tests::safe_resolve_parent`
Expected: FAIL — `safe_resolve_parent` not found.

- [ ] **Step 3: Implement `safe_resolve_parent` in `src/projects.rs`**

Add next to `safe_resolve`:

```rust
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
```

Note the absolute-path case: `/etc/newfile` has its leading `/` stripped, so it resolves under the project and its parent `etc` will not exist — an error either way. The test asserts the behaviour, not the mechanism.

- [ ] **Step 4: Run, expect pass**

Run: `cargo test projects`
Expected: all pass (6 tests)

- [ ] **Step 5: Write `src/fileops.rs` with tests only**

```rust
//! File mutations: creation, deletion, rename, and the conflict-guarded
//! atomic save. Every path here is confined before use.
use crate::projects::{safe_resolve, safe_resolve_parent};
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn proj() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.txt"), "one\n").unwrap();
        fs::create_dir(d.path().join("sub")).unwrap();
        d
    }

    #[test]
    fn save_writes_when_the_base_hash_matches() {
        let d = proj();
        let base = crate::workspace::hash_text("one\n");
        let out = save(d.path(), "a.txt", "two\n", base, false).unwrap();
        assert!(matches!(out, SaveOutcome::Written));
        assert_eq!(fs::read_to_string(d.path().join("a.txt")).unwrap(), "two\n");
    }

    #[test]
    fn save_refuses_when_disk_changed_underneath() {
        let d = proj();
        let stale = crate::workspace::hash_text("what the buffer was opened with\n");
        let out = save(d.path(), "a.txt", "mine\n", stale, false).unwrap();
        match out {
            SaveOutcome::Conflict { disk_text } => assert_eq!(disk_text, "one\n"),
            SaveOutcome::Written => panic!("stale save must not clobber"),
        }
        assert_eq!(
            fs::read_to_string(d.path().join("a.txt")).unwrap(),
            "one\n",
            "the file must be untouched after a refused save"
        );
    }

    #[test]
    fn force_overrides_the_conflict() {
        let d = proj();
        let stale = crate::workspace::hash_text("stale\n");
        let out = save(d.path(), "a.txt", "mine\n", stale, true).unwrap();
        assert!(matches!(out, SaveOutcome::Written));
        assert_eq!(fs::read_to_string(d.path().join("a.txt")).unwrap(), "mine\n");
    }

    #[test]
    fn save_preserves_file_mode() {
        let d = proj();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = d.path().join("a.txt");
            fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
            let base = crate::workspace::hash_text("one\n");
            save(d.path(), "a.txt", "two\n", base, false).unwrap();
            let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o640, "atomic rename must not reset permissions");
        }
    }

    #[test]
    fn save_is_confined() {
        let d = proj();
        assert!(save(d.path(), "../outside.txt", "x", 0, true).is_err());
    }

    #[test]
    fn create_delete_rename_round_trip() {
        let d = proj();
        create_file(d.path(), "new.txt").unwrap();
        assert!(d.path().join("new.txt").is_file());
        create_dir(d.path(), "sub/deeper").unwrap();
        assert!(d.path().join("sub/deeper").is_dir());
        rename(d.path(), "new.txt", "sub/moved.txt").unwrap();
        assert!(d.path().join("sub/moved.txt").is_file());
        delete(d.path(), "sub/moved.txt").unwrap();
        assert!(!d.path().join("sub/moved.txt").exists());
    }

    #[test]
    fn create_file_refuses_to_clobber() {
        let d = proj();
        assert!(create_file(d.path(), "a.txt").is_err(), "must not truncate an existing file");
    }

    #[test]
    fn delete_is_non_recursive() {
        let d = proj();
        std::fs::write(d.path().join("sub/inner.txt"), "x").unwrap();
        assert!(delete(d.path(), "sub").is_err(), "a misclick must not take out a tree");
        assert!(d.path().join("sub/inner.txt").exists());
        // an empty directory is fine
        std::fs::create_dir(d.path().join("empty")).unwrap();
        assert!(delete(d.path(), "empty").is_ok());
    }

    #[test]
    fn operations_are_confined() {
        let d = proj();
        assert!(create_file(d.path(), "../evil.txt").is_err());
        assert!(delete(d.path(), "../../etc/passwd").is_err());
        assert!(rename(d.path(), "a.txt", "../evil.txt").is_err());
    }
}
```

- [ ] **Step 6: Add `pub mod fileops;` to `src/lib.rs`, run, expect compile failure**

Run: `cargo test fileops`
Expected: FAIL — `save`, `create_file` not found.

- [ ] **Step 7: Add the implementation above the test module**

```rust
const MAX_WRITE_BYTES: usize = 2_000_000;

pub enum SaveOutcome {
    Written,
    Conflict { disk_text: String },
}

/// Write atomically: temp file in the same directory, copy the original's
/// mode, then rename. Never truncate in place — a crash mid-save must not
/// leave a half-written source file.
fn atomic_write(path: &Path, text: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent directory")?;
    let tmp = dir.join(format!(
        ".{}.deadlight.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("buf")
    ));
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode();
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
        }
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

pub fn save(
    project_dir: &Path,
    rel: &str,
    text: &str,
    base_hash: u64,
    force: bool,
) -> Result<SaveOutcome, String> {
    if text.len() > MAX_WRITE_BYTES {
        return Err(format!("file too large ({} bytes)", text.len()));
    }
    let abs = safe_resolve(project_dir, rel)?;
    let disk = std::fs::read_to_string(&abs).map_err(|e| e.to_string())?;
    if !force && crate::workspace::hash_text(&disk) != base_hash {
        return Ok(SaveOutcome::Conflict { disk_text: disk });
    }
    atomic_write(&abs, text)?;
    Ok(SaveOutcome::Written)
}

pub fn create_file(project_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let abs = safe_resolve_parent(project_dir, rel)?;
    if abs.exists() {
        return Err(format!("already exists: {rel}"));
    }
    std::fs::write(&abs, "").map_err(|e| e.to_string())?;
    Ok(abs)
}

pub fn create_dir(project_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let abs = safe_resolve_parent(project_dir, rel)?;
    if abs.exists() {
        return Err(format!("already exists: {rel}"));
    }
    std::fs::create_dir(&abs).map_err(|e| e.to_string())?;
    Ok(abs)
}

/// Non-recursive by design: files and empty directories only. Not because
/// recursive delete is an escalation — the terminal is right there — but so a
/// misclick in a tree cannot remove `target/` or `.git`.
pub fn delete(project_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let abs = safe_resolve(project_dir, rel)?;
    let meta = std::fs::metadata(&abs).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        std::fs::remove_dir(&abs).map_err(|_| format!("directory not empty: {rel}"))?;
    } else {
        std::fs::remove_file(&abs).map_err(|e| e.to_string())?;
    }
    Ok(abs)
}

pub fn rename(project_dir: &Path, from: &str, to: &str) -> Result<PathBuf, String> {
    let src = safe_resolve(project_dir, from)?;
    let dst = safe_resolve_parent(project_dir, to)?;
    if dst.exists() {
        return Err(format!("already exists: {to}"));
    }
    std::fs::rename(&src, &dst).map_err(|e| e.to_string())?;
    Ok(dst)
}
```

- [ ] **Step 8: Run tests, expect pass**

Run: `cargo test fileops`
Expected: 9 passed

- [ ] **Step 9: Commit**

```bash
git add src/fileops.rs src/projects.rs src/lib.rs
git commit -m "v3: file operations, creation-safe resolver, conflict-guarded atomic save"
```

---

### Task 5: `hub` — per-project state, subscribers, broadcast

**Files:**
- Create: `src/hub.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `workspace`, `wsstate`, `fileops`, `proto`, `render::diff_html`.
- Produces: `hub::ConnId(String)`; `hub::Hub` with `Hub::for_project(name: &str, dir: PathBuf) -> Arc<Mutex<Hub>>` (process-wide registry, one hub per project); `Hub::subscribe(&mut self) -> (ConnId, Receiver<String>)`; `Hub::unsubscribe(&mut self, &ConnId)`; `Hub::handle(&mut self, &ConnId, Intent)`; `Hub::broadcast(&mut self, &Event)`; `Hub::send_to(&mut self, &ConnId, &Event)`; `Hub::snapshot_event(&self, origin: &ConnId) -> Event`.

Subscribers are `std::sync::mpsc::Sender<String>` holding pre-encoded JSON; each socket thread owns the matching receiver and writes frames. Dead senders are pruned on send, which is how disconnects are noticed without a separate reaper.

- [ ] **Step 1: Write `src/hub.rs` with tests only**

```rust
//! One Hub per project: owns the Workspace, the subscriber list, and the
//! dispatch from intent to either a pure transition or a file operation.
//! Everything the sockets do goes through here, so mirroring is automatic.
use crate::proto::{Event, Intent};
use crate::workspace::{self, Workspace};
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{self, Mode, Tab};

    // Helper: drain whatever a receiver has without blocking.
    fn drain(rx: &Receiver<String>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(m) = rx.try_recv() {
            out.push(m);
        }
        out
    }

    #[test]
    fn a_mutation_reaches_every_subscriber() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (_a, rx_a) = h.subscribe();
        let (b, rx_b) = h.subscribe();
        drain(&rx_a);
        drain(&rx_b);

        h.handle(&b, Intent::OpenTab {
            pane: proto::MIDDLE,
            tab: Tab::File { rel: "a.txt".into(), mode: Mode::Preview },
        });

        let to_a = drain(&rx_a);
        let to_b = drain(&rx_b);
        assert!(to_a.iter().any(|m| m.contains(r#""t":"State""#)), "the other client must mirror");
        assert!(to_b.iter().any(|m| m.contains(r#""t":"State""#)), "originator sees it too");
        assert!(to_a.iter().any(|m| m.contains("a.txt")));
    }

    #[test]
    fn buffer_text_is_not_echoed_to_its_author() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (a, rx_a) = h.subscribe();
        let (_b, rx_b) = h.subscribe();
        drain(&rx_a);
        drain(&rx_b);

        h.handle(&a, Intent::EditBuffer { rel: "a.txt".into(), text: "typed".into() });

        let to_a = drain(&rx_a);
        let to_b = drain(&rx_b);
        assert!(
            !to_a.iter().any(|m| m.contains(r#""t":"BufferText""#)),
            "echoing text back stomps the author's cursor"
        );
        assert!(to_b.iter().any(|m| m.contains("typed")), "other clients must receive the text");
    }

    #[test]
    fn version_advances_on_change_only() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        drain(&rx);
        let before = h.ws.version;
        h.handle(&c, Intent::ActivateTab { pane: proto::MIDDLE, idx: 9 }); // invalid
        assert_eq!(h.ws.version, before, "a rejected intent must not bump the version");
        assert!(drain(&rx).iter().any(|m| m.contains(r#""t":"Error""#)));
    }

    #[test]
    fn save_conflict_is_reported_and_the_file_is_untouched() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.txt"), "on disk\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        // buffer opened against different content => base_hash mismatch
        h.handle(&c, Intent::EditBuffer { rel: "a.txt".into(), text: "mine\n".into() });
        drain(&rx);
        h.handle(&c, Intent::SaveBuffer { rel: "a.txt".into(), force: false });
        assert!(drain(&rx).iter().any(|m| m.contains(r#""t":"SaveConflict""#)));
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "on disk\n");
    }

    #[test]
    fn dropped_subscribers_are_pruned() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (a, rx_a) = h.subscribe();
        let (_b, rx_b) = h.subscribe();
        drop(rx_b);
        h.handle(&a, Intent::Resize { sizes: proto::Sizes::default() });
        assert_eq!(h.subs.len(), 1, "a closed socket must not accumulate");
        drop(rx_a);
    }
}
```

- [ ] **Step 2: Add `pub mod hub;` to `src/lib.rs`, run, expect compile failure**

Run: `cargo test hub`
Expected: FAIL — `Hub` not found.

- [ ] **Step 3: Add the implementation above the test module**

```rust
pub type ConnId = String;

pub struct Hub {
    pub project: String,
    pub dir: std::path::PathBuf,
    pub ws: Workspace,
    pub subs: HashMap<ConnId, Sender<String>>,
    next_id: u64,
    /// Paths deadlight itself just wrote, with the resulting hash. The watcher
    /// (Task 8) drops matching events so a save does not echo back.
    pub self_writes: HashMap<String, u64>,
}

static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<Mutex<Hub>>>>> = OnceLock::new();

impl Hub {
    pub fn new(project: &str, dir: std::path::PathBuf) -> Hub {
        let (ws, warn) = crate::wsstate::load(project);
        if let Some(w) = warn {
            eprintln!("deadlight: {w}");
        }
        Hub {
            project: project.to_string(),
            dir,
            ws,
            subs: HashMap::new(),
            next_id: 0,
            self_writes: HashMap::new(),
        }
    }

    /// One hub per project, shared by every connection to it.
    pub fn for_project(project: &str, dir: std::path::PathBuf) -> Arc<Mutex<Hub>> {
        let reg = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
        let mut map = reg.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(project.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(Hub::new(project, dir))))
            .clone()
    }

    pub fn subscribe(&mut self) -> (ConnId, Receiver<String>) {
        self.next_id += 1;
        let id = format!("c{}", self.next_id);
        let (tx, rx) = channel();
        self.subs.insert(id.clone(), tx);
        (id, rx)
    }

    pub fn unsubscribe(&mut self, id: &ConnId) {
        self.subs.remove(id);
    }

    /// Send to everyone; prune receivers that have gone away. That pruning is
    /// how a closed socket is noticed — there is no separate reaper.
    pub fn broadcast(&mut self, ev: &Event) {
        let msg = crate::proto::encode(ev);
        self.subs.retain(|_, tx| tx.send(msg.clone()).is_ok());
    }

    pub fn broadcast_except(&mut self, skip: &ConnId, ev: &Event) {
        let msg = crate::proto::encode(ev);
        self.subs.retain(|id, tx| id == skip || tx.send(msg.clone()).is_ok());
    }

    pub fn send_to(&mut self, id: &ConnId, ev: &Event) {
        let msg = crate::proto::encode(ev);
        if let Some(tx) = self.subs.get(id) {
            if tx.send(msg).is_err() {
                self.subs.remove(id);
            }
        }
    }

    pub fn snapshot_event(&self, origin: &ConnId) -> Event {
        Event::State { version: self.ws.version, origin: origin.clone(), ws: self.ws.view() }
    }

    fn persist(&mut self) {
        if let Err(e) = crate::wsstate::save(&self.project.clone(), &self.ws) {
            eprintln!("deadlight: state save failed: {e}");
        }
    }

    pub fn handle(&mut self, from: &ConnId, intent: Intent) {
        match &intent {
            Intent::RequestState => {
                let ev = self.snapshot_event(from);
                self.send_to(from, &ev);
                return;
            }
            Intent::EditBuffer { rel, text } => {
                // Text goes to everyone *but* the author, so their cursor survives.
                let ev = Event::BufferText {
                    rel: rel.clone(),
                    text: text.clone(),
                    origin: from.clone(),
                };
                if let Err(e) = workspace::apply_layout(&mut self.ws, &intent) {
                    let ev = Event::Error { msg: e };
                    self.send_to(from, &ev);
                    return;
                }
                self.ws.version += 1;
                self.broadcast_except(from, &ev);
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
                self.persist();
                return;
            }
            Intent::SaveBuffer { rel, force } => return self.do_save(from, rel.clone(), *force),
            Intent::CreateFile { rel } => return self.do_fileop(from, crate::fileops::create_file(&self.dir.clone(), rel)),
            Intent::CreateDir { rel } => return self.do_fileop(from, crate::fileops::create_dir(&self.dir.clone(), rel)),
            Intent::DeleteFile { rel } => return self.do_fileop(from, crate::fileops::delete(&self.dir.clone(), rel)),
            Intent::RenamePath { from: f, to } => {
                let r = crate::fileops::rename(&self.dir.clone(), f, to);
                return self.do_fileop(from, r);
            }
            _ => {}
        }
        match workspace::apply_layout(&mut self.ws, &intent) {
            Ok(true) => {
                self.ws.version += 1;
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
                self.persist();
            }
            Ok(false) => {}
            Err(e) => {
                let ev = Event::Error { msg: e };
                self.send_to(from, &ev);
            }
        }
    }

    fn do_fileop(&mut self, from: &ConnId, r: Result<std::path::PathBuf, String>) {
        match r {
            Ok(_) => self.broadcast(&Event::TreeChanged),
            Err(e) => {
                let ev = Event::Error { msg: e };
                self.send_to(from, &ev);
            }
        }
    }

    fn do_save(&mut self, from: &ConnId, rel: String, force: bool) {
        let Some(buf) = self.ws.buffers.get(&rel).cloned() else {
            let ev = Event::Error { msg: format!("no buffer for {rel}") };
            return self.send_to(from, &ev);
        };
        let dir = self.dir.clone();
        match crate::fileops::save(&dir, &rel, &buf.text, buf.base_hash, force) {
            Ok(crate::fileops::SaveOutcome::Written) => {
                let hash = workspace::hash_text(&buf.text);
                if let Some(b) = self.ws.buffers.get_mut(&rel) {
                    b.dirty = false;
                    b.stale = false;
                    b.base_hash = hash;
                    b.base_mtime = std::fs::metadata(dir.join(&rel)).ok().and_then(|m| m.modified().ok());
                }
                self.self_writes.insert(rel.clone(), hash);
                self.ws.version += 1;
                self.broadcast(&Event::SaveOk { rel: rel.clone() });
                self.broadcast(&Event::FileChanged { rel });
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
                self.persist();
            }
            Ok(crate::fileops::SaveOutcome::Conflict { disk_text }) => {
                let diff_html = crate::render::diff_html(&conflict_diff(&disk_text, &buf.text));
                let ev = Event::SaveConflict { rel, diff_html };
                self.send_to(from, &ev);
            }
            Err(e) => {
                let ev = Event::Error { msg: e };
                self.send_to(from, &ev);
            }
        }
    }
}

/// A minimal unified-diff rendering of disk vs buffer. Uses the existing
/// classifier in `render`, so the conflict view looks like every other diff.
fn conflict_diff(disk: &str, buf: &str) -> String {
    let mut out = String::from("--- a/disk\n+++ b/your buffer\n@@ conflict @@\n");
    for l in disk.lines() {
        out.push('-');
        out.push_str(l);
        out.push('\n');
    }
    for l in buf.lines() {
        out.push('+');
        out.push_str(l);
        out.push('\n');
    }
    out
}
```

- [ ] **Step 4: Run tests, expect pass**

Run: `cargo test hub`
Expected: 5 passed

- [ ] **Step 5: Commit**

```bash
git add src/hub.rs src/lib.rs
git commit -m "v3: per-project hub with broadcast, echo rule, and save dispatch"
```

---

### Task 6: `wsconn` — the workspace socket endpoint

**Files:**
- Create: `src/wsconn.rs`
- Modify: `src/lib.rs` (ws dispatch), `tests/integration.rs`

**Interfaces:**
- Consumes: `hub::Hub`, `proto`, `origin::origin_allowed`, `config::allowed_origins`.
- Produces: `wsconn::handle(stream: TcpStream, project: &str, dir: PathBuf)`.

The connection spawns a writer thread draining the subscriber `Receiver` to the socket, and reads intents on the current thread — the same two-direction shape `term.rs` already uses.

- [ ] **Step 1: Add the integration test to `tests/integration.rs`**

```rust
fn ws_connect_path(
    port: u16,
    path: &str,
) -> Result<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>, tungstenite::Error>
{
    use tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{port}{path}").into_client_request().unwrap();
    req.headers_mut().insert("origin", "http://127.0.0.1:8444".parse().unwrap());
    let (ws, _r) = tungstenite::connect(req)?;
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    }
    Ok(ws)
}

fn read_until<'a>(
    ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    needle: &str,
) -> String {
    for _ in 0..40 {
        match ws.read() {
            Ok(tungstenite::Message::Text(t)) => {
                if t.contains(needle) {
                    return t.to_string();
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    panic!("never saw {needle:?}");
}

#[test]
fn workspace_state_mirrors_between_two_clients() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
    let (_d, port) = fixture();
    let mut a = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    let mut b = ws_connect_path(port, "/ws/proj/_workspace").unwrap();

    a.send(tungstenite::Message::Text(
        r#"{"t":"OpenTab","pane":2,"tab":{"k":"File","rel":"hello.md","mode":"Preview"}}"#.into(),
    ))
    .unwrap();

    // the *other* browser must learn about it without asking
    let seen = read_until(&mut b, "hello.md");
    assert!(seen.contains(r#""t":"State""#));
    let _ = a.close(None);
    let _ = b.close(None);
    std::env::remove_var("DEADLIGHT_STATE_DIR");
}

#[test]
fn workspace_socket_rejects_foreign_origin() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let (_d, port) = fixture();
    use tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{port}/ws/proj/_workspace").into_client_request().unwrap();
    req.headers_mut().insert("origin", "https://evil.example.com".parse().unwrap());
    assert!(tungstenite::connect(req).is_err(), "the write socket must not be cross-origin");
}
```

- [ ] **Step 2: Run, expect failure**

Run: `cargo test --test integration workspace_state_mirrors`
Expected: FAIL — the endpoint does not exist, connection refused or closed.

- [ ] **Step 3: Create `src/wsconn.rs`**

```rust
//! The /ws/{project}/_workspace endpoint. Intents up, events down. Two
//! directions over one socket, as term.rs does: a writer thread drains the
//! hub's channel, this thread reads intents.
use crate::hub::Hub;
use crate::proto;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tungstenite::handshake::server::{Request as WsRequest, Response as WsResponse};
use tungstenite::protocol::Role;
use tungstenite::{accept_hdr, Message, WebSocket};

pub fn handle(stream: TcpStream, project: &str, dir: PathBuf) {
    let allowed = crate::config::allowed_origins();
    let accepted = accept_hdr(stream, |req: &WsRequest, resp: WsResponse| {
        let origin = req.headers().get("origin").and_then(|v| v.to_str().ok());
        if !crate::origin::origin_allowed(origin, &allowed) {
            eprintln!("deadlight: rejected workspace ws origin={origin:?}");
            return Err(tungstenite::http::Response::builder()
                .status(403)
                .body(Some("origin not allowed".to_string()))
                .expect("static 403"));
        }
        Ok(resp)
    });
    let Ok(mut ws_read) = accepted else { return };

    let hub: Arc<Mutex<Hub>> = Hub::for_project(project, dir);
    let (id, rx) = {
        let mut h = hub.lock().unwrap_or_else(|e| e.into_inner());
        h.subscribe()
    };

    let Ok(write_half) = ws_read.get_ref().try_clone() else { return };
    let mut ws_write: WebSocket<TcpStream> =
        WebSocket::from_raw_socket(write_half, Role::Server, None);

    let writer = std::thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            if ws_write.send(Message::Text(msg.into())).is_err() {
                break;
            }
        }
        let _ = ws_write.close(None);
        let _ = ws_write.get_ref().shutdown(std::net::Shutdown::Both);
    });

    // Send the current state immediately so a fresh tab renders without asking.
    {
        let mut h = hub.lock().unwrap_or_else(|e| e.into_inner());
        let ev = h.snapshot_event(&id);
        h.send_to(&id, &ev);
    }

    loop {
        match ws_read.read() {
            Ok(Message::Text(t)) => {
                let mut h = hub.lock().unwrap_or_else(|e| e.into_inner());
                match proto::decode(&t) {
                    Ok(intent) => h.handle(&id, intent),
                    Err(e) => {
                        let ev = proto::Event::Error { msg: e };
                        h.send_to(&id, &ev);
                    }
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }

    {
        let mut h = hub.lock().unwrap_or_else(|e| e.into_inner());
        h.unsubscribe(&id);
    }
    let _ = writer.join();
}
```

- [ ] **Step 4: Route it in `src/lib.rs`**

Replace the `is_ws` branch body in `serve` so ws paths are split by shape. Add to `lib.rs`:

```rust
pub mod fileops;
pub mod hub;
pub mod proto;
pub mod session;
pub mod watch;
pub mod workspace;
pub mod wsconn;
pub mod wsstate;
```

and change the dispatch:

```rust
std::thread::spawn(move || {
    if is_ws(&stream) {
        route_ws(stream, &roots);
    } else {
        routes::handle(stream, &roots);
    }
});
```

```rust
/// `/ws/{project}/_workspace` and `/ws/{project}/term/{name}` are peeked
/// apart here so each gets its own handler; both re-check Origin themselves.
fn route_ws(stream: TcpStream, roots: &[PathBuf]) {
    let mut buf = [0u8; 512];
    let Ok(n) = stream.peek(&mut buf) else { return };
    let head = String::from_utf8_lossy(&buf[..n]);
    let Some(target) = head.split_whitespace().nth(1) else { return };
    let segs: Vec<&str> = target.trim_start_matches("/ws/").split('/').collect();
    let Some(project) = segs.first().copied().filter(|s| !s.is_empty()) else { return };
    let Some(dir) = projects::resolve_project(roots, project) else { return };
    match segs.get(1).copied() {
        Some("_workspace") => wsconn::handle(stream, project, dir),
        _ => term::handle_ws(stream, roots),
    }
}
```

- [ ] **Step 5: Run tests, expect pass**

Run: `cargo test`
Expected: all pass, including `workspace_state_mirrors_between_two_clients`

- [ ] **Step 6: Commit**

```bash
git add src/wsconn.rs src/lib.rs tests/integration.rs
git commit -m "v3: workspace socket endpoint with live mirroring"
```

---

### Task 7: `session` — deadlight-owned terminals over dtach

**Files:**
- Create: `src/session.rs`
- Modify: `src/term.rs` (rewrite to use the registry), `src/lib.rs`, `tests/integration.rs`

**Interfaces:**
- Produces: `session::valid_name(&str) -> bool`; `session::default_command(project: &str, name: &str) -> Vec<String>`; `session::min_geometry(&HashMap<u64,(u16,u16)>) -> Option<(u16,u16)>`; `session::push_scrollback(&mut VecDeque<u8>, &[u8])`; `session::attach(project: &str, name: &str, dir: &Path) -> Result<Attachment, String>` where `Attachment { id: u64, key: String, rx: Receiver<Vec<u8>> }`; `session::write_input(key: &str, data: &[u8]) -> Result<(), String>`; `session::resize(key: &str, id: u64, cols: u16, rows: u16)`; `session::detach(key: &str, id: u64)`.

Input is written through `session::write_input` rather than an owned writer, because the PTY writer lives inside the registry's mutex alongside the session it belongs to.

Sessions are keyed `{project}-{name}` and outlive any single attachment. Scrollback is a 1 MB ring replayed on attach. `resize` records each attachment's geometry and applies the **smallest** across attachments.

- [ ] **Step 1: Write `src/session.rs` with tests only**

```rust
//! Terminal session registry. deadlight owns the PTY; dtach owns survival
//! across a deadlight restart. Multiple attachments to one session mirror.
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_names_are_strictly_validated() {
        assert!(valid_name("shell"));
        assert!(valid_name("claude-2"));
        assert!(valid_name("A_b-9"));
        // these land in a socket path and a command line
        assert!(!valid_name(""));
        assert!(!valid_name("../../etc/passwd"));
        assert!(!valid_name("a b"));
        assert!(!valid_name("a;rm -rf /"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(&"x".repeat(33)));
        assert!(valid_name(&"x".repeat(32)));
    }

    #[test]
    fn default_command_wraps_dtach_with_no_ui() {
        let c = default_command("proj", "shell");
        assert_eq!(c[0], "dtach");
        assert!(c.contains(&"-E".to_string()), "no escape character");
        assert!(c.contains(&"-z".to_string()), "no suspend key");
        assert!(c.iter().any(|a| a.contains("proj-shell")), "socket is per project+session");
    }

    #[test]
    fn env_override_replaces_the_command() {
        std::env::set_var("DEADLIGHT_CMD", "cat");
        assert_eq!(default_command("proj", "shell"), vec!["cat".to_string()]);
        std::env::remove_var("DEADLIGHT_CMD");
    }

    #[test]
    fn smallest_attachment_geometry_wins() {
        let mut sizes = HashMap::new();
        sizes.insert(1u64, (100u16, 40u16));
        sizes.insert(2u64, (80u16, 24u16));
        sizes.insert(3u64, (120u16, 50u16));
        assert_eq!(min_geometry(&sizes), Some((80, 24)), "nobody may see clipped output");
        assert_eq!(min_geometry(&HashMap::new()), None);
    }

    #[test]
    fn scrollback_ring_is_bounded() {
        let mut ring = VecDeque::new();
        for _ in 0..(MAX_SCROLLBACK / 10 + 100) {
            push_scrollback(&mut ring, &[b'x'; 10]);
        }
        assert!(ring.len() <= MAX_SCROLLBACK);
    }
}
```

- [ ] **Step 2: Add `pub mod session;` to `src/lib.rs`, run, expect compile failure**

Run: `cargo test session`
Expected: FAIL — `valid_name` not found.

- [ ] **Step 3: Add the implementation above the test module**

```rust
pub const MAX_SCROLLBACK: usize = 1_000_000;
pub const MAX_SESSIONS_PER_PROJECT: usize = 16;

/// Session names land in a dtach socket path and a command line. Anything
/// outside this set is a path-traversal or argument-injection vector.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn default_command(project: &str, name: &str) -> Vec<String> {
    if let Ok(c) = std::env::var("DEADLIGHT_CMD") {
        if !c.trim().is_empty() {
            return c.split_whitespace().map(String::from).collect();
        }
    }
    let sock = crate::wsstate::state_dir().join("sock").join(format!("{project}-{name}"));
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    vec![
        "dtach".into(),
        "-A".into(),
        sock.to_string_lossy().into_owned(),
        "-E".into(), // no escape character
        "-r".into(),
        "winch".into(), // repaint full-screen apps on attach
        "-z".into(), // no suspend key
        shell,
        "-l".into(),
    ]
}

pub fn min_geometry(sizes: &HashMap<u64, (u16, u16)>) -> Option<(u16, u16)> {
    let cols = sizes.values().map(|(c, _)| *c).min()?;
    let rows = sizes.values().map(|(_, r)| *r).min()?;
    Some((cols, rows))
}

pub fn push_scrollback(ring: &mut VecDeque<u8>, data: &[u8]) {
    ring.extend(data.iter().copied());
    while ring.len() > MAX_SCROLLBACK {
        ring.pop_front();
    }
}

struct Session {
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    scrollback: VecDeque<u8>,
    subs: HashMap<u64, Sender<Vec<u8>>>,
    sizes: HashMap<u64, (u16, u16)>,
    next_id: u64,
}

pub struct Attachment {
    pub id: u64,
    pub key: String,
    pub rx: Receiver<Vec<u8>>,
}

static SESSIONS: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, Session>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Attach to a session, creating it if needed. The new subscriber is sent the
/// scrollback immediately so a reconnecting browser sees where it was.
pub fn attach(project: &str, name: &str, dir: &Path) -> Result<Attachment, String> {
    if !valid_name(name) {
        return Err("invalid session name".into());
    }
    if !valid_name(project) && project.contains('/') {
        return Err("invalid project name".into());
    }
    let key = format!("{project}-{name}");
    let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let live_for_project = map.keys().filter(|k| k.starts_with(&format!("{project}-"))).count();
    if !map.contains_key(&key) && live_for_project >= MAX_SESSIONS_PER_PROJECT {
        return Err("too many terminal sessions".into());
    }

    if !map.contains_key(&key) {
        let cmd = default_command(project, name);
        if cmd.is_empty() {
            return Err("empty command".into());
        }
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())?;
        let mut cb = CommandBuilder::new(&cmd[0]);
        cb.args(&cmd[1..]);
        cb.cwd(dir);
        cb.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(cb).map_err(|e| e.to_string())?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        map.insert(
            key.clone(),
            Session {
                writer,
                master: pair.master,
                child,
                scrollback: VecDeque::new(),
                subs: HashMap::new(),
                sizes: HashMap::new(),
                next_id: 0,
            },
        );
        let pump_key = key.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
                        let Some(s) = map.get_mut(&pump_key) else { break };
                        push_scrollback(&mut s.scrollback, &buf[..n]);
                        let chunk = buf[..n].to_vec();
                        s.subs.retain(|_, tx| tx.send(chunk.clone()).is_ok());
                    }
                }
            }
            // PTY closed: drop the session so the next attach respawns it.
            let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(mut s) = map.remove(&pump_key) {
                let _ = s.child.kill();
                let _ = s.child.wait();
            }
        });
    }

    let s = map.get_mut(&key).ok_or("session vanished")?;
    s.next_id += 1;
    let id = s.next_id;
    let (tx, rx) = channel();
    let replay: Vec<u8> = s.scrollback.iter().copied().collect();
    if !replay.is_empty() {
        let _ = tx.send(replay);
    }
    s.subs.insert(id, tx);
    Ok(Attachment { id, key, rx })
}

pub fn write_input(key: &str, data: &[u8]) -> Result<(), String> {
    let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let s = map.get_mut(key).ok_or("no such session")?;
    s.writer.write_all(data).map_err(|e| e.to_string())?;
    s.writer.flush().map_err(|e| e.to_string())
}

pub fn resize(key: &str, id: u64, cols: u16, rows: u16) {
    let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = map.get_mut(key) else { return };
    s.sizes.insert(id, (cols, rows));
    if let Some((c, r)) = min_geometry(&s.sizes) {
        let _ = s.master.resize(PtySize { rows: r, cols: c, pixel_width: 0, pixel_height: 0 });
    }
}

/// Detach only. The PTY keeps running and dtach keeps the session alive, so
/// reopening the same name reattaches.
pub fn detach(key: &str, id: u64) {
    let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = map.get_mut(key) else { return };
    s.subs.remove(&id);
    s.sizes.remove(&id);
    if let Some((c, r)) = min_geometry(&s.sizes) {
        let _ = s.master.resize(PtySize { rows: r, cols: c, pixel_width: 0, pixel_height: 0 });
    }
}
```

- [ ] **Step 4: Rewrite `src/term.rs` to pump against the registry**

Replace the whole file:

```rust
//! Terminal websocket: one connection = one attachment to a session in
//! `session`. The session owns the PTY and outlives this connection.
use crate::session;
use std::net::TcpStream;
use std::path::PathBuf;
use tungstenite::handshake::server::{Request as WsRequest, Response as WsResponse};
use tungstenite::protocol::Role;
use tungstenite::{accept_hdr, Message, WebSocket};

pub fn handle_ws(stream: TcpStream, roots: &[PathBuf]) {
    let mut path = String::new();
    let allowed = crate::config::allowed_origins();
    let accepted = accept_hdr(stream, |req: &WsRequest, resp: WsResponse| {
        path = req.uri().path().to_string();
        let origin = req.headers().get("origin").and_then(|v| v.to_str().ok());
        if !crate::origin::origin_allowed(origin, &allowed) {
            eprintln!("deadlight: rejected ws origin={origin:?} (set allowed_origins)");
            return Err(tungstenite::http::Response::builder()
                .status(403)
                .body(Some("origin not allowed".to_string()))
                .expect("static 403"));
        }
        Ok(resp)
    });
    let Ok(mut ws_read) = accepted else { return };

    // /ws/{project}/term/{name}
    let rest = path.trim_start_matches("/ws/");
    let segs: Vec<&str> = rest.split('/').collect();
    let (Some(project), Some(&"term"), Some(name)) =
        (segs.first().copied(), segs.get(1), segs.get(2).copied())
    else {
        let _ = ws_read.close(None);
        return;
    };
    let Some(dir) = crate::projects::resolve_project(roots, project) else {
        let _ = ws_read.close(None);
        return;
    };
    let att = match session::attach(project, name, &dir) {
        Ok(a) => a,
        Err(_) => {
            let _ = ws_read.close(None);
            return;
        }
    };

    let Ok(write_half) = ws_read.get_ref().try_clone() else { return };
    let mut ws_write: WebSocket<TcpStream> =
        WebSocket::from_raw_socket(write_half, Role::Server, None);
    let rx = att.rx;
    let out = std::thread::spawn(move || {
        while let Ok(chunk) = rx.recv() {
            if ws_write.send(Message::Binary(chunk.into())).is_err() {
                break;
            }
        }
        let _ = ws_write.close(None);
        let _ = ws_write.get_ref().shutdown(std::net::Shutdown::Both);
    });

    loop {
        match ws_read.read() {
            Ok(Message::Binary(b)) => {
                if session::write_input(&att.key, &b).is_err() {
                    break;
                }
            }
            Ok(Message::Text(t)) => {
                if let Some(sz) = t.strip_prefix("resize:") {
                    if let Some((c, r)) = sz.split_once('x') {
                        if let (Ok(cols), Ok(rows)) = (c.parse(), r.parse()) {
                            session::resize(&att.key, att.id, cols, rows);
                        }
                    }
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    session::detach(&att.key, att.id); // detach only; the session survives
    let _ = out.join();
}
```

- [ ] **Step 5: Update the ws integration tests for the new path**

In `tests/integration.rs`, change `ws_connect` to target `/ws/proj/term/shell` instead of `/ws/proj`, and add:

```rust
#[test]
fn two_terminal_clients_mirror_one_session() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    std::env::set_var("DEADLIGHT_CMD", "cat");
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
    let (_d, port) = fixture();
    let mut a = ws_connect_path(port, "/ws/proj/term/shell").unwrap();
    let mut b = ws_connect_path(port, "/ws/proj/term/shell").unwrap();
    a.send(tungstenite::Message::Binary(b"mirrored\r".to_vec().into())).unwrap();

    for ws in [&mut a, &mut b] {
        let mut seen = String::new();
        for _ in 0..60 {
            match ws.read() {
                Ok(tungstenite::Message::Binary(x)) => seen.push_str(&String::from_utf8_lossy(&x)),
                Ok(_) => {}
                Err(_) => break,
            }
            if seen.contains("mirrored") {
                break;
            }
        }
        assert!(seen.contains("mirrored"), "both attachments must see the output");
    }
    let _ = a.close(None);
    let _ = b.close(None);
    std::env::remove_var("DEADLIGHT_STATE_DIR");
}

#[test]
fn invalid_session_name_is_refused() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    std::env::set_var("DEADLIGHT_CMD", "cat");
    let (_d, port) = fixture();
    let mut ws = ws_connect_path(port, "/ws/proj/term/bad%20name").unwrap();
    // the server closes immediately rather than spawning anything
    let mut closed = false;
    for _ in 0..20 {
        match ws.read() {
            Ok(tungstenite::Message::Close(_)) | Err(_) => { closed = true; break; }
            Ok(_) => {}
        }
    }
    assert!(closed);
}
```

- [ ] **Step 6: Run all tests, expect pass**

Run: `cargo test`
Expected: all pass. Note `terminal_ws_echoes_through_pty` and `ws_closes_when_child_exits_first` now use `/ws/proj/term/shell`.

- [ ] **Step 7: Commit**

```bash
git add src/session.rs src/term.rs src/lib.rs tests/integration.rs
git commit -m "v3: deadlight-owned terminal sessions over dtach, zellij dropped"
```

---

### Task 8: `watch` — filesystem watching

**Files:**
- Create: `src/watch.rs`
- Modify: `src/hub.rs` (apply watch events), `src/lib.rs`, `Cargo.toml`

**Interfaces:**
- Produces: `watch::Class { Tree, Status, Buffer(String), Ignore }`; `watch::classify(rel: &str, open_buffers: &[String], hide: &[String]) -> Class` (**pure**); `watch::spawn(project: &str, dir: PathBuf, hub: Arc<Mutex<Hub>>, debounce: Duration) -> bool` (false = degraded).

- [ ] **Step 1: Add dependencies to `Cargo.toml`**

```toml
notify = "8.2"
notify-debouncer-full = "0.7"
```

- [ ] **Step 2: Write `src/watch.rs` with tests only**

```rust
//! Filesystem watching. deadlight is for AI engineering: Claude edits files
//! in the background, so a viewer that does not reflect that is showing
//! something false. Classification is pure so the routing table is testable
//! without an OS event or a sleep.
use crate::hub::Hub;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(test)]
mod tests {
    use super::*;

    fn bufs() -> Vec<String> {
        vec!["src/main.rs".to_string()]
    }

    #[test]
    fn git_index_and_head_drive_the_status_pane() {
        assert!(matches!(classify(".git/index", &bufs(), &[]), Class::Status));
        assert!(matches!(classify(".git/HEAD", &bufs(), &[]), Class::Status));
    }

    #[test]
    fn other_git_internals_are_ignored() {
        assert!(matches!(classify(".git/objects/ab/cdef", &bufs(), &[]), Class::Ignore));
        assert!(matches!(classify(".git/logs/HEAD", &bufs(), &[]), Class::Ignore));
    }

    #[test]
    fn open_buffers_beat_the_generic_tree_class() {
        match classify("src/main.rs", &bufs(), &[]) {
            Class::Buffer(rel) => assert_eq!(rel, "src/main.rs"),
            other => panic!("expected Buffer, got {other:?}"),
        }
    }

    #[test]
    fn ordinary_files_refresh_the_tree() {
        assert!(matches!(classify("src/other.rs", &bufs(), &[]), Class::Tree));
        assert!(matches!(classify("README.md", &bufs(), &[]), Class::Tree));
    }

    #[test]
    fn skip_dirs_and_hide_are_ignored_entirely() {
        // a cargo build must not generate a storm of tree refreshes
        assert!(matches!(classify("target/debug/deadlight", &bufs(), &[]), Class::Ignore));
        assert!(matches!(classify("node_modules/x/y.js", &bufs(), &[]), Class::Ignore));
        assert!(matches!(classify(".venv/lib/p.py", &bufs(), &[]), Class::Ignore));
        let hide = vec!["dist".to_string()];
        assert!(matches!(classify("dist/bundle.js", &bufs(), &hide), Class::Ignore));
    }

    #[test]
    fn self_writes_are_suppressed_once() {
        let mut seen = std::collections::HashMap::new();
        seen.insert("a.rs".to_string(), 42u64);
        // deadlight just wrote this content; the resulting event is ours
        assert!(is_self_write(&mut seen, "a.rs", 42));
        // and only once — a later external edit with other content is real
        assert!(!is_self_write(&mut seen, "a.rs", 43));
    }
}
```

- [ ] **Step 3: Add `pub mod watch;` to `src/lib.rs`, run, expect compile failure**

Run: `cargo test watch`
Expected: FAIL — `classify` not found.

- [ ] **Step 4: Add the implementation above the test module**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Class {
    Tree,
    Status,
    Buffer(String),
    Ignore,
}

/// Pure. `rel` is project-relative with `/` separators.
pub fn classify(rel: &str, open_buffers: &[String], hide: &[String]) -> Class {
    let first = rel.split('/').next().unwrap_or("");
    if first == ".git" {
        return match rel {
            ".git/index" | ".git/HEAD" => Class::Status,
            _ => Class::Ignore,
        };
    }
    if crate::projects::SKIP_DIRS.contains(&first) || hide.iter().any(|h| h == first) {
        return Class::Ignore;
    }
    if open_buffers.iter().any(|b| b == rel) {
        return Class::Buffer(rel.to_string());
    }
    Class::Tree
}

/// True when this event was caused by deadlight's own save. Consumes the
/// record, so a later external edit is not swallowed too.
pub fn is_self_write(
    seen: &mut std::collections::HashMap<String, u64>,
    rel: &str,
    disk_hash: u64,
) -> bool {
    match seen.get(rel) {
        Some(h) if *h == disk_hash => {
            seen.remove(rel);
            true
        }
        _ => false,
    }
}

/// Register per-directory, non-recursive watches while walking the tree,
/// skipping SKIP_DIRS. Returns false if watching could not be established
/// (inotify limits) — correctness never depends on it.
pub fn spawn(
    project: &str,
    dir: PathBuf,
    hub: Arc<Mutex<Hub>>,
    debounce: Duration,
) -> bool {
    use notify::{RecursiveMode, Watcher};
    let (tx, rx) = std::sync::mpsc::channel();
    let mut debouncer = match notify_debouncer_full::new_debouncer(debounce, None, tx) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("deadlight: watcher unavailable: {e}");
            return false;
        }
    };
    let mut ok = true;
    let mut stack = vec![dir.clone()];
    while let Some(d) = stack.pop() {
        if debouncer.watch(&d, RecursiveMode::NonRecursive).is_err() {
            ok = false;
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() && !crate::projects::SKIP_DIRS.contains(&name.as_str()) {
                stack.push(p);
            }
        }
    }
    // .git itself is skipped for the tree but index/HEAD drive the status pane
    let _ = debouncer.watch(&dir.join(".git"), RecursiveMode::NonRecursive);

    let base = dir.clone();
    let project = project.to_string();
    std::thread::spawn(move || {
        let _keep = debouncer; // dropping the debouncer stops the watch
        for res in rx {
            let Ok(events) = res else { continue };
            let mut h = hub.lock().unwrap_or_else(|e| e.into_inner());
            let open: Vec<String> = h.ws.buffers.keys().cloned().collect();
            let mut tree = false;
            let mut status = false;
            for ev in events {
                for path in &ev.paths {
                    let Ok(rel) = path.strip_prefix(&base) else { continue };
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    match classify(&rel, &open, &[]) {
                        Class::Tree => tree = true,
                        Class::Status => status = true,
                        Class::Buffer(r) => h.file_changed_externally(&base, &r),
                        Class::Ignore => {}
                    }
                }
            }
            if tree {
                h.broadcast(&crate::proto::Event::TreeChanged);
            }
            if status {
                h.broadcast(&crate::proto::Event::StatusChanged);
            }
        }
        drop(project);
    });
    ok
}
```

- [ ] **Step 5: Add `Hub::file_changed_externally` to `src/hub.rs`**

```rust
impl Hub {
    /// A file with an open buffer changed on disk. Clean buffers follow the
    /// file so you watch Claude's edits land; dirty buffers are only flagged,
    /// so unsaved work is never overwritten by a background writer.
    pub fn file_changed_externally(&mut self, base: &std::path::Path, rel: &str) {
        let Ok(disk) = std::fs::read_to_string(base.join(rel)) else { return };
        let disk_hash = workspace::hash_text(&disk);
        if crate::watch::is_self_write(&mut self.self_writes, rel, disk_hash) {
            return; // our own save; broadcasting it would echo back at the author
        }
        let Some(b) = self.ws.buffers.get_mut(rel) else { return };
        if b.dirty {
            b.stale = true;
            let ev = Event::BufferStale { rel: rel.to_string() };
            self.broadcast(&ev);
        } else {
            b.text = disk.clone();
            b.base_hash = disk_hash;
            b.stale = false;
            let ev = Event::BufferText {
                rel: rel.to_string(),
                text: disk,
                origin: String::new(), // no author: everyone applies it
            };
            self.broadcast(&ev);
        }
        self.ws.version += 1;
        self.broadcast(&Event::FileChanged { rel: rel.to_string() });
    }
}
```

- [ ] **Step 6: Start the watcher when a hub is created**

In `Hub::for_project`, after inserting a new hub:

```rust
let arc = map.entry(project.to_string())
    .or_insert_with(|| Arc::new(Mutex::new(Hub::new(project, dir.clone()))))
    .clone();
// idempotent: only the first creation spawns a watcher
if !arc.lock().unwrap_or_else(|e| e.into_inner()).watching {
    let ms: u64 = std::env::var("DEADLIGHT_DEBOUNCE_MS").ok()
        .and_then(|v| v.parse().ok()).unwrap_or(300);
    let ok = crate::watch::spawn(project, dir, arc.clone(), std::time::Duration::from_millis(ms));
    let mut h = arc.lock().unwrap_or_else(|e| e.into_inner());
    h.watching = true;
    h.ws.watch_degraded = !ok;
}
arc
```

Add `pub watching: bool` to `Hub` (initialised `false` in `Hub::new`).

- [ ] **Step 7: Add the integration test to `tests/integration.rs`**

```rust
#[test]
fn external_edit_updates_a_clean_buffer_live() {
    let _g = WS_TEST_LOCK.lock().unwrap();
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("DEADLIGHT_STATE_DIR", sd.path());
    std::env::set_var("DEADLIGHT_DEBOUNCE_MS", "10");
    let (d, port) = fixture();
    let mut a = ws_connect_path(port, "/ws/proj/_workspace").unwrap();
    a.send(tungstenite::Message::Text(
        r#"{"t":"EditBuffer","rel":"hello.md","text":"# Hello\n"}"#.into(),
    ))
    .unwrap();
    a.send(tungstenite::Message::Text(
        r#"{"t":"SaveBuffer","rel":"hello.md","force":true}"#.into(),
    ))
    .unwrap();
    let _ = read_until(&mut a, "SaveOk"); // buffer is now clean

    // Claude, in the next pane, rewrites the file
    std::fs::write(d.path().join("proj/hello.md"), "# Rewritten by Claude\n").unwrap();
    let seen = read_until(&mut a, "Rewritten by Claude");
    assert!(seen.contains(r#""t":"BufferText""#), "a clean buffer must follow the file");

    let _ = a.close(None);
    std::env::remove_var("DEADLIGHT_STATE_DIR");
    std::env::remove_var("DEADLIGHT_DEBOUNCE_MS");
}
```

- [ ] **Step 8: Run all tests, expect pass**

Run: `cargo test`
Expected: all pass

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock src/watch.rs src/hub.rs src/lib.rs tests/integration.rs
git commit -m "v3: filesystem watching with pure classification and self-write suppression"
```

---

### Task 9: Frontend — four-pane layout, universal tabs, terminal pooling

**Files:**
- Rewrite: `static/app.js`, `static/style.css`
- Modify: `src/render.rs` (`workspace_page` emits the four-pane skeleton)

**Interfaces:**
- Consumes: the workspace socket protocol from Task 1, `/frag/...` endpoints (unchanged).

- [ ] **Step 1: Rewrite `workspace_page` in `src/render.rs`**

Replace the `<main>` block of the returned document with the pane skeleton and drop the old `<nav>` tab buttons:

```rust
// inside workspace_page's format! string, replacing <main>...</main>
r#"<main id="grid">
  <section class="pane" data-pane="0"><div class="tabstrip"></div><div class="content"></div></section>
  <div class="divider" data-div="left-split"></div>
  <section class="pane" data-pane="1"><div class="tabstrip"></div><div class="content"></div></section>
  <div class="divider" data-div="left-w"></div>
  <section class="pane" data-pane="2"><div class="tabstrip"></div><div class="content"></div></section>
  <div class="divider" data-div="right-w"></div>
  <section class="pane" data-pane="3"><div class="tabstrip"></div><div class="content"></div></section>
</main>
<div id="termpool" hidden></div>"#
```

`#termpool` is where xterm nodes live when their tab is not visible — they are moved, never rebuilt.

Update `render::tests::workspace_page_wires_everything` to assert `data-pane="3"` and `id="termpool"` are present instead of the old `id="tab-terminal"`.

- [ ] **Step 2: Run render tests, expect the updated assertions to pass**

Run: `cargo test render`
Expected: pass

- [ ] **Step 3: Write `static/style.css`**

```css
:root { --divider: 4px; }
#grid {
  display: grid;
  height: calc(100vh - var(--header-h, 36px));
  grid-template-columns: var(--left-w, 260px) var(--divider) 1fr var(--divider) var(--right-w, 520px);
  grid-template-rows: var(--left-split, 60%) var(--divider) 1fr;
}
/* left column stacks; middle and right span both rows */
.pane[data-pane="0"] { grid-column: 1; grid-row: 1; }
.divider[data-div="left-split"] { grid-column: 1; grid-row: 2; cursor: row-resize; }
.pane[data-pane="1"] { grid-column: 1; grid-row: 3; }
.divider[data-div="left-w"] { grid-column: 2; grid-row: 1 / -1; cursor: col-resize; }
.pane[data-pane="2"] { grid-column: 3; grid-row: 1 / -1; }
.divider[data-div="right-w"] { grid-column: 4; grid-row: 1 / -1; cursor: col-resize; }
.pane[data-pane="3"] { grid-column: 5; grid-row: 1 / -1; }

.pane { display: flex; flex-direction: column; overflow: hidden; border: 1px solid var(--border); }
.tabstrip { display: flex; overflow-x: auto; background: var(--bg2); flex: 0 0 auto; }
.tabstrip .tab { padding: 4px 10px; cursor: pointer; white-space: nowrap; border-right: 1px solid var(--border); }
.tabstrip .tab.active { background: var(--bg); font-weight: 600; }
.tabstrip .tab .dirty { color: var(--accent); }
.tabstrip .tab .stale { color: var(--warn, #d79921); }
.tabstrip .tab .x { opacity: .5; margin-left: 6px; }
.content { flex: 1 1 auto; overflow: auto; position: relative; }
.content .termhost { position: absolute; inset: 0; }
.divider { background: var(--border); }
#termpool { display: none; }
.editor { width: 100%; height: 100%; border: 0; font: inherit; resize: none; }
.conflict { border: 1px solid var(--warn, #d79921); padding: 8px; margin: 8px; }
```

- [ ] **Step 4: Write `static/app.js`**

```js
// Workspace client. Chrome is rendered from mirrored server state; terminals
// are pooled DOM nodes that are MOVED, never rebuilt — rebuilding drops the
// socket and detaches the session.
const PROJECT = document.body.dataset.project;
const wsUrl = (p) => `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}${p}`;

let state = null;
let myOrigin = null;
let ctrl = null;
const terms = new Map();   // session -> {node, term, fit, sock}
const editors = new Map(); // rel -> textarea

function send(intent) {
  if (ctrl && ctrl.readyState === 1) ctrl.send(JSON.stringify(intent));
}

function connectControl() {
  ctrl = new WebSocket(wsUrl(`/ws/${PROJECT}/_workspace`));
  ctrl.onmessage = (e) => onEvent(JSON.parse(e.data));
  ctrl.onclose = () => setTimeout(connectControl, 1000);
}

function onEvent(ev) {
  switch (ev.t) {
    case "State":
      myOrigin = myOrigin || ev.origin;
      state = ev.ws;
      render();
      break;
    case "BufferText": {
      // Skip our own text or the cursor jumps; empty origin = external change.
      if (ev.origin && ev.origin === myOrigin) break;
      const ta = editors.get(ev.rel);
      if (ta && ta.value !== ev.text) ta.value = ev.text;
      break;
    }
    case "TreeChanged": refreshKind("Tree"); break;
    case "StatusChanged": refreshKind("Changes"); break;
    case "FileChanged": refreshKind("Diff"); break;
    case "SaveConflict": showConflict(ev); break;
    case "Error": console.warn("deadlight:", ev.msg); break;
  }
}

function tabKey(t) {
  switch (t.k) {
    case "File": return `File:${t.rel}`;
    case "Diff": return `Diff:${t.rel || ""}`;
    case "Terminal": return `Terminal:${t.session}`;
    default: return t.k;
  }
}

function tabLabel(t) {
  switch (t.k) {
    case "Tree": return "Files";
    case "Changes": return "Changes";
    case "File": return t.rel.split("/").pop();
    case "Diff": return t.rel ? `± ${t.rel.split("/").pop()}` : "± full diff";
    case "Terminal": return t.session;
  }
}

function render() {
  if (!state) return;
  document.documentElement.style.setProperty("--left-w", state.sizes.left_w + "px");
  document.documentElement.style.setProperty("--right-w", state.sizes.right_w + "px");
  document.documentElement.style.setProperty("--left-split", state.sizes.left_split + "%");

  state.panes.forEach((pane, pi) => {
    const el = document.querySelector(`.pane[data-pane="${pi}"]`);
    const strip = el.querySelector(".tabstrip");
    const content = el.querySelector(".content");
    strip.innerHTML = "";
    pane.tabs.forEach((t, ti) => {
      const b = document.createElement("span");
      b.className = "tab" + (ti === pane.active ? " active" : "");
      const meta = t.k === "File" ? state.buffers.find((x) => x.rel === t.rel) : null;
      b.innerHTML =
        (meta && meta.dirty ? '<span class="dirty">●</span> ' : "") +
        (meta && meta.stale ? '<span class="stale">⚠</span> ' : "") +
        escapeHtml(tabLabel(t));
      b.onclick = () => send({ t: "ActivateTab", pane: pi, idx: ti });
      const x = document.createElement("span");
      x.className = "x";
      x.textContent = "×";
      x.onclick = (e) => { e.stopPropagation(); closeTab(pi, ti, t); };
      b.appendChild(x);
      strip.appendChild(b);
    });
    // Park every terminal before clearing, so nodes are never destroyed.
    content.querySelectorAll(".termhost").forEach((n) => pool().appendChild(n));
    content.innerHTML = "";
    const active = pane.tabs[pane.active];
    if (active) mountTab(content, active);
  });
}

function pool() { return document.getElementById("termpool"); }

function mountTab(content, t) {
  if (t.k === "Terminal") {
    const e = ensureTerm(t.session);
    content.appendChild(e.node);   // MOVE, not rebuild — the socket survives
    requestAnimationFrame(() => { try { e.fit.fit(); sendResize(e); } catch {} });
    return;
  }
  if (t.k === "File" && t.mode === "Edit") { mountEditor(content, t.rel); return; }
  const url =
    t.k === "Tree" ? `/frag/${PROJECT}/tree`
    : t.k === "Changes" ? `/frag/${PROJECT}/changes`
    : t.k === "File" ? `/frag/${PROJECT}/file?path=${encodeURIComponent(t.rel)}`
    : `/frag/${PROJECT}/diff${t.rel ? "?path=" + encodeURIComponent(t.rel) : ""}`;
  content.dataset.url = url;
  fetch(url).then((r) => r.text()).then((html) => {
    content.innerHTML = html;
    content.querySelectorAll("pre code").forEach((b) => window.hljs && hljs.highlightElement(b));
    wireFragment(content);
  });
}

function wireFragment(content) {
  content.querySelectorAll("a.file[data-rel]").forEach((a) => {
    a.onclick = (e) => {
      e.preventDefault();
      const rel = a.dataset.rel;
      const isDiff = a.getAttribute("hx-get")?.includes("/diff");
      send({
        t: "OpenTab",
        pane: 2,
        tab: isDiff ? { k: "Diff", rel: rel || null } : { k: "File", rel, mode: "Preview" },
      });
    };
  });
}

function refreshKind(kind) {
  if (!state) return;
  state.panes.forEach((pane, pi) => {
    const active = pane.tabs[pane.active];
    if (active && active.k === kind) {
      const content = document.querySelector(`.pane[data-pane="${pi}"] .content`);
      mountTab(content, active);
    }
  });
}

function ensureTerm(session) {
  if (terms.has(session)) return terms.get(session);
  const node = document.createElement("div");
  node.className = "termhost";
  node.dataset.session = session;
  pool().appendChild(node);
  const term = new Terminal({ convertEol: false, fontSize: 13 });
  const fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  term.open(node);
  const sock = new WebSocket(wsUrl(`/ws/${PROJECT}/term/${session}`));
  sock.binaryType = "arraybuffer";
  sock.onmessage = (e) => term.write(new Uint8Array(e.data));
  term.onData((d) => { if (sock.readyState === 1) sock.send(new TextEncoder().encode(d)); });
  const entry = { node, term, fit, sock };
  terms.set(session, entry);
  return entry;
}

function sendResize(e) {
  if (e.sock.readyState === 1) e.sock.send(`resize:${e.term.cols}x${e.term.rows}`);
}

function mountEditor(content, rel) {
  const ta = document.createElement("textarea");
  ta.className = "editor";
  ta.spellcheck = false;
  const existing = editors.get(rel);
  ta.value = existing ? existing.value : "";
  editors.set(rel, ta);
  let timer = null;
  ta.oninput = () => {
    clearTimeout(timer);
    timer = setTimeout(() => send({ t: "EditBuffer", rel, text: ta.value }), 200);
  };
  ta.onkeydown = (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "s") {
      e.preventDefault();
      send({ t: "EditBuffer", rel, text: ta.value });
      send({ t: "SaveBuffer", rel, force: false });
    }
  };
  content.appendChild(ta);
  if (!existing) fetch(`/frag/${PROJECT}/raw?path=${encodeURIComponent(rel)}`)
    .then((r) => r.text()).then((txt) => { ta.value = txt; send({ t: "EditBuffer", rel, text: txt }); });
}

function showConflict(ev) {
  const box = document.createElement("div");
  box.className = "conflict";
  box.innerHTML =
    `<b>${escapeHtml(ev.rel)} changed on disk since you opened it.</b>` + ev.diff_html;
  const over = document.createElement("button");
  over.textContent = "overwrite";
  over.onclick = () => { send({ t: "SaveBuffer", rel: ev.rel, force: true }); box.remove(); };
  const reload = document.createElement("button");
  reload.textContent = "discard mine";
  reload.onclick = () => { send({ t: "CloseBuffer", rel: ev.rel }); box.remove(); };
  box.append(over, reload);
  document.querySelector('.pane[data-pane="2"] .content').prepend(box);
}

function closeTab(pi, ti, t) {
  const meta = t.k === "File" ? state.buffers.find((x) => x.rel === t.rel) : null;
  if (meta && meta.dirty && !confirm(`${t.rel} has unsaved changes. Close it?`)) return;
  send({ t: "CloseTab", pane: pi, idx: ti });
}

function escapeHtml(s) {
  return s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
}

// dividers
let drag = null;
document.querySelectorAll(".divider").forEach((d) => {
  d.onmousedown = (e) => { drag = { which: d.dataset.div, x: e.clientX, y: e.clientY }; e.preventDefault(); };
});
window.onmouseup = () => {
  if (drag && state) send({ t: "Resize", sizes: state.sizes });
  drag = null;
};
window.onmousemove = (e) => {
  if (!drag || !state) return;
  if (drag.which === "left-w") state.sizes.left_w = Math.max(120, e.clientX);
  if (drag.which === "right-w") state.sizes.right_w = Math.max(200, window.innerWidth - e.clientX);
  if (drag.which === "left-split") state.sizes.left_split = Math.min(90, Math.max(10, (e.clientY / window.innerHeight) * 100));
  render();
};

window.addEventListener("resize", () => terms.forEach((e) => { try { e.fit.fit(); sendResize(e); } catch {} }));
connectControl();
```

- [ ] **Step 5: Wire the file-operation context menu in `static/app.js`**

The `CreateFile` / `CreateDir` / `DeleteFile` / `RenamePath` intents exist and
are tested server-side in Task 4; this is their only user-facing affordance.
Add to `wireFragment` so it applies to every rendered tree:

```js
function wireFragment(content) {
  content.querySelectorAll("a.file[data-rel]").forEach((a) => {
    a.onclick = (e) => {
      e.preventDefault();
      const rel = a.dataset.rel;
      const isDiff = a.getAttribute("hx-get")?.includes("/diff");
      send({
        t: "OpenTab",
        pane: 2,
        tab: isDiff ? { k: "Diff", rel: rel || null } : { k: "File", rel, mode: "Preview" },
      });
    };
    a.oncontextmenu = (e) => { e.preventDefault(); fileMenu(e, a.dataset.rel); };
  });
  // right-clicking blank space in a tree targets the project root
  content.oncontextmenu = (e) => {
    if (e.target.closest("a.file")) return;
    e.preventDefault();
    fileMenu(e, "");
  };
}

// Deliberately prompt-based: no menu widget to build, and every destructive
// action gets a confirmation step for free.
function fileMenu(e, rel) {
  const dir = rel.includes("/") ? rel.slice(0, rel.lastIndexOf("/")) : "";
  const choice = prompt(
    `${rel || "(project root)"}\n\n` +
      "1 = new file   2 = new folder   3 = rename   4 = delete\n" +
      "Enter a number:",
    "1"
  );
  if (choice === "1") {
    const name = prompt("New file path:", dir ? `${dir}/untitled.txt` : "untitled.txt");
    if (name) send({ t: "CreateFile", rel: name });
  } else if (choice === "2") {
    const name = prompt("New folder path:", dir ? `${dir}/newdir` : "newdir");
    if (name) send({ t: "CreateDir", rel: name });
  } else if (choice === "3" && rel) {
    const to = prompt("Rename to:", rel);
    if (to && to !== rel) send({ t: "RenamePath", from: rel, to });
  } else if (choice === "4" && rel) {
    // the server refuses non-empty directories regardless of what we ask
    if (confirm(`Delete ${rel}?`)) send({ t: "DeleteFile", rel });
  }
}
```

The server broadcasts `TreeChanged` after each successful operation, so every
open tree pane refreshes itself without the client doing anything further.

- [ ] **Step 6: Add the `raw` fragment endpoint in `src/routes.rs`**

The editor needs unrendered text. Inside `serve_frag`'s `match what`:

```rust
["raw"] => match req.query.get("path") {
    None => http::respond(w, 200, "OK", "text/plain; charset=utf-8", b""),
    Some(rel) => match projects::safe_resolve(&dir, rel).and_then(|p| projects::read_text_file(&p)) {
        Ok(content) => http::respond(w, 200, "OK", "text/plain; charset=utf-8", content.as_bytes()),
        Err(e) => http::respond(w, 200, "OK", "text/plain; charset=utf-8", e.as_bytes()),
    },
},
```

- [ ] **Step 7: Verify in a browser**

```bash
DEADLIGHT_ROOTS="$HOME/Projects" cargo run --quiet 8444
```

Then, with pinchtab against `http://127.0.0.1:8444/deadlight`:

- [ ] All four panes render; tree in left-top, changes in left-bottom, terminal in right.
- [ ] Clicking a file in the tree opens a tab in the middle pane.
- [ ] Dragging each of the three dividers resizes and the size survives a reload.
- [ ] **The re-parenting check:** note `terms.get("shell").sock.readyState` and the term object, move the terminal tab to the middle pane via `send({t:"MoveTab",from:3,idx:0,to:2,at:0})`, then assert `readyState` is still `1` and `terms.get("shell").term` is the *same object*. A rebuild would show a new object or a closed socket.
- [ ] Open the same project in a second browser tab: opening a file in one opens it in the other.

- [ ] **Step 8: Commit**

```bash
git add static/app.js static/style.css src/render.rs src/routes.rs
git commit -m "v3: four-pane client with universal tabs and pooled terminal nodes"
```

---

### Task 10: Deployment

**Files:**
- Modify: `HANDOFF.md`

- [ ] **Step 1: Install dtach on both machines**

```bash
brew install dtach                                  # Mac
tailscale ssh claude@ubuntu-16gb-hel1-2 'sudo apt-get install -y dtach || echo NEEDS_SUDO_PASSWORD'
```

If sudo is unavailable, ask Peter to run `! ssh claude@ubuntu-16gb-hel1-2 'sudo apt-get install -y dtach'`.

- [ ] **Step 2: Full test run**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 3: Deploy, using the documented install step**

```bash
git push origin master
tailscale ssh claude@ubuntu-16gb-hel1-2 'cd /home/claude/projects/deadlight && git pull --ff-only && cargo build --release && install -m 755 ~/.cache/cargo-target/release/deadlight ~/.local/bin/deadlight && systemctl --user restart deadlight && systemctl --user is-active deadlight'
```

Confirm the new binary is actually running — a plain `cargo build` updates neither path the unit uses:

```bash
tailscale ssh claude@ubuntu-16gb-hel1-2 'strings ~/.local/bin/deadlight | grep -c _workspace'
```

Expected: non-zero.

- [ ] **Step 4: Verify over the tailnet**

```bash
curl -s -o /dev/null -w "%{http_code}\n" https://ubuntu-16gb-hel1-2.tail66d083.ts.net:8444/deadlight
```

Expected: 200. Then browse it and confirm the four panes and a working terminal.

- [ ] **Step 5: Update `HANDOFF.md`**

Change the summary line from "persistent zellij terminal + stateless read-only viewer" to describe the v3 workspace, point at the v3 spec and this plan, note that terminal sessions are deadlight-owned over `dtach` (zellij no longer required), and document `DEADLIGHT_STATE_DIR` and `DEADLIGHT_DEBOUNCE_MS` alongside the existing env overrides.

- [ ] **Step 6: Commit**

```bash
git add HANDOFF.md
git commit -m "v3: handoff notes for the workspace release"
git push origin master
```

Old zellij sessions are not adopted; they keep running under zellij and can be attached from a shell until retired.
