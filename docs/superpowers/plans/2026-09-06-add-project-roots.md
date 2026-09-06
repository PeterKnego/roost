# Add Project Roots Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** roost starts with no roots, explains the state on the front page with an **Add path** button, and lets the front page add roots at any time through a **+** beside the header's roots list.

**Architecture:** A new module `src/roots.rs` validates a path and appends it to the global config's `roots` through the existing `config::write_setting`, and serves a one-exchange Origin-checked websocket `/ws/_roots` dispatched from `route_ws`. The accept loop re-resolves roots per connection. The front page gains the dialog module, a roots list with **+**, and an empty state; `overview.js` opens the socket on confirm.

**Tech Stack:** Rust (tungstenite 0.24, serde, toml_edit via `config::write_setting`), plain JS (`static/overview.js`, `static/dialog.js`), deno browser tests.

**Spec:** `docs/superpowers/specs/2026-09-06-add-project-roots-design.md`

## Global Constraints

- HTTP stays `GET` plus `POST /upload` and `POST /paste`; the write travels on a websocket. Every browser-facing websocket checks `Origin` in its handshake with `crate::origin::origin_allowed` and refuses a handshake that carries none.
- "Could not look" is a third outcome: a `symlink_metadata` error other than `NotFound` is refused as "cannot read", never folded into "no such directory".
- `ROOST_ROOTS` set and non-empty → the write is refused by name; the environment wins over the file for every read.
- The written entry is the canonical path; a duplicate is detected canonically.
- A config file that does not parse is refused with its error and left alone (`config::write_setting` already does this).
- No HTML built from data in the client; the front page renders roots and messages through `textContent`/`createElement`, and the server escapes everything it interpolates.
- `cargo test -- --test-threads=1`, never `--release`; browser tests one at a time; every new test revert-checked and the failure recorded in its comment.

---

## File structure

| File | Responsibility |
|---|---|
| `src/roots.rs` (new) | `add_root` validation + append; `handle_ws` for `/ws/_roots`; the intent/event types. |
| `src/config.rs` | `expand_home` becomes `pub(crate)`; the `roots` doc string. |
| `src/lib.rs` | roots per connection; `/ws/_roots` dispatch. |
| `src/main.rs` | no exit on empty roots; shorter notice. |
| `src/routes.rs` | index passes the root list; projects fragment passes `roots.is_empty()`. |
| `src/render.rs` | front page: roots list + `#addroot`, dialog shells, dialog.js, structural CSS; projects fragment empty state. |
| `static/overview.js` | the Add flow: dialog → socket → refresh/banner. |
| `static/style.css` | header roots list, the empty state, the `+`. |
| `tests/browser/roots.mjs` (new) | the end-to-end proof. |
| `docs/deploy.md`, `README.md`, `tests/browser/README.md` | docs. |

---

### Task 1: `roots::add_root`

**Files:**
- Create: `src/roots.rs`
- Modify: `src/lib.rs` (`pub mod roots;`), `src/config.rs` (`expand_home` → `pub(crate) fn expand_home`)

**Interfaces:**
- Produces: `roots::add_root(path: &str, current: &[PathBuf], env_roots: Option<&str>, global: &Path) -> Result<Vec<PathBuf>, String>` — validates, appends to `global`'s `roots`, returns the new list (current + the canonical new path).

- [ ] **Step 1: Write the failing tests** at the bottom of the new `src/roots.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn global(d: &tempfile::TempDir) -> std::path::PathBuf { d.path().join("config.toml") }

    #[test]
    fn a_relative_path_is_refused_by_name() {
        let d = tempfile::tempdir().unwrap();
        let e = add_root("projects", &[], None, &global(&d)).unwrap_err();
        assert!(e.contains("projects") && e.contains("absolute"), "{e}");
        assert!(!global(&d).exists(), "nothing written");
    }

    #[test]
    fn missing_and_unreadable_and_file_are_three_different_refusals() {
        let d = tempfile::tempdir().unwrap();
        let missing = d.path().join("nope");
        let e = add_root(missing.to_str().unwrap(), &[], None, &global(&d)).unwrap_err();
        assert!(e.contains("no such directory"), "{e}");
        let file = d.path().join("f.txt");
        fs::write(&file, "x").unwrap();
        let e = add_root(file.to_str().unwrap(), &[], None, &global(&d)).unwrap_err();
        assert!(e.contains("not a directory"), "{e}");
        // Unreadable: a directory we may not search. Skipped as root, who can.
        #[cfg(unix)]
        if !nix_is_root() {
            use std::os::unix::fs::PermissionsExt;
            let locked = d.path().join("locked");
            fs::create_dir(&locked).unwrap();
            let inner = locked.join("inner");
            fs::create_dir(&inner).unwrap();
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
            let e = add_root(inner.to_str().unwrap(), &[], None, &global(&d)).unwrap_err();
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            // Revert-check: mapping every metadata error to "no such
            // directory" fails here — "cannot look" must stay distinct.
            assert!(e.contains("cannot read"), "{e}");
        }
        assert!(!global(&d).exists(), "nothing written by a refusal");
    }

    #[test]
    fn a_duplicate_is_detected_canonically_and_tilde_expands() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("projects");
        fs::create_dir(&dir).unwrap();
        let canon = dir.canonicalize().unwrap();
        // First add writes the canonical path.
        let list = add_root(dir.to_str().unwrap(), &[], None, &global(&d)).unwrap();
        assert_eq!(list, vec![canon.clone()]);
        let text = fs::read_to_string(global(&d)).unwrap();
        assert!(text.contains(&format!("roots = [\"{}\"]", canon.display())), "{text}");
        // Same directory spelled with a dot segment: refused as already present.
        let spelled = format!("{}/./projects", d.path().display());
        let e = add_root(&spelled, &[canon.clone()], None, &global(&d)).unwrap_err();
        assert!(e.contains("already a root"), "{e}");
        // `~/` expands against HOME.
        let home = d.path().join("home");
        fs::create_dir_all(home.join("work")).unwrap();
        std::env::set_var("HOME", &home);
        let list = add_root("~/work", &[canon.clone()], None, &global(&d)).unwrap();
        assert_eq!(list.last().unwrap(), &home.join("work").canonicalize().unwrap());
    }

    #[test]
    fn appending_keeps_the_existing_list_comments_and_other_keys() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a"); let b = d.path().join("b");
        fs::create_dir(&a).unwrap(); fs::create_dir(&b).unwrap();
        let before = format!("# mine\ntheme = \"dark\"\nroots = [\"{}\"] # first\n", a.display());
        fs::write(global(&d), &before).unwrap();
        let list = add_root(b.to_str().unwrap(), &[a.clone()], None, &global(&d)).unwrap();
        assert_eq!(list, vec![a.clone(), b.canonicalize().unwrap()]);
        let after = fs::read_to_string(global(&d)).unwrap();
        assert!(after.starts_with("# mine\ntheme = \"dark\"\n"), "{after}");
        assert!(after.contains(&a.display().to_string()) && after.contains(&b.canonicalize().unwrap().display().to_string()), "{after}");
        assert!(after.contains("# first"), "the inline comment survives: {after}");
    }

    #[test]
    fn roost_roots_in_the_environment_refuses_the_write() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("p"); fs::create_dir(&dir).unwrap();
        let e = add_root(dir.to_str().unwrap(), &[], Some("/somewhere"), &global(&d)).unwrap_err();
        assert!(e.contains("ROOST_ROOTS"), "{e}");
        assert!(!global(&d).exists());
        // Set but empty is "unset": `roots_from` ignores it, so the file rules.
        assert!(add_root(dir.to_str().unwrap(), &[], Some(""), &global(&d)).is_ok());
    }

    #[cfg(unix)]
    fn nix_is_root() -> bool {
        std::fs::metadata("/proc/self").map(|m| { use std::os::unix::fs::MetadataExt; m.uid() == 0 }).unwrap_or(false)
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib roots:: -- --test-threads=1`
Expected: compile error, `roots` module / `add_root` missing.

- [ ] **Step 3: Implement `src/roots.rs`** (the socket half comes in Task 2):

```rust
//! Adding a project root from the front page.
//!
//! A root is a directory roost scans for projects; today they come only from
//! `ROOST_ROOTS` or the global config's `roots`. This is the one place a
//! browser may extend that list, behind the same Origin check every
//! shell-spawning socket has. Validation reads metadata and canonicalises;
//! it never creates, lists or follows into anything — the scan that follows
//! is the existing one.
use std::path::{Path, PathBuf};

use crate::proto::SettingValue;

/// Validate `path` and append it to `global`'s `roots`. Returns the new list.
///
/// `current` is the root list in effect (so a duplicate is caught against
/// what the server actually serves), `env_roots` is `ROOST_ROOTS` if set.
pub fn add_root(path: &str, current: &[PathBuf], env_roots: Option<&str>, global: &Path) -> Result<Vec<PathBuf>, String> {
    let raw = path.trim();
    if raw.is_empty() {
        return Err("enter a directory path".into());
    }
    let expanded = crate::config::expand_home(raw);
    if !expanded.is_absolute() {
        return Err(format!("`{raw}` is not an absolute path"));
    }
    // Three outcomes, not two: absent, present, and *could not look*. The
    // last is never folded into the first — see CLAUDE.md.
    match std::fs::symlink_metadata(&expanded) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(format!("{}: no such directory", expanded.display())),
        Err(e) => return Err(format!("cannot read {}: {e}", expanded.display())),
        Ok(_) => {}
    }
    // A symlink to a directory is fine as a root; `metadata` follows it for
    // this one question.
    match std::fs::metadata(&expanded) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(format!("{}: not a directory", expanded.display())),
        Err(e) => return Err(format!("cannot read {}: {e}", expanded.display())),
    }
    let canon = expanded.canonicalize().map_err(|e| format!("cannot resolve {}: {e}", expanded.display()))?;
    let already = current.iter().any(|r| r.canonicalize().map(|c| c == canon).unwrap_or(false));
    if already {
        return Err(format!("{} is already a root", canon.display()));
    }
    // The environment wins over the file for every read; writing the file
    // would change nothing visible and read as a silent failure.
    if env_roots.map(|v| !v.trim().is_empty()).unwrap_or(false) {
        return Err("this roost's roots come from ROOST_ROOTS; add it there (the unit file, for a service)".into());
    }
    let mut list: Vec<String> = match crate::config::raw_setting(global, "roots") {
        Some(SettingValue::List(l)) => l,
        _ => Vec::new(),
    };
    list.push(canon.display().to_string());
    crate::config::write_setting(global, "roots", Some(&SettingValue::List(list)))?;
    let mut out = current.to_vec();
    out.push(canon);
    Ok(out)
}
```

In `src/config.rs`, change `fn expand_home(s: &str) -> PathBuf` to `pub(crate) fn expand_home(s: &str) -> PathBuf`. In `src/lib.rs` add `pub mod roots;` in alphabetical position.

`write_setting`'s global-file lock: `config::set_setting` takes the `GLOBAL_WRITE` mutex for global writes; `add_root` writes the global file too, so expose the lock: in `config.rs` add `pub(crate) fn with_global_write_lock<T>(f: impl FnOnce() -> T) -> T { let _g = GLOBAL_WRITE.lock().unwrap_or_else(|e| e.into_inner()); f() }` and wrap the `write_setting` call in `add_root` with it.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib roots:: -- --test-threads=1 && cargo test --lib -- --test-threads=1`
Expected: PASS. (The tilde test sets `HOME`; it runs single-threaded so no other test observes it — note this in the test.)

- [ ] **Step 5: Revert-check.** Change the `Err(e) =>` arm of the `symlink_metadata` match to return "no such directory" — the unreadable case fails. Restore. Change `canon` to `expanded` in the written entry — the canonical assertion in `a_duplicate_is_detected_canonically_and_tilde_expands` fails on the `./` spelling. Restore. Record both.

- [ ] **Step 6: Commit**

```bash
git add src/roots.rs src/lib.rs src/config.rs
git commit -m "roots: add_root validates a directory and appends it to the global config"
```

---

### Task 2: The roots socket and per-connection roots

**Files:**
- Modify: `src/roots.rs` (`handle_ws`), `src/lib.rs` (`route_ws` dispatch; accept loop), `src/main.rs` (no exit)

**Interfaces:**
- Produces: `roots::handle_ws(stream: TcpStream)`; `/ws/_roots` accepting `{"t":"AddRoot","path":"…"}` and answering `{"t":"Roots","roots":[…]}` or `{"t":"Error","msg":"…"}`.

- [ ] **Step 1: Write the failing test** in `tests/integration.rs`, modelled on the existing websocket tests there (find one that opens `ws://127.0.0.1:{port}/ws/…` with an `Origin` header and reads a frame — reuse its helper for the handshake):

```rust
/// `/ws/_roots`: one exchange, Origin-checked. A missing Origin is refused at
/// the handshake like every browser-facing socket; a loopback Origin gets one
/// reply and the file changes; a second connection sees the new root.
#[test]
fn roots_socket_adds_a_root_and_refuses_a_handshake_without_origin() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let global = d.path().join("config.toml");
    std::env::set_var("ROOST_CONFIG", &global);
    std::env::remove_var("ROOST_ROOTS");
    let dir = d.path().join("projects"); std::fs::create_dir(&dir).unwrap();
    let port = start_server_with_no_roots(); // the file's existing start helper, with an empty root list
    // No Origin: refused.
    assert!(ws_connect(port, "/ws/_roots", None).is_err(), "a handshake without Origin must be refused");
    // Loopback Origin: one exchange.
    let mut ws = ws_connect(port, "/ws/_roots", Some(&format!("http://127.0.0.1:{port}"))).unwrap();
    ws.send(tungstenite::Message::Text(format!(r#"{{"t":"AddRoot","path":"{}"}}"#, dir.display()).into())).unwrap();
    let reply = ws.read().unwrap().into_text().unwrap();
    assert!(reply.contains(r#""t":"Roots""#) && reply.contains(&dir.canonicalize().unwrap().display().to_string()), "{reply}");
    assert!(std::fs::read_to_string(&global).unwrap().contains("roots = ["), "the global file gained the list");
    // The next request sees it: the front page's roots label names it.
    let page = http_get(port, "/");
    assert!(page.contains(&dir.canonicalize().unwrap().display().to_string()), "roots are re-read per connection");
    std::env::remove_var("ROOST_CONFIG");
}
```

Adapt `start_server_with_no_roots`, `ws_connect` and `http_get` to whatever helpers `tests/integration.rs` already has (search for `fn start`, `tungstenite::client`, and an existing GET helper); if the start helper always sets roots, add a variant that passes an empty list. Name the helpers you used in the report.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test integration roots_socket -- --test-threads=1`
Expected: FAIL — the handshake with Origin reaches the terminal fallback and is refused or hangs; the "Roots" assertion never holds.

- [ ] **Step 3: Implement.** In `src/roots.rs`:

```rust
use std::net::TcpStream;
use serde::Deserialize;
use tungstenite::handshake::server::{Request as WsRequest, Response as WsResponse};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::Message;

#[derive(Debug, Deserialize)]
#[serde(tag = "t")]
enum RootsIntent {
    AddRoot { path: String },
}

/// `/ws/_roots`: the front page's one write. One exchange per connection.
pub fn handle_ws(stream: TcpStream) {
    // WebSocket handshakes bypass the same-origin policy: without this check
    // any page the user visits could extend the directories roost serves.
    // Same check as wsconn.rs and term.rs.
    let allowed = crate::config::allowed_origins();
    let config = WebSocketConfig { max_message_size: Some(64 * 1024), ..Default::default() };
    let accepted = tungstenite::accept_hdr_with_config(
        stream,
        |req: &WsRequest, resp: WsResponse| {
            let origin = req.headers().get("origin").and_then(|v| v.to_str().ok());
            if !crate::origin::origin_allowed(origin, &allowed) {
                eprintln!("roost: rejected roots ws origin={origin:?} (set allowed_origins)");
                return Err(tungstenite::http::Response::builder().status(403).body(Some("origin not allowed".to_string())).expect("static 403 response"));
            }
            Ok(resp)
        },
        Some(config),
    );
    let Ok(mut ws) = accepted else { return };
    let reply = match ws.read() {
        Ok(Message::Text(t)) => match serde_json::from_str::<RootsIntent>(&t) {
            Ok(RootsIntent::AddRoot { path }) => {
                let current = crate::projects::roots();
                let env = std::env::var("ROOST_ROOTS").ok();
                match add_root(&path, &current, env.as_deref(), &crate::config::global_config_path()) {
                    Ok(list) => serde_json::json!({ "t": "Roots", "roots": list.iter().map(|p| p.display().to_string()).collect::<Vec<_>>() }),
                    Err(msg) => serde_json::json!({ "t": "Error", "msg": msg }),
                }
            }
            Err(e) => serde_json::json!({ "t": "Error", "msg": format!("bad intent: {e}") }),
        },
        _ => return,
    };
    let _ = ws.send(Message::Text(reply.to_string().into()));
    let _ = ws.close(None);
}
```

If the crate's tungstenite is configured through a `wsio` gate elsewhere, this single-threaded socket needs none: one reader, one writer, same thread.

In `src/lib.rs` `route_ws`, before the terminal fallback:

```rust
    if segs == ["_roots"] {
        return roots::handle_ws(stream);
    }
```

In the accept loop, replace `let roots = roots.clone();` with `let roots = projects::roots();` and add above the loop: `// Per connection, not once: a root added from the front page must be seen by the next request. An env read and one small file parse, the cost the rest of config already pays per request.` Keep the startup `roots` for `registry::reconcile` (rename it `startup_roots` to make the split visible).

In `src/main.rs`, replace the `if roots.is_empty() { eprintln!(…); std::process::exit(2); }` block with:

```rust
    if roots.is_empty() {
        // Not fatal any more: the front page explains the state and offers
        // Add path. The notice stays for whoever reads the log first.
        eprintln!(
            "roost: no project roots configured — add one from the front page, \
             or set ROOST_ROOTS / `roots` in ~/.config/roost/config.toml"
        );
    }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -- --test-threads=1`
Expected: PASS, including the new integration test. If an existing integration test asserted that an empty root list exits 2, update it to assert the notice and a running server instead, and say so in the report.

- [ ] **Step 5: Revert-check.** Restore `let roots = roots.clone();` in the accept loop — the "re-read per connection" assertion fails. Restore. Remove the `Origin` check callback (accept unconditionally) — the no-Origin assertion fails. Restore. Record both.

- [ ] **Step 6: Commit**

```bash
git add src/roots.rs src/lib.rs src/main.rs tests/integration.rs
git commit -m "roots: /ws/_roots adds a root; roots are re-read per connection; an empty list no longer refuses to start"
```

---

### Task 3: The front page

**Files:**
- Modify: `src/routes.rs` (`serve_index`, the `_overview_projects` arm), `src/render.rs` (`overview_page`, `overview_projects`), `static/style.css`

**Interfaces:**
- Produces: `render::overview_page(sel, roots: &[String])`; `render::overview_projects(sel, projects, roots_empty: bool)`; markup: header `<span class="roots">` holds one `<span class="root">` per root and `<button id="addroot" title="add a project root">+</button>`; the empty state is `<div class="ovempty"><p>…</p><button class="addroot" type="button">Add path</button></div>`; the page loads `/static/dialog.js` and carries the `dlg-confirm` and `dlg-text` shells plus `DIALOG_STRUCTURAL_CSS`.

- [ ] **Step 1: Write the failing tests** in `src/render.rs`'s `mod tests`:

```rust
    #[test]
    fn the_front_page_lists_roots_with_an_add_control_and_carries_the_dialogs() {
        let h = overview_page("", &["/home/x/projects".into(), "/srv/<code>".into()]);
        assert!(h.contains(r#"<span class="root">/home/x/projects</span>"#), "{h}");
        assert!(h.contains("/srv/&lt;code&gt;"), "escaped: {h}");
        assert!(h.contains(r#"<button id="addroot" type="button" title="add a project root">+</button>"#), "{h}");
        assert!(h.contains(r#"<script src="/static/dialog.js"></script>"#), "{h}");
        for id in ["dlg-confirm", "dlg-text"] {
            assert!(h.contains(&format!(r#"id="{id}""#)), "no {id} shell on the front page");
        }
        assert!(h.contains(DIALOG_STRUCTURAL_CSS), "the lock guards the front page's dialogs too");
        // No roots: the header still carries the control, so the page has two ways in.
        let none = overview_page("", &[]);
        assert!(none.contains(r#"id="addroot""#), "{none}");
    }

    #[test]
    fn the_projects_fragment_explains_an_empty_root_list_and_offers_add_path() {
        let h = overview_projects("", &[], true);
        assert!(h.contains("Roost has no defined paths where to look for projects."), "{h}");
        assert!(h.contains("search it for git repositories"), "{h}");
        assert!(h.contains(r#"<button class="addroot" type="button">Add path</button>"#), "{h}");
        // Roots exist but hold no projects: no explanation, an ordinary empty list.
        let empty = overview_projects("", &[], false);
        assert!(!empty.contains("Add path"), "{empty}");
    }
```

Update the two existing callers in tests: `overview_page(sel, "/roots")` → `overview_page(sel, &["/roots".into()])`; every `overview_projects(sel, &projects)` → `overview_projects(sel, &projects, false)`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib render::tests::the_front_page -- --test-threads=1`
Expected: compile error (signature).

- [ ] **Step 3: Implement.** `overview_page(sel: &str, roots: &[String])`: build the roots markup as

```rust
    let roots_html: String = roots.iter().map(|r| format!("<span class=\"root\">{}</span>", esc(r))).collect();
```

and in the template replace `<span class=\"roots\" title=\"{roots}\">{roots}</span>` with `<span class=\"roots\">{roots_html}<button id=\"addroot\" type=\"button\" title=\"add a project root\">+</button></span>`; add after `<link … style.css>`: `{DIALOG_STRUCTURAL_CSS}`; before `<script src="/static/overview.js">` add the two shells copied verbatim from the workspace template (`dlg-confirm`, `dlg-text`) and `<script src=\"/static/dialog.js\"></script>`. `DIALOG_STRUCTURAL_CSS` must come after style.css, as on the workspace page.

`overview_projects(sel, projects, roots_empty: bool)`: when `roots_empty`, return

```rust
    if roots_empty {
        return "<div class=\"ovempty\"><p>Roost has no defined paths where to look for projects. Add a path where your projects live; roost will search it for git repositories and list them here.</p><button class=\"addroot\" type=\"button\">Add path</button></div>".to_string();
    }
```

`routes.rs`: `serve_index` passes `roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>()`; the `_overview_projects` arm passes `roots.is_empty()`; `_overview_worktrees` is unchanged.

`static/style.css`, in the overview section (find `.overview-body` or `.roots`):

```css
header .roots { display: inline-flex; align-items: center; gap: 6px; flex-wrap: wrap; color: var(--muted); font-size: 12px; }
header .roots .root { font-family: var(--mono); }
header .roots .root + .root::before { content: ""; display: inline-block; width: 1px; height: 12px; background: var(--border); margin-right: 6px; vertical-align: -2px; }
#addroot { font: inherit; font-size: 14px; line-height: 1; width: 22px; height: 22px; border: 1px solid var(--border); border-radius: 6px; background: none; color: var(--muted); cursor: pointer; }
#addroot:hover { color: var(--fg); border-color: var(--accent); }
.ovempty { padding: 24px 16px; max-width: 46em; }
.ovempty p { color: var(--muted); line-height: 1.5; margin: 0 0 14px; }
.ovempty .addroot { font: inherit; padding: 6px 14px; border: 1px solid var(--accent); border-radius: 6px; background: none; color: var(--fg); cursor: pointer; }
```

If an existing `.roots` rule (e.g. `title` ellipsis) conflicts, replace it.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib render::tests -- --test-threads=1 && cargo test --lib routes -- --test-threads=1`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/routes.rs src/render.rs static/style.css
git commit -m "front page: the roots list with +, the empty-roots explanation with Add path, and the dialogs"
```

---

### Task 4: The Add flow in `overview.js`

**Files:**
- Modify: `static/overview.js`
- Create: `tests/browser/roots.mjs`

**Interfaces:**
- Consumes: `askText({ title, label, value, confirm })` from `dialog.js` (a global), `/ws/_roots`, the markup from Task 3, `refresh(which, sel)` inside the IIFE.

- [ ] **Step 1: Write the failing browser test** `tests/browser/roots.mjs`:

```js
//! Adding project roots from the front page: the empty state, Add path, the
//! + beside the roots, and a refused path. Starts roost with ROOST_ROOTS
//! empty (which `roots_from` treats as unset) and the global config on an
//! empty file, so there are no roots at all.
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const globalToml = `${fx.base}/global.toml`;
await Deno.writeTextFile(globalToml, "# global\n");
const second = `${fx.base}/more`;
await Deno.mkdir(`${second}/other`, { recursive: true });
await new Deno.Command("git", { args: ["init", "-q"], cwd: `${second}/other`, stdout: "null", stderr: "null" }).output();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: "", port: await freePort(), extraEnv: { ROOST_CONFIG: globalToml } });
const browser = await startBrowser(profileDir(repoRoot));
let page;
try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/`);
  const { evalIn } = page;
  await until(() => evalIn(`!!document.querySelector("#ovprojects .ovempty, #ovprojects .ovtree")`), 20, "projects pane");

  console.log("A. no roots: the explanation and Add path");
  ok(await evalIn(`/no defined paths where to look for projects/.test(document.querySelector("#ovprojects .ovempty")?.textContent || "")`), "the pane explains the state");
  ok(await evalIn(`!!document.querySelector("#ovprojects .ovempty .addroot")`), "and offers Add path");
  ok((await evalIn(`document.querySelectorAll("header .roots .root").length`)) === 0, "the header lists no roots");

  console.log("\nB. Add path lists the fixture's projects and writes the file");
  await evalIn(`document.querySelector("#ovprojects .ovempty .addroot").click(); 0`);
  ok(await until(() => evalIn(`document.getElementById("dlg-text").open`), 5, "dialog"), "the text dialog opened");
  ok(/project root/i.test(await evalIn(`document.querySelector("#dlg-text .dlg-title").textContent`)), "titled for a root");
  await evalIn(`(() => { const i = document.getElementById("dlg-input"); i.value = ${JSON.stringify(fx.roots)}; i.dispatchEvent(new Event("input")); })(); document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  ok(await until(() => evalIn(`!!document.querySelector('#ovprojects .ovrow')`), 15, "projects"), "the projects list appeared");
  ok(await until(() => evalIn(`[...document.querySelectorAll("header .roots .root")].some((r) => r.textContent === ${JSON.stringify(fx.roots)})`), 5, "header"), "the header shows the root");
  ok(await until(async () => /roots = \[/.test(await Deno.readTextFile(globalToml)), 5, "file"), "the global file holds the list");
  ok(/^# global\n/.test(await Deno.readTextFile(globalToml)), "and kept its comment");

  console.log("\nC. + adds a second root");
  await evalIn(`document.getElementById("addroot").click(); 0`);
  await until(() => evalIn(`document.getElementById("dlg-text").open`), 5, "dialog again");
  await evalIn(`(() => { const i = document.getElementById("dlg-input"); i.value = ${JSON.stringify(second)}; i.dispatchEvent(new Event("input")); })(); document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  ok(await until(() => evalIn(`document.querySelectorAll("header .roots .root").length === 2`), 5, "two roots"), "the header shows two roots");
  ok(await until(() => evalIn(`[...document.querySelectorAll("#ovprojects .ovrow")].some((r) => /other/.test(r.textContent))`), 15, "other project"), "and the second root's project is listed");

  console.log("\nD. a path that does not exist is refused and the text is kept");
  await evalIn(`document.getElementById("addroot").click(); 0`);
  await until(() => evalIn(`document.getElementById("dlg-text").open`), 5, "dialog");
  await evalIn(`(() => { const i = document.getElementById("dlg-input"); i.value = "/nowhere/at/all"; i.dispatchEvent(new Event("input")); })(); document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  ok(await until(() => evalIn(`/no such directory/.test([...document.querySelectorAll(".error-banner")].map((b) => b.textContent).join(" "))`), 5, "banner"), "a banner names the refusal");
  ok(await until(() => evalIn(`document.getElementById("dlg-text").open && document.getElementById("dlg-input").value === "/nowhere/at/all"`), 5, "reopened"), "the dialog reopened with the text kept");
  await evalIn(`document.querySelector("#dlg-text .dlg-cancel").click(); 0`);
  ok((await evalIn(`document.querySelectorAll("header .roots .root").length`)) === 2, "and the root list is unchanged");
} finally {
  try { await page?.close(); } catch {}
  browser.close();
  await roost.close();
  await fx.cleanup();
}
console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
```

`startRoost` passes `roots` straight into `ROOST_ROOTS`; an empty string is treated as unset by `roots_from`. `fx.roots` is the fixture's roots directory containing `proj`. Check `fixture()` returns `roots` (it does: `{ base, roots, project, dir, stateDir, cleanup }`).

- [ ] **Step 2: Run to verify it fails**

Run: `deno run -A tests/browser/roots.mjs`
Expected: section A passes (Task 3 shipped the markup), section B fails at "the text dialog opened".

- [ ] **Step 3: Implement** in `static/overview.js`, inside the IIFE, before the `dblclick` handler:

```js
  // Adding a root. The front page has no socket of its own, so the one write
  // it makes travels on /ws/_roots — one exchange, then closed. Everything
  // shown comes back through textContent: a path is data.
  function banner(text) {
    const box = document.createElement("div");
    box.className = "conflict error-banner";
    const b = document.createElement("b"); b.textContent = text;
    const dismiss = document.createElement("button"); dismiss.textContent = "dismiss"; dismiss.onclick = () => box.remove();
    box.append(b, dismiss);
    document.body.appendChild(box);
    setTimeout(() => box.remove(), 8000);
  }
  function renderRoots(list) {
    const span = document.querySelector("header .roots");
    if (!span) return;
    span.querySelectorAll(".root").forEach((r) => r.remove());
    const add = document.getElementById("addroot");
    for (const r of list) {
      const s = document.createElement("span"); s.className = "root"; s.textContent = r;
      span.insertBefore(s, add);
    }
  }
  function sendAddRoot(path) {
    return new Promise((resolve) => {
      const ws = new WebSocket(`ws://${location.host}/ws/_roots`);
      let done = false;
      const finish = (v) => { if (!done) { done = true; resolve(v); } try { ws.close(); } catch {} };
      ws.onopen = () => ws.send(JSON.stringify({ t: "AddRoot", path }));
      ws.onmessage = (e) => { try { finish(JSON.parse(e.data)); } catch { finish({ t: "Error", msg: "unreadable reply" }); } };
      ws.onerror = () => finish({ t: "Error", msg: "could not reach roost" });
      ws.onclose = () => finish({ t: "Error", msg: "connection closed before a reply" });
    });
  }
  async function addRootFlow(prefill = "") {
    const path = await askText({ title: "Add a project root", label: "Directory to scan for projects", value: prefill, confirm: "Add" });
    if (!path) return;
    const reply = await sendAddRoot(path);
    if (reply.t === "Roots") {
      renderRoots(reply.roots);
      const sel = selNow();
      refresh("proj", sel);
      refresh("sess", sel);
    } else {
      banner(`Error: ${reply.msg}`);
      // A typo is one edit away: reopen with the text kept.
      addRootFlow(path);
    }
  }
  document.addEventListener("click", (e) => {
    if (e.target.closest("#addroot, .addroot")) { e.preventDefault(); addRootFlow(); }
  });
```

`askText` is defined by `dialog.js`, loaded before `overview.js` (Task 3). `location.host` carries the port; over `tailscale serve` on 443 it carries the host alone and `ws://` must become `wss://` when `location.protocol === "https:"` — use `` `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws/_roots` ``, which is how the workspace connects its sockets (check `connectControl` in app.js and match it).

- [ ] **Step 4: Run the test**

Run: `deno run -A tests/browser/roots.mjs`
Expected: PASS, all sections.

- [ ] **Step 5: Revert-checks**, restored and recorded in the test:
  1. Remove `renderRoots(reply.roots)` — section B's header assertion fails.
  2. Remove the `addRootFlow(path)` re-open — section D's "reopened with the text kept" fails.

- [ ] **Step 6: Commit**

```bash
git add static/overview.js tests/browser/roots.mjs
git commit -m "front page: Add path and + open the text dialog and add the root over /ws/_roots"
```

---

### Task 5: Docs and the full run

**Files:**
- Modify: `src/config.rs` (the `roots` doc string), `docs/deploy.md`, `README.md`, `tests/browser/README.md`, the spec's status line

- [ ] **Step 1: `config.rs`.** The `roots` row's doc becomes `"Directories scanned for projects. Add one from the front page."`. Run `cargo test --lib config::tests::the_settings_view -- --test-threads=1`.

- [ ] **Step 2: deploy.md.** In the `ROOST_ROOTS` row of the environment table and the paragraph that says an unset `ROOST_ROOTS` refuses to start, replace the refusal with: roost starts, prints a notice, and the front page offers **Add path**; roots added there land in `~/.config/roost/config.toml`'s `roots` list; when `ROOST_ROOTS` is set the front page refuses and says where to add it. Search for `no project roots configured` and `exit 2` wording and correct it.

- [ ] **Step 3: README.** In the Install block, after `ROOST_ROOTS="$HOME/Projects" roost 8444`, add a comment line: `# or just: roost 8444 — then add your projects directory on the front page`.

- [ ] **Step 4: tests/browser/README.md.** Add after the `settings.mjs` line:

```
deno run -A tests/browser/roots.mjs      # no roots: the front page explains and Add path works; + adds another; a bad path is refused
```

- [ ] **Step 5: Spec status** → `*2026-09-06. Status: implemented (see the plan of the same date).*`

- [ ] **Step 6: The full run**

Run: `cargo test -- --test-threads=1`; then one at a time `deno run -A tests/browser/roots.mjs`, `overview.mjs`, `dialogs.mjs`. Paste the summary lines into the commit message.

- [ ] **Step 7: Commit**

```bash
git add src/config.rs docs/deploy.md README.md tests/browser/README.md docs/superpowers/specs/2026-09-06-add-project-roots-design.md
git commit -m "docs: adding project roots from the front page"
```
