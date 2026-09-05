# In-page dialogs implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the ten native browser dialogs in `static/app.js` (four `prompt`, five `confirm`, one `alert`) with themed in-page dialogs built on the native `<dialog>` element.

**Architecture:** One new file, `static/dialog.js`, exposes three promise-returning globals — `askConfirm`, `askText`, `askMenu` — over three empty `<dialog>` shells that ship in the server-rendered workspace page. `<dialog>.showModal()` supplies focus trapping, Escape, the top layer and an inert background, so none of that is implemented here. Nothing in the feature builds an HTML string; every value reaching the DOM goes through `textContent` or `createElement`.

**Tech Stack:** Rust (`src/render.rs`), plain JS with no framework (`static/dialog.js`, `static/app.js`), CSS custom properties (`static/style.css`), Deno + CDP browser tests (`tests/browser/`).

**Spec:** `docs/superpowers/specs/2026-09-05-in-page-dialogs-design.md`

## Global Constraints

- **No library.** `<dialog>` + `.showModal()` is Baseline 2022, >95% support. `static/vendor/` gains nothing.
- **No HTML strings.** Every interpolated value uses `textContent` or `createElement`. A path is attacker-influenced — roost opens cloned repos, and a repo may contain a file named `<img src=x onerror=…>`. `src/render.rs:3024`'s existing `noticepanel` test states this same rule for the notification centre.
- **No `hidden` attribute on a `<dialog>`.** A `<dialog>` without `open` is already `display: none` from the UA stylesheet. Copying the `#searchoverlay[hidden] { display: none; }` idiom produces a dialog that can never be shown.
- **`::backdrop` uses a literal colour, never `var()`.** It does not participate in inheritance. `static/style.css:819` already uses `rgba(0,0,0,.35)`; match it.
- **No animation.** `@starting-style` + `transition-behavior: allow-discrete` is the only route and it is the one part of the styling story with sharp edges. `confirm()` had none.
- **`danger: true` focuses Cancel.** A deliberate departure from native `confirm()`, where Enter accepts. Enter cancels; destroying takes a click or Tab-then-Enter.
- **Tests:** `cargo test -- --test-threads=1` (a bare `cargo test` hangs on this host). Browser tests are `deno run -A tests/browser/<file>.mjs`. Browser tests flake under contention — re-run a suspected failure alone before believing it.
- **Build from this checkout only.** The host shares one cargo `target-dir` and `build.rs` bakes absolute asset paths; a build from a second checkout silently rewrites them.

---

### Task 1: Dialog shells, stylesheet, and `askConfirm`

**Files:**
- Modify: `src/render.rs` (workspace page markup, after `#searchoverlay`; new test in `mod tests`)
- Create: `static/dialog.js`
- Modify: `static/style.css` (append)
- Create: `tests/browser/dialogs.mjs`

**Interfaces:**
- Consumes: nothing.
- Produces: global `askConfirm({ title, lines, confirm, danger, blocked }) -> Promise<boolean>`. `lines` is an array of strings, one paragraph each. `blocked` is a reason string: when non-empty the confirm button is disabled and the reason is shown. Resolves `false` on Escape, backdrop click, or Cancel.

- [ ] **Step 1: Write the failing Rust test**

In `src/render.rs`, inside `mod tests`, next to `the_workspace_page_carries_the_notification_centre`:

```rust
    #[test]
    fn the_workspace_page_ships_empty_dialog_shells() {
        let s = crate::config::Settings::default();
        let html = workspace_page("proj", "proj", &s, None, false, &[]);
        for id in ["dlg-confirm", "dlg-text", "dlg-menu"] {
            assert!(html.contains(&format!(r#"id="{id}""#)), "no {id} shell");
        }
        // Filled from JS with textContent, so they must ship empty — the same
        // rule the notification centre follows above. A shell carrying text
        // would mean a path was interpolated into HTML somewhere.
        assert!(html.contains(r#"<dialog id="dlg-menu" class="roost"><div class="dlg-items"></div></dialog>"#),
            "the menu shell must ship empty");
        // A <dialog> is display:none without `open`. Marking one `hidden`
        // (copying the #searchoverlay idiom) yields a dialog that can never
        // be shown, so the attribute must never appear on one.
        for frag in ["<dialog id=\"dlg-confirm\" class=\"roost\" hidden",
                     "<dialog id=\"dlg-text\" class=\"roost\" hidden",
                     "<dialog id=\"dlg-menu\" class=\"roost\" hidden"] {
            assert!(!html.contains(frag), "a dialog must not carry `hidden`: {frag}");
        }
        assert!(html.contains(r#"<script src="/static/dialog.js"></script>"#), "dialog.js not loaded");
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -- --test-threads=1 the_workspace_page_ships_empty_dialog_shells`
Expected: FAIL, `no dlg-confirm shell`.

- [ ] **Step 3: Add the shells and the script tag to `src/render.rs`**

In the workspace page's raw string, immediately after the `</div>` closing `#searchoverlay` (currently `src/render.rs:1657-1662`):

```html
<dialog id="dlg-confirm" class="roost">
  <h2 class="dlg-title"></h2>
  <div class="dlg-body"></div>
  <p class="dlg-blocked" hidden></p>
  <div class="dlg-buttons">
    <button type="button" class="dlg-cancel">Cancel</button>
    <button type="button" class="dlg-ok"></button>
  </div>
</dialog>
<dialog id="dlg-text" class="roost">
  <h2 class="dlg-title"></h2>
  <label class="dlg-label" for="dlg-input"></label>
  <input id="dlg-input" class="dlg-input" type="text" autocomplete="off" spellcheck="false">
  <div class="dlg-buttons">
    <button type="button" class="dlg-cancel">Cancel</button>
    <button type="button" class="dlg-ok"></button>
  </div>
</dialog>
<dialog id="dlg-menu" class="roost"><div class="dlg-items"></div></dialog>
```

The `hidden` on `.dlg-blocked` is correct — that is a `<p>`, not a `<dialog>`.

And change the script block near `src/render.rs:1670` so `dialog.js` loads before `app.js`:

```html
<script src="/static/dialog.js"></script>
<script src="/static/app.js"></script>
```

- [ ] **Step 4: Run the Rust test and watch it pass**

Run: `cargo test -- --test-threads=1 the_workspace_page_ships_empty_dialog_shells`
Expected: PASS.

- [ ] **Step 5: Create `static/dialog.js`**

```js
// Modal dialogs. Replaces the native confirm/prompt/alert the workspace asked
// every question through: those cannot be themed, cannot disable a button, and
// cannot offer a menu — which is why the file menu spent so long as a numbered
// prompt().
//
// Built on <dialog>.showModal(), so focus trapping, Escape, the top layer and
// an inert background all come from the browser and none of them are
// implemented here. Being in the top layer is also why there is no z-index to
// coordinate with #searchoverlay (40) or body.searching header (41).
//
// Nothing here builds an HTML string. Every value that reaches the DOM goes
// through textContent or createElement, because a path is attacker-influenced:
// roost opens cloned repositories, and a repository may contain a file named
// `<img src=x onerror=...>`. Rendering that as markup would be script
// injection into the document that holds the websocket that spawns shells.
// Same rule as the notification centre and the markdown sanitizer — make the
// dangerous operation unreachable rather than remember to escape at each site.

// One dialog at a time. A second call resolves with the dismissal value rather
// than stacking: two modals over a live terminal is worse than a dropped stray
// event.
let openDlg = null;

// `fill` populates the shell and returns either nothing or a function to run
// once the dialog is actually laid out. Anything needing measurement or
// selection must go in that returned function — offsetWidth is 0 and
// setSelectionRange is unreliable while the dialog is still display:none.
function runDialog(el, fill, dismissed) {
  if (openDlg) return Promise.resolve(dismissed);
  const restore = document.activeElement;
  openDlg = el;
  return new Promise((resolve) => {
    let done = false;
    const finish = (v) => {
      if (done) return;          // Escape during a click handler, etc.
      done = true;
      openDlg = null;
      el.removeEventListener("cancel", onCancel);
      el.removeEventListener("click", onClick);
      el.close();
      // Explicit rather than trusting the browser's own focus restoration:
      // roost's terminals are pooled DOM nodes moved between panes with
      // appendChild, and how showModal() interacts with a focused xterm is not
      // something to assume. tests/browser/dialogs.mjs asserts this.
      try { if (restore && restore.focus) restore.focus(); } catch { /* gone */ }
      resolve(v);
    };
    // Escape arrives as `cancel`. preventDefault so the close path is the one
    // above and every exit resolves the promise exactly once.
    const onCancel = (e) => { e.preventDefault(); finish(dismissed); };
    // A click on the backdrop has the dialog element itself as its target.
    const onClick = (e) => { if (e.target === el) finish(dismissed); };
    el.addEventListener("cancel", onCancel);
    el.addEventListener("click", onClick);
    const ready = fill(finish);
    el.showModal();
    if (ready) ready();
  });
}

function askConfirm({ title, lines = [], confirm = "OK", danger = false, blocked = "" }) {
  const el = document.getElementById("dlg-confirm");
  return runDialog(el, (finish) => {
    el.querySelector(".dlg-title").textContent = title;
    const body = el.querySelector(".dlg-body");
    body.replaceChildren();
    for (const line of lines) {
      const p = document.createElement("p");
      p.textContent = line;
      body.appendChild(p);
    }
    const why = el.querySelector(".dlg-blocked");
    why.textContent = blocked;
    why.hidden = !blocked;
    const okBtn = el.querySelector(".dlg-ok");
    const cancelBtn = el.querySelector(".dlg-cancel");
    okBtn.textContent = confirm;
    okBtn.disabled = !!blocked;
    okBtn.classList.toggle("danger", danger);
    okBtn.onclick = () => finish(true);
    cancelBtn.onclick = () => finish(false);
    // Destructive dialogs focus Cancel, so Enter cancels. Native confirm()
    // accepts on Enter; this deliberately does not, because the burden of
    // proof is on destroying, not on keeping.
    return () => (danger || blocked ? cancelBtn : okBtn).focus();
  }, false);
}
```

- [ ] **Step 6: Append the stylesheet to `static/style.css`**

```css
/* Modal dialogs. <dialog> supplies focus trapping, Escape, the top layer and
   an inert background; these rules only override the UA stylesheet's border,
   padding and `background: canvas`. No `hidden` attribute appears on a
   <dialog> anywhere — one without `open` is already display:none, and copying
   #searchoverlay[hidden] here would produce a dialog that can never show. */
dialog.roost { background: var(--bg2); color: var(--fg);
               border: 1px solid var(--border); border-radius: 8px;
               padding: 0; width: min(420px, 92vw); font: inherit; }
/* A literal, not var(): ::backdrop does not participate in inheritance, so a
   custom property would not resolve here. Same value as #searchoverlay. */
dialog.roost::backdrop { background: rgba(0,0,0,.35); }
.dlg-title { font-size: 13px; font-weight: 600; padding: 10px 14px;
             border-bottom: 1px solid var(--border); }
.dlg-body { padding: 12px 14px; display: flex; flex-direction: column; gap: 6px; }
.dlg-body p { font-size: 12px; }
.dlg-blocked { padding: 0 14px 12px; font-size: 12px; color: var(--warn); }
.dlg-label { display: block; padding: 12px 14px 4px; font-size: 12px; color: var(--muted); }
.dlg-input { width: calc(100% - 28px); margin: 0 14px; padding: 5px 7px;
             background: var(--bg); color: var(--fg); font: inherit;
             border: 1px solid var(--border); border-radius: 4px; }
.dlg-buttons { display: flex; justify-content: flex-end; gap: 8px; padding: 12px 14px; }
.dlg-buttons button { padding: 5px 12px; border-radius: 4px; font: inherit;
                      background: var(--bg3); color: var(--fg);
                      border: 1px solid var(--border); cursor: pointer; }
.dlg-buttons button:disabled { opacity: .5; cursor: default; }
.dlg-buttons .danger:not(:disabled) { border-color: var(--del-fg); color: var(--del-fg); }
.dlg-buttons button:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
```

Every token used (`--bg2`, `--fg`, `--border`, `--bg3`, `--del-fg`, `--warn`, `--muted`, `--bg`, `--accent`) is defined in all five files in `static/themes/`; `--row-on` used in Task 3 is derived in `style.css`'s own `:root`.

- [ ] **Step 7: Write the browser test `tests/browser/dialogs.mjs`**

```js
//! The dialog primitive: askConfirm's four exits, and focus restoration.
//!
//! Traps this file is written against (see README):
//!   - Every "no intent was sent" assertion records intents FIRST and proves
//!     the recorder works by observing a real one, so a cancel assertion
//!     cannot pass because the hook was never wired.
//!   - Focus restoration is asserted against a specific element that was
//!     focused beforehand, not against "something has focus" — document.body
//!     always satisfies the weaker form.
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const port = await freePort();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port });
const browser = await startBrowser(profileDir(repoRoot));
let page;
try {
  page = await openPage(browser.port, `http://127.0.0.1:${port}/proj`);
  const evalIn = page.evalIn;
  await until(async () => await evalIn("typeof askConfirm === 'function'"), 10, "dialog.js loaded");

  // Record every intent, and prove the recorder works before relying on its
  // silence. Without this proof a "cancel sends nothing" assertion passes
  // just as well when send() was never wrapped at all.
  await evalIn(`window.__sent = []; const __s = window.send;
                window.send = (m) => { window.__sent.push(m); return __s(m); }; 0`);
  await evalIn(`send({ t: "ActivateTab", pane: 0, idx: 0 }); 0`);
  ok((await evalIn("window.__sent.length")) === 1, "the intent recorder itself records");
  await evalIn("window.__sent = []; 0");

  // A: Cancel resolves false.
  await evalIn(`window.__r = null;
     askConfirm({ title: "T", lines: ["L"], confirm: "Go", danger: true })
       .then((v) => { window.__r = v; }); 0`);
  ok(await evalIn(`document.getElementById("dlg-confirm").open`), "the dialog opened");
  ok(await evalIn(`document.activeElement === document.querySelector("#dlg-confirm .dlg-cancel")`),
     "danger focuses Cancel, so Enter cancels rather than destroys");
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click(); 0`);
  ok(await until(async () => (await evalIn("window.__r")) === false, 5, "cancel"),
     "Cancel resolves false");
  ok((await evalIn("window.__sent.length")) === 0, "and sends no intent");

  // B: Escape resolves false.
  await evalIn(`window.__r = null; askConfirm({ title: "T" }).then((v) => { window.__r = v; }); 0`);
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  ok(await until(async () => (await evalIn("window.__r")) === false, 5, "escape"),
     "Escape resolves false");
  ok(!(await evalIn(`document.getElementById("dlg-confirm").open`)), "and closes the dialog");

  // C: the confirm button resolves true.
  await evalIn(`window.__r = null; askConfirm({ title: "T", confirm: "Go" }).then((v) => { window.__r = v; }); 0`);
  await evalIn(`document.querySelector("#dlg-confirm .dlg-ok").click(); 0`);
  ok(await until(async () => (await evalIn("window.__r")) === true, 5, "confirm"),
     "Confirm resolves true");

  // D: `blocked` disables the confirm button and shows the reason.
  await evalIn(`window.__r = null;
     askConfirm({ title: "T", confirm: "Go", blocked: "Save first." }).then((v) => { window.__r = v; }); 0`);
  ok(await evalIn(`document.querySelector("#dlg-confirm .dlg-ok").disabled`),
     "blocked disables the confirm button");
  ok((await evalIn(`document.querySelector("#dlg-confirm .dlg-blocked").textContent`)) === "Save first.",
     "and states the reason");
  ok(!(await evalIn(`document.querySelector("#dlg-confirm .dlg-blocked").hidden`)),
     "with the reason actually visible");
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click(); 0`);
  await until(async () => (await evalIn("window.__r")) === false, 5, "blocked cancel");

  // E: focus returns to the exact element that had it.
  await evalIn(`document.getElementById("closeproj").focus();
     window.__r = null; askConfirm({ title: "T" }).then((v) => { window.__r = v; }); 0`);
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click(); 0`);
  await until(async () => (await evalIn("window.__r")) === false, 5, "focus case");
  ok(await evalIn(`document.activeElement === document.getElementById("closeproj")`),
     "focus returns to the element that had it, not merely to something");

  // F: a value that would be markup if it were ever interpolated.
  await evalIn(`window.__r = null;
     askConfirm({ title: "T", lines: ['<img src=x onerror="window.__pwned=1">'] })
       .then((v) => { window.__r = v; }); 0`);
  ok((await evalIn(`document.querySelector("#dlg-confirm .dlg-body").querySelectorAll("img").length`)) === 0,
     "a path that looks like markup produces no element");
  ok((await evalIn("window.__pwned === undefined")), "and runs nothing");
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click(); 0`);
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
```

- [ ] **Step 8: Run the browser test**

Run: `deno run -A tests/browser/dialogs.mjs`
Expected: PASS, 13 `ok` lines.

- [ ] **Step 9: Revert-check the focus restoration**

Comment out the `restore.focus()` line in `runDialog`, re-run `deno run -A tests/browser/dialogs.mjs`, and confirm assertion E — and only E — fails. Restore the line, re-run, confirm PASS. Record the observed failure in a comment above the line.

- [ ] **Step 10: Commit**

```bash
git add src/render.rs static/dialog.js static/style.css tests/browser/dialogs.mjs
git commit -m "feat: dialog shells and askConfirm on the native <dialog> element"
```

---

### Task 2: `askText`

**Files:**
- Modify: `static/dialog.js`
- Modify: `tests/browser/dialogs.mjs`

**Interfaces:**
- Consumes: `runDialog` from Task 1.
- Produces: global `askText({ title, label, value, confirm }) -> Promise<string|null>`. Resolves the trimmed input, or `null` if empty or dismissed.

- [ ] **Step 1: Write the failing assertions**

Append to `tests/browser/dialogs.mjs`, before the `finally`:

```js
  // G: askText preselects the basename, so typing replaces the name and not
  // the directory. prompt() could not do this, and a rename that silently
  // ate the directory would be a data-loss bug, not a UX one.
  await evalIn(`window.__r = null;
     askText({ title: "Rename", value: "src/main.rs", confirm: "Rename" })
       .then((v) => { window.__r = v; }); 0`);
  ok((await evalIn(`document.getElementById("dlg-input").value`)) === "src/main.rs",
     "askText prefills the whole path");
  ok((await evalIn(`document.getElementById("dlg-input").selectionStart`)) === 4 &&
     (await evalIn(`document.getElementById("dlg-input").selectionEnd`)) === 11,
     "and selects only the basename");
  ok(await evalIn(`document.activeElement === document.getElementById("dlg-input")`),
     "with focus in the field");
  await evalIn(`document.getElementById("dlg-input").value = "src/other.rs";
     document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  ok(await until(async () => (await evalIn("window.__r")) === "src/other.rs", 5, "text ok"),
     "confirming resolves the typed value");

  // H: an empty field resolves null, never the empty string — every caller
  // guards with `if (name)`, and "" would slip through a truthiness check as
  // a create/rename of a path with no name.
  await evalIn(`window.__r = "unset"; askText({ title: "New file", value: "x" }).then((v) => { window.__r = v; }); 0`);
  await evalIn(`document.getElementById("dlg-input").value = "   ";
     document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  ok(await until(async () => (await evalIn("window.__r")) === null, 5, "empty"),
     "a blank field resolves null, not an empty string");

  // I: Escape resolves null.
  await evalIn(`window.__r = "unset"; askText({ title: "T", value: "y" }).then((v) => { window.__r = v; }); 0`);
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  ok(await until(async () => (await evalIn("window.__r")) === null, 5, "text escape"),
     "Escape resolves null");
```

- [ ] **Step 2: Run and watch it fail**

Run: `deno run -A tests/browser/dialogs.mjs`
Expected: FAIL — `askText is not defined`.

- [ ] **Step 3: Implement `askText` in `static/dialog.js`**

```js
function askText({ title, label = "", value = "", confirm = "OK" }) {
  const el = document.getElementById("dlg-text");
  return runDialog(el, (finish) => {
    el.querySelector(".dlg-title").textContent = title;
    const lab = el.querySelector(".dlg-label");
    lab.textContent = label;
    lab.hidden = !label;
    const input = el.querySelector(".dlg-input");
    input.value = value;
    // Empty resolves null, never "": every caller guards with `if (name)`,
    // and an empty string would pass a truthiness check as a create or
    // rename of a path with no name.
    const take = () => finish(input.value.trim() || null);
    const okBtn = el.querySelector(".dlg-ok");
    okBtn.textContent = confirm;
    okBtn.disabled = false;
    okBtn.classList.remove("danger");
    okBtn.onclick = take;
    el.querySelector(".dlg-cancel").onclick = () => finish(null);
    // Enter confirms. A <form method="dialog"> would do this natively but
    // would also make the shell submit-shaped, and a stray Enter elsewhere in
    // the page then has a form to submit.
    input.onkeydown = (e) => { if (e.key === "Enter") { e.preventDefault(); take(); } };
    return () => {
      input.focus();
      // Select the basename only, so typing replaces the name and leaves the
      // directory. lastIndexOf returns -1 for a bare name, and -1 + 1 === 0
      // selects the whole thing, which is what a new file wants.
      input.setSelectionRange(value.lastIndexOf("/") + 1, value.length);
    };
  }, null);
}
```

- [ ] **Step 4: Run and watch it pass**

Run: `deno run -A tests/browser/dialogs.mjs`
Expected: PASS, 20 `ok` lines.

- [ ] **Step 5: Revert-check the basename selection**

Change `input.setSelectionRange(value.lastIndexOf("/") + 1, value.length)` to `input.select()`, re-run, and confirm the "selects only the basename" assertion fails with `selectionStart` 0. Restore and re-run.

- [ ] **Step 6: Commit**

```bash
git add static/dialog.js tests/browser/dialogs.mjs
git commit -m "feat: askText, with the basename preselected"
```

---

### Task 3: `askMenu`

**Files:**
- Modify: `static/dialog.js`
- Modify: `static/style.css` (append)
- Modify: `tests/browser/dialogs.mjs`

**Interfaces:**
- Consumes: `runDialog` from Task 1.
- Produces: global `askMenu({ items, x, y }) -> Promise<string|null>`, where `items` is `[{ id, label }]`. Resolves the chosen `id`, or `null` if dismissed.

- [ ] **Step 1: Write the failing assertions**

Append to `tests/browser/dialogs.mjs`, before the `finally`:

```js
  // J: the menu renders one button per item and resolves the chosen id.
  await evalIn(`window.__r = "unset";
     askMenu({ x: 40, y: 60, items: [{ id: "a", label: "Alpha" }, { id: "b", label: "Beta" }] })
       .then((v) => { window.__r = v; }); 0`);
  ok((await evalIn(`document.querySelectorAll("#dlg-menu .dlg-item").length`)) === 2,
     "one button per item");
  ok(await evalIn(`document.activeElement === document.querySelector("#dlg-menu .dlg-item")`),
     "the first item takes focus");
  // ArrowDown moves focus. Asserted on the focused element, not on a class:
  // a highlight class with no focus move leaves Enter activating the wrong row.
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyDown", key: "ArrowDown", code: "ArrowDown", windowsVirtualKeyCode: 40 });
  ok(await evalIn(`document.activeElement === document.querySelectorAll("#dlg-menu .dlg-item")[1]`),
     "ArrowDown moves focus to the next item");
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyDown", key: "ArrowUp", code: "ArrowUp", windowsVirtualKeyCode: 38 });
  ok(await evalIn(`document.activeElement === document.querySelectorAll("#dlg-menu .dlg-item")[0]`),
     "and ArrowUp back again");
  await evalIn(`document.querySelectorAll("#dlg-menu .dlg-item")[1].click(); 0`);
  ok(await until(async () => (await evalIn("window.__r")) === "b", 5, "menu pick"),
     "clicking an item resolves its id");

  // K: a label that would be markup if it were ever interpolated.
  await evalIn(`window.__r = "unset";
     askMenu({ x: 10, y: 10, items: [{ id: "x", label: '<b>bold</b>' }] })
       .then((v) => { window.__r = v; }); 0`);
  ok((await evalIn(`document.querySelectorAll("#dlg-menu .dlg-item b").length`)) === 0,
     "a menu label that looks like markup produces no element");
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  ok(await until(async () => (await evalIn("window.__r")) === null, 5, "menu escape"),
     "Escape resolves null");

  // L: a menu opened at the far edge is clamped back on screen. Without the
  // clamp the last item is unreachable, which is not visible in any test that
  // only asserts the menu opened.
  await evalIn(`window.__r = "unset";
     askMenu({ x: innerWidth - 4, y: innerHeight - 4, items: [{ id: "a", label: "Alpha" }] })
       .then((v) => { window.__r = v; }); 0`);
  ok(await evalIn(`(() => { const r = document.getElementById("dlg-menu").getBoundingClientRect();
     return r.right <= innerWidth && r.bottom <= innerHeight; })()`),
     "a menu at the viewport edge is clamped back on screen");
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await until(async () => (await evalIn("window.__r")) === null, 5, "menu edge escape");
```

- [ ] **Step 2: Run and watch it fail**

Run: `deno run -A tests/browser/dialogs.mjs`
Expected: FAIL — `askMenu is not defined`.

- [ ] **Step 3: Implement `askMenu` in `static/dialog.js`**

```js
function askMenu({ items, x, y }) {
  const el = document.getElementById("dlg-menu");
  return runDialog(el, (finish) => {
    const list = el.querySelector(".dlg-items");
    list.replaceChildren();
    for (const it of items) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "dlg-item";
      b.textContent = it.label;
      b.onclick = () => finish(it.id);
      list.appendChild(b);
    }
    // Focus IS the selection here — no separate highlight class. A class that
    // moves without focus leaves Enter activating whatever the browser still
    // considers focused, which is the wrong row.
    el.onkeydown = (e) => {
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      e.preventDefault();
      const btns = [...list.querySelectorAll(".dlg-item")];
      const i = btns.indexOf(document.activeElement);
      const n = (e.key === "ArrowDown" ? i + 1 : i - 1 + btns.length) % btns.length;
      btns[n].focus();
    };
    el.style.left = `${x}px`;
    el.style.top = `${y}px`;
    return () => {
      // Clamped after showModal: getBoundingClientRect reads 0 while the
      // dialog is still display:none, so measuring in fill() would clamp
      // against a zero-sized box and never move anything.
      const r = el.getBoundingClientRect();
      if (r.right > innerWidth - 8) el.style.left = `${Math.max(8, innerWidth - r.width - 8)}px`;
      if (r.bottom > innerHeight - 8) el.style.top = `${Math.max(8, innerHeight - r.height - 8)}px`;
      const first = list.querySelector(".dlg-item");
      if (first) first.focus();
    };
  }, null);
}
```

- [ ] **Step 4: Append the menu styles to `static/style.css`**

```css
/* Anchored at the pointer by askMenu; margin:0 defeats the UA stylesheet's
   auto-centring, and the top layer means there is no z-index to coordinate. */
dialog#dlg-menu { position: fixed; margin: 0; width: max-content;
                  min-width: 160px; padding: 4px; }
.dlg-items { display: flex; flex-direction: column; }
.dlg-item { text-align: left; padding: 5px 12px; font: inherit; cursor: pointer;
            background: none; color: var(--fg); border: 0; border-radius: 4px; }
/* --row-on is the token added for a modal's keyboard selection: mixed against
   the surface it is drawn on, so it lifts in a dark theme and darkens in a
   light one. See its rationale at the top of this file. */
.dlg-item:hover, .dlg-item:focus { background: var(--row-on); outline: none; }
```

- [ ] **Step 5: Run and watch it pass**

Run: `deno run -A tests/browser/dialogs.mjs`
Expected: PASS, 28 `ok` lines.

- [ ] **Step 6: Revert-check the clamp**

Delete the two clamping lines from the returned function, re-run, and confirm assertion L — and only L — fails. Restore, re-run, confirm PASS. Record the observed failure in a comment.

- [ ] **Step 7: Commit**

```bash
git add static/dialog.js static/style.css tests/browser/dialogs.mjs
git commit -m "feat: askMenu, a pointer-anchored context menu"
```

---

### Task 4: Convert the file menu

**Files:**
- Modify: `static/app.js:1276-1299` (`fileMenu`)
- Modify: `tests/browser/mdlinks.mjs:215-245`

**Interfaces:**
- Consumes: `askMenu`, `askText`, `askConfirm`.
- Produces: `fileMenu(e, rel)` is now `async`. Its callers at `static/app.js:1262` and `1272` are unchanged — both call `e.preventDefault()` synchronously before it.

- [ ] **Step 1: Rewrite `tests/browser/mdlinks.mjs`'s vacuous assertions**

`mdlinks.mjs:220-245` stubs `window.prompt` and asserts `window.__prompts.length === 0` — that clicking a markdown link does not pop the file menu. Once nothing calls `window.prompt`, that count is zero forever: **the test passes while asserting nothing.** Replace the stub block and both assertions with checks on the real menu.

Delete the `window.__realPrompt` / `window.__prompts` stub at 220-222 and the restore at 245. Replace the two count assertions with:

```js
  // The file menu is now #dlg-menu, not prompt(). Asserting on the dialog's
  // `open` property rather than on a stubbed prompt matters: after the
  // dialogs change, `window.prompt` is called by nothing, so the old
  // `__prompts.length === 0` assertion was true no matter what this click
  // did — green, and testing nothing.
  const menuOpen = async () => await evalIn(`document.getElementById("dlg-menu").open`);
  ok(!(await menuOpen()), "clicking a markdown link does not open the file menu");
```

and, for the tree case at the former line 238:

```js
  ok(await menuOpen(), "but right-clicking the tree row does open it");
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
```

The positive assertion is what makes the negative one mean something: a selector that matched nothing would fail the second check rather than silently satisfy the first.

- [ ] **Step 2: Run and watch it fail**

Run: `deno run -A tests/browser/mdlinks.mjs`
Expected: FAIL on "but right-clicking the tree row does open it" — `fileMenu` still calls `prompt`, so `#dlg-menu` never opens.

- [ ] **Step 3: Rewrite `fileMenu` in `static/app.js`**

Replace `fileMenu` and the comment above it (currently lines 1276-1299):

```js
// A real menu now, rather than a numbered prompt(). The prompt was never a
// menu by choice — it was the only way prompt() could offer four options —
// and it cost a second dialog for every action.
async function fileMenu(e, rel) {
  const dir = rel.includes("/") ? rel.slice(0, rel.lastIndexOf("/")) : "";
  const items = [
    { id: "new", label: "New file…" },
    { id: "newdir", label: "New folder…" },
  ];
  // Rename and Delete need a target. The prompt version offered them at the
  // project root and then silently did nothing, because its guards were
  // `choice === "3" && rel`. A menu can simply not offer them.
  if (rel) items.push({ id: "rename", label: "Rename…" }, { id: "delete", label: "Delete" });
  const choice = await askMenu({ items, x: e.clientX, y: e.clientY });
  if (choice === "new") {
    const name = await askText({ title: "New file", label: "Path",
      value: dir ? `${dir}/untitled.txt` : "untitled.txt", confirm: "Create" });
    if (name) send({ t: "CreateFile", rel: name });
  } else if (choice === "newdir") {
    const name = await askText({ title: "New folder", label: "Path",
      value: dir ? `${dir}/newdir` : "newdir", confirm: "Create" });
    if (name) send({ t: "CreateDir", rel: name });
  } else if (choice === "rename") {
    const to = await askText({ title: "Rename", label: "New path", value: rel, confirm: "Rename" });
    if (to && to !== rel) send({ t: "RenamePath", from: rel, to });
  } else if (choice === "delete") {
    // the server refuses non-empty directories regardless of what we ask
    const yes = await askConfirm({ title: "Delete", lines: [`Delete ${rel}?`],
      confirm: "Delete", danger: true });
    if (yes) send({ t: "DeleteFile", rel });
  }
}
```

- [ ] **Step 4: Run and watch it pass**

Run: `deno run -A tests/browser/mdlinks.mjs`
Expected: PASS.

- [ ] **Step 5: Verify the flows by hand in a real browser**

Right-click a file: create, rename and delete each round-trip. Right-click the empty tree area: the menu offers only New file and New folder. No Rust test reaches `static/app.js`, and no browser test in this task covers create/rename/delete end to end.

- [ ] **Step 6: Commit**

```bash
git add static/app.js tests/browser/mdlinks.mjs
git commit -m "feat: the file menu is a real context menu"
```

---

### Task 5: Convert `closeTab`, with the stale-index fix

**Files:**
- Modify: `static/app.js:2318-2332` (`closeTab` and its comment)
- Create: `tests/browser/closetab.mjs`

**Interfaces:**
- Consumes: `askConfirm`.
- Produces: `closeTab(pi, ti, t, detach)` is now `async`. Its only caller, `static/app.js:722`, calls `e.stopPropagation()` synchronously before it and is unchanged.

**Why this task is separate:** it is the only one where the change can destroy the wrong thing. `confirm()` blocked the event loop, so no `State` event could arrive between the question and the send. An in-page dialog does not, and `CloseTab { pane, idx }` (`src/proto.rs:61`) addresses its target by position.

- [ ] **Step 1: Write the failing test `tests/browser/closetab.mjs`**

```js
//! Closing a dirty file tab: the confirmation must not let the tab strip move
//! underneath it.
//!
//! confirm() blocked the event loop, so nothing could arrive between the
//! question and the send. An in-page dialog does not, and CloseTab is
//! addressed by INDEX (proto.rs). So a State event that arrives while the
//! dialog is open — another client closing a tab to the left, a session
//! ending — renumbers the strip, and the index gathered before the wait now
//! names a different tab.
//!
//! Traps this file is written against (see README):
//!   - Section B proves the setup it later negates: three tabs, in a known
//!     order, asserted before the dialog opens. Without that, "the right tab
//!     survived" could pass against a strip that never had the others.
//!   - The assertion is on WHICH tab remains, not on the count. A count is
//!     equally satisfied by closing the wrong one.
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

// autosave off: the buffer has to STAY dirty for the dialog to appear at all.
// With autosave on this test would silently take the no-dialog path and prove
// nothing — the README's "buffers that have to stay dirty" trap.
const fx = await fixture({ autosave: false });
const port = await freePort();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port });
const browser = await startBrowser(profileDir(repoRoot));
let page;
try {
  await Deno.writeTextFile(`${fx.roots}/proj/a.txt`, "a\n");
  await Deno.writeTextFile(`${fx.roots}/proj/b.txt`, "b\n");
  await Deno.writeTextFile(`${fx.roots}/proj/c.txt`, "c\n");
  page = await openPage(browser.port, `http://127.0.0.1:${port}/proj`);
  const evalIn = page.evalIn;
  await until(async () => await evalIn("typeof askConfirm === 'function'"), 10, "dialog.js loaded");

  // Pane 2 is MIDDLE (proto.rs:8), where file tabs open (app.js:3482).
  const tabs = async () => await evalIn(
    `JSON.stringify(state.panes[2].tabs.filter((t) => t.k === "File").map((t) => t.rel))`);

  for (const f of ["a.txt", "b.txt", "c.txt"]) {
    await evalIn(`send({ t: "OpenTab", pane: 2,
      tab: { k: "File", rel: ${JSON.stringify(f)}, mode: "Edit" } }); 0`);
    await until(async () => (await tabs()).includes(f), 10, `${f} opened`);
  }
  // A: the setup this test later negates.
  ok((await tabs()) === '["a.txt","b.txt","c.txt"]', "three file tabs, in order");

  // Make c.txt dirty so its close asks.
  await evalIn(`send({ t: "EditBuffer", rel: "c.txt", text: "c changed\\n" }); 0`);
  await until(async () => await evalIn(`state.buffers.some((b) => b.rel === "c.txt" && b.dirty)`),
    10, "c.txt dirty");

  // B: open c.txt's close dialog, then move the strip underneath it.
  const ci = await evalIn(`state.panes[2].tabs.findIndex((t) => t.k === "File" && t.rel === "c.txt")`);
  await evalIn(`closeTab(2, ${ci}, state.panes[2].tabs[${ci}], false); 0`);
  ok(await evalIn(`document.getElementById("dlg-confirm").open`), "the dirty-close dialog opened");
  // a.txt closes while the dialog is up: every later tab shifts down one.
  await evalIn(`send({ t: "CloseTab", pane: 2, idx: state.panes[2].tabs.findIndex((t) => t.k === "File" && t.rel === "a.txt") }); 0`);
  await until(async () => !(await tabs()).includes("a.txt"), 10, "a.txt gone");
  ok((await tabs()) === '["b.txt","c.txt"]', "the strip moved while the dialog was open");

  await evalIn(`document.querySelector("#dlg-confirm .dlg-ok").click(); 0`);
  await until(async () => !(await tabs()).includes("c.txt"), 10, "c.txt closed");
  // C: WHICH tab remains. A count assertion passes just as well when the
  // wrong tab was closed.
  ok((await tabs()) === '["b.txt"]', "the tab the user clicked was closed, not the one at its old index");

  // D: cancelling closes nothing.
  await evalIn(`send({ t: "EditBuffer", rel: "b.txt", text: "b changed\\n" }); 0`);
  await until(async () => await evalIn(`state.buffers.some((x) => x.rel === "b.txt" && x.dirty)`),
    10, "b.txt dirty");
  const bi = await evalIn(`state.panes[2].tabs.findIndex((t) => t.k === "File" && t.rel === "b.txt")`);
  await evalIn(`closeTab(2, ${bi}, state.panes[2].tabs[${bi}], false); 0`);
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click(); 0`);
  await new Promise((r) => setTimeout(r, 500));
  ok((await tabs()) === '["b.txt"]', "cancelling the dialog closes nothing");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
```

- [ ] **Step 2: Run and watch it fail**

Run: `deno run -A tests/browser/closetab.mjs`
Expected: FAIL — `closeTab` still calls `confirm()`, so `#dlg-confirm` never opens.

- [ ] **Step 3: Rewrite `closeTab` in `static/app.js`**

```js
// `detach` (alt-click) keeps the old meaning: drop the tab, leave the shell
// running. A plain close ends the session, because a tab that quietly outlives
// its × is how a project accumulates shells nothing can reach — there is no
// session list, and the per-project cap is 16.
async function closeTab(pi, ti, t, detach) {
  const meta = t.k === "File" ? state.buffers.find((x) => x.rel === t.rel) : null;
  if (meta && meta.dirty) {
    const yes = await askConfirm({ title: "Unsaved changes",
      lines: [`${t.rel} has unsaved changes. Close it?`], confirm: "Close", danger: true });
    if (!yes) return;
    // The dialog did not block the event loop, so the tab strip may have been
    // rebuilt from a State event while it was open and `ti` may now address a
    // different tab. Re-resolve by rel — the idiom focusSession already uses.
    // The `< 0` branch is the point: not finding the tab is "I cannot tell",
    // never "close index ti anyway".
    const ti2 = state.panes[pi].tabs.findIndex((x) => x.k === "File" && x.rel === t.rel);
    if (ti2 < 0) { showError(`${t.rel} is no longer open`); return; }
    send({ t: "CloseTab", pane: pi, idx: ti2 });
    return;
  }
  if (t.k === "Terminal" && !detach) {
    const yes = await askConfirm({ title: "End session",
      lines: [`End session "${t.session}"?`,
              "This kills the shell and anything running in it."],
      confirm: "End session", danger: true });
    if (!yes) return;
    send({ t: "EndSession", session: t.session });
    return;
  }
  // No dialog was shown on this path, so nothing awaited and `ti` is still
  // the index the click was made against.
  send({ t: "CloseTab", pane: pi, idx: ti });
}
```

The dirty branch now returns instead of falling through to the bottom `send`. That is equivalent: `meta` is only ever set when `t.k === "File"`, so a dirty tab could never reach the `Terminal` branch below it.

- [ ] **Step 4: Run and watch it pass**

Run: `deno run -A tests/browser/closetab.mjs`
Expected: PASS, 5 `ok` lines.

- [ ] **Step 5: Revert-check the re-resolution — apply it, do not reason about it**

Replace the re-resolution with the original positional send:

```js
    send({ t: "CloseTab", pane: pi, idx: ti });
```

Run `deno run -A tests/browser/closetab.mjs`. Expected: assertion C fails, reporting `["c.txt"]` — b.txt closed instead of c.txt, because c.txt's index shifted from 2 to 1 while the dialog was open. Restore the fix, re-run, confirm PASS. Record the observed failure verbatim in a comment above the `findIndex` line.

- [ ] **Step 6: Verify the terminal path by hand**

Close a terminal tab: the dialog appears and the session ends. Alt-click a terminal tab: no dialog, the tab detaches, the shell stays running. The alt-click path is not covered by `closetab.mjs`.

- [ ] **Step 7: Commit**

```bash
git add static/app.js tests/browser/closetab.mjs
git commit -m "feat: closeTab asks in-page, and re-resolves its target after"
```

---

### Task 6: Convert Close Project

**Files:**
- Modify: `static/app.js:2395-2410` (the `closeproj` handler)
- Modify: `tests/browser/closeproject.mjs:121,172,221`

**Interfaces:**
- Consumes: `askConfirm`, including its `blocked` parameter.
- Produces: nothing new.

- [ ] **Step 1: Update `tests/browser/closeproject.mjs`**

Three lines stub `confirm`/`alert` to auto-accept. Replace each with a real click on the dialog. At line 121:

```js
  await ws.evalIn(`document.getElementById("closeproj").click(); 0`);
  await ws.evalIn(`document.querySelector("#dlg-confirm .dlg-ok").click(); 0`);
```

At 172 and 221, replace `window.confirm = () => true; window.alert = () => {};` with the same second line, placed after whatever click each already performs. Add to the file's `//!` trap list:

```
//!   - The confirm() stub is gone: after the in-page dialogs change,
//!     `window.confirm = () => true` is a no-op, and a test that kept it
//!     would be clicking a button whose dialog nothing ever answers.
```

- [ ] **Step 2: Run and watch it fail**

Run: `deno run -A tests/browser/closeproject.mjs`
Expected: FAIL — the handler still calls `confirm()`, so `#dlg-confirm .dlg-ok` does not exist to click.

- [ ] **Step 3: Rewrite the handler in `static/app.js`**

```js
const closeBtn = document.getElementById("closeproj");
if (closeBtn) closeBtn.onclick = async () => {
  const live = (state && state.live_sessions) || [];
  const dirty = ((state && state.buffers) || []).filter((b) => b.dirty).map((b) => b.rel);
  const lines = [live.length
    ? `${live.length} terminal session(s) will be ended: ${live.join(", ")}`
    : "No terminal sessions are running."];
  if (dirty.length) lines.push(`Unsaved changes in: ${dirty.join(", ")}`);
  // One dialog in both states. The alert/confirm split existed only because a
  // native dialog cannot disable its OK button; `blocked` can. The check still
  // mirrors the server's own CloseRefused rather than making its own ruling —
  // it is told before sending anything, rather than after a round trip that
  // would have changed nothing.
  const yes = await askConfirm({
    title: `Close ${PROJECT}?`,
    lines,
    confirm: "End sessions",
    danger: true,
    blocked: dirty.length ? "Save or discard them first." : "",
  });
  if (yes) send({ t: "CloseProject" });
};
```

- [ ] **Step 4: Run and watch it pass**

Run: `deno run -A tests/browser/closeproject.mjs`
Expected: PASS.

- [ ] **Step 5: Verify the blocked state by hand**

With an unsaved buffer open, click Close: the dialog lists the dirty file, the End sessions button is disabled, and the reason is visible. Save, click Close again: the button is live. No test asserts the disabled state against real dirty buffers — `dialogs.mjs` assertion D covers `blocked` in isolation only.

- [ ] **Step 6: Commit**

```bash
git add static/app.js tests/browser/closeproject.mjs
git commit -m "feat: Close Project is one dialog, disabled while buffers are dirty"
```

---

### Task 7: Convert Remove worktree

**Files:**
- Modify: `static/app.js:2460-2470` (`wtPanel.onclick`)
- Modify: `tests/browser/worktree-launch.mjs:243,263`

**Interfaces:**
- Consumes: `askConfirm`.
- Produces: nothing new.

- [ ] **Step 1: Update `tests/browser/worktree-launch.mjs`**

Line 243 stubs `confirm` before sending an `EndSession` intent directly — that send does not go through `closeTab`, so the stub was never needed for it. Drop the stub:

```js
    await page2.evalIn(`send({ t: "EndSession", session: ${JSON.stringify(s2)} })`);
```

Line 263 clicks the remove control. Replace the stub with a real click on the dialog:

```js
  await evalIn(`document.querySelector("#wtstrip .wtremove").click(); 0`);
  await evalIn(`document.querySelector("#dlg-confirm .dlg-ok").click(); 0`);
```

- [ ] **Step 2: Run and watch it fail**

Run: `deno run -A tests/browser/worktree-launch.mjs`
Expected: FAIL — the handler still calls `confirm()`, so `#dlg-confirm .dlg-ok` does not exist.

- [ ] **Step 3: Rewrite the handler branch in `static/app.js`**

```js
  wtPanel.onclick = async (e) => {
    const rm = e.target.closest(".wtremove");
    if (rm) {
      e.preventDefault();
      const key = rm.dataset.key;
      const name = rm.closest(".wtrow")?.textContent.trim().split(/\s+/)[1] || key;
      const yes = await askConfirm({ title: "Remove worktree",
        lines: [`Remove worktree ${name} and its branch?`,
                "roost re-checks that it is clean, idle and merged first."],
        confirm: "Remove", danger: true });
      if (yes) send({ t: "RemoveWorktree", key });
      return;
    }
    if (e.target.closest("a")) wtPanel.hidden = true;
  };
```

`e.preventDefault()` and `e.target.closest` both run before the first `await`, so nothing is lost to the microtask boundary.

- [ ] **Step 4: Run and watch it pass**

Run: `deno run -A tests/browser/worktree-launch.mjs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add static/app.js tests/browser/worktree-launch.mjs
git commit -m "feat: Remove worktree asks in-page"
```

---

### Task 8: Regression guard and full verification

**Files:**
- Modify: `tests/browser/dialogs.mjs`
- Modify: `tests/browser/README.md`

**Interfaces:**
- Consumes: everything above.
- Produces: nothing new.

- [ ] **Step 1: Add the guard to `tests/browser/dialogs.mjs`**

Insert immediately after the `dialog.js loaded` wait, so it is armed for the whole file:

```js
  // Native dialogs are gone from app.js and must stay gone. A grep would
  // match comments and strings; this fails only if a code path actually calls
  // one. It cannot be a Rust test — no Rust test reaches static/app.js.
  //
  // Deliberately NOT applied in termlinks.mjs: xterm's own OSC 8 fallback
  // legitimately calls confirm(), and that file asserts on its silence as
  // positive evidence that roost's linkHandler took the activation instead.
  await evalIn(`window.__native = [];
     for (const k of ["confirm", "prompt", "alert"]) {
       window[k] = (...a) => { window.__native.push([k, ...a]); return null; };
     } 0`);
```

and immediately before the `finally`:

```js
  ok((await evalIn("window.__native.length")) === 0,
     "no code path in this file reached a native browser dialog");
```

Then exercise the converted paths so the guard has something to catch:

```js
  // M: drive each converted entry point once, so the guard above is armed
  // against real call sites and not only against the primitive.
  await evalIn(`fileMenu({ preventDefault(){}, clientX: 30, clientY: 30 }, "a.txt"); 0`);
  ok(await evalIn(`document.getElementById("dlg-menu").open`), "the file menu opens in-page");
  await evalIn(`document.querySelector("#dlg-menu .dlg-item").click(); 0`);
  await until(async () => await evalIn(`document.getElementById("dlg-text").open`), 5, "text dialog");
  await evalIn(`document.querySelector("#dlg-text .dlg-cancel").click(); 0`);
  await evalIn(`document.getElementById("closeproj").click(); 0`);
  await until(async () => await evalIn(`document.getElementById("dlg-confirm").open`), 5, "close dialog");
  ok(true, "Close Project opens in-page");
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click(); 0`);
```

- [ ] **Step 2: Run and watch it pass**

Run: `deno run -A tests/browser/dialogs.mjs`
Expected: PASS.

- [ ] **Step 3: Revert-check the guard**

Temporarily change one converted call site back to `confirm(...)` — the `delete` branch of `fileMenu` is the smallest — and drive it. Confirm the "no code path reached a native browser dialog" assertion fails. Restore.

- [ ] **Step 4: Run the full Rust suite**

Run: `cargo test -- --test-threads=1`
Expected: PASS. A bare `cargo test` hangs on this host; the flag is required, not stylistic.

- [ ] **Step 5: Run the full browser suite on the Linux host**

Run each of `dialogs.mjs`, `closetab.mjs`, `mdlinks.mjs`, `closeproject.mjs`, `worktree-launch.mjs`, `termlinks.mjs`, `popups.mjs`, `search.mjs`.

`termlinks.mjs` is the one to watch: it must still pass **unchanged**, including its `window.confirm` stub and its `__confirms.length === 0` assertion. That stub is about xterm's internal fallback, not roost's dialogs, and removing it would silently destroy a real assertion.

These flake under contention. Re-run any failure on its own before believing it.

- [ ] **Step 6: Confirm the running binary changed**

`cargo build` updates neither path the service uses. Follow `docs/deploy.md`, then confirm the running binary is the new one — a passing test suite says nothing about what is deployed.

- [ ] **Step 7: Document the new tests**

Add `dialogs.mjs` and `closetab.mjs` to `tests/browser/README.md`'s file list, each with its revert-check log, following the format the existing entries use.

- [ ] **Step 8: Commit**

```bash
git add tests/browser/dialogs.mjs tests/browser/README.md
git commit -m "test: guard against native dialogs returning to app.js"
```
