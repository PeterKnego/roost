# Clickable Links in the Terminal — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a reference printed in a terminal reachable — a URL opens in a
browser tab, a path to a file in this project opens as a tab in the middle pane
— with everything resh guesses at behind a modifier key.

**Architecture:** Two xterm link providers scrape URLs and paths out of each
row and return nothing unless Cmd/Ctrl is held; xterm's already-registered
`OscLinkProvider` is switched on by supplying the `linkHandler` option, and is
*not* gated because the application declared those links itself. A path is never
resolved in the browser: the matched span travels verbatim in a new
`Intent::OpenPath`, and one function in `projects.rs` strips the `:line` suffix,
maps `~`/absolute forms onto the project root, and ends at `safe_resolve` —
which answers both "inside the project" and "actually there" — before any tab
reaches the shared layout.

**Tech Stack:** Rust (no async, no runtime), hand-rolled HTTP + websockets,
vendored xterm.js 5.x, plain JS with no framework, Deno + Chromium for browser
tests.

**Spec:** `docs/superpowers/specs/2026-08-21-terminal-links-design.md`

## Where to run this

**Implement in the primary checkout `/home/claude/projects/resh`, not in a
worktree.** The spec was written in `.claude/worktrees/terminal-links`, which is
safe for markdown and unsafe for cargo: this host points every workspace at one
shared `target-dir`, and `build.rs` bakes *absolute* asset paths into its
generated table. A `cargo build` from a second checkout rewrites that table with
the other checkout's paths and leaves the shared binary built from the other
tree — while reporting `Fresh resh` and letting the browser tests go on passing
against the wrong source. If you have already built from the worktree, recover
with `cargo clean -p resh` and confirm with the `grep` in CLAUDE.md's
*Verify, don't trust*.

## Global Constraints

Copied from CLAUDE.md and the spec. Every task's requirements include these.

- **Every filesystem path is confined** before use — `projects::safe_resolve`
  for existing targets. A regex is a convenience, never the boundary.
- **HTTP stays GET-only** apart from the two existing POSTs. This feature adds
  **no HTTP surface at all**: it is a websocket intent, and inherits the
  socket's `Origin` check.
- **`cargo test`, never `cargo test --release`.**
- **Module-level `//!` doc; `#[cfg(test)] mod tests` at the bottom of the same
  file.** Comments give rationale, not mechanics.
- **No panics may escape a socket thread.** Every new path returns `Result`.
- **"I could not determine X" is never folded into "X is false."** Where this
  plan reads metadata, it matches three ways — present, absent, and unreadable.
  Nothing here is destructive, so refusing is always the safe answer.
- **Every new test gets the revert-the-fix check**: apply the broken version,
  run it, read the failure, restore — and record the failure mode in the test's
  own comment. A test that cannot fail is the dominant defect class here.
- **Confinement tests must create a real file at the escape target**, or the
  test errors with `ENOENT` before reaching the confinement check and passes
  green while proving nothing.
- **`send_to`, never `broadcast`, for a refusal.** Tests for it need **two**
  subscribers or the two are indistinguishable.
- **http and https only**, everywhere a URL is opened, including OSC 8
  destinations chosen by the running application.

---

### Task 1: The resolver

The whole trust boundary, in one pure function with no I/O beyond `metadata`
and `canonicalize`. Everything else in this plan is wiring.

**Files:**
- Modify: `src/projects.rs` (add beside `safe_resolve`; tests into the existing
  `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `projects::safe_resolve(project_dir: &Path, rel: &str) -> Result<PathBuf, String>` (exists)
- Produces: `projects::resolve_terminal_path(project_dir: &Path, text: &str) -> Result<String, String>` — `Ok` carries the project-relative path with the `:line` suffix removed; `Err` carries a message meant to be shown to a person in a terminal flash.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/projects.rs`:

```rust
    #[test]
    fn terminal_path_resolves_relative_absolute_and_tilde() {
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

        let err = resolve_terminal_path(&root, outside.to_str().unwrap()).unwrap_err();
        assert!(
            err.contains("outside this project"),
            "expected a confinement refusal naming the reason, got {err:?}"
        );

        let err = resolve_terminal_path(&root, "../secret.txt").unwrap_err();
        assert!(!err.is_empty(), "a ../ escape must refuse");
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

    #[test]
    fn terminal_path_refuses_a_file_that_is_not_there() {
        let root = tempfile::tempdir().unwrap();
        let err = resolve_terminal_path(root.path(), "src/gone.rs").unwrap_err();
        assert!(!err.is_empty(), "a missing file must refuse, not resolve");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib projects::tests::terminal_path`
Expected: FAIL — `cannot find function 'resolve_terminal_path' in this scope`.

- [ ] **Step 3: Implement the resolver**

Add to `src/projects.rs`, immediately after `safe_resolve`:

```rust
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

    let abs = safe_resolve(project_dir, &rel)?;
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
fn abs_to_rel(project_dir: &Path, abs: &Path) -> Result<String, String> {
    let root = project_dir
        .canonicalize()
        .map_err(|e| format!("project root unreadable: {e}"))?;
    let abs = abs.canonicalize().map_err(|_| "no such file".to_string())?;
    abs.strip_prefix(&root)
        .map_err(|_| "path is outside this project".to_string())
        .map(|p| p.to_string_lossy().into_owned())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib projects::tests::terminal_path`
Expected: PASS, 5 tests.

- [ ] **Step 5: Revert-the-fix check**

Do this for real — apply, run, read, restore. Not a thought experiment.

1. Replace the `abs.strip_prefix(&root)` arm in `abs_to_rel` with
   `Ok(abs.to_string_lossy().into_owned())`. Expected:
   `terminal_path_refuses_a_real_file_outside_the_project` fails.
2. Delete the `Ok(m) if m.is_dir()` arm. Expected:
   `terminal_path_refuses_a_directory` fails.
3. Make `strip_line_suffix` return `text` unchanged. Expected:
   `terminal_path_strips_line_and_column` fails.

Restore all three, re-run, and record the exact failure messages in a comment
above the tests.

- [ ] **Step 6: Commit**

```bash
git add src/projects.rs
git commit -m "projects: resolve a path a terminal printed, or refuse it"
```

---

### Task 2: The wire types

**Files:**
- Modify: `src/proto.rs` (the `Intent` enum, the `Event` enum, and `mod tests`)

**Interfaces:**
- Produces: `Intent::OpenPath { text: String }` and `Event::PathRefused { text: String, msg: String }`, both externally tagged on `"t"` like every other variant.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/proto.rs`:

```rust
    #[test]
    fn decodes_open_path_verbatim() {
        let i = decode(r#"{"t":"OpenPath","text":"~/p/resh/src/a.rs:42"}"#).unwrap();
        // Verbatim is the contract: the client does no parsing, so the suffix
        // and the tilde must both survive the wire intact.
        match i {
            Intent::OpenPath { text } => assert_eq!(text, "~/p/resh/src/a.rs:42"),
            other => panic!("decoded to {other:?}"),
        }
    }

    #[test]
    fn encodes_path_refused_with_both_fields() {
        let s = encode(&Event::PathRefused {
            text: "src/gone.rs".into(),
            msg: "cannot read src/gone.rs".into(),
        });
        // The client matches on `text` to find the terminal that was clicked,
        // so dropping it would leave the message with nowhere to go.
        assert!(s.contains(r#""t":"PathRefused""#), "got {s}");
        assert!(s.contains(r#""text":"src/gone.rs""#), "got {s}");
        assert!(s.contains(r#""msg":"cannot read src/gone.rs""#), "got {s}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib proto::tests`
Expected: FAIL — `no variant named 'OpenPath'` / `no variant named 'PathRefused'`.

- [ ] **Step 3: Add the variants**

In `src/proto.rs`, add to `enum Intent` after `NewTerminal`:

```rust
    /// A span matched in terminal output, sent **verbatim** —
    /// `~/projects/resh/src/a.rs:42` and all. Deliberately not pre-parsed by
    /// the client: the parser and the confinement it feeds belong together, in
    /// Rust, next to `safe_resolve`.
    ///
    /// Separate from `OpenTab` because `OpenTab` validates nothing —
    /// `apply_layout` pushes the tab straight into the layout that is then
    /// broadcast to every connected browser. A path a regex guessed at cannot
    /// be allowed down that road.
    OpenPath { text: String },
```

And to `enum Event`, after `Error`:

```rust
    /// A terminal link that would not resolve.
    ///
    /// Distinct from `Error` for two reasons: `Error` funnels to the workspace
    /// banner, which is the wrong shape for a link that missed and is already
    /// on the backlog to be redesigned; and it carries no way back to the
    /// terminal that was clicked (see the `Error` case in app.js, which says
    /// exactly that). Sent with `send_to` and never broadcast — one person's
    /// mis-click must not flash every window in the project.
    PathRefused { text: String, msg: String },
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib proto::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/proto.rs
git commit -m "proto: an intent for a path a terminal printed, and its refusal"
```

---

### Task 3: The hub handler

**Files:**
- Modify: `src/hub.rs` (a match arm in `handle`, a new `do_open_path`, tests)

**Interfaces:**
- Consumes: `projects::resolve_terminal_path` (Task 1), `Intent::OpenPath` and `Event::PathRefused` (Task 2), and the existing `workspace::apply_layout`, `Hub::send_to`, `Hub::broadcast`, `Hub::snapshot_event`, `Hub::persist`, `Hub::dir`.
- Produces: `fn do_open_path(&mut self, from: &ConnId, text: String)`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/hub.rs`. Match the fixture style already used there
(`Hub::new` / `Hub::for_project` plus `h.handle(&conn, intent)`); read a
neighbouring test first and follow it rather than inventing a second style.

```rust
    #[test]
    fn open_path_opens_the_file_it_names() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), b"fn main() {}").unwrap();
        let mut h = Hub::new("linkopen", dir.path().to_path_buf());
        let (conn, _rx) = h.subscribe();

        let abs = dir.path().join("src/a.rs");
        h.handle(&conn, Intent::OpenPath { text: format!("{}:42", abs.display()) });

        // The rel, not the count. "a tab opened" passes for the wrong file.
        let tabs = &h.ws.panes[proto::MIDDLE as usize].tabs;
        assert!(
            tabs.iter().any(|t| matches!(t, Tab::File { rel, mode: Mode::Preview } if rel == "src/a.rs")),
            "expected a Preview tab for src/a.rs, got {tabs:?}"
        );
    }

    /// Two subscribers, deliberately. With one, `send_to` and `broadcast` are
    /// indistinguishable and this test would pass with the privacy removed —
    /// which is on CLAUDE.md's own list of tests that passed for the wrong
    /// reason.
    #[test]
    fn open_path_refusal_reaches_only_the_client_that_asked() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = Hub::new("linkrefuse", dir.path().to_path_buf());
        let (asker, rx_asker) = h.subscribe();
        let (_other, rx_other) = h.subscribe();

        h.handle(&asker, Intent::OpenPath { text: "src/gone.rs".into() });

        let got: Vec<String> = rx_asker.try_iter().collect();
        assert!(
            got.iter().any(|m| m.contains("PathRefused")),
            "the asking client got no refusal: {got:?}"
        );
        let others: Vec<String> = rx_other.try_iter().collect();
        assert!(
            !others.iter().any(|m| m.contains("PathRefused")),
            "a refusal leaked to a second browser: {others:?}"
        );
    }

    #[test]
    fn open_path_refuses_without_touching_the_layout() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = Hub::new("linknotab", dir.path().to_path_buf());
        let (conn, _rx) = h.subscribe();
        let before = h.ws.panes[proto::MIDDLE as usize].tabs.len();

        h.handle(&conn, Intent::OpenPath { text: "../../etc/passwd".into() });

        // The whole reason resolution happens before the layout changes: a
        // dead tab would land in every connected browser's window.
        assert_eq!(
            h.ws.panes[proto::MIDDLE as usize].tabs.len(),
            before,
            "a refused path still added a tab"
        );
    }

    #[test]
    fn open_path_coerces_an_image_to_preview() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        let mut h = Hub::new("linkpng", dir.path().to_path_buf());
        let (conn, _rx) = h.subscribe();

        h.handle(&conn, Intent::OpenPath { text: "shot.png".into() });

        // Proves it went THROUGH apply_layout/coerce_tab rather than around
        // it — the reason the handler builds an OpenTab instead of pushing a
        // tab itself.
        let tabs = &h.ws.panes[proto::MIDDLE as usize].tabs;
        assert!(
            tabs.iter().any(|t| matches!(t, Tab::File { rel, mode: Mode::Preview } if rel == "shot.png")),
            "expected a coerced Preview tab for shot.png, got {tabs:?}"
        );
    }
```

No test helper is needed: `Hub::subscribe(&mut self) -> (ConnId, Receiver<String>)`
already hands back both the id and the channel that client receives on, which
is exactly what tells "sent to this one" apart from "sent to everyone". Calling
it twice is the whole two-subscriber fixture.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib hub::tests::open_path`
Expected: FAIL — the `Intent::OpenPath` arm falls through to `apply_layout`,
which returns `Err("...")` for an unhandled intent, so no tab appears and no
`PathRefused` is ever sent.

- [ ] **Step 3: Implement the handler**

In `src/hub.rs`, add a match arm in `handle` beside the other delegating arms
(next to `Intent::NewTerminal { pane } => ...`):

```rust
            Intent::OpenPath { text } => return self.do_open_path(from, text.clone()),
```

And the method, beside `do_new_terminal`:

```rust
    /// A terminal link resolves *before* anything reaches the layout.
    ///
    /// `OpenTab` validates nothing: `apply_layout` pushes the tab straight in
    /// and the resulting snapshot goes to every connected browser. Since a
    /// path scraped out of terminal output is a guess, opening optimistically
    /// would leave a dead tab in everyone's window for one person's false
    /// positive — so the guess is settled here, and only a real file is
    /// allowed to become an `OpenTab`.
    ///
    /// Building that `OpenTab` rather than reaching into the panes directly is
    /// what makes a `.png` from a terminal coerce exactly as one clicked in
    /// the tree does, and what gets tab de-duplication (`find_tab`) for free.
    fn do_open_path(&mut self, from: &ConnId, text: String) {
        let rel = match crate::projects::resolve_terminal_path(&self.dir, &text) {
            Ok(rel) => rel,
            Err(msg) => {
                let ev = Event::PathRefused { text, msg };
                return self.send_to(from, &ev);
            }
        };
        let intent = Intent::OpenTab {
            pane: proto::MIDDLE,
            tab: Tab::File { rel, mode: Mode::Preview },
        };
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib hub::tests::open_path`
Expected: PASS, 4 tests.
Then run the whole suite and **time it** — a deadlock hangs rather than fails,
so a green count alone says nothing about lock ordering:
`time cargo test`

- [ ] **Step 5: Revert-the-fix check**

1. Change `self.send_to(from, &ev)` in the `Err` arm to `self.broadcast(&ev)`.
   Expected: `open_path_refusal_reaches_only_the_client_that_asked` fails on
   the second assertion — the leak. This is the one that proves the two-
   subscriber fixture is doing work.
2. Move the `apply_layout` call above the `resolve_terminal_path` call, using
   the raw `text` as the rel. Expected:
   `open_path_refuses_without_touching_the_layout` fails.

Restore both and record the failure messages in a comment above the tests.

- [ ] **Step 6: Commit**

```bash
git add src/hub.rs
git commit -m "hub: a terminal link resolves before it reaches anyone's layout"
```

---

### Task 4: The matchers and the modifier gate

First client task. Produces links that underline while armed and do nothing
when clicked — deliberately, so the gate can be reviewed on its own.

**Files:**
- Modify: `static/app.js` (module-level helpers, plus a call inside `newTerminal`)
- Create: `tests/browser/termlinks.mjs`
- Modify: `tests/browser/README.md` (add the run line)

**Interfaces:**
- Consumes: the `entry` object built in `newTerminal` (`{ node, term, fit, sock, … }`).
- Produces: `registerTermLinks(term, entry)`, called from `newTerminal` after `term.open(node)`; module-level `linksArmed`, `linkModifier(e)`, `openUrl(u)`, `SAFE_URL`.

- [ ] **Step 1: Write the browser test**

Create `tests/browser/termlinks.mjs`, modelled on `tests/browser/copyselect.mjs`
(same shape: real resh, real dtach, real Chromium). Read that file first and
reuse its fixture calls rather than inventing new ones. Assertions for this
task:

```js
//! Are terminal links marked only when the user asks for them?
//!
//! No Rust test reaches static/app.js, so the matchers, the modifier gate and
//! everything a click does live entirely outside `cargo test`.
//!
//! The trap this file is written against: asserting that a mouse event was
//! accepted rather than that a link exists. xterm returns link ranges from a
//! provider; "the provider ran" is true with the gate deleted.

// 1. armed state is off by default
await assert(
  "no link is offered with the modifier up",
  async () => (await linksAt(page, "docs/backlog.md")) === 0,
);

// 2. and on while held
await assert(
  "a path is offered as a link while the modifier is held",
  async () => (await linksAt(page, "docs/backlog.md", { modifier: true })) === 1,
);

// 3. a URL wins over the path inside it
await assert(
  "https://example.com/a/b is one URL link, not a path link",
  async () => (await linkTextAt(page, "https://example.com/a/b", { modifier: true }))
    === "https://example.com/a/b",
);

// 4. a bare filename is deliberately not a path
await assert(
  "a bare filename with no directory offers no link",
  async () => (await linksAt(page, "backlog.md", { modifier: true })) === 0,
);
```

`linksAt` drives xterm's own provider through `page.evaluate`, asking the
registered providers directly rather than synthesising mouse events — the
providers are the unit under test and the mouse path is Task 7's problem.

- [ ] **Step 2: Run it to verify it fails**

Run: `deno run -A tests/browser/termlinks.mjs`
Expected: FAIL on assertion 2 — no provider is registered, so a held modifier
offers nothing.

- [ ] **Step 3: Implement the matchers and the gate**

Add near the top of `static/app.js`, beside the other module-level constants:

```js
// Cmd on macOS, Ctrl everywhere else. Not a preference: Ctrl+click on a Mac is
// right-click emulation, so binding there would pop a context menu and open a
// link at once. This is the same platform split xterm itself makes in
// shouldForceSelection (alt on Mac, shift elsewhere).
const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
const linkModifier = (e) => (IS_MAC ? e.metaKey : e.ctrlKey);

// Tracked rather than read off the event, because provideLinks is never handed
// one. Cleared on blur: a user who switches apps with the key down would
// otherwise come back to a terminal that is silently armed.
let linksArmed = false;

// http and https only. `javascript:`, `data:` and `file:` never become a link
// at all — not a refused one, one that was never offered. Applied to OSC 8
// destinations too (Task 6), which the running application chooses and which
// are therefore no more trustworthy than plain text.
const SAFE_URL = /^https?:\/\//i;
const URL_RE = /\bhttps?:\/\/[^\s"'<>`]+/gi;
// A slash is the evidence. Bare `main.rs` is deliberately not a path: a repo
// has many, so resolution would have to guess, and the same shape matches a
// version string and the `foo.bar` in an error message.
const PATH_RE = /(?:~\/|\.{1,2}\/|\/)?(?:[\w.@+-]+\/)+[\w.@+-]+(?::\d+(?::\d+)?)?/g;

function openUrl(u) {
  if (!SAFE_URL.test(u)) return;
  // noopener,noreferrer: an opened page gets no handle back to the workspace.
  window.open(u, "_blank", "noopener,noreferrer");
}

// A URL at the end of a sentence, or inside prose parentheses, must not
// swallow the punctuation. A parenthesised segment *within* the URL survives,
// which is why the bracket trim counts rather than strips.
function trimUrl(u) {
  u = u.replace(/[.,;:!?'"]+$/, "");
  while (
    u.endsWith(")") &&
    (u.match(/\(/g) || []).length < (u.match(/\)/g) || []).length
  ) {
    u = u.slice(0, -1);
  }
  return u;
}
```

Then the provider factory and its registration:

```js
// One provider per pattern, URL registered first. Where the two overlap — the
// path-looking tail of a URL — xterm resolves it by provider index in
// _removeIntersectingLinks, so registration order is the entire mechanism.
// xterm's own OscLinkProvider is registered at construction, ahead of both,
// which is the ordering this wants for free: a link an application declared
// beats anything resh would have guessed over the same cells.
function matchProvider(term, re, activate) {
  return {
    provideLinks(y, cb) {
      // The gate. No link exists to hover, so nothing underlines and nothing
      // can be clicked — rather than a link that exists and refuses.
      if (!linksArmed) return cb(undefined);
      const line = term.buffer.active.getLine(y - 1);
      if (!line) return cb(undefined);
      const text = line.translateToString(true);
      const out = [];
      re.lastIndex = 0;
      for (let m; (m = re.exec(text)); ) {
        const raw = m[0];
        out.push({
          range: {
            start: { x: m.index + 1, y },
            end: { x: m.index + raw.length, y },
          },
          text: raw,
          // Re-checked at click time: an underline left stale by a missed
          // keyup — alt-tabbing away while holding the key — must not open
          // anything.
          activate: (ev) => {
            if (linkModifier(ev)) activate(raw, ev);
          },
        });
      }
      cb(out.length ? out : undefined);
    },
  };
}

function registerTermLinks(term, entry) {
  term.registerLinkProvider(matchProvider(term, URL_RE, (raw) => openUrl(trimUrl(raw))));
  term.registerLinkProvider(matchProvider(term, PATH_RE, (raw) => openTermPath(entry, raw)));
}
```

`openTermPath` is Task 5. For this task only, stub it so the gate can be
reviewed on its own — and make the stub obvious rather than silent:

```js
// Replaced in Task 5.
function openTermPath(entry, raw) {
  console.warn("resh: terminal path link not wired yet:", raw);
}
```

Arming, near the other window-level listeners:

```js
// Arming has to nudge xterm to ask again: the Linkifier caches the last cell
// it resolved (_lastBufferCell) and will not re-ask for the same position, so
// a bare re-dispatch at the current spot is ignored. Moving through a
// different cell first invalidates that cache using nothing but public events.
//
// If this proves unreliable, the graceful degradation is that arming takes
// effect on the next real pointer movement, which is what a user holding a
// modifier is about to do anyway.
let lastPointer = null;
addEventListener("mousemove", (e) => { lastPointer = { x: e.clientX, y: e.clientY }; }, true);

function nudgeLinks() {
  if (!lastPointer) return;
  const el = document.elementFromPoint(lastPointer.x, lastPointer.y);
  const host = el && el.closest && el.closest(".termhost");
  if (!host) return;
  const r = host.getBoundingClientRect();
  const cell = Math.max(1, Math.round(r.width / 80));
  const away = { x: r.left + (lastPointer.x - r.left > cell ? 1 : cell + 1), y: lastPointer.y };
  for (const p of [away, lastPointer]) {
    host.dispatchEvent(
      new MouseEvent("mousemove", { clientX: p.x, clientY: p.y, bubbles: true }),
    );
  }
}

function setArmed(on) {
  if (linksArmed === on) return;
  linksArmed = on;
  nudgeLinks();
}

addEventListener("keydown", (e) => { if (linkModifier(e)) setArmed(true); });
addEventListener("keyup", (e) => { if (!linkModifier(e)) setArmed(false); });
addEventListener("blur", () => setArmed(false));
```

And call it in `newTerminal`, immediately after `term.open(node)` — `entry` is
built on the next line today, so move the `registerTermLinks` call to just
after the `const entry = { … }` assignment:

```js
  registerTermLinks(term, entry);
```

- [ ] **Step 4: Run the browser test to verify it passes**

Run: `deno run -A tests/browser/termlinks.mjs`
Expected: PASS, 4 assertions.

- [ ] **Step 5: Revert-the-fix check**

1. Delete the `if (!linksArmed) return cb(undefined);` line. Expected:
   assertion 1 fails — a link is offered with the modifier up.
2. Register the path provider before the URL provider. Expected: assertion 3
   fails, reporting the path text rather than the whole URL.
3. Change `PATH_RE` to allow zero directory segments. Expected: assertion 4
   fails.

Restore all three and record the failure messages in the file's header
comment, following the style of `mdlinks.mjs`.

- [ ] **Step 6: Commit**

```bash
git add static/app.js tests/browser/termlinks.mjs tests/browser/README.md
git commit -m "app: mark a terminal link only while the user is asking for one"
```

---

### Task 5: What a click does

**Files:**
- Modify: `static/app.js` (replace the `openTermPath` stub, add the `PathRefused` case in `onEvent`)
- Modify: `tests/browser/termlinks.mjs` (extend)

**Interfaces:**
- Consumes: `send(intent)`, `termFlash(entry, text)`, `Intent::OpenPath` (Task 2), `Event::PathRefused` (Task 2), `do_open_path` (Task 3).
- Produces: `openTermPath(entry, raw)`; module-level `pendingLink`.

- [ ] **Step 1: Write the failing assertions**

Append to `tests/browser/termlinks.mjs`:

```js
// 5. a real path opens the file it names — the rel, not the tab count
await assert(
  "modifier+click on a real path opened docs/backlog.md",
  async () => (await openTabRels(page, 2)).includes("docs/backlog.md"),
);

// 6. a false positive opens nothing and says so. Asserting the tab count is
//    unchanged, so "opened the wrong file" cannot pass as "correctly refused".
const before = (await openTabRels(page, 2)).length;
await clickLink(page, "nope/missing.rs", { modifier: true });
await assert(
  "a path that does not resolve added no tab",
  async () => (await openTabRels(page, 2)).length === before,
);
await assert(
  "and flashed the refusal in the terminal that was clicked",
  async () => /cannot read|no such file/.test(await flashText(page)),
);
```

- [ ] **Step 2: Run to verify they fail**

Run: `deno run -A tests/browser/termlinks.mjs`
Expected: FAIL on assertion 5 — `openTermPath` is still the console.warn stub.

- [ ] **Step 3: Implement**

Replace the stub in `static/app.js`:

```js
// The click that was sent, so a refusal can be shown in the terminal it came
// from. A single slot, not a map: only one link can be clicked at a time, and
// a map keyed by text would grow for the life of the page.
let pendingLink = null;

function openTermPath(entry, raw) {
  pendingLink = { entry, text: raw };
  // The line number is matched so the whole reference underlines, then dropped
  // — the viewer has no line addressing to spend it on. Saying so is the only
  // thing between "we ignored part of what you clicked" and silence.
  const line = raw.match(/:(\d+)(?::\d+)?$/);
  if (line) termFlash(entry, `line ${line[1]} — opening file`);
  // Verbatim. The client does no parsing; resolution and confinement are one
  // function in projects.rs.
  send({ t: "OpenPath", text: raw });
}
```

And add to the `switch` in `onEvent`, beside the `Error` case:

```js
    case "PathRefused":
      // Not showError: that funnels to the workspace banner, which is the
      // wrong shape here and — as the Error case below notes — carries no way
      // back to the terminal that was clicked. This does, via the click still
      // in flight.
      console.warn("resh:", ev.msg);
      if (pendingLink && pendingLink.text === ev.text) {
        termFlash(pendingLink.entry, ev.msg);
      }
      pendingLink = null;
      break;
```

- [ ] **Step 4: Run to verify it passes**

Run: `deno run -A tests/browser/termlinks.mjs`
Expected: PASS, 7 assertions.
Then: `cargo test` — the Rust side is untouched but the intent is now exercised
end to end, and a decode mismatch would surface here.

- [ ] **Step 5: Revert-the-fix check**

1. Change the `PathRefused` case to call `showError(ev.msg)` instead of
   `termFlash`. Expected: assertion 7 fails — no flash on the terminal.
2. Have `openTermPath` send `{ t: "OpenTab", pane: 2, tab: { k: "File", rel: raw, mode: "Preview" } }`
   instead. Expected: assertion 6 fails — the dead tab appears, which is the
   whole defect this design exists to prevent.

Restore both and record the failure messages in the header comment.

- [ ] **Step 6: Commit**

```bash
git add static/app.js tests/browser/termlinks.mjs
git commit -m "app: open what a terminal link names, or say why not"
```

---

### Task 6: OSC 8 hyperlinks

The one kind of link that is not gated, because the application marked it
itself.

**Files:**
- Modify: `static/app.js` (the `new Terminal({ … })` options in `newTerminal`)
- Modify: `tests/browser/termlinks.mjs` (extend)

**Interfaces:**
- Consumes: `openUrl`, `SAFE_URL` (Task 4).
- Produces: the `linkHandler` option on every terminal.

- [ ] **Step 1: Write the failing assertions**

Append to `tests/browser/termlinks.mjs`. Write the OSC 8 sequence into the real
shell with `printf`, so the terminal parses it exactly as an application's
would:

```js
// 8. an application's own hyperlink needs no modifier
await typeInTerm(page, String.raw`printf '\e]8;;https://example.com/osc\e\\click me\e]8;;\e\\\n'` + "\n");
await assert(
  "an OSC 8 link is offered with no modifier held",
  async () => (await linksAt(page, "click me")) === 1,
);

// 9. and its destination is still scheme-checked
await typeInTerm(page, String.raw`printf '\e]8;;javascript:alert(1)\e\\bad\e]8;;\e\\\n'` + "\n");
await assert(
  "a javascript: OSC 8 destination opened nothing",
  async () => (await windowOpenCalls(page)).length === 0,
);
```

`windowOpenCalls` stubs `window.open` on the page and records its arguments —
asserting on what was *asked for*, since Chromium blocks the popup either way
and "no new tab appeared" would pass with the check deleted. That is the same
trap `mdlinks.mjs` hit with its `javascript:` assertion; read its note 6.

- [ ] **Step 2: Run to verify they fail**

Run: `deno run -A tests/browser/termlinks.mjs`
Expected: FAIL on assertion 8 — `linkHandler` is `null`, so the ranges
`OscLinkProvider` tracks are inert.

- [ ] **Step 3: Implement**

In `static/app.js`, add to the `new Terminal({ … })` options in `newTerminal`:

```js
    // xterm already registers an OscLinkProvider, so OSC 8 sequences are
    // parsed and their ranges tracked; this option is the only thing missing,
    // and it defaults to null. Not gated on the modifier, unlike the matchers:
    // the application said in a control sequence that these cells are a link,
    // so there is no guess to protect the user from.
    //
    // The destination is still scheme-checked in openUrl. What is running
    // chooses it, which makes it exactly as trustworthy as plain text.
    linkHandler: {
      activate: (ev, uri) => openUrl(uri),
    },
```

- [ ] **Step 4: Run to verify it passes**

Run: `deno run -A tests/browser/termlinks.mjs`
Expected: PASS, 9 assertions.

- [ ] **Step 5: Revert-the-fix check**

1. Set `linkHandler: null`. Expected: assertion 8 fails.
2. Delete the `SAFE_URL` guard in `openUrl`. Expected: assertion 9 fails,
   recording a `window.open` call with a `javascript:` argument. If it still
   passes, the stub is not capturing — fix the test before trusting it.

Restore both and record the failure messages in the header comment.

- [ ] **Step 6: Commit**

```bash
git add static/app.js tests/browser/termlinks.mjs
git commit -m "app: honour the hyperlinks an application declares for itself"
```

---

### Task 7: Settle the mouse-reporting question

The spec's stated open risk, and the only task whose outcome is not known in
advance. It is a task rather than a check because the answer may require code.

**Files:**
- Modify: `tests/browser/termlinks.mjs` (extend)
- Modify: `static/app.js` (only if the answer is "no")

**Interfaces:**
- Consumes: everything from Tasks 4–6.

- [ ] **Step 1: Write the assertion that asks the question**

Needs a program that actually turns mouse reporting on — a shell prompt does
not. Enable it directly, the way an application would, then verify resh's own
copy-on-select comment still describes reality:

```js
// 10. a plain click still belongs to the running application
await typeInTerm(page, String.raw`printf '\e[?1000h'` + "\n"); // mouse reporting on
await clickLink(page, "docs/backlog.md", { modifier: false });
await assert(
  "a plain click on a path reached the application, not resh",
  async () => (await openTabRels(page, 2)).length === tabsBefore,
);

// 11. and a modifier+click still opens, with the app holding the mouse
await clickLink(page, "docs/backlog.md", { modifier: true });
await assert(
  "modifier+click opened the file even with mouse reporting on",
  async () => (await openTabRels(page, 2)).includes("docs/backlog.md"),
);
```

- [ ] **Step 2: Run it and read the answer**

Run: `deno run -A tests/browser/termlinks.mjs`

Two outcomes, both legitimate:

- **Assertion 11 passes** — xterm's `Linkifier` does fire under mouse
  reporting. Nothing to implement. Record the result in the header comment and
  in the spec's *risk* section, replacing the open question with the answer,
  and go to Step 4.
- **Assertion 11 fails** — the core's `cancel(e)` is eating the event before
  the `Linkifier` sees it. Implement the fallback in Step 3.

- [ ] **Step 3: The fallback, only if assertion 11 failed**

A capture-phase listener on the `.termhost` node, ahead of both xterm handlers,
which does its own hit-testing and never consults the `Linkifier`:

```js
// Capture phase, and only while armed. xterm's core cancels mousedown once an
// application has asked for mouse reporting, which takes the event away from
// the Linkifier before it can offer a link — so while armed, resh resolves the
// cell itself and stops the event before either handler runs.
//
// Deliberately inert when not armed: an unarmed click must reach the
// application untouched, which is the whole point of the gate.
node.addEventListener("mousedown", (e) => {
  if (!linksArmed || !linkModifier(e) || e.button !== 0) return;
  const hit = linkAtPoint(term, node, e.clientX, e.clientY);
  if (!hit) return;
  e.preventDefault();
  e.stopPropagation();
  hit.activate(e);
}, true);
```

`linkAtPoint` maps client coordinates to a buffer cell via the node's bounding
rect and `term.cols`/`term.rows`, then runs the same `URL_RE`/`PATH_RE` scan
over that row and returns the match covering the column — reusing
`matchProvider`'s body rather than duplicating the patterns. Factor the scan
out of `matchProvider` into `scanRow(term, y, re)` and have both call it.

Re-run and confirm assertions 10 and 11 both pass.

- [ ] **Step 4: Verify against a real Claude session, by hand**

Automation established what xterm does with a raw escape sequence. Whether it
feels right under the actual application is the thing this project has twice
learned a green suite does not answer.

Start resh, open a terminal, run `claude`, and confirm by hand:
- Clicking in Claude's UI still does what Claude expects, modifier up.
- Holding the modifier underlines the paths in Claude's messages.
- Clicking one opens the file, and Claude does not also react to the click.
- Releasing the modifier removes the underline.

- [ ] **Step 5: Run everything, on the Linux host too**

```bash
time cargo test
deno run -A tests/browser/termlinks.mjs
deno run -A tests/browser/copyselect.mjs   # nearest neighbour: shares the mouse path
deno run -A tests/browser/altscreen.mjs    # shares the mouse-reporting path
```

Then `ssh` to the Linux host and run `cargo test` there. Both the host run and
the by-hand browser check have caught defects here that 100+ passing tests did
not.

- [ ] **Step 6: Update the spec and commit**

Replace the spec's *"The risk this design cannot resolve on paper"* section
with what was actually observed, so the next reader inherits the answer rather
than the question.

```bash
git add tests/browser/termlinks.mjs static/app.js docs/superpowers/specs/2026-08-21-terminal-links-design.md
git commit -m "app: settle whether a link survives an app that owns the mouse"
```

---

## Self-Review

**Spec coverage.** Every section maps to a task: the gate and its platform
split → Task 4; arming/disarming → Task 4; the matchers, `:line` consumption
and the bare-filename exclusion → Task 4; `OpenPath` and server-side resolution
→ Tasks 1–3; optimistic-mark-not-optimistic-open → Task 3's
`open_path_refuses_without_touching_the_layout`; `PathRefused` over `Error`,
`send_to` over `broadcast` → Tasks 2, 3, 5; OSC 8 and the scheme allowlist →
Task 6; the open mouse-reporting risk → Task 7. The spec's three open questions
are answered by their stated defaults, plus the flash naming a dropped line
number in Task 5.

**Type consistency.** `resolve_terminal_path(&Path, &str) -> Result<String, String>`
is defined in Task 1 and consumed with that exact signature in Task 3.
`Intent::OpenPath { text }` and `Event::PathRefused { text, msg }` are defined
in Task 2 and used with those field names in Tasks 3 and 5. `openTermPath(entry, raw)`
is stubbed in Task 4 and replaced with the same signature in Task 5.
`scanRow(term, y, re)` appears only in Task 7 and only if the fallback is
needed, where it is factored out of Task 4's `matchProvider`.

**Known softness, flagged rather than hidden.** Two things in Task 4 are
written from a reading of minified xterm and may need adjusting against the
real API: whether `provideLinks`'s `y` indexes the absolute buffer (assumed
yes, matching `WebLinksAddon`'s `getLine(y - 1)`), and whether `nudgeLinks`
reliably invalidates `_lastBufferCell`. The first is a one-line fix if wrong;
the second degrades to "arming takes effect on the next mouse move", which is
acceptable and must be recorded in the comment rather than left as a silent
difference. Column mapping also assumes no double-width characters in a matched
span, which is true of paths and URLs.
