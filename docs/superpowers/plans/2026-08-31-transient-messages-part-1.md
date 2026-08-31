# Transient Messages, Part 1: the primitives Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split `.conflict`'s four tenants into a notice, a status and a
question, each placed where its lifetime implies, fixing three measured
defects: simultaneous notices rendering as one, a notice overlapping the
header, and upload progress squeezing the layout.

**Architecture:** One message primitive (`showMessage`) rendering into one of
two regions — a per-pane region between `.panehead` and `.content`, or a fixed
column below the header for messages with no pane. `showBanner` and `showError`
become thin wrappers over it, so the fifteen existing call sites are untouched
by this part. Progress stops being a box entirely and becomes an absolutely
positioned bar on the pane's own bottom edge, which is what removes the layout
cost rather than reducing it.

**Tech Stack:** Plain JS in `static/app.js`, hand-written CSS in
`static/style.css`, browser tests in Deno driving Chromium over CDP through
`tests/browser/harness.mjs`. No framework, no build step for the client.

**Spec:** `docs/superpowers/specs/2026-08-31-transient-messages-design.md`

**Scope:** This plan is Part 1 of three. Part 2 (the ten native
`prompt()`/`confirm()`/`alert()` sites) and Part 3 (the bell panel) get their
own plans once this lands — they build on the vocabulary this establishes.

## Global Constraints

- **No new theme tokens.** Use only `--del-fg`, `--warn`, `--add-fg`,
  `--accent`, `--bg2`, `--bg3`, `--border`, `--fg`, `--muted`, `--tab-hover`,
  `--mono`. Every shipped theme defines these; adding a token means editing all
  five plus breaking user themes.
- **Kind classes must be namespaced `m-*`.** A bare `.warn` already exists as a
  global utility at `static/style.css:120` (`color: #d29922; cursor: help`).
  `.msg.warn` silently inherits both.
- **`.msgs:empty { display: none }` is load-bearing.** A pane with no messages
  must render byte-identically to today, or every pane gains a phantom gap.
- **A question never auto-expires.** Only notices carry a TTL.
- **Run the Rust suite as `cargo test -- --test-threads=1`.** A bare
  `cargo test` hangs in this repo.
- **Browser tests are the load-bearing ones.** No Rust test reaches
  `static/app.js`. Every new assertion must be checked by reverting the fix and
  watching it fail — see `tests/browser/README.md` for the four traps.
- **All HTML built in Rust lives in `render.rs`** (CLAUDE.md). This part builds
  no server-side HTML; everything here is client-constructed DOM, which is the
  existing pattern for these four surfaces.

---

## File Structure

- **Modify `static/style.css`** — add the `.msgs` / `.msg` / `#globalmsgs` /
  `.paneprog` / `.panestat` rules; delete `.conflict.error-banner`. `.conflict`
  itself stays until Part 2, because `showConflict` and `showClaudeHere` still
  use it until Task 3.
- **Modify `static/app.js`** — add `paneMessages`, `globalMessages`,
  `showMessage`, `paneProgress`; rewrite `showBanner`, `showError`,
  `setUploadProgress`, `showConflict`, `showClaudeHere`.
- **Create `tests/browser/messages.mjs`** — the whole surface's browser tests.
  A new file rather than an addition to an existing one: `popups.mjs` is about
  the header popups' focus behaviour and shares no fixture with this.

---

### Task 1: The notice primitive and its two regions

Fixes two measured defects: three simultaneous notices occupying one identical
band (all at `top: 20`, `height: 52`, right-anchored, so `elementFromPoint`
returns only the last), and the notice box overlapping the header band by 18px.

**Files:**
- Modify: `static/style.css` (append after the `.conflict.error-banner` rule at
  `:379`, which this task deletes)
- Modify: `static/app.js:2155-2170` (`showBanner`, `showError`)
- Test: `tests/browser/messages.mjs` (create)

**Interfaces:**
- Produces, relied on by Tasks 2 and 3:
  - `paneMessages(pi)` → the `.msgs` element for pane index `pi`, created on
    demand and inserted before that pane's `.content`.
  - `globalMessages()` → the `#globalmsgs` element, created on demand.
  - `showMessage({ kind, text, html, pane, actions, ttl })` → the `.msg`
    element. `kind` is `"m-err" | "m-warn" | "m-ok"`. Exactly one of `text`
    (escaped, safe) or `html` (trusted, caller-built) must be given. `pane` is
    a pane index or `null` for the global column. `actions` is an array of
    `{ label, onClick }`. `ttl` in ms, or `0`/omitted for no expiry.
- Consumes: nothing.

- [ ] **Step 1: Write the failing test**

Create `tests/browser/messages.mjs`:

```js
//! The transient message surfaces: do simultaneous notices stack, and do they
//! stay clear of the header?
//!
//! No Rust test reaches static/app.js, so none of this is covered otherwise.
//!
//! The trap this file exists to avoid: "three notices are in the DOM" is TRUE
//! TODAY — the defect was never that they are missing, it is that they are
//! drawn on top of each other. Every assertion here is geometric.
//!
//! Run: deno run -A tests/browser/messages.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");

  // ---- 1. Three notices at once do not overlap ----------------------------
  await evalIn(`showError("first failure"); showError("second failure"); showError("third failure")`);
  await sleep(300);
  const boxes = await evalIn(
    `JSON.stringify([...document.querySelectorAll("#globalmsgs .msg")].map(n => {
       const r = n.getBoundingClientRect();
       return { top: Math.round(r.top), bottom: Math.round(r.bottom),
                left: Math.round(r.left), right: Math.round(r.right) }; }))`
  ).then(JSON.parse);
  ok(boxes.length === 3, `three notices rendered (got ${boxes.length})`);
  // Disjoint vertically, in order. Comparing tops alone is not enough: two
  // boxes can share a top and differ in left purely because their text is a
  // different width, which is exactly how the old bug read as "not identical".
  let disjoint = true;
  for (let i = 1; i < boxes.length; i++) {
    if (boxes[i].top < boxes[i - 1].bottom) disjoint = false;
  }
  ok(disjoint, `each notice sits below the previous one (${JSON.stringify(boxes.map(b => [b.top, b.bottom]))})`);

  // Independent probe: the topmost element at each notice's own corner is that
  // notice. This is what actually failed before — elementFromPoint at the first
  // banner's corner returned the third.
  const owned = await evalIn(
    `JSON.stringify([...document.querySelectorAll("#globalmsgs .msg")].map((n, i) => {
       const r = n.getBoundingClientRect();
       const hit = document.elementFromPoint(r.left + 6, r.top + 6);
       return hit && hit.closest(".msg") === n; }))`
  ).then(JSON.parse);
  ok(owned.every(Boolean), `each notice is the element at its own corner (${JSON.stringify(owned)})`);

  // ---- 2. The notice column clears the header -----------------------------
  // Box intersection, NOT a centre-point hit test: the old banner's box
  // overlapped the header by 18px while the buttons' centres escaped by one
  // pixel, so a centre-point probe passed against the bug.
  const clears = await evalIn(
    `(() => { const h = document.querySelector("header").getBoundingClientRect();
       return [...document.querySelectorAll("#globalmsgs .msg")]
         .every(n => n.getBoundingClientRect().top >= h.bottom); })()`
  );
  ok(clears, "no notice's box intersects the header band");

  // ---- 3. A notice about a pane renders inside that pane ------------------
  await evalIn(`document.querySelectorAll("#globalmsgs .msg").forEach(n => n.remove())`);
  await evalIn(`showMessage({ kind: "m-err", text: "upload failed", pane: 0 })`);
  await sleep(200);
  ok(await evalIn(`!!document.querySelector('.pane[data-pane="0"] .msgs .msg')`),
    "a message with a pane renders in that pane");
  // Under the pane's own chrome, not above it — the placement correction.
  ok(await evalIn(
    `(() => { const p = document.querySelector('.pane[data-pane="0"]');
       const head = p.querySelector(":scope > .panehead");
       const msgs = p.querySelector(":scope > .msgs");
       return !!(head && msgs) &&
         (head.compareDocumentPosition(msgs) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0; })()`),
    "and it sits after the pane head in document order, not before it");

  // ---- 4. An empty region takes no space ----------------------------------
  await evalIn(`document.querySelectorAll(".msgs .msg").forEach(n => n.remove())`);
  await sleep(200);
  ok(await evalIn(
    `[...document.querySelectorAll(".msgs")].every(m => m.getBoundingClientRect().height === 0)`),
    "an emptied region occupies zero height");

  // ---- 4b. A message for a pane that does not exist still appears ---------
  // Silently dropping it would be this codebase's recurring bug: treating
  // "could not place this" as "nothing to say".
  await evalIn(`showMessage({ kind: "m-err", text: "orphaned message", pane: 99 })`);
  await sleep(200);
  ok(await evalIn(`[...document.querySelectorAll("#globalmsgs .msg")].some(n => n.textContent.includes("orphaned"))`),
    "a message naming a pane that does not exist falls back to the column");
  await evalIn(`document.querySelectorAll("#globalmsgs .msg").forEach(n => n.remove())`);

  // ---- 5. Text is escaped, not parsed -------------------------------------
  await evalIn(`showMessage({ kind: "m-err", text: '<img src=x onerror="window.__xss=1">' })`);
  await sleep(300);
  ok(await evalIn(`window.__xss === undefined`), "a message's text is escaped, never parsed as HTML");
  ok(await evalIn(`document.body.textContent.includes("onerror")`),
    "and the offending text is still shown to the user, as text");
} finally {
  try { page && page.close && page.close(); } catch {}
  browser.close();
  await resh.close();
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
```

- [ ] **Step 2: Run it and watch it fail**

Run: `deno run -A tests/browser/messages.mjs`

Expected: fails at the first assertion — `#globalmsgs` does not exist, so
`boxes.length` is `0`, and `showMessage` is not defined.

- [ ] **Step 3: Add the CSS**

In `static/style.css`, **delete** the `.conflict.error-banner` rule and its
comment (currently `:376-379`), and append:

```css
/* ---- transient messages ------------------------------------------------- */
/* One region per pane, between .panehead and .content. A message belongs to
   the pane whose work produced it, and sits UNDER that pane's identity rather
   than above it: the old boxes prepended into .content, which pushed the
   breadcrumb and the tab strip down and left the message reading as though it
   belonged to nothing. .pane is already a flex column, so this slots in.
   `:empty` is load-bearing — a pane with no messages must be byte-identical to
   before, or every pane grows a gap. */
.msgs { flex: 0 1 auto; display: flex; flex-direction: column;
        max-height: 45%; overflow-y: auto; }
.msgs:empty { display: none; }

/* Severity is a 2px left rule, never a full outline: .pane already draws a
   border, and a second rectangle inside it is what made .conflict read as
   detached furniture floating in a pane rather than as part of it.
   The kind classes are namespaced because a bare `.warn` is already a global
   utility above (amber text, cursor:help) — `.msg.warn` inherits both, and
   renders the whole sentence amber. */
.msg { display: grid; grid-template-columns: 1fr auto; gap: 8px;
       padding: 7px 8px 7px 10px; background: var(--bg2);
       border-bottom: 1px solid var(--border);
       border-left: 2px solid var(--muted); font-size: 12px; line-height: 1.5; }
.msg.m-err  { border-left-color: var(--del-fg); }
.msg.m-warn { border-left-color: var(--warn); }
.msg.m-ok   { border-left-color: var(--add-fg); }

/* The subject of a message is a thing on disk — a path, a session name — so it
   takes the mono face, the same rule the tree and the breadcrumb follow. */
.msg .subj { font-family: var(--mono); color: var(--fg); }
.msg .msgtext { min-width: 0; overflow-wrap: anywhere; }
.msg .msgclose { align-self: start; display: flex; align-items: center;
                 justify-content: center; width: 18px; height: 18px; padding: 0;
                 border: 1px solid transparent; border-radius: 4px; background: none;
                 color: var(--muted); font: inherit; line-height: 1; cursor: pointer; }
.msg .msgclose:hover { color: var(--fg); background: var(--tab-hover); }
.msg .msgclose:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
.msgactions { grid-column: 1 / -1; display: flex; gap: 8px; margin-top: 8px; }

/* Messages with no pane to belong to — a delete or rename failure. A real flex
   column, never N independently fixed boxes at the same top, which is why three
   simultaneous errors used to render as one. Anchored below the header, which
   is what removes the 18px collision by construction rather than by tuning. */
#globalmsgs { position: fixed; top: calc(var(--header-h, 38px) + 6px); right: 12px;
              z-index: 30; width: 380px; max-width: calc(100vw - 24px);
              display: flex; flex-direction: column; gap: 6px; }
#globalmsgs .msg { border: 1px solid var(--border); border-left: 2px solid var(--muted);
                   border-radius: 6px; box-shadow: 0 6px 18px rgba(0,0,0,.35); }
#globalmsgs .msg.m-err  { border-left-color: var(--del-fg); }
#globalmsgs .msg.m-warn { border-left-color: var(--warn); }
#globalmsgs .msg.m-ok   { border-left-color: var(--add-fg); }
```

- [ ] **Step 4: Implement the primitive**

In `static/app.js`, replace `showBanner` and `showError` (currently
`:2155-2170`, including the doc comment above `showBanner`) with:

```js
/// Where a message about `pi` goes: a region between that pane's head and its
/// content, created on demand. Under the head, never above it — a message that
/// pushes its pane's own chrome down reads as belonging to nothing.
function paneMessages(pi) {
  const pane = document.querySelector(`.pane[data-pane="${pi}"]`);
  if (!pane) return null;
  let region = pane.querySelector(":scope > .msgs");
  if (!region) {
    region = document.createElement("div");
    region.className = "msgs";
    pane.insertBefore(region, pane.querySelector(":scope > .content"));
  }
  return region;
}

/// The column for messages with no pane to belong to. A real flex column: the
/// bug this replaces was three `position: fixed` boxes at one identical top,
/// where only the last was reachable.
function globalMessages() {
  let region = document.getElementById("globalmsgs");
  if (!region) {
    region = document.createElement("div");
    region.id = "globalmsgs";
    region.className = "msgs";
    document.body.appendChild(region);
  }
  return region;
}

/// The one message primitive.
///
/// `text` is escaped; `html` is inserted as-is and is only for markup this file
/// built itself (a diff from the server, a `.subj` span). Passing both is a
/// caller bug and throws rather than silently preferring one.
///
/// `ttl` is optional and must be omitted for anything awaiting an answer: a
/// question that expires is a decision made by timeout.
function showMessage({ kind = "m-warn", text, html, pane = null, actions = [], ttl = 0 }) {
  if ((text == null) === (html == null)) {
    throw new Error("showMessage needs exactly one of text or html");
  }
  // A named pane that no longer exists falls back to the column rather than
  // returning null. The spec's "a question that outlives its pane" case: a
  // SaveConflict can arrive after the user closed that pane, and dropping it
  // silently would be this codebase's own recurring mistake — treating "I could
  // not place this" as "there was nothing to say".
  const host = (pane === null ? null : paneMessages(pane)) || globalMessages();
  const box = document.createElement("div");
  box.className = `msg ${kind}`;
  const body = document.createElement("div");
  body.className = "msgtext";
  if (text != null) body.textContent = text;
  else body.innerHTML = html;
  const close = document.createElement("button");
  close.className = "msgclose";
  close.type = "button";
  close.setAttribute("aria-label", "dismiss");
  close.textContent = "×";
  close.onclick = () => box.remove();
  box.append(body, close);
  if (actions.length) {
    const bar = document.createElement("div");
    bar.className = "msgactions";
    for (const a of actions) {
      const b = document.createElement("button");
      b.className = "savebtn";
      b.type = "button";
      b.textContent = a.label;
      b.onclick = () => { a.onClick(); box.remove(); };
      bar.appendChild(b);
    }
    box.appendChild(bar);
  }
  host.appendChild(box);
  if (ttl) setTimeout(() => box.remove(), ttl);
  return box;
}

/// Kept as the name fifteen call sites already use. A notice: it expires.
function showBanner(text) {
  return showMessage({ kind: "m-warn", text, ttl: 8000 });
}

/// The red rule says "error" more clearly than a prefix did, and no test
/// asserts on the old "Error: " string (checked 2026-08-31).
function showError(msg) {
  return showMessage({ kind: "m-err", text: msg, ttl: 8000 });
}
```

Note `app.js:393` calls `showBanner` and then appends a link to
`.error-banner:last-of-type b` (`:396`). That selector no longer matches.
Change those two lines to use the returned element:

```js
      // Before: showBanner(...) then querySelector(".error-banner:last-of-type b").
      // showMessage returns the box, so the link goes straight into it.
      const box = showBanner(`opened ${ev.url.split("/").pop()} — `);
      box && box.querySelector(".msgtext")?.append(a);
```

- [ ] **Step 5: Run the test and watch it pass**

Run: `deno run -A tests/browser/messages.mjs`
Expected: `all passed`, 9 assertions.

- [ ] **Step 6: Verify the tests can fail**

Apply each break, run, confirm the named assertion fails, restore:

1. In `#globalmsgs`, change `flex-direction: column` to `display: block` and
   add `position: fixed; top: 20px` to `#globalmsgs .msg` — reproduces the old
   bug. Expected failures: "each notice sits below the previous one" **and**
   "each notice is the element at its own corner".
2. Change `#globalmsgs`'s `top` to `12px` (above the 38px header). Expected
   failure: "no notice's box intersects the header band".
3. In `showMessage`, change `body.textContent = text` to
   `body.innerHTML = text`. Expected failure: "a message's text is escaped".

Record the observed failure lines in the test file's header comment, in the
style of `tests/browser/mdlinks.mjs:11-54`.

- [ ] **Step 7: Run the Rust suite**

Run: `cargo test -- --test-threads=1`
Expected: all pass. Nothing here touches Rust, so a failure means a stale
assertion on served CSS — investigate rather than adjust.

- [ ] **Step 8: Commit**

```bash
git add static/style.css static/app.js tests/browser/messages.mjs
git commit -m "messages: one primitive, two regions, and notices that stack"
```

---

### Task 2: Progress on the pane's own edge

Fixes the third measured defect: the progress box squeezes pane 0 by 32px and
pane 1 by 21px for as long as an upload runs.

**Files:**
- Modify: `static/style.css` (append to the block from Task 1)
- Modify: `static/app.js:2634-2650` (`setUploadProgress`), `:2651` (`postFiles`),
  `:2682` (`uploadFiles`), `:2717-2740` (the drop handler)
- Test: `tests/browser/messages.mjs` (extend)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: `paneProgress(pi, fraction, label)` — `fraction` in `0..1`, or
  `null` to clear. `pi` is a pane index; `null` targets pane 0, the Files pane,
  which is where a drop with no resolvable pane came from.

- [ ] **Step 1: Write the failing test**

Append to `tests/browser/messages.mjs`, before the `finally`:

```js
  // ---- 6. Progress costs no layout ---------------------------------------
  // The measurement that matters is pane geometry, not the bar's presence.
  // "an upload shows progress" is true today; the defect is that showing it
  // shrinks every pane, so that is what this asserts.
  await evalIn(`document.querySelectorAll(".msgs .msg").forEach(n => n.remove())`);
  await sleep(200);
  const geom = () => evalIn(
    `JSON.stringify([...document.querySelectorAll(".pane")]
       .map(p => Math.round(p.getBoundingClientRect().height)))`).then(JSON.parse);
  const before = await geom();
  await evalIn(`paneProgress(0, 0.62, "photo.png 62%")`);
  await sleep(300);
  const during = await geom();
  ok(JSON.stringify(before) === JSON.stringify(during),
    `pane heights are unchanged while progress shows (${JSON.stringify(before)} vs ${JSON.stringify(during)})`);

  // And it is actually drawn — otherwise "unchanged heights" passes trivially
  // with paneProgress doing nothing at all.
  ok(await evalIn(
    `(() => { const b = document.querySelector('.pane[data-pane="0"] > .paneprog');
       if (!b) return false;
       const p = b.parentElement.getBoundingClientRect(), r = b.getBoundingClientRect();
       // ~62% of the pane's width, and sitting on its bottom edge.
       return Math.abs(r.width / p.width - 0.62) < 0.03 && Math.abs(r.bottom - p.bottom) < 3; })()`),
    "the bar is drawn at the right fraction, on the pane's bottom edge");

  ok(await evalIn(`document.querySelector('.pane[data-pane="0"] .panestat')?.textContent === "photo.png 62%"`),
    "the label names the file and the percentage");

  // The label must not be inside .panehead: it competed with the tab strip for
  // width there, truncating the pane title to "F" and giving the strip a
  // scrollbar.
  ok(await evalIn(`!document.querySelector('.pane[data-pane="0"] .panehead .panestat')`),
    "and the label is not inside the pane head");

  await evalIn(`paneProgress(0, null)`);
  await sleep(200);
  const after = await geom();
  ok(JSON.stringify(before) === JSON.stringify(after), "clearing restores the layout");
  ok(await evalIn(`!document.querySelector(".paneprog") && !document.querySelector(".panestat")`),
    "and removes both the bar and the label");
```

- [ ] **Step 2: Run it and watch it fail**

Run: `deno run -A tests/browser/messages.mjs`
Expected: fails with `paneProgress is not defined`.

- [ ] **Step 3: Add the CSS**

Append to `static/style.css`:

```css
/* Progress is drawn ON the pane's bottom edge, not in a box. A box costs
   layout — measured 2026-08-31, the old one squeezed pane 0 by 32px and pane 1
   by 21px for the whole upload — and the edge of the pane receiving the files
   is where "this pane is filling up" belongs. .pane already carries a 1px
   border, so this replaces an edge rather than adding a shape. */
.paneprog { position: absolute; left: 0; bottom: 0; height: 2px;
            background: var(--accent); transition: width .12s linear;
            pointer-events: none; z-index: 2; }
/* Deliberately NOT in .panehead: a pane is narrow and its head is already
   full, and putting the label there shrank the tab strip until the pane title
   truncated to a single letter and the strip grew a scrollbar. */
/* Bottom LEFT, not right. A terminal's flash badge already owns bottom-right
   (.content .termhost[data-flash]::before, and the socket-status badge owns
   top-right) — and pasting an image into a terminal runs through the same
   progress path, so a right-aligned pill would land on top of it. */
.panestat { position: absolute; left: 8px; bottom: 8px; z-index: 2;
            padding: 2px 7px; border: 1px solid var(--border); border-radius: 5px;
            background: var(--bg2); font: 11px/1.5 var(--mono); color: var(--muted);
            white-space: nowrap; pointer-events: none; }
```

`.paneprog` needs a positioned ancestor. Add `position: relative;` to the
existing `.pane` rule at `static/style.css:153`.

**This re-anchors nothing — checked 2026-08-31.** `.content` is already
`position: relative` (`static/style.css:219`) and `.termhost` is
`position: absolute` inside it (`:224`), so the two absolutely-positioned
badges at `:240` and `:252` are pseudo-elements resolving against `.termhost`,
which is nearer than `.pane` and stays nearer.

- [ ] **Step 4: Implement**

Replace `setUploadProgress` (`static/app.js:2634-2650`, comment included) with:

```js
/// Upload progress for one pane, drawn on that pane's bottom edge.
///
/// Absolutely positioned, which is the whole point: the box this replaces was
/// in-flow and squeezed every pane for the duration of the upload. `fraction`
/// is 0..1, or null to clear.
function paneProgress(pi, fraction, label) {
  const pane = document.querySelector(`.pane[data-pane="${pi ?? 0}"]`);
  if (!pane) return;
  let bar = pane.querySelector(":scope > .paneprog");
  let stat = pane.querySelector(":scope > .panestat");
  if (fraction === null) {
    bar && bar.remove();
    stat && stat.remove();
    return;
  }
  if (!bar) {
    bar = document.createElement("div");
    bar.className = "paneprog";
    pane.appendChild(bar);
  }
  if (!stat) {
    stat = document.createElement("span");
    stat.className = "panestat";
    pane.appendChild(stat);
  }
  bar.style.width = `${Math.round(fraction * 1000) / 10}%`;
  stat.textContent = label;
}
```

Then thread the pane through the upload path. In `postFiles`
(`static/app.js:2651`), take a fourth argument and use it:

```js
function postFiles(url, files, label, pane) {
  const form = new FormData();
  for (const f of files) form.append("file", f, f.name);
  const xhr = new XMLHttpRequest();
  xhr.open("POST", url);
  // XHR rather than fetch: fetch exposes no upload progress, and a 100 MB send
  // with no feedback is indistinguishable from a hang.
  xhr.upload.onprogress = (e) => {
    if (e.lengthComputable) paneProgress(pane, e.loaded / e.total, `${label} ${Math.round(e.loaded / e.total * 100)}%`);
  };
  xhr.onload = () => {
    paneProgress(pane, null);
    if (xhr.status !== 200) return showMessage({ kind: "m-err", text: `${label}: ${xhr.responseText || xhr.status}`, pane, ttl: 8000 });
    let body = {};
    try { body = JSON.parse(xhr.responseText); } catch { return; }
    // One message per failed part, in the pane the drop landed on. Sixteen are
    // possible (MAX_UPLOAD_PARTS); before this they were sixteen fixed boxes at
    // one identical top, of which the user saw one.
    for (const r of body.results || []) {
      if (!r.ok) showMessage({ kind: "m-err", text: `${r.name}: ${r.error}`, pane, ttl: 8000 });
    }
  };
  xhr.onerror = () => {
    paneProgress(pane, null);
    showMessage({ kind: "m-err", text: `${label}: upload failed`, pane, ttl: 8000 });
  };
  xhr.send(form);
}
```

`uploadFiles` (`:2682`) takes and forwards the pane:

```js
function uploadFiles(files, dir, pane) {
  const refusal = tooManyFiles(files);
  if (refusal) return showMessage({ kind: "m-err", text: refusal, pane, ttl: 8000 });
  const q = dir ? `?dir=${dir.split("/").map(encodeURIComponent).join("/")}` : "";
  postFiles(`/upload/${PROJECT}${q}`, files, `upload to ${dir || "project root"}`, pane);
}
```

In the drop handler (`:2717-2740`), derive the pane from the event target — it
is already the thing `uploadTargetDir` and `sessionUnder` are given:

```js
document.addEventListener("drop", (e) => {
  if (!dragHasFiles(e.dataTransfer)) return;
  e.preventDefault();
  // The pane the drop landed on: where its progress and any failures belong.
  const pane = e.target.closest && e.target.closest(".pane");
  const pi = pane ? Number(pane.dataset.pane) : null;

  const session = sessionUnder(e.target);
  if (session) {
    const img = e.dataTransfer.files.length ? firstImage(e.dataTransfer.files) : null;
    if (img) {
      postFiles(`/paste/${PROJECT}/${session}`, [img], "paste", pi);
      return;
    }
    return showMessage({ kind: "m-err", pane: pi, ttl: 8000,
      text: "only images can be dropped on a terminal — drop other files on the Files pane" });
  }

  const dir = uploadTargetDir(e.target);
  if (dir === null) {
    return showMessage({ kind: "m-err", pane: pi, ttl: 8000,
      text: "drop files on the Files pane to upload them" });
  }
  const dirs = droppedDirectories(e.dataTransfer);
  if (dirs.length) {
    return showMessage({ kind: "m-err", pane: pi, ttl: 8000,
      text: `folders are not uploaded (${dirs.join(", ")}) — use git or scp for a directory` });
  }
  if (e.dataTransfer.files.length) uploadFiles(e.dataTransfer.files, dir, pi);
});
```

The paste path at `:2723` and `:2755` also calls `postFiles`. Both paste into a
terminal, so pass that terminal's pane:

```js
    postFiles(`/paste/${PROJECT}/${session}`, [img], "paste",
      Number(document.querySelector(`.termhost[data-session="${session}"]`)?.closest(".pane")?.dataset.pane ?? 0));
```

This selector is correct as written: `mountTerminal` sets
`node.className = "termhost"` and `node.dataset.session = session`
(`static/app.js:1483-1484`), and `sessionUnder` already reads terminals back
that way (`:2619-2620`).

- [ ] **Step 5: Run the test and watch it pass**

Run: `deno run -A tests/browser/messages.mjs`
Expected: `all passed`, 15 assertions.

- [ ] **Step 6: Verify the tests can fail**

1. Change `.paneprog` from `position: absolute` to `position: static`.
   Expected failure: "pane heights are unchanged while progress shows".
2. Make `paneProgress` a no-op (`return;` as its first line). Expected
   failures: "the bar is drawn at the right fraction" and "the label names the
   file" — note the heights assertion still PASSES, which is why both exist.
3. Append the label to `pane.querySelector(".panehead")` instead. Expected
   failure: "the label is not inside the pane head".

Record the observed failures in the file header.

- [ ] **Step 7: Check a real upload end to end**

Run: `deno run -A tests/browser/upload.mjs`
Expected: all pass. This exercises the real `POST /upload` path that Task 2
rewired; `messages.mjs` only calls `paneProgress` directly.

- [ ] **Step 8: Commit**

```bash
git add static/style.css static/app.js tests/browser/messages.mjs
git commit -m "messages: progress rides the pane's edge instead of squeezing the layout"
```

---

### Task 3: The two questions move under the pane's chrome

**Files:**
- Modify: `static/app.js:2105-2121` (`showConflict`), `:2123-2153`
  (`showClaudeHere`)
- Modify: `static/style.css` — delete `.conflict`, `.conflict button`,
  `.claudehere`, `.claudehere button`, `.claudehere .wt-new` once nothing uses
  them
- Test: `tests/browser/messages.mjs` (extend), `tests/browser/save.mjs` (check)

**Interfaces:**
- Consumes: `showMessage` from Task 1.
- Produces: nothing further.

- [ ] **Step 1: Write the failing test**

Append to `tests/browser/messages.mjs`, before the `finally`:

```js
  // ---- 7. A save conflict is a question in the file's own pane ------------
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "hello.md", mode: "Edit" } })`);
  await until(() => evalIn(`!!document.querySelector('.pane[data-pane="2"] .content')`), 15, "pane 2");
  await evalIn(`showConflict({ rel: "hello.md", diff_html: '<pre class="diffview"><div class="dl del">- a</div></pre>' })`);
  await sleep(300);
  ok(await evalIn(`!!document.querySelector('.pane[data-pane="2"] .msgs .msg.m-warn')`),
    "a save conflict renders as a question in the file's pane");
  ok(await evalIn(
    `(() => { const p = document.querySelector('.pane[data-pane="2"]');
       const head = p.querySelector(":scope > .panehead");
       const msgs = p.querySelector(":scope > .msgs");
       return (head.compareDocumentPosition(msgs) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0; })()`),
    "and below the pane's head, not above it");
  ok(await evalIn(`document.querySelectorAll('.pane[data-pane="2"] .msgactions .savebtn').length === 2`),
    "with two quiet buttons, not native chrome");
  // A question must never expire: an auto-dismissed question is a decision
  // made by timeout. 8s is the notice TTL, so this is the discriminating wait.
  await sleep(9000);
  ok(await evalIn(`!!document.querySelector('.pane[data-pane="2"] .msgs .msg.m-warn')`),
    "and it is still there after the notice TTL has passed");
  ok(await evalIn(`!document.querySelector(".conflict")`),
    "nothing on the page still uses the old .conflict class");
```

- [ ] **Step 2: Run it and watch it fail**

Run: `deno run -A tests/browser/messages.mjs`
Expected: the first of the five fails — `showConflict` still renders
`.conflict` into `.content`, so no `.msgs .msg.m-warn` exists in pane 2.

- [ ] **Step 3: Rewrite `showConflict`**

Replace `static/app.js:2105-2121` with:

```js
/// A question, not a notice: no TTL, because a save conflict that dismissed
/// itself would be a decision about the user's file made by a timer.
///
/// Rendered into pane 2's message region rather than prepended into .content,
/// which used to place it above the file's own breadcrumb.
function showConflict(ev) {
  showMessage({
    kind: "m-warn",
    pane: 2,
    // html, not text: diff_html is markup this app's own server built
    // (render::diff_html), and the rel is escaped into it here.
    html: `<b><span class="subj">${escapeHtml(ev.rel)}</span> changed on disk since you opened it.</b>` +
          `<div class="msgdetail">${ev.diff_html}</div>`,
    actions: [
      { label: "Overwrite the file", onClick: () => send({ t: "SaveBuffer", rel: ev.rel, force: true }) },
      { label: "Discard my changes", onClick: () => send({ t: "CloseBuffer", rel: ev.rel }) },
    ],
  });
}
```

Add to `static/style.css`, with the message rules:

```css
/* A diff inside a question. Bounded so a large conflict cannot fill the pane
   and push its own buttons out of reach. */
.msgdetail { margin: 6px 0 0; max-height: 140px; overflow: auto; }
```

Neither button is accented, for the reason `static/style.css:366` already gives
for `.proposal-actions`: both options destroy something — overwrite discards
the disk's changes, discard-mine discards yours — so the affordance must not
lean on the answer.

- [ ] **Step 4: Rewrite `showClaudeHere`**

Replace `static/app.js:2123-2153` with:

```js
/// The "a Claude is already here" prompt. Per-browser and transient: a question
/// to the person who clicked, not a state of the project.
///
/// In the terminal pane's own message region now, under its tab strip — it used
/// to prepend into the pane and push that strip down.
function showClaudeHere(pane, terminals) {
  document.querySelectorAll(".msg.claudehere").forEach((n) => n.remove());
  const box = showMessage({
    kind: "m-warn",
    pane,
    html: terminals.length
      ? `A Claude is already working in this project (<span class="subj">${terminals.map(escapeHtml).join(", ")}</span>).`
      : "A Claude is already working in this project.",
    actions: [
      {
        label: "Start in a new worktree",
        onClick: () => {
          // Opened synchronously, inside this click's user-gesture, so the
          // popup blocker allows it; WorktreeReady navigates it once the
          // server responds.
          pendingTab = window.open("about:blank");
          send({ t: "NewWorktree", launch: "claude" });
        },
      },
      { label: "Start here anyway", onClick: () => send({ t: "NewTerminal", pane, launch: "claude", force: true }) },
    ],
  });
  box && box.classList.add("claudehere");
}
```

- [ ] **Step 5: Delete the dead CSS**

Confirm nothing references them, then delete `.conflict`, `.conflict button`,
`.claudehere`, `.claudehere button` and `.claudehere .wt-new` from
`static/style.css`:

```bash
grep -rn 'conflict\|claudehere' static/ src/ tests/
```

Expected: only `showConflict`'s function name, the `SaveConflict` event name in
`src/proto.rs`, and the `.msg.claudehere` marker class above. The
`.claudehere` **selector** in CSS must be gone; the class stays only as a
marker for the `querySelectorAll` de-dupe.

- [ ] **Step 6: Run everything**

```bash
deno run -A tests/browser/messages.mjs
deno run -A tests/browser/save.mjs
deno run -A tests/browser/upload.mjs
deno run -A tests/browser/worktree-launch.mjs
cargo test -- --test-threads=1
```

Expected: all pass. `save.mjs` and `worktree-launch.mjs` are the two suites
that exercise these questions for real; if either asserts on `.conflict` or
`.claudehere` markup, update it to the new structure rather than restoring the
old class.

- [ ] **Step 7: Verify the tests can fail**

1. Restore `showConflict`'s `.content` prepend. Expected failures: "renders as
   a question in the file's pane" and "below the pane's head".
2. Add `ttl: 8000` to `showConflict`'s `showMessage` call. Expected failure:
   "still there after the notice TTL has passed".

Record both in the file header.

- [ ] **Step 8: Check it in a browser, in more than one theme**

Per CLAUDE.md's dev/prod substitution table, no browser means no confidence.
Start a scratch instance (never the live one on 8444 — a headless tab clamps
every terminal's PTY), provoke each of the four surfaces, and look at them in
`dark` and `light` at minimum:

```bash
RESH_ROOTS=<scratch>/roots RESH_STATE_DIR=<scratch>/state \
  RESH_STATIC=$PWD/static <target-dir>/debug/resh <free-port>
```

- [ ] **Step 9: Commit**

```bash
git add static/style.css static/app.js tests/browser/messages.mjs
git commit -m "messages: the two questions sit under their pane's own chrome"
```

---

## What this part deliberately leaves alone

- The ten native `prompt()` / `confirm()` / `alert()` sites — Part 2.
- `#noticepanel` — Part 3.
- The `reconcileList` pending-row hazard: it only matters once the tree has an
  inline editing row, which is Part 2. The requirement is recorded in the spec.
- Contrast measurement across all five themes. Step 8 above is a look, not a
  measurement; the rasterise-through-a-canvas probe belongs with Part 3, when
  the bell panel brings the last surface into the vocabulary.
