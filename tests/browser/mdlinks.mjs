//! Do a preview's own references actually work in a browser?
//!
//! No Rust test reaches static/app.js, so the selector that turns <a data-rel>
//! into an OpenTab intent, the mode-switch suppression, and whether an <img> element's
//! bytes ever arrived are all untested without this.
//!
//! naturalWidth, not presence: an <img> exists in the DOM whether or not the
//! request succeeded, so `querySelector("img") !== null` is one of the four
//! traps README.md warns about — it passes with the route deleted.
//!
//! Revert-the-fix, all four watched fail for real and then restored:
//!   1. Commented out the ["raw"] arm in serve_frag (src/routes.rs) so
//!      GET /frag/<project>/raw always 404s. Assertion 1 failed with:
//!        FAIL  a project image actually loaded its bytes
//!      (naturalWidth was 0, not 1 — the <img> tag still existed, only its
//!      bytes never arrived, which is exactly the trap this file's own
//!      header comment warns about).
//!   2. Restored the pre-fix `if (t.k === "File")` tab-icon gate at
//!      static/app.js (dropping `&& !refusesTextEdit(t.rel)`), so an image tab grew
//!      the ✎ toggle again. The last assertion failed with:
//!        FAIL  an image tab offers no edit toggle
//!   3. Widened the double-contextmenu guard in wireFragment back down to
//!      `e.target.closest("a.file")` (its pre-fix state; a `.mdlink` anchor
//!      has no `file` class). The new assertion failed with:
//!        FAIL  right-clicking a markdown link opened the file menu exactly
//!        once (got 2)
//!   4. Disabled the coerce_tab arm in apply_layout's OpenTab
//!      (src/workspace.rs) so a raw Edit request on an image passed through
//!      unmodified. Both coercion assertions failed:
//!        FAIL  an OpenTab intent requesting mode:"Edit" on a
//!        never-before-opened image was coerced to Preview (got Edit)
//!        FAIL  the pane rendered the picture (bytes loaded), not a
//!        textarea, after the coerced open
//!      This revert only discriminates against docs/raw-open.png, a rel
//!      never opened earlier in the run — tab_identity_eq (workspace.rs)
//!      matches tabs on rel alone, ignoring mode, so the first version of
//!      this assertion reused docs/shot.png (already open from step 4) and
//!      passed even with the coercion deleted: OpenTab just reactivated the
//!      existing Preview tab and never built a new Tab::File to coerce.
//! All four restored afterwards; `deno run -A tests/browser/mdlinks.mjs`
//! passes clean again (see task-6-report.md for the exact terminal output).
//!
//! Later reverts, for the fixes added after the branch review:
//!   5. Put "svg" back on NO_TEXT_EDIT_EXT in static/app.js and pointed the
//!      workspace.rs guards back at `is_image`. Both svg assertions failed:
//!        FAIL  an svg tab offers the edit toggle
//!        FAIL  clicking it mounts a textarea holding the svg's real text
//!      (the second timed out waiting for the editor, since the toggle that
//!      would have mounted it was never drawn).
//!   6. Added "javascript" to HREF_SCHEMES (src/render.rs). One assertion
//!      failed:
//!        FAIL  no javascript: href reached the page
//!      — and, importantly, "clicking it executed nothing" still printed ok,
//!      because that variant carries target="_blank" and Chromium blocks the
//!      popup. Classifying javascript: as Passthrough instead (plain href,
//!      no target) failed both. The CONTROL assertion exists because of
//!      this: without it, "clicking it executed nothing" would be
//!      indistinguishable from a browser that never runs javascript: hrefs.
//!
//! Run: deno run -A tests/browser/mdlinks.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
// A real 1x1 PNG, so naturalWidth is meaningfully 1 rather than 0.
const PNG = Uint8Array.from(atob(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
), (c) => c.charCodeAt(0));
await Deno.mkdir(`${fx.roots}/${fx.project}/docs`, { recursive: true });
await Deno.writeFile(`${fx.roots}/${fx.project}/docs/shot.png`, PNG);
// A second, distinct image never opened earlier in the run: tab_identity_eq
// (workspace.rs) matches on rel alone, ignoring mode, so re-sending OpenTab
// for an already-open rel just reactivates the existing tab and never
// constructs a new Tab::File at all — which would make a coercion assertion
// against docs/shot.png pass even with the coercion arm deleted.
await Deno.writeFile(`${fx.roots}/${fx.project}/docs/raw-open.png`, PNG);
await Deno.writeTextFile(`${fx.roots}/${fx.project}/docs/other.md`, "# other\n");
// SVG is on IMAGE_EXT (it renders as a picture) but NOT on NO_TEXT_EDIT_EXT
// (it is text, and read_text_file has always served it), so it must keep its
// ✎ toggle. The two lists differing by exactly this one entry is the point.
await Deno.writeTextFile(`${fx.roots}/${fx.project}/docs/logo.svg`,
  `<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"></svg>\n`);
await Deno.writeTextFile(
  `${fx.roots}/${fx.project}/docs/index.md`,
  "# index\n\n![local](shot.png)\n\n![remote](https://example.invalid/x.png)\n\n" +
    "[to other](other.md)\n\n[danger](javascript:window.__xss=1)\n" +
    // Appended AFTER [to other], never before: assertion 3 clicks the *first*
    // a.mdlink in the preview, so a new local link above it would silently
    // retarget that test at a different file.
    "\n[to running](long.md#running)\n",
);
// Long enough that the target heading is far below the fold. Without the
// filler the pane never scrolls, scrollTop stays 0 whatever the code does, and
// the anchor assertions below would pass with the whole feature deleted.
const FILLER = Array.from({ length: 120 }, (_, i) => `filler line ${i}`).join("\n\n");
await Deno.writeTextFile(
  `${fx.roots}/${fx.project}/docs/long.md`,
  `# Long\n\n[jump to running](#running)\n\n${FILLER}\n\n` +
    "## Running\n\nthe running section\n\n## Files & folders\n\nx\n\n" +
    "## Notes\n\na\n\n## Notes\n\nb\n",
);

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { evalIn } = page;
  await until(() => evalIn("ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");

  const urlBefore = await evalIn("location.href");
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "docs/index.md", mode: "Preview" } })`);
  await until(() => evalIn(`!!document.querySelector(".markdown-body a.mdlink")`), 15, "preview");

  // ---- 1. A project image renders -----------------------------------------
  await until(() => evalIn(
    `(() => { const i = [...document.querySelectorAll(".markdown-body img")]
        .find(x => x.src.includes("raw?path=")); return !!i && i.complete; })()`), 15, "image load");
  ok(await evalIn(
    `[...document.querySelectorAll(".markdown-body img")]
       .find(x => x.src.includes("raw?path=")).naturalWidth === 1`),
    "a project image actually loaded its bytes");

  // ---- 2. The remote image is gone, its alt text is not --------------------
  ok(await evalIn(`!document.body.innerHTML.includes("example.invalid")`),
    "no request to a remote image host");
  ok(await evalIn(`document.querySelector(".markdown-body").textContent.includes("remote")`),
    "the dropped image left its alt text behind");

  // ---- 3. A link opens a tab and does NOT navigate -------------------------
  await evalIn(`document.querySelector(".markdown-body a.mdlink").click()`);
  // until() returns false on timeout rather than throwing — capture and
  // assert on it, or a dead click (no OpenTab sent at all) still prints ok.
  const opened = await until(() => evalIn(
    `state.panes.some(p => p.tabs.some(t => t.rel === "docs/other.md"))`), 15, "tab opened");
  ok(opened, "clicking a local link opened it as a tab");
  ok(await evalIn("location.href") === urlBefore,
    "and the workspace page did not navigate away");

  // ---- 3b. A link naming a heading lands ON that heading -------------------
  // The discriminating measurement is scrollTop, not tab identity: `[to
  // running](long.md#running)` opens long.md whether or not the fragment
  // survives, so an assertion that only checked the tab would pass with
  // link_open's data-hash and app.js's revealAnchor both deleted.
  const paneContent = `document.querySelector('.pane[data-pane="2"] .content')`;
  // Assertion 3 just navigated this pane to other.md, so index.md's preview —
  // and the link about to be clicked — is no longer mounted. Re-open it.
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "docs/index.md", mode: "Preview" } })`);
  await until(() => evalIn(
    `!!document.querySelector('.markdown-body a.mdlink[data-rel="docs/long.md"]')`), 15, "index preview back");
  await evalIn(`document.querySelector('.markdown-body a.mdlink[data-rel="docs/long.md"]').click()`);
  const landed = await until(() => evalIn(
    `(() => { const c = ${paneContent}; if (!c) return false;
       const h = c.querySelector("article.markdown-body #running");
       if (!h) return false;
       // Near the top of the pane, not merely present in the document.
       const d = h.getBoundingClientRect().top - c.getBoundingClientRect().top;
       return c.scrollTop > 0 && d >= -4 && d < 80; })()`), 15, "anchor landed");
  ok(landed, "a deploy.md#running-style link scrolls the preview to that heading");
  ok(await evalIn("location.href") === urlBefore,
    "and it still did not navigate the workspace");

  // ---- 3c. Heading ids are GitHub's slugs, not just any ids ----------------
  const ids = await evalIn(
    `JSON.stringify([...document.querySelectorAll("article.markdown-body h1[id],article.markdown-body h2[id]")]
       .map(h => h.id))`).then(JSON.parse);
  ok(ids.includes("running"), `a heading id is its slug (got ${JSON.stringify(ids)})`);
  // Two hyphens: GitHub strips the "&" and turns both surviving spaces into
  // hyphens without collapsing them. A single-hyphen result would mean a
  // tidier algorithm that disagrees with every link copied from GitHub.
  ok(ids.includes("files--folders"), `punctuation slugs GitHub's way (got ${JSON.stringify(ids)})`);
  ok(ids.includes("notes") && ids.includes("notes-1"),
    `a repeated heading gets a numbered id (got ${JSON.stringify(ids)})`);

  // ---- 3d. A same-document #anchor needs no JS at all ----------------------
  // It is a plain href, so this asserts the ids are reachable by the browser's
  // own anchor handling — a different code path from 3b entirely.
  await evalIn(`${paneContent}.scrollTop = 0`);
  const pathBefore = await evalIn("location.pathname");
  await evalIn(`document.querySelector('article.markdown-body a[href="#running"]').click()`);
  const nativeLanded = await until(() => evalIn(
    `(() => { const c = ${paneContent};
       return c && c.scrollTop > 0; })()`), 10, "native anchor scroll");
  ok(nativeLanded, "a same-document #anchor scrolls without any JS of ours");
  ok(await evalIn("location.pathname") === pathBefore,
    "and it changed only the fragment, not the workspace path");

  // ---- 4. An image tab shows a picture and offers no editor ----------------
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "docs/shot.png", mode: "Preview" } })`);
  await until(() => evalIn(`!!document.querySelector("img.imgview")`), 15, "image tab");
  ok(await evalIn(
    `(() => { const i = document.querySelector("img.imgview"); return i.complete && i.naturalWidth === 1; })()`),
    "an image tab shows the picture, not a binary-file error");
  ok(await evalIn(
    `![...document.querySelectorAll(".tabstrip .tab")]
       .some(b => b.textContent.includes("shot.png") && b.querySelector("span.x[title*='edit']"))`),
    "an image tab offers no edit toggle");

  // Step 3 switched pane 2's active tab to docs/other.md and Step 4 switched
  // it again to the image tab; re-open the markdown preview so its .mdlink
  // anchor exists in the DOM again before right-clicking it.
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "docs/index.md", mode: "Preview" } })`);
  await until(() => evalIn(`!!document.querySelector(".markdown-body a.mdlink")`), 15, "preview reopened");

  // ---- 5. Right-click on a markdown link opens the menu exactly once -------
  // wireFileLinks assigns oncontextmenu per anchor with no stopPropagation().
  // wireFragment's container handler is the only thing that stops a second
  // fileMenu() firing, and it does that by testing
  // e.target.closest("a[data-rel]") — a .mdlink anchor has no class="file",
  // so a guard written as closest("a.file") misses it and prompt() pops
  // twice, the second time with rel="" (the project root, with
  // create/rename/delete armed). fileMenu is prompt()-based and would
  // otherwise block this headless run forever, so stub prompt with a counter
  // instead of letting it show.
  await evalIn(`window.__realPrompt = window.prompt;
    window.__prompts = [];
    window.prompt = (msg) => { window.__prompts.push(msg); return null; };`);
  await evalIn(`(() => {
    const a = document.querySelector(".markdown-body a.mdlink");
    a.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
  })()`);
  const linkPrompts = await evalIn("window.__prompts.length");
  ok(linkPrompts === 1,
    `right-clicking a markdown link opened the file menu exactly once (got ${linkPrompts})`);

  // Blank space in the tree (not a row, not a link) must still reach the
  // project-root menu — the widened guard must not swallow that fallback.
  await evalIn(`window.__prompts = []`);
  await evalIn(`(() => {
    const ul = [...document.querySelectorAll("ul.tree")][0];
    ul.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
  })()`);
  const treePrompts = await evalIn("window.__prompts");
  ok(treePrompts.length === 1 && treePrompts[0].includes("(project root)"),
    `right-clicking blank tree space still opens the project-root menu: ${JSON.stringify(treePrompts)}`);

  // Restore the real prompt() — a landmine otherwise: any assertion appended
  // after this point would silently run against the stub instead of a real
  // dialog, with no signal that it was doing so.
  await evalIn(`window.prompt = window.__realPrompt;`);

  // ---- 6. A raw OpenTab{mode:"Edit"} on an image is coerced to Preview -----
  // NO_TEXT_EDIT_EXT only gates the client's own ✎ toggle; a handcrafted intent
  // skips the UI entirely and is the actual data-loss path — a textarea
  // mounting empty over a real PNG, whose save truncates the file to
  // whatever the empty textarea held. Sent over the real websocket, so this
  // exercises workspace.rs's server-side coercion, not a client illusion.
  //
  // Uses docs/raw-open.png, never opened earlier in this run — not
  // docs/shot.png. tab_identity_eq (workspace.rs) matches tabs on rel alone,
  // ignoring mode, so re-requesting an already-open rel would just reactivate
  // the existing (already-Preview) tab and never construct a new Tab::File
  // at all, which would let this assertion pass even with the coercion
  // deleted — caught by actually deleting it and watching this pass anyway
  // before this file used a fresh rel here (see task-6-report.md).
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "docs/raw-open.png", mode: "Edit" } })`);
  await until(() => evalIn(
    `state.panes.some(p => p.tabs.some(t => t.rel === "docs/raw-open.png"))`), 15, "image tab opened via Edit intent");
  const imgMode = await evalIn(
    `state.panes.flatMap(p => p.tabs).find(t => t.rel === "docs/raw-open.png").mode`);
  ok(imgMode === "Preview",
    `an OpenTab intent requesting mode:"Edit" on a never-before-opened image was coerced to Preview (got ${imgMode})`);
  await until(() => evalIn(
    `(() => { const i = document.querySelector("img.imgview"); return !!i && i.complete; })()`), 15, "image tab re-rendered");
  ok(await evalIn(
    `(() => { const i = document.querySelector("img.imgview"); return !!i && i.naturalWidth === 1; })()`),
    "the pane rendered the picture (bytes loaded), not a textarea, after the coerced open");
  // ---- 7. An SVG is text: it keeps the Edit switch and really opens -------
  // A .svg previews as a picture like any image, but it is text on disk, so
  // gating Edit on "renders as a picture" silently made every SVG in every
  // project read-only — with no switch left to get back out of a tab already
  // in Edit. Driven through the real switch in the filename stripe (where the
  // old per-tab ✎ moved on 2026-08-24), not a handcrafted intent, so this
  // covers the client list and the server guard together.
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "docs/logo.svg", mode: "Preview" } })`);
  await until(() => evalIn(
    `(() => { const c = document.querySelector('.pane[data-pane="2"] .content');
       return !!c && c.textContent.includes("logo.svg") && !!c.querySelector(".path .modebtn"); })()`),
    15, "svg preview with its mode switch");
  const svgToggle = await evalIn(
    `(() => { const b = document.querySelector('.pane[data-pane="2"] .content .path .modebtn');
      if (b) b.click();
      return b ? b.title : false; })()`);
  ok(svgToggle === "switch to edit", "an svg preview's stripe offers the switch to edit");
  const svgEdits = await until(() => evalIn(
    `(() => { const t = document.querySelector("textarea");
       return !!t && t.value.includes("<svg"); })()`), 15, "svg editor");
  ok(svgEdits, "clicking it mounts a textarea holding the svg's real text");
  // The switch back must be VISIBLE in the edit stripe. It once carried the
  // .savebtn class, so paintSaveState's querySelector(".savebtn") found it
  // first and hid the toggle instead of managing Save (autosave on hides
  // Save on a clean buffer — the wrong element vanished).
  ok(await until(() => evalIn(
    `(() => { const b = document.querySelector('.pane[data-pane="2"] .content .path .modebtn');
       return !!b && !b.hidden && b.title === "switch to preview"; })()`), 10, "the preview switch"),
     "and the edit stripe offers the visible switch back to the picture");

  // ---- 8. A javascript: link cannot run --------------------------------
  // One click on a cloned repo's README, in the origin that drives every
  // terminal websocket. Clicking for real, not just grepping the HTML: an
  // href the browser would execute is the only thing that matters here, and
  // window.__xss is set by the payload itself if it ever runs.
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "docs/index.md", mode: "Preview" } })`);
  await until(() => evalIn(`!!document.querySelector(".markdown-body a.mdlink")`), 15, "preview reopened");
  ok(await evalIn(
    `![...document.querySelectorAll(".markdown-body a")]
       .some(a => (a.getAttribute("href") || "").toLowerCase().startsWith("javascript"))`),
    "no javascript: href reached the page");
  const urlBeforeClick = await evalIn("location.href");
  // CONTROL: prove this browser really does execute a javascript: href on a
  // synthetic click, so the assertion below means "it was refused" and not
  // "clicks do not run javascript: URLs here anyway".
  await evalIn(`(() => { const a = document.createElement("a");
      a.href = "javascript:window.__ctrl=1"; a.textContent = "c";
      document.body.appendChild(a); a.click(); })()`);
  const ctrl_ran = await until(() => evalIn(`window.__ctrl === 1`), 3, "control javascript: href");
  ok(ctrl_ran, "CONTROL: a javascript: href does execute on click in this browser");
  await evalIn(`(() => { const a = [...document.querySelectorAll(".markdown-body a")]
      .find(x => x.textContent === "danger"); if (a) a.click(); return !!a; })()`);
  // A javascript: URL is queued as a navigation, not run synchronously by
  // click(), so reading window.__xss on the next line passes even when the
  // href really did execute — verified by putting "javascript" back on
  // HREF_SCHEMES and watching the immediate form print ok. Poll instead, and
  // require that it never appears (no label: the timeout is the pass).
  //
  // What this assertion does and does not catch: the exact pre-fix output
  // routed javascript: through the Remote arm, which adds target="_blank",
  // and Chromium blocks that as a popup — so this line printed ok even with
  // the hole wide open. The href assertion above is what catches that
  // variant; this one catches the plain-href variant (javascript: classified
  // as Passthrough), verified to fail. Both are kept for that reason.
  const ran = await until(() => evalIn(`typeof window.__xss !== "undefined"`), 2);
  ok(!ran, "clicking it executed nothing");
  ok(await evalIn("location.href") === urlBeforeClick,
    "and it did not navigate the workspace");
} finally {
  try { await page?.close?.(); } catch {}
  try { browser.close(); } catch {}
  try { await roost.close(); } catch {}
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
