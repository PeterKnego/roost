//! Do a preview's own references actually work in a browser?
//!
//! No Rust test reaches static/app.js, so the selector that turns <a data-rel>
//! into an OpenTab intent, the ✎ suppression, and whether an <img> element's
//! bytes ever arrived are all untested without this.
//!
//! naturalWidth, not presence: an <img> exists in the DOM whether or not the
//! request succeeded, so `querySelector("img") !== null` is one of the four
//! traps README.md warns about — it passes with the route deleted.
//!
//! Revert-the-fix, both watched fail for real and then restored:
//!   1. Commented out the ["raw"] arm in serve_frag (src/routes.rs) so
//!      GET /frag/<project>/raw always 404s. Assertion 1 failed with:
//!        FAIL  a project image actually loaded its bytes
//!      (naturalWidth was 0, not 1 — the <img> tag still existed, only its
//!      bytes never arrived, which is exactly the trap this file's own
//!      header comment warns about).
//!   2. Restored the pre-fix `if (t.k === "File")` tab-icon gate at
//!      static/app.js (dropping `&& !isImage(t.rel)`), so an image tab grew
//!      the ✎ toggle again. The last assertion failed with:
//!        FAIL  an image tab offers no edit toggle
//!   3. Widened the double-contextmenu guard in wireFragment back down to
//!      `e.target.closest("a.file")` (its pre-fix state; a `.mdlink` anchor
//!      has no `file` class). The new assertion failed with:
//!        FAIL  right-clicking a markdown link opened the file menu exactly
//!        once (got 2)
//! All three restored afterwards; `deno run -A tests/browser/mdlinks.mjs`
//! passes clean again (see task-6-report.md for the exact terminal output).
//!
//! Run: deno run -A tests/browser/mdlinks.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until }
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
await Deno.writeTextFile(`${fx.roots}/${fx.project}/docs/other.md`, "# other\n");
await Deno.writeTextFile(
  `${fx.roots}/${fx.project}/docs/index.md`,
  "# index\n\n![local](shot.png)\n\n![remote](https://example.invalid/x.png)\n\n[to other](other.md)\n",
);

const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`);
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
  await until(() => evalIn(
    `state.panes.some(p => p.tabs.some(t => t.rel === "docs/other.md"))`), 15, "tab opened");
  ok(true, "clicking a local link opened it as a tab");
  ok(await evalIn("location.href") === urlBefore,
    "and the workspace page did not navigate away");

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
  await evalIn(`window.__prompts = [];
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

  // ---- 6. A raw OpenTab{mode:"Edit"} on an image is coerced to Preview -----
  // IMAGE_EXT only gates the client's own ✎ toggle; a handcrafted intent
  // skips the UI entirely and is the actual data-loss path — a textarea
  // mounting empty over a real PNG, whose save truncates the file to
  // whatever the empty textarea held. Sent over the real websocket, so this
  // exercises workspace.rs's server-side coercion, not a client illusion.
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "docs/shot.png", mode: "Edit" } })`);
  await until(() => evalIn(
    `state.panes.some(p => p.tabs.some(t => t.rel === "docs/shot.png"))`), 15, "image tab opened via Edit intent");
  const imgMode = await evalIn(
    `state.panes.flatMap(p => p.tabs).find(t => t.rel === "docs/shot.png").mode`);
  ok(imgMode === "Preview",
    `an OpenTab intent requesting mode:"Edit" on an image was coerced to Preview (got ${imgMode})`);
  await until(() => evalIn(`!!document.querySelector("img.imgview")`), 15, "image tab re-rendered");
  ok(await evalIn(`!!document.querySelector("img.imgview")`),
    "the pane rendered the picture, not a textarea, after the coerced open");
} finally {
  try { await page?.close?.(); } catch {}
  try { browser.close(); } catch {}
  try { await resh.close(); } catch {}
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
