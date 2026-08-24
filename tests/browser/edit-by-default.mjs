//! Clicking a text file opens it in an editor, the way an IDE does.
//!
//! Preview used to be the default for everything, so reading code meant one
//! click and editing it meant two — and the second was a toggle most people never
//! found. Preview survives only where it is not a stand-in for an editor:
//! markdown, whose rendered form is the point of the file, and the formats a
//! textarea would destroy.
//!
//! What no Rust test can see: the default lives in the tree's click handler in
//! static/app.js, and the Edit/Preview switch in the filename stripe too.
//!
//! Run: deno run -A tests/browser/edit-by-default.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
await Deno.writeTextFile(`${fx.roots}/proj/main.rs`, "fn main() {}\n");
await Deno.writeTextFile(`${fx.roots}/proj/notes.md`, "# heading\n");
// Not on NO_TEXT_EDIT_EXT, so nothing but the *read* can save this one — it is
// the server's fallback under test, not an extension list.
await Deno.writeFile(`${fx.roots}/proj/blob.bin`, new Uint8Array([0x61, 0x00, 0x62]));
await Deno.writeTextFile(`${fx.roots}/proj/logo.svg`, '<svg xmlns="http://www.w3.org/2000/svg"/>\n');

const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn(`typeof state !== "undefined" && !!(state && state.panes)`), 15, "workspace state");

  // Through the tree's own click handler, not by sending an intent: the
  // default being tested lives in that handler, and a test that sent
  // OpenTab{mode} itself would be asserting its own argument.
  const clickInTree = async (name) => {
    // Wait for the row rather than assuming it: the tree pane is a fetched
    // fragment, and on a loaded host the click can arrive before it has
    // rendered. Failing here as "no tree row" told us nothing about the
    // default mode, which is what this file is for.
    const row = `[...document.querySelectorAll('.pane[data-pane="0"] .content a.file')]
      .some((x) => x.dataset.rel === ${JSON.stringify(name)})`;
    if (!await until(() => evalIn(row), 10, `the tree row for ${name}`)) {
      throw new Error(`tree never listed ${name}`);
    }
    const clicked = await evalIn(`(() => {
      const a = [...document.querySelectorAll('.pane[data-pane="0"] .content a.file')]
        .find((x) => x.dataset.rel === ${JSON.stringify(name)});
      if (!a) return false;
      a.dispatchEvent(new MouseEvent("click", { bubbles: true })); return true;
    })()`);
    if (!clicked) throw new Error(`no tree row for ${name}`);
    return await until(async () => (await tabFor(name)) !== null, 10, `a tab for ${name}`);
  };
  const tabFor = (rel) => evalIn(
    `JSON.stringify(state.panes[2].tabs.find((t) => t.k === "File" && t.rel === ${JSON.stringify(rel)}) || null)`)
    .then((s) => JSON.parse(s));
  // The Edit/Preview switch lives in the filename stripe under the tabs (it
  // moved out of the tab on 2026-08-24), so it exists only for the active
  // tab — which each clickInTree above has just made this file. Guarding on
  // the stripe naming the right file keeps this from passing on some other
  // tab's button. Returns the button's label, or false.
  const modeBtnFor = (rel) => evalIn(`(() => {
    const c = document.querySelector('.pane[data-pane="2"] .content');
    if (!c || !c.textContent.includes(${JSON.stringify(rel)})) return "wrong tab";
    const b = c.querySelector(".path .modebtn");
    return b ? b.textContent : false;
  })()`);

  console.log("A. a code file opens in its editor");
  ok(await clickInTree("main.rs"), "clicking main.rs in the tree opens a tab");
  ok((await tabFor("main.rs")).mode === "Edit", `and it is in Edit (got ${(await tabFor("main.rs")).mode})`);
  ok(await until(() => evalIn(`!!document.querySelector('.pane[data-pane="2"] textarea.editor')`), 10, "editor"),
     "the pane really shows an editor, not a preview");
  ok((await modeBtnFor("main.rs")) === false, "and its stripe offers no mode switch — there is no second mode to reach");

  console.log("\nB. markdown keeps its rendered preview, and its toggle");
  ok(await clickInTree("notes.md"), "clicking notes.md opens a tab");
  ok((await tabFor("notes.md")).mode === "Preview", `and it is in Preview (got ${(await tabFor("notes.md")).mode})`);
  ok(await until(async () => (await modeBtnFor("notes.md")) === "Edit", 5, "the Edit switch"),
     "the preview's filename stripe offers the Edit switch");

  console.log("\nC. a file the editor cannot hold lands in Preview by itself");
  // The client asks for Edit — blob.bin's extension is not on any list it
  // consults. Only the server can know, and it moves the tab back.
  ok(await clickInTree("blob.bin"), "clicking blob.bin opens a tab");
  ok(await until(async () => (await tabFor("blob.bin")).mode === "Preview", 10, "the demotion"),
     "the server moved it out of Edit once the read failed");
  ok(!(await evalIn(`!!document.querySelector('.pane[data-pane="2"] textarea.editor')`)),
     "so the user is not left typing into an empty box over a binary");
  ok((await evalIn(`document.querySelectorAll('.error-banner, .conflict').length`)) === 0,
     "and no banner, because nobody asked for Edit — the click just defaulted there");
  console.log("\nD. svg has both a picture and text, and keeps both modes");
  // The regression this exists to stop: an svg draws like an image *and* is
  // text, and it has been editable since before image tabs existed. A rule
  // that hands the ✎ to markdown alone silently takes that away, which is the
  // shape CLAUDE.md records having shipped once already.
  ok(await clickInTree("logo.svg"), "clicking logo.svg opens a tab");
  ok((await tabFor("logo.svg")).mode === "Preview",
     `it opens on the picture (got ${(await tabFor("logo.svg")).mode})`);
  ok(await until(async () => (await modeBtnFor("logo.svg")) === "Edit", 5, "the Edit switch"),
     "and its stripe still offers the switch to its text");

} finally {
  page?.close();
  browser.close();
  await resh.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
