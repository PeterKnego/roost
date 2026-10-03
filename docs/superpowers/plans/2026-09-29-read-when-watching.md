# Read When Watching Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A notice for the terminal you are looking at is read: on returning to the page, on clicking into the terminal, and on arrival (with no desktop banner), behind a new `read_when_watching` setting.

**Architecture:** One new config key, following `follow_tree`'s path through `config.rs` and the settings dialog, but read live from each `State` snapshot rather than embedded at page load. One client function, `watchedSession()`, decides "watching" from the DOM (visibility, window focus, keyboard focus inside a rendered termhost). Four call sites use it. No server behaviour changes: `MarkNoticeRead` already exists, and `wsconn.rs` already rebroadcasts `Notices` to every client of the project after it.

**Tech Stack:** Rust (config cascade, `toml`), plain JS (`static/app.js`, `static/dialog.js`), Deno + Chromium over CDP for browser tests (`tests/browser/harness.mjs`).

**Spec:** `docs/superpowers/specs/2026-09-29-read-when-watching-design.md` (issue #129)

## Global Constraints

- Key name `read_when_watching`, bool, default **true**, writable in `project` and `global` (in `PROJECT_KEYS`, not `GLOBAL_ONLY_KEYS`).
- Dialog row `reload: false` — the client follows it live via `followSettings()`; no `data-` attribute in `render.rs`.
- Watching = `document.visibilityState === "visible"` **and** `document.hasFocus()` **and** `document.activeElement` inside that session's `.termhost` **and** that host `getClientRects().length > 0`. Never `lastFocusedSession`.
- `focusSession`'s clearing is untouched and unconditional (explicit gestures clear regardless of the setting).
- A watched arrival is still stored and listed; only its unread state and its OS banner are suppressed.
- Tests: `cargo test --lib -- --test-threads=1` (a bare `cargo test` hangs on this host); never `--release`. Browser tests: `deno run -A tests/browser/<file>.mjs`.
- Build from this checkout only (shared `target-dir`; see CLAUDE.md "Build from one checkout").
- This work ships as **one PR** for #129 with the plan and the code together.

## Review Focus

1. **Focus in the editor, terminal active in another pane:** that terminal's notices must stay unread on return. Covered by Task 2, test B.
2. **Page visible but another OS window focused:** a notice arriving must stay unread and still banner. Probed 2026-09-29 in this harness's Chromium: a page put behind another *tab* reports `hidden` **and** `hasFocus: false` (events `blur`, `vis:hidden`, then `vis:visible`, `focus` on return, with `activeElement` preserved throughout). A tab switch therefore cannot separate the `hasFocus()` guard from the visibility guard. Task 2, test F tries a second **window** (`Target.createTarget` with `newWindow: true`) to get visible-but-unfocused. If that doesn't produce `visible && !hasFocus()`, F says so in its output, and revert check 4 is recorded as failing nothing: a known coverage gap, not a pass.
3. **Setting turned off from the dialog while the page is open:** the change must take effect without a reload. Covered by Task 2, test E, which flips it via `SetSetting` mid-run.
4. **Terminal tab no longer active but its textarea still holds focus** (tab switched by another client). `getClientRects()` must exclude the pooled host. Covered by Task 2, test D's second half.
5. **A notice from a different session while you watch one:** it must stay unread and banner. Covered by Task 2, test C, which raises in the watched terminal and then asserts on a second terminal.

---

### Task 1: The `read_when_watching` setting

**Files:**
- Modify: `src/config.rs` — `RawConfig` (~line 9), `Settings` (~line 25), `impl Default for Settings` (~line 84), `PROJECT_KEYS` (line 99), `validate` bool arm (line 155), `load` (~line 199), `settings_view` rows (~line 664), tests module
- Modify: `static/dialog.js:518-523` (`LABELS`)
- Modify: `tests/browser/settings.mjs:113` (row-order assertion)
- Modify: `docs/deploy.md` (new `### Read when watching` section after `### Autosave`, and the dialog paragraph at ~line 627 that lists writable keys)

**Interfaces:**
- Produces: `Settings::read_when_watching: bool`; a `SettingRow` with `key == "read_when_watching"`, `kind == "bool"`, `effective: SettingValue::Bool(_)`, `writable == ["project","global"]`, `reload == false`. Task 2 reads it client-side as `state.settings.keys.find(r => r.key === "read_when_watching").effective`.

- [ ] **Step 1: Write the failing tests** in `src/config.rs`'s `mod tests`, next to `autosave_defaults_on_and_either_layer_can_turn_it_off`:

```rust
    // Same shape as autosave's test and for the same reason: asserting only
    // the default passes with the cascade never reading the key at all.
    #[test]
    fn read_when_watching_defaults_on_and_either_layer_can_turn_it_off() {
        let d = tempfile::tempdir().unwrap();
        let g = d.path().join("global.toml");
        let p = d.path().join("project.toml");
        fs::write(&g, "hide = [\"dist\"]").unwrap();
        assert!(load(&[&g]).read_when_watching, "on unless something says otherwise");

        fs::write(&p, "read_when_watching = false").unwrap();
        let s = load(&[&g, &p]);
        assert!(!s.read_when_watching, "a project can turn it off for itself");
        assert_eq!(s.hide, vec!["dist"], "and the global key still survives");

        fs::write(&g, "read_when_watching = false").unwrap();
        assert!(!load(&[&g]).read_when_watching);
        fs::write(&p, "read_when_watching = true").unwrap();
        assert!(load(&[&g, &p]).read_when_watching, "a project can turn it back on");
        assert!(load(&[&g, &p]).warning.is_none());
    }
```

In `values_must_match_the_key_and_a_theme_must_exist`, after the `autosave` pair, add:

```rust
        let e = validate(Scope::Project, "read_when_watching", Some(&V::Str("yes".into()))).unwrap_err();
        assert!(e.contains("read_when_watching") && e.contains("true or false"), "{e}");
        assert!(validate(Scope::Project, "read_when_watching", Some(&V::Bool(false))).is_ok(),
            "a project may set it: it grants nothing and raises no ceiling");
```

In `the_settings_view_reports_effective_project_global_and_default_per_key`, after the `follow_tree` block, add:

```rust
        // Project-scoped for the spec's reason: a checkout setting it decides
        // only whether a notice from a terminal you are typing in is shown
        // unread. Live, not embedded — app.js re-reads it from every snapshot.
        let r = row("read_when_watching");
        assert_eq!(r.writable, vec!["project", "global"], "read_when_watching is not global-only");
        assert_eq!(r.default, V::Bool(true));
        assert!(!r.reload, "followed live from State, not embedded at page load");
```

and change the order assertion to:

```rust
        assert_eq!(keys, ["theme", "hide", "show_hidden", "autosave", "follow_tree", "read_when_watching", "share_selection", "worktree_prompt", "relaunch", "allowed_origins", "max_upload_bytes", "ide", "roots"]);
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --lib config -- --test-threads=1`
Expected: compile error `no field read_when_watching on type Settings` (E0609).

- [ ] **Step 3: Plumb the field only** (`RawConfig`, `Settings`, `Default`, `load`, below), then re-run.
Expected: the cascade test passes; `values_must_match…` fails with `read_when_watching is not a setting`, and the settings-view test panics on the missing row. That is the red for the next two pieces. Only now add the `PROJECT_KEYS` entry, the `validate` arm and the `push` row.

The code for all of it:

`RawConfig`: add `read_when_watching: Option<bool>,` after `follow_tree`.

`Settings`: after `follow_tree`, add

```rust
    /// Whether a notice for the terminal you are looking at is read on sight
    /// (on return to the page, on clicking in, and on arrival, which then
    /// raises no OS banner). Project-scoped: a checkout setting it only
    /// decides what you see as unread, which grants nothing and raises no
    /// ceiling — `autosave`'s argument. On by default because off is the
    /// behaviour #129 reported as a bug.
    pub read_when_watching: bool,
```

`Default`: `read_when_watching: true,`.

`PROJECT_KEYS`: `&["theme", "hide", "show_hidden", "autosave", "follow_tree", "read_when_watching"]`.

`validate`: add `| "read_when_watching"` to the bool arm's key pattern.

`load`: after the `follow_tree` block,

```rust
                if let Some(v) = raw.read_when_watching {
                    s.read_when_watching = v;
                }
```

`settings_view`: after the `follow_tree` push,

```rust
    push("read_when_watching", "bool", V::Bool(s.read_when_watching), V::Bool(true), false,
        "Mark a terminal's notices read while you are typing in it, with no desktop banner for them.");
```

`static/dialog.js` `LABELS`: add `read_when_watching: "Read notices you are watching",` after `follow_tree`.

`tests/browser/settings.mjs:113`: insert `read_when_watching` after `follow_tree` in the expected string.

- [ ] **Step 4: Run and pass**

Run: `cargo test --lib config -- --test-threads=1`
Expected: all `config::tests` pass. Then run `grep -rn "Settings {" src/ | grep -v "pub struct"` and confirm that no other struct literal needs the field; the compiler already enforces this.

- [ ] **Step 5: Docs.** In `docs/deploy.md`, add this after the Autosave section:

~~~markdown
### Read when watching

A notice from the terminal you are typing in is marked read, and raises no
desktop banner; returning to the page, or clicking into a terminal, reads the
ones already waiting for it. "Typing in" means the page is visible, its window
has focus, and the keyboard focus is in that terminal — a terminal merely
visible in another pane keeps its dot. To keep every notice unread until you
click it:

```toml
read_when_watching = false
```

Globally or per project, and live: an open page follows a change on the next
snapshot, no reload. Per-project for `autosave`'s reason — a checkout setting
it changes only what you see as unread.
~~~

Also add `read_when_watching` to the list of keys the dialog writes, in the paragraph at ~line 627 ("It writes the display keys only — …").

- [ ] **Step 6: Commit**

```bash
git add src/config.rs static/dialog.js tests/browser/settings.mjs docs/deploy.md
git commit -m "config: read_when_watching, a live per-project setting (#129)"
```

---

### Task 2: Reading what you are watching

**Files:**
- Create: `tests/browser/readwatch.mjs`
- Modify: `static/app.js`: add a global near `AUTOSAVE` (line 44); update `followSettings()` (~line 4507); change the termhost `focusin` listener (line 2850); add `watchedSession`/`readWatched` next to `markSessionNoticesRead` (~line 4719); change `onNotice` (~line 4755); add two listeners next to it
- Modify: `docs/notifications.md` ("In the browser" section, after the first paragraph)

**Interfaces:**
- Consumes: the `read_when_watching` settings row from Task 1.
- Produces: `watchedSession(): string | null` and `readWatched(): void`, both global functions in `app.js`.

- [ ] **Step 1: Write the browser test** `tests/browser/readwatch.mjs`. Model it on `tests/browser/notices.mjs`: same imports, `ok`/`fail`, `fixture`/`startRoost`/`startBrowser`, and the same `wire(page)` helpers (`ready`, `__t`, `__last`, `raise`). Copy `raise` verbatim, then change it to take a `session` argument, so that it raises in a *given* terminal:

```js
  // Prints one OSC 777 through `session`'s real shell. The terminal must
  // already exist and be attached; see `openTerm`.
  const raise = async (session, title) => {
    await until(async () => (await evalIn(`__last(${JSON.stringify(session)})`)).trimEnd().endsWith("$"), 30, "shell prompt");
    await evalIn(`terms.get(${JSON.stringify(session)}).term.input(${JSON.stringify(`printf '\\033]777;notify;${title};b\\007'\r`)})`);
  };
```

(`__last` takes a session name here: `window.__last = (s) => { const b = terms.get(s).term.buffer.active; … }`.)

The file needs these additional helpers:

```js
  // Server truth, read from the OTHER page: after MarkNoticeRead, wsconn.rs
  // rebroadcasts Notices to every client of the project, so the observer's
  // array is what the server now says. The acting page's own array is a
  // client cache and may have been marked locally — asserting on it would
  // pass with no intent sent at all (notices.mjs, section E).
  const readState = (title) => evalIn(`(() => { const n = notices.find((x) => x.title === ${JSON.stringify(title)}); return n ? (n.read ? "read" : "unread") : "absent"; })()`);
  const focusTerm = (s) => evalIn(`(terms.get(${JSON.stringify(s)}).term.focus(), document.activeElement.closest(".termhost")?.dataset.session || null)`);
  const blurAll = () => evalIn(`(document.activeElement && document.activeElement.blur(), document.activeElement === document.body)`);
  // Banner spy. canNotify() needs a secure context (127.0.0.1 is one) and
  // permission "granted"; this stubs the permission and the service worker
  // so the test observes roost's decision to banner, not Chrome's prompt UI.
  // A departure from the spec's "Browser.grantPermissions", recorded here:
  // that is a browser-target CDP command this harness does not hold.
  const spyBanners = () => evalIn(`(() => { window.__posts = [];
    Object.defineProperty(Notification, "permission", { get: () => "granted", configurable: true });
    swReg = { active: { postMessage: (m) => window.__posts.push(m) } }; return true; })()`);
  const banners = () => evalIn(`window.__posts.length`);
```

Sections. `A` is the page under test and `O` is an observer page on the same project. Each section raises a fresh title, so the sections cannot read each other's notices.

```text
A. Return.       A: open terminal t (NewTerminal pane 0, StartTerminal), focusTerm(t) === t.
                 O.bringToFront() → A hidden (assert A visibilityState hidden).
                 A.raise(t, "ret1"); until O.readState("ret1") === "unread"; sleep 1000; still "unread"
                   (hidden page must NOT read it).
                 A.bringToFront(); until O.readState("ret1") === "read" (10 s).
B. Not focused.  A.focusTerm(t); A.blurAll() === true; O.bringToFront(); A.raise(t, "ret2");
                 A.bringToFront(); sleep 1500; O.readState("ret2") === "unread".
                 Then A.focusTerm(t) (the focusin path); until O.readState("ret2") === "read".
C. Arrival.      A fronted, A.spyBanners(), A.focusTerm(t).
                 A.raise(t, "arr1"); until O.readState("arr1") === "read"; A.banners() === 0.
                 Open a second terminal u in pane 3; A.focusTerm(t) again (u took focus).
                 A.raise(u, "arr2"); until O.readState("arr2") === "unread"; A.banners() === 1
                   — the pair, because 0 banners alone also passes with the spy wired to nothing.
                 O.bringToFront() (A now hidden); A.raise(t, "arr3"); until O.readState("arr3") === "unread";
                 A.banners() === 2 — hidden while focused-in-terminal is not watching.
D. Two panes.    A.bringToFront(); t (pane 0) and u (pane 3) both active. A.focusTerm(u) === u
                 → the focusin path reads arr2: until O.readState("arr2") === "read".
                 With u still focused, raise(t, "two1") → sleep 1500 → "unread" (visible but not focused ⇒ kept).
                 Pooled host: with u focused, have O switch pane 3 to a new terminal v
                 (O: send({t:"NewTerminal", pane:3})), so u's host goes to the pool while its textarea
                 held focus. PRECONDITION, asserted and printed: is A's activeElement still inside u's host?
                 - yes → raise(u, "pool1"), sleep 1500, "unread" (not rendered ⇒ not watched): this
                   covers the getClientRects guard.
                 - no (Chrome's focus fixup moved it to body) → print "pool case unreachable: focus
                   fixup" and skip. Do NOT call textarea.focus() on the hidden host to force it: a
                   display:none element cannot take focus, and the assertion would then pass because
                   nothing was focused, not because the guard worked (README trap: asserting nothing).
E. Setting off.  A: send({t:"SetSetting", scope:"project", key:"read_when_watching", value:false});
                 until A state row effective === false (live, no reload). A: window.__posts = [].
                 A.focusTerm(t); A.raise(t, "off1"); sleep 1500; O.readState("off1") === "unread"; A.banners() === 1.
                 O.bringToFront(); A.bringToFront(); sleep 1500; still "unread".
                 Restore: SetSetting value:null; until effective === true.
F. Unfocused but visible (best effort). A.bringToFront(); A.focusTerm(t); open a new window with
                 browser-level `Target.createTarget({url: "about:blank", newWindow: true})`. The harness's
                 page `cmd` may not reach the Target domain: use startBrowser's browser websocket if
                 harness.mjs exposes one, else skip F with a printed reason.
                 If A reports visibilityState "visible" && !hasFocus(): raise(t, "unf1"), sleep 1500,
                 "unread". Otherwise print "visible-but-unfocused not reproducible here" — not ok(true).
```

Wrap everything in `try/finally` to close the pages and stop roost. End with `Deno.exit(fail ? 1 : 0)`. Every `until` must use a label, because a timeout that returns `false` has to produce a `FAIL` line, not a silent pass. Read `tests/browser/README.md`'s four traps before writing the file.

- [ ] **Step 2: Run it and watch it fail**

Run: `deno run -A tests/browser/readwatch.mjs`
Expected: section A fails at `until O.readState("ret1") === "read"`, and C fails with `readState("arr1")` still `"unread"` and `banners() === 1`. B's first half and E pass, because they assert today's behaviour. Record which lines failed.

- [ ] **Step 3: Implement in `static/app.js`**

Near `AUTOSAVE` (line 44):

```js
// Whether a notice for the terminal you are looking at is read on sight.
// Not embedded like AUTOSAVE: followSettings() re-reads it from every
// snapshot, and "watching" needs a laid-out snapshot anyway, so there is no
// moment before the first State when the value is wanted.
let READ_WHEN_WATCHING = true;
```

In `followSettings()`, after the `autosave` pair:

```js
  const rw = row("read_when_watching");
  if (rw) READ_WHEN_WATCHING = rw.effective === true;
```

After `markSessionNoticesRead`:

```js
// The session whose notices are being read right now, or null. Keyboard
// focus, deliberately not lastFocusedSession: that one means "the terminal a
// mention is aimed at" and outlives focus moving to the editor, and a Claude
// working in one pane while you type in another is precisely the terminal
// whose notices you have NOT read. getClientRects covers both ways a host is
// off-screen — pooled behind another tab, or in a collapsed phone pane —
// reading from the DOM what the user actually sees.
function watchedSession() {
  if (!READ_WHEN_WATCHING) return null;
  if (document.visibilityState !== "visible" || !document.hasFocus()) return null;
  const host = document.activeElement && document.activeElement.closest(".termhost");
  if (!host || !host.getClientRects().length) return null;
  const s = host.dataset.session;
  return s && terms.has(s) && terms.get(s).node === host ? s : null;
}

function readWatched() {
  const s = watchedSession();
  if (s) markSessionNoticesRead(s);
}

// Coming back to the page: switching browser tabs raises visibilitychange,
// switching OS windows with the page left visible raises only focus. A tab
// switch raises both; markSessionNoticesRead sends nothing when nothing is
// unread, so the second is free.
document.addEventListener("visibilitychange", () => { if (!document.hidden) readWatched(); });
window.addEventListener("focus", readWatched);
```

In `onNotice`, replace the body with:

```js
function onNotice(n) {
  notices.push(n);
  // Read on arrival when it comes from the terminal being typed in: the
  // server is told, and the local row is marked too, so the dot does not
  // flash before the rebroadcast. No OS banner for it: a banner for the
  // terminal under your cursor is noise. Other windows still banner — they
  // received the same Notice and are not watching (spec, "What it does not do").
  if (n.project === PROJECT && n.session === watchedSession()) {
    n.read = true;
    send({ t: "MarkNoticeRead", id: n.id });
  } else if (canNotify() && Notification.permission === "granted") {
    if (swReg) swReg.active && swReg.active.postMessage({ kind: "notify", notice: n });
    // Fallback when there's no service worker: same attribution rule as
    // sw.js — project/session (server truth) in the title, payload text in
    // the body — so a hostile payload cannot forge another project's banner.
    else new Notification(`${n.project} · ${n.session}`, { body: `${n.title} — ${n.body}`, tag: `${n.project}/${n.session}` });
  }
  renderNotices();
  render();
}
```

Termhost `focusin` (line 2850):

```js
  node.addEventListener("focusin", () => { lastFocusedSession = session; readWatched(); });
```

- [ ] **Step 4: Run and pass**

Run: `deno run -A tests/browser/readwatch.mjs`
Expected: every line `ok`, and the process exits 0.

- [ ] **Step 5: Revert checks.** Do each one separately. Back up with `cp static/app.js /tmp/…`, never `git checkout` (see memory), run, read the failure, then restore:
  1. Remove the `visibilitychange` and `focus` listeners → A fails; B's second half still passes.
  2. Return `lastFocusedSession` in `watchedSession` instead of the focus check → **B** fails (the discriminator).
  3. Drop `!host.getClientRects().length` → D's pool case fails.
  4. Drop `!document.hasFocus()` → F fails if F ran. If F was skipped, this fails **nothing** (a tab switch hides the page, so the visibility guard catches it). Record that as the gap it is.
  5. Remove the `onNotice` branch → C fails on read state *and* banner count.

Record each result in the test file's header comment, in `notices.mjs`'s style: what was reverted, and what failed or what didn't.

- [ ] **Step 6: Neighbours.** Run `deno run -A tests/browser/notices.mjs` and `deno run -A tests/browser/settings.mjs`. Both must pass. `settings.mjs` covers the row order changed in Task 1. If one flakes, re-run it alone before calling it a regression (see memory: browser tests flake under contention).

- [ ] **Step 7: Docs.** In `docs/notifications.md` "In the browser", add a paragraph after the first one:

```markdown
A notice from the terminal you are typing in is read on arrival and raises no
OS banner, and coming back to the page — or clicking into a terminal — reads
the ones already waiting for it. "Typing in" is keyboard focus in a visible,
focused window, not merely a terminal on screen: one in another pane keeps its
dot. Other windows on the same project still banner, having received the same
notice without watching it. `read_when_watching = false` turns this off (see
[deploy.md](deploy.md)); clicking a tab or a notice clears it either way.
```

- [ ] **Step 8: Commit**

```bash
git add static/app.js tests/browser/readwatch.mjs docs/notifications.md
git commit -m "notices: a terminal you are looking at has nothing unread (#129)"
```

---

### Task 3: Whole-branch verification and PR

- [ ] **Step 1:** Run `cargo test --lib -- --test-threads=1` and `cargo test --test '*' -- --test-threads=1`. Both must be all green; quote the summary lines.
- [ ] **Step 2:** Check that the build came from this checkout: run the `assets_table.rs` grep from CLAUDE.md, which must print `/home/claude/projects/roost/static`.
- [ ] **Step 3:** Push `read-when-watching` and open **one** PR against `develop` that closes #129. Its body names the spec (already on develop from #130), this plan, and the revert-check results.
