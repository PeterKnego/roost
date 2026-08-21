# Buffers Without Text — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every open file tab has a buffer that records what its content was based on; only an edited buffer holds the file's text.

**Architecture:** A buffer splits into a *base* (`base_hash`, `base_mtime`) captured by a real disk read at open time, and *content* which is either `Clean` (nothing held; disk is the truth) or `Edited(String)`. The server decides which by hashing incoming text against the base, so no client path can materialise text by accident. The filesystem watcher is fed the set of open file *tabs* rather than the set of buffers, so a previewed file gets change notifications whether or not it has a buffer.

**Tech Stack:** Rust (no async runtime, thread per connection), hand-rolled websocket protocol in `src/proto.rs`, plain JS in `static/app.js`, `cargo test` for Rust and Deno + headless Chromium for anything in `static/`.

**Spec:** `docs/superpowers/specs/2026-08-21-buffers-without-text-design.md`

## Global Constraints

- `cargo test`, never `cargo test --release`.
- Tests live in `#[cfg(test)] mod tests` at the bottom of the same file as the implementation.
- Module-level `//!` docs explain *why*; comments give rationale, not mechanics.
- **Absence of evidence is not evidence of absence.** A failed read is a third outcome, never folded into "the file is empty" or "the file is gone". Before anything destructive use `symlink_metadata` and match `Err(NotFound)` → absent, `Err(_)` → cannot tell, do nothing.
- **A save must never write from a buffer with no text.** `fileops::save` writes what it is handed; the type must make "edited with no text" unrepresentable, and `do_save` must refuse rather than write an empty string.
- Never hold a lock across blocking I/O.
- Caps: ≤50 buffers *holding text*, 2 MB per file for reads and buffer writes (`workspace::MAX_TEXT_BYTES`).
- Existing on-disk state files must keep loading. Their `BufferDisk` is `{text, dirty, base_hash}`.
- Browser tests are not run by `cargo test` and must skip when no browser is present.

---

### Task 1: The watcher notices any open file, not only buffers

Today `watch.rs` feeds `classify` the *buffer* keys, so a file open in Preview is classified `Class::Tree` and the browser is told only "something changed". This task makes the per-file path apply to every open file tab. It is the server half of the reported bug and is worth shipping on its own.

**Files:**
- Modify: `src/workspace.rs` (add `open_file_rels`, with tests)
- Modify: `src/watch.rs:377` (feed classify from open tabs ∪ buffers)
- Modify: `src/hub.rs:522` (`file_changed_externally` must not return early when there is no buffer)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `workspace::Workspace::open_file_rels(&self) -> Vec<String>` — every `Tab::File`'s `rel` across all panes, deduplicated, in no guaranteed order.

- [ ] **Step 1: Write the failing test for `open_file_rels`**

In `src/workspace.rs`, in `mod tests`:

```rust
#[test]
fn open_file_rels_lists_previewed_files_not_just_edited_ones() {
    let mut w = Workspace::default_layout();
    apply_layout(&mut w, &Intent::OpenTab {
        pane: proto::MIDDLE,
        tab: Tab::File { rel: "read.md".into(), mode: Mode::Preview },
    }).unwrap();
    apply_layout(&mut w, &Intent::OpenTab {
        pane: proto::RIGHT,
        tab: Tab::File { rel: "write.rs".into(), mode: Mode::Edit },
    }).unwrap();
    let mut got = w.open_file_rels();
    got.sort();
    // The Preview entry is the whole point: it has no buffer, so the old
    // buffers-only list could not contain it.
    assert_eq!(got, vec!["read.md".to_string(), "write.rs".to_string()]);
    assert!(w.buffers.is_empty(), "no buffer exists yet — that is why this list is needed");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test open_file_rels_lists_previewed`
Expected: FAIL — `no method named 'open_file_rels'`.

- [ ] **Step 3: Implement it**

In `src/workspace.rs`, in `impl Workspace`:

```rust
    /// Every file a tab currently shows, in either mode.
    ///
    /// Separate from `buffers.keys()` on purpose: the watcher used that, so a
    /// file open in Preview — which has no buffer — was classified as a
    /// generic tree change and its pane never heard that it had changed. A
    /// tab is the thing that means "somebody is looking at this file", which
    /// is the question the watcher is actually asking.
    pub fn open_file_rels(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        self.panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter_map(|t| match t {
                Tab::File { rel, .. } => Some(rel.clone()),
                _ => None,
            })
            .filter(|rel| seen.insert(rel.clone()))
            .collect()
    }
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test open_file_rels_lists_previewed`
Expected: PASS.

- [ ] **Step 5: Write the failing test for the hub broadcast**

In `src/hub.rs`, in `mod tests`. Follow the existing tests' construction of a hub (`fn hub_fixture` or the pattern its neighbours use — read one first and copy it exactly):

```rust
    /// A previewed file has no buffer, and `file_changed_externally` used to
    /// return before its broadcast in exactly that case:
    ///
    ///     let Some(b) = self.ws.buffers.get_mut(rel) else { return true };
    ///
    /// so nothing downstream ever heard that the file on screen had changed.
    /// Reverting that line to the early return fails this test.
    #[test]
    fn an_external_change_to_a_previewed_file_is_broadcast() {
        let (mut h, dir, rx) = hub_with_project();
        std::fs::write(dir.path().join("read.md"), "before\n").unwrap();
        let a = ConnId::from("a");
        h.subscribe(&a, /* … as the neighbouring tests do … */);
        h.handle(&a, Intent::OpenTab {
            pane: proto::MIDDLE,
            tab: Tab::File { rel: "read.md".into(), mode: Mode::Preview },
        });
        drain(&rx);

        std::fs::write(dir.path().join("read.md"), "after\n").unwrap();
        assert!(h.file_changed_externally(dir.path(), "read.md"));

        let msgs = drain(&rx);
        assert!(
            msgs.iter().any(|m| m.contains(r#""t":"FileChanged""#) && m.contains("read.md")),
            "a previewed file's change must reach the browser, got {msgs:?}"
        );
    }
```

- [ ] **Step 6: Run it and watch it fail**

Run: `cargo test an_external_change_to_a_previewed_file`
Expected: FAIL — no `FileChanged` in the drained messages.

- [ ] **Step 7: Make the broadcast unconditional**

In `src/hub.rs`, in `file_changed_externally`, replace the early return with a branch that still falls through to the broadcast:

```rust
        // No buffer is not "nothing to tell anyone": a file open in Preview
        // has no buffer and still has a pane showing it. The buffer branches
        // below update what a *buffer* holds; the broadcast at the end is
        // what every open tab needs either way.
        if let Some(b) = self.ws.buffers.get_mut(rel) {
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
                    origin: String::new(),
                };
                self.broadcast(&ev);
            }
        }
        self.ws.version += 1;
        self.broadcast(&Event::FileChanged { rel: rel.to_string() });
        true
```

- [ ] **Step 8: Feed the watcher from tabs as well as buffers**

In `src/watch.rs`, at the line that builds `open` (currently `let open: Vec<String> = h.ws.buffers.keys().cloned().collect();`):

```rust
                // Tabs, not just buffers: a previewed file has no buffer, and
                // classifying it as a generic tree change is why its pane
                // never refreshed. Buffers are still unioned in because one
                // can outlive its tab for as long as it takes a close to be
                // processed.
                let mut open: Vec<String> = h.ws.open_file_rels();
                open.extend(h.ws.buffers.keys().cloned());
                open.sort();
                open.dedup();
```

- [ ] **Step 9: Run the whole suite**

Run: `cargo test`
Expected: PASS, all of it. Note the run time; a deadlock here hangs rather than fails.

- [ ] **Step 10: Commit**

```bash
git add src/workspace.rs src/watch.rs src/hub.rs
git commit -m "watch: a file being looked at is a file worth reporting

classify was fed the buffer list, so a file open in Preview — which has
no buffer — came through as a generic tree change and its pane was
never told. file_changed_externally could not have helped anyway: it
returned before its own FileChanged broadcast whenever the changed file
had no buffer.

Both now key off the tabs. open_file_rels is the set of files somebody
is actually looking at, which is the question the watcher was asking
all along."
```

---

### Task 2: The browser re-fetches a previewed file when it changes

`app.js` handles `FileChanged` with `refreshKind("Diff")` — diffs only, and by kind rather than by file. This is the client half of the same bug.

**Files:**
- Modify: `static/app.js:172` (the `FileChanged` case) and add a helper beside `refreshKind`
- Create: `tests/browser/preview-follows.mjs`
- Modify: `tests/browser/README.md` (run list and the deleted-code evidence list)

**Interfaces:**
- Consumes: `Event::FileChanged { rel }`, now broadcast for previewed files (Task 1).
- Produces: `refreshFile(rel)` in `static/app.js` — re-mounts any pane whose active tab is a `File` in `Preview` mode for exactly `rel`.

- [ ] **Step 1: Write the failing browser test**

Create `tests/browser/preview-follows.mjs`. Copy the harness preamble from `tests/browser/save.mjs` verbatim (imports, `ok`, fixture, `startResh`, `startBrowser`, `openPage`, the `Emulation.setDeviceMetricsOverride` call — the default 800x600 window collapses the middle pane to zero width). Then:

```js
  console.log("A. a previewed file follows the disk");
  await Deno.writeTextFile(`${fx.roots}/proj/watched.md`, "# before\n");
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "watched.md", mode: "Preview" } })`);
  const shown = () => evalIn(`(document.querySelector('.pane[data-pane="2"] .content') || {}).textContent || ""`);
  ok(await until(async () => (await shown()).includes("before"), 10, "the preview"),
     "the file is on screen");

  // Written by something other than resh, which is the whole case: an edit
  // resh itself made is suppressed as a self-write.
  await Deno.writeTextFile(`${fx.roots}/proj/watched.md`, "# after\n");
  ok(await until(async () => (await shown()).includes("after"), 15, "the update"),
     "it updates without a reload");
  ok(!(await shown()).includes("before"), "and the old content is gone");
```

- [ ] **Step 2: Run it and watch it fail**

Run: `deno run -A tests/browser/preview-follows.mjs`
Expected: FAIL — "it updates without a reload" times out with the old text still on screen.

- [ ] **Step 3: Implement `refreshFile` and wire it up**

In `static/app.js`, beside `refreshKind`:

```js
/// Re-mounts a previewed file after it changed on disk.
///
/// By `rel`, not by kind like `refreshKind`: several panes can show several
/// files, and re-fetching all of them because one changed would throw away
/// the scroll position of panes that did not. Edit mode is deliberately not
/// here — an editor follows the file through `BufferText`, which preserves
/// the buffer's own state machine around dirty and stale.
function refreshFile(rel) {
  if (!state) return;
  state.panes.forEach((pane, pi) => {
    const active = pane.tabs[pane.active];
    if (active && active.k === "File" && active.mode === "Preview" && active.rel === rel) {
      mountTab(document.querySelector(`.pane[data-pane="${pi}"] .content`), active);
    }
  });
}
```

And change the event case:

```js
    case "FileChanged": refreshKind("Diff"); refreshFile(ev.rel); break;
```

- [ ] **Step 4: Run it and watch it pass**

Run: `deno run -A tests/browser/preview-follows.mjs`
Expected: PASS.

- [ ] **Step 5: Prove the test can fail**

Comment out the `refreshFile(ev.rel)` call, run the test again, confirm the update assertion fails, then restore it. Record the count in `tests/browser/README.md`'s deleted-code list, and add the file to the run list at the top of that README.

- [ ] **Step 6: Run every browser test**

Run each of `tests/browser/*.mjs` in turn. Expected: all pass. `mdlinks.mjs` and `save.mjs` are the ones that touch preview and editors respectively.

- [ ] **Step 7: Commit**

```bash
git add static/app.js tests/browser/preview-follows.mjs tests/browser/README.md
git commit -m "preview: a file on screen follows the file on disk

FileChanged was handled by refreshKind('Diff') — diffs only, and by
kind rather than by file. A previewed file was a one-shot fetch that
nothing invalidated, so it showed whatever the file said when it was
opened, forever.

refreshFile matches on rel so one file changing does not throw away the
scroll position of every other pane, and leaves Edit mode alone: an
editor follows its file through BufferText, which carries the dirty and
stale machinery a re-mount would skip."
```

---

### Task 3: A previewed image follows the disk too

An image tab re-mounts to the same `/frag/{proj}/raw?path=…` URL, so the browser may serve the old bytes from cache and the re-fetch changes nothing on screen.

**Files:**
- Modify: `src/render.rs` (`image_fragment`, with tests)

**Interfaces:**
- Consumes: nothing.
- Produces: `render::image_fragment` emits an `img` whose `src` carries a `&v=<mtime-secs>` cache key. Its signature gains the file's modified time: `pub fn image_fragment(project: &str, rel: &str, mtime_secs: u64) -> String`. Callers in `src/routes.rs` pass `std::fs::metadata(&path).ok().and_then(|m| m.modified().ok())` converted with `duration_since(UNIX_EPOCH)`, defaulting to `0` when unavailable.

- [ ] **Step 1: Write the failing test**

In `src/render.rs`, in `mod tests`:

```rust
    /// Without a cache key the img src is byte-identical before and after the
    /// file changes, so the browser is free to reuse what it already has and
    /// the re-mount shows the old picture. Deleting the `&v=` fails this.
    #[test]
    fn an_image_fragment_carries_a_cache_key_that_tracks_the_file() {
        let a = image_fragment("proj", "shot.png", 1_000);
        let b = image_fragment("proj", "shot.png", 2_000);
        assert!(a.contains("v=1000"), "{a}");
        assert!(a != b, "the same file at a different mtime must not reuse one URL");
        // The path is still the path: the key is additional, not a rewrite.
        assert!(a.contains("path=shot.png"), "{a}");
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test an_image_fragment_carries_a_cache_key`
Expected: FAIL — the function takes two arguments, then once that is fixed, no `v=1000`.

- [ ] **Step 3: Implement it**

In `src/render.rs`:

```rust
pub fn image_fragment(project: &str, rel: &str, mtime_secs: u64) -> String {
    format!(
        // `v` is a cache key, not a parameter the route reads: an <img> whose
        // src is unchanged is served from the browser's cache, so a re-mount
        // after the file changed would repaint the old picture.
        "<div class=\"path\">{}</div><img class=\"imgview\" src=\"/frag/{}/raw?path={}&v={}\" alt=\"{}\">",
        esc(rel),
        crate::http::percent_encode(project),
        crate::http::percent_encode(rel),
        mtime_secs,
        esc(rel)
    )
}
```

Update the call site in `src/routes.rs`, reading the mtime beside the existing `safe_resolve`:

```rust
    let mtime_secs = std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        // Not a failure worth refusing the page for: an unreadable mtime only
        // costs the cache key, and 0 is a legitimate "I could not tell".
        .unwrap_or(0);
```

- [ ] **Step 4: Run the suite**

Run: `cargo test`
Expected: PASS. Fix any other `image_fragment` call sites the compiler names.

- [ ] **Step 5: Commit**

```bash
git add src/render.rs src/routes.rs
git commit -m "preview: an image's URL tracks the file it shows

Re-mounting an image tab re-requested the same URL, which the browser
is entitled to serve from cache, so a picture that changed on disk went
on showing its old self. The mtime rides along as a cache key."
```

---

### Task 4: `Content` — a buffer is clean or it holds text

The type change the rest of the work rests on. `dirty` stops being a settable flag and becomes the discriminant, which is what makes "dirty with no text" — whose only reading at save time is "write an empty file over the user's work" — unrepresentable.

**Files:**
- Modify: `src/workspace.rs` (the `Buffer` struct, `Default`, `apply_layout`'s `EditBuffer` arm, `view()`, and every test that constructs a buffer)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `workspace::Content` — `enum Content { Clean, Edited(String) }`
  - `workspace::Buffer { pub content: Content, pub base_mtime: Option<SystemTime>, pub base_hash: u64, pub stale: bool }` — note `dirty` and `text` are gone as fields
  - `Buffer::dirty(&self) -> bool`
  - `Buffer::edited_text(&self) -> Option<&str>` — `Some` only for `Edited`
  - `Buffer::set_text(&mut self, text: String)` — sets `Edited(text)`, or `Clean` when `hash_text(&text) == self.base_hash`

- [ ] **Step 1: Write the failing tests**

In `src/workspace.rs`, in `mod tests`:

```rust
    /// The load-bearing half of the type: a save writes whatever the buffer
    /// holds, so "edited, but holding nothing" must not be expressible. This
    /// asserts the behaviour that replaces it — an edit that matches the base
    /// leaves the buffer clean and holding nothing at all.
    #[test]
    fn an_edit_that_matches_the_base_leaves_the_buffer_clean() {
        let mut w = Workspace::default_layout();
        let base = hash_text("on disk\n");
        w.buffers.insert(
            "a.rs".into(),
            Buffer { base_hash: base, ..Buffer::default() },
        );
        apply_layout(&mut w, &Intent::EditBuffer {
            rel: "a.rs".into(),
            text: "on disk\n".into(),
        }).unwrap();
        let b = &w.buffers["a.rs"];
        assert!(!b.dirty(), "text equal to the base is not an edit");
        assert_eq!(b.edited_text(), None, "and nothing is held for it");
    }

    /// ⌘S on a file that was only looked at goes through pushEdit, which
    /// sends the whole text unconditionally. Without the hash rule above that
    /// would mark the buffer dirty and rewrite the file identically.
    #[test]
    fn a_real_edit_is_held_and_an_undone_one_is_dropped_again() {
        let mut w = Workspace::default_layout();
        let base = hash_text("on disk\n");
        w.buffers.insert("a.rs".into(), Buffer { base_hash: base, ..Buffer::default() });

        apply_layout(&mut w, &Intent::EditBuffer { rel: "a.rs".into(), text: "typed\n".into() }).unwrap();
        assert!(w.buffers["a.rs"].dirty());
        assert_eq!(w.buffers["a.rs"].edited_text(), Some("typed\n"));

        apply_layout(&mut w, &Intent::EditBuffer { rel: "a.rs".into(), text: "on disk\n".into() }).unwrap();
        assert!(!w.buffers["a.rs"].dirty(), "typing a character and deleting it is not an edit");
        assert_eq!(w.buffers["a.rs"].edited_text(), None);
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test an_edit_that_matches_the_base a_real_edit_is_held`
Expected: FAIL — no `dirty()` method, no `edited_text`.

- [ ] **Step 3: Introduce the type**

In `src/workspace.rs`, replacing the current `Buffer` and its `Default`:

```rust
/// What a buffer holds, which is *nothing* until the content actually differs
/// from the file.
///
/// An enum rather than `Option<String>` beside a `dirty` flag, because that
/// pair leaves `dirty: true` with no text expressible, and the only reading
/// of that state at save time is "write an empty file over the user's work".
/// Here `dirty` is the discriminant and cannot disagree with what is held.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Clean,
    Edited(String),
}

#[derive(Debug, Clone)]
pub struct Buffer {
    pub content: Content,
    pub base_mtime: Option<SystemTime>,
    /// What the content was based on when the file was opened. The one piece
    /// of a buffer that cannot be rebuilt later: derived at first edit
    /// instead, it would be the hash of whatever is on disk *then*, silently
    /// swallowing the change it exists to detect.
    pub base_hash: u64,
    pub stale: bool,
}

impl Default for Buffer {
    fn default() -> Self {
        Buffer { content: Content::Clean, base_mtime: None, base_hash: 0, stale: false }
    }
}

impl Buffer {
    pub fn dirty(&self) -> bool {
        matches!(self.content, Content::Edited(_))
    }

    pub fn edited_text(&self) -> Option<&str> {
        match &self.content {
            Content::Edited(t) => Some(t),
            Content::Clean => None,
        }
    }

    /// The one place content becomes dirty. Compares against the base rather
    /// than trusting the caller: `pushEdit` in app.js sends the whole text
    /// before every save, including a ⌘S on a file nobody typed into, and
    /// an undone edit arrives the same way.
    pub fn set_text(&mut self, text: String) {
        self.content = if hash_text(&text) == self.base_hash {
            Content::Clean
        } else {
            Content::Edited(text)
        };
    }
}
```

- [ ] **Step 4: Update `apply_layout`'s `EditBuffer` arm**

```rust
            let b = w.buffers.entry(rel.clone()).or_default();
            b.set_text(text.clone());
            Ok(true)
```

- [ ] **Step 5: Fix the compiler's list**

Run `cargo build` and work through every `b.dirty` → `b.dirty()` and `b.text` → `b.edited_text()` the compiler names *within `src/workspace.rs` only*; leave `hub.rs`, `wsstate.rs` and `wsconn.rs` for Tasks 5–7 by giving them the minimum that compiles (`b.edited_text().unwrap_or_default().to_string()`), and mark each with `// TASK-5`, `// TASK-6`, `// TASK-7` so the next task can find them.

- [ ] **Step 6: Run the suite**

Run: `cargo test`
Expected: PASS. The buffer cap check in the `EditBuffer` arm now counts buffers regardless of content; Task 8 narrows it.

- [ ] **Step 7: Commit**

```bash
git add src/workspace.rs
git commit -m "workspace: a buffer is clean, or it holds an edit

dirty was a flag anything could set beside a text field anything could
leave empty. 'dirty with no text' had exactly one reading at save time
— write an empty file over the user's work — so it becomes
unrepresentable: Content::Clean | Content::Edited(String), with dirty
as the discriminant.

set_text compares against the base rather than trusting its caller,
because app.js's pushEdit sends the whole text before every save,
including a cmd-S on a file nobody typed into. An edit that matches the
file is not an edit."
```

---

### Task 5: The hub's four read sites

**Files:**
- Modify: `src/hub.rs` — `reconcile_buffers_with_disk` (~line 102), `open_for_edit` (~line 472), `file_changed_externally` (~line 517), `do_save` (~line 630)

**Interfaces:**
- Consumes: `Content`, `Buffer::dirty()`, `Buffer::edited_text()`, `Buffer::set_text` from Task 4.
- Produces: no new API; `do_save` returns early with `Event::Error` for a clean buffer.

- [ ] **Step 1: Write the failing test for the save path**

In `src/hub.rs`, in `mod tests`:

```rust
    /// A clean buffer holds no text, so a save that read one would write an
    /// empty string over the file. ⌘S on an untouched file reaches here via
    /// pushEdit, so this is a real path and not a hypothetical one.
    #[test]
    fn saving_a_clean_buffer_writes_nothing() {
        let (mut h, dir, rx) = hub_with_project();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();
        let a = ConnId::from("a");
        h.subscribe(&a, /* … as the neighbouring tests do … */);
        h.handle(&a, Intent::OpenTab {
            pane: proto::MIDDLE,
            tab: Tab::File { rel: "a.rs".into(), mode: Mode::Edit },
        });
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        drain(&rx);

        h.handle(&a, Intent::SaveBuffer { rel: "a.rs".into(), force: false });

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fn main() {}\n");
        // mtime too: an identical rewrite is still a write, and it would make
        // the watcher fire and every other client re-fetch.
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), before);
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test saving_a_clean_buffer_writes_nothing`
Expected: FAIL — the file is rewritten (mtime moves), or an empty write truncates it.

- [ ] **Step 3: Implement the four sites**

`do_save`, before it reaches `fileops::save`:

```rust
        // Nothing to write is not an error the user caused: ⌘S on a file that
        // was opened and never edited is a reasonable thing to press, and the
        // answer is that it is already saved.
        let Some(text) = buf.edited_text().map(|t| t.to_string()) else {
            self.send_to(from, &Event::SaveOk { rel: rel.clone() });
            return;
        };
```

`open_for_edit`: keep the disk read, use it for the base and the push, and store text only if the buffer is already dirty:

```rust
                Ok(text) => {
                    let hash = workspace::hash_text(&text);
                    let mtime =
                        std::fs::metadata(self.dir.join(rel)).ok().and_then(|m| m.modified().ok());
                    let b = self.ws.buffers.entry(rel.to_string()).or_default();
                    b.base_hash = hash;
                    b.base_mtime = mtime;
                    b.content = workspace::Content::Clean;
                    b.stale = false;
                    // Pushed from the read, not from the buffer: the buffer
                    // holds nothing, and the client needs the text to render.
                    self.broadcast(&Event::BufferText {
                        rel: rel.to_string(),
                        text,
                        origin: String::new(),
                    });
                    return;
                }
```

`file_changed_externally`'s clean branch: update the base and broadcast the text it just read, storing none of it.

`reconcile_buffers_with_disk`: compare only for `dirty()` buffers; for a clean one, re-derive `base_hash`/`base_mtime` from disk. A read that fails leaves the buffer untouched — it is not evidence the file is empty.

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test saving_a_clean_buffer_writes_nothing` then `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/hub.rs
git commit -m "hub: read from disk, hold only what was edited

open_for_edit still reads the file — that read is what establishes the
base a conflict is detected against — but pushes the text to the client
instead of keeping a copy. A clean buffer that changes on disk updates
its base and forwards the read. A save against a buffer holding nothing
reports SaveOk and writes nothing, because cmd-S on an untouched file
is a reasonable thing to press and rewriting it identically would fire
the watcher for every other client."
```

---

### Task 6: Persistence stops carrying untouched files

**Files:**
- Modify: `src/wsstate.rs` — `BufferDisk` (line 29), `save` (~line 103), `load` (~line 185)

**Interfaces:**
- Consumes: `Content`, `Buffer::edited_text()` from Task 4.
- Produces: `BufferDisk { text: Option<String>, dirty: bool, base_hash: Option<u64> }`. `text` is `None` for a clean buffer. Old files with a bare `text: String` must still load — serde needs `#[serde(default)]` on the field and a shape that accepts both.

- [ ] **Step 1: Write the failing test**

```rust
    /// The .env case from hub.rs's own comment: a file opened and never typed
    /// into must leave nothing behind. Searched for as a literal in the whole
    /// serialised file rather than by key, so it fails if the text is stored
    /// anywhere under any name.
    #[test]
    fn a_clean_buffer_puts_no_file_content_in_the_state_file() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path());
        let mut w = Workspace::default_layout();
        w.buffers.insert(
            ".env".into(),
            Buffer { base_hash: crate::workspace::hash_text("SECRET=hunter2\n"), ..Buffer::default() },
        );
        save("proj", &w).unwrap();
        let raw = std::fs::read_to_string(d.path().join("proj.json")).unwrap();
        assert!(!raw.contains("hunter2"), "an unedited file's contents must not be persisted: {raw}");
        assert!(raw.contains(".env"), "the buffer itself is still recorded");
    }

    /// The other direction, and the existing guarantee: unsaved work survives
    /// a restart. This is the assertion that stops the fix above from being
    /// implemented by simply not persisting buffers.
    #[test]
    fn an_edited_buffer_still_round_trips_its_text() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATE_DIR", d.path());
        let mut w = Workspace::default_layout();
        let mut b = Buffer { base_hash: crate::workspace::hash_text("on disk\n"), ..Buffer::default() };
        b.set_text("unsaved\n".into());
        w.buffers.insert("a.rs".into(), b);
        save("proj", &w).unwrap();
        let (got, _) = load("proj");
        assert_eq!(got.buffers["a.rs"].edited_text(), Some("unsaved\n"));
        assert!(got.buffers["a.rs"].dirty());
    }
```

- [ ] **Step 2: Run them and watch the first fail**

Run: `cargo test a_clean_buffer_puts_no_file_content`
Expected: FAIL — `hunter2` is in the file.

- [ ] **Step 3: Implement**

```rust
#[derive(Serialize, Deserialize)]
struct BufferDisk {
    /// Absent for a clean buffer: its text is whatever the file says, and
    /// writing it here is how a `.env` opened once ended up in this file for
    /// as long as its tab stayed open. `default` so state files written
    /// before this change still load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    dirty: bool,
    base_hash: Option<u64>,
}
```

`save` maps `b.edited_text().map(|t| t.to_string())` into `text` and `b.dirty()` into `dirty`. `load` rebuilds: `Content::Edited(t)` when `dirty` *and* text is present, `Content::Clean` otherwise — a `dirty: true` with no text is a corrupt or hand-edited file and must load as clean rather than as an empty edit.

- [ ] **Step 4: Run the suite**

Run: `cargo test`
Expected: PASS, including the pre-existing "unsaved text is crash-safe" test unchanged.

- [ ] **Step 5: Commit**

```bash
git add src/wsstate.rs
git commit -m "wsstate: persist an edit, not a file

hub.rs's own comment names the case — 'a .env opened once in Edit' was
written into the state file and kept there. A clean buffer now persists
as its base and nothing else. A dirty: true with no text is a corrupt
file and loads as clean, never as an empty edit."
```

---

### Task 7: A connecting client is served from disk

**Files:**
- Modify: `src/wsconn.rs:91`

**Interfaces:**
- Consumes: `Buffer::edited_text()` from Task 4.
- Produces: no new API.

- [ ] **Step 1: Write the failing test**

This path has no Rust test today because it needs a live socket; the assertion belongs in the browser suite. In `tests/browser/save.mjs`, after the existing sections:

```js
  // --- 6. A second browser onto the same workspace sees the file, not a
  // blank editor. Its text comes from disk now that a clean buffer holds
  // none, so this is the assertion that the disk read happens at all.
  const second = await openPage(browser.port, `http://127.0.0.1:${resh.port}/proj`);
  try {
    await until(() => second.evalIn(`typeof state !== "undefined" && !!(state && state.panes)`), 15, "state");
    ok(await until(async () =>
      ((await second.evalIn(`(document.querySelector("textarea.editor") || {}).value`)) || "").includes("focused edit"),
      10, "the second browser's editor"),
      "a second browser onto an open editor is served its text");
  } finally { second.close(); }
```

- [ ] **Step 2: Run it and watch it fail after Task 6**

Run: `deno run -A tests/browser/save.mjs`
Expected: FAIL — the second browser's editor is empty, because `wsconn` sent `b.text` which no longer exists.

- [ ] **Step 3: Implement**

```rust
        // Text for the edits, disk for the rest: a clean buffer holds
        // nothing, and this client still has to render the file. A read that
        // fails is skipped rather than sent as an empty string — a blank
        // editor over a file that exists is how work gets overwritten.
        let open: Vec<(String, Option<String>)> = h
            .ws
            .buffers
            .iter()
            .map(|(rel, b)| (rel.clone(), b.edited_text().map(|t| t.to_string())))
            .collect();
        for (rel, edited) in open {
            let text = match edited {
                Some(t) => t,
                None => match crate::projects::safe_resolve(&h.dir, &rel)
                    .and_then(|p| crate::projects::read_text_file(&p))
                {
                    Ok(t) => t,
                    Err(_) => continue,
                },
            };
            let ev = proto::Event::BufferText { rel, text, origin: String::new() };
            h.send_to(&id, &ev);
        }
```

- [ ] **Step 4: Run it and watch it pass**

Run: `deno run -A tests/browser/save.mjs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/wsconn.rs tests/browser/save.mjs
git commit -m "wsconn: serve a clean buffer's text from the file

State carries no buffer text, so a connecting client is sent BufferText
for everything open. A clean buffer now holds none of it, so the file is
read instead — and a read that fails is skipped, because a blank editor
over a file that exists is how work gets overwritten."
```

---

### Task 8: A stub for every open file, and a cap that bounds text

**Files:**
- Modify: `src/hub.rs` (the `OpenTab` dispatch ~line 415, `open_for_edit`, the cap check ~line 475)
- Modify: `src/workspace.rs` (the cap check in the `EditBuffer` arm, lines 275-277)

**Interfaces:**
- Consumes: everything above.
- Produces: `MAX_BUFFERS` counts buffers where `dirty()` is true.

- [ ] **Step 1: Write the failing tests**

```rust
    /// Every open file has a buffer, which is what puts a previewed file in
    /// the watcher's list — and it holds nothing until it is edited.
    #[test]
    fn opening_a_file_in_preview_creates_a_buffer_holding_nothing() {
        let (mut h, dir, rx) = hub_with_project();
        std::fs::write(dir.path().join("read.md"), "hello\n").unwrap();
        let a = ConnId::from("a");
        h.subscribe(&a, /* … */);
        h.handle(&a, Intent::OpenTab {
            pane: proto::MIDDLE,
            tab: Tab::File { rel: "read.md".into(), mode: Mode::Preview },
        });
        let b = h.ws.buffers.get("read.md").expect("a previewed file has a buffer");
        assert_eq!(b.edited_text(), None, "and holds nothing");
        assert_eq!(b.base_hash, workspace::hash_text("hello\n"), "with a base taken at open time");
    }

    /// The cap bounds memory, and memory is text. Fifty open files is
    /// browsing; fifty unsaved edits is the thing worth refusing.
    #[test]
    fn the_cap_counts_edits_not_open_files() {
        let mut w = Workspace::default_layout();
        for i in 0..MAX_BUFFERS + 5 {
            w.buffers.insert(format!("f{i}.rs"), Buffer::default());
        }
        // Clean buffers past the cap are fine…
        assert!(apply_layout(&mut w, &Intent::EditBuffer {
            rel: "f0.rs".into(), text: "typed\n".into(),
        }).is_ok());
        // …until that many of them hold edits.
        for i in 1..MAX_BUFFERS {
            w.buffers.get_mut(&format!("f{i}.rs")).unwrap().set_text(format!("edit {i}\n"));
        }
        let err = apply_layout(&mut w, &Intent::EditBuffer {
            rel: format!("f{}.rs", MAX_BUFFERS + 1), text: "one too many\n".into(),
        }).unwrap_err();
        assert!(err.contains("too many"), "got {err}");
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test opening_a_file_in_preview_creates_a_buffer the_cap_counts_edits`
Expected: FAIL — no buffer for a previewed file; the cap refuses at 50 clean buffers.

- [ ] **Step 3: Implement**

In `hub.rs`, the `OpenTab` dispatch loses its `mode: Mode::Edit` restriction — a File tab in *either* mode calls `open_for_edit`, which is now about establishing a base rather than about editing. Keep dispatching off the coerced tab, and keep images out: `refuses_text_edit(rel)` files get no buffer, because reading their bytes as lossy UTF-8 to hash them is meaningless and the watcher reaches them through `open_file_rels` anyway (Task 1).

Rename `open_for_edit` to `open_buffer_for` in the same commit, since the name is now wrong.

In `workspace.rs`, the cap check counts edits:

```rust
            if !w.buffers.get(rel).map(|b| b.dirty()).unwrap_or(false)
                && w.buffers.values().filter(|b| b.dirty()).count() >= MAX_BUFFERS
            {
                return Err("too many unsaved files".into());
            }
```

Mirror the same rule in `hub.rs`'s check.

- [ ] **Step 4: Run everything**

Run: `cargo test`, then every `tests/browser/*.mjs`.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/hub.rs src/workspace.rs
git commit -m "hub: a buffer for every open file, a cap for every edit

A buffer is now what records where a file's content came from, so it is
created when a tab opens in either mode rather than only for Edit —
open_for_edit is renamed accordingly. Images still get none: hashing
their bytes as lossy UTF-8 would be meaningless, and the watcher
reaches them through open_file_rels.

The cap follows the memory. Fifty open files is browsing; fifty unsaved
edits is the thing worth refusing."
```

---

### Task 9: The browser proves the whole loop

**Files:**
- Create: `tests/browser/buffer-lifecycle.mjs`
- Modify: `tests/browser/README.md`

**Interfaces:** consumes everything above.

- [ ] **Step 1: Write the test**

Preamble as in Task 2. Then, with `watched.rs` open in Edit:

```js
  console.log("A. navigating does not make a file dirty");
  // Real key events, not send(): what this is testing is which browser event
  // the client listens on. The input event fires only when the value changes,
  // so none of these must reach EditBuffer.
  for (const [key, code, vk] of [["ArrowDown","ArrowDown",40], ["End","End",35],
                                 ["PageDown","PageDown",34], ["ArrowRight","ArrowRight",39]]) {
    for (const type of ["rawKeyDown", "keyUp"]) {
      await cmd("Input.dispatchKeyEvent", { type, key, code, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk });
    }
  }
  await sleep(1500); // past the 1s autosave timer
  ok(!(await evalIn(`!!(state.buffers.find((b) => b.rel === "watched.rs") || {}).dirty`)),
     "arrows, End and PageDown leave the buffer clean");
  ok((await Deno.stat(file)).mtime.getTime() === mtimeBefore, "and nothing was written");

  console.log("B. ⌘S on an untouched file writes nothing");
  await press(2); // ctrl
  await sleep(500);
  ok((await Deno.stat(file)).mtime.getTime() === mtimeBefore, "the file is untouched");

  console.log("C. typing a character and deleting it comes back clean");
  await type("x");
  ok(await until(async () => await dirty(), 5, "dirty"), "typing marks it dirty");
  await cmd("Input.dispatchKeyEvent", { type: "rawKeyDown", key: "Backspace", code: "Backspace",
                                        windowsVirtualKeyCode: 8, nativeVirtualKeyCode: 8 });
  await cmd("Input.dispatchKeyEvent", { type: "keyUp", key: "Backspace", code: "Backspace",
                                        windowsVirtualKeyCode: 8, nativeVirtualKeyCode: 8 });
  ok(await until(async () => !(await dirty()), 5, "clean again"), "undoing it comes back clean");
```

- [ ] **Step 2: Run it**

Run: `deno run -A tests/browser/buffer-lifecycle.mjs`
Expected: PASS.

- [ ] **Step 3: Prove each assertion can fail**

Revert `Buffer::set_text`'s hash comparison to an unconditional `Content::Edited(text)`, run, and confirm B and C fail while A still passes. Restore. Record the counts in `tests/browser/README.md`.

- [ ] **Step 4: Run the entire suite, both halves**

Run: `cargo test`, then every `tests/browser/*.mjs` in turn. Time the `cargo test` run — a deadlock hangs rather than fails.

- [ ] **Step 5: Update the docs and commit**

Add a line to `CLAUDE.md`'s caps list: the buffer cap is on files with unsaved changes, not open files.

```bash
git add tests/browser/buffer-lifecycle.mjs tests/browser/README.md CLAUDE.md
git commit -m "test: navigation is not an edit, and neither is undoing one

Driven with real key events rather than send(), because what these
assert is which browser event the client listens on — input fires only
when the value changes, which is why arrows and PageDown never reach
EditBuffer. cmd-S on an untouched file and a typed-then-deleted
character are the two paths that reach the server anyway, and the hash
rule is what makes them no-ops."
```

---

## Self-Review

**Spec coverage.** Stub buffers → Task 8. Text on real change only → Task 4. The hash rule → Task 4. `Content` enum → Task 4. Watcher sees open files → Task 1. `file_changed_externally`'s three outcomes → Tasks 1 and 5. Client acts on `FileChanged` by `rel` → Task 2. Cap bounds text → Task 8. Persistence → Task 6. The seven readers of `b.text`: `reconcile` and `open_for_edit` and `do_save` and the conflict diff → Task 5, `wsconn` → Task 7, `wsstate` in and out → Task 6. Read failures → the Global Constraints plus Tasks 5 and 7. Navigation keys → Task 9.

Two things the spec raises that no task implements, both deliberate and named in its own "Out of scope": edit-by-default, and `pdf` on `NO_TEXT_EDIT_EXT`.

One gap found and closed while writing: the spec argues the preview fix falls out of stubs, but images never get a stub (hashing their bytes as lossy UTF-8 is meaningless), so an image preview would not have been covered. Task 1 feeds the watcher from `open_file_rels` instead of relying on stubs, which covers images, and Task 3 handles their cache key.

**Type consistency.** `Content::Clean`/`Content::Edited(String)`, `Buffer::dirty()`, `Buffer::edited_text() -> Option<&str>`, `Buffer::set_text(String)`, `Workspace::open_file_rels() -> Vec<String>`, `refreshFile(rel)`, `image_fragment(project, rel, mtime_secs)` — each defined once and used under the same name and signature everywhere after.
