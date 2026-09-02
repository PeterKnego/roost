//! Cmd/Ctrl-S saves the open file even when the editor is not focused.
//!
//! Reported twice by hand, both times after a page reload (a deploy, a
//! reconnect) had left focus on the body: the keystroke went to Chrome's own
//! save dialog and never reached the app at all. The handler used to be bound
//! to the textarea, so it only existed while the textarea had focus.
//!
//! No Rust test can see this — the binding lives entirely in static/app.js —
//! and no server-side test can either: from the server's side a save that was
//! never sent and a save that was never typed look identical.
//!
//! The discriminating detail is the focus assertion before each keystroke. Skip
//! it and the test passes against the old textarea-bound handler, because a
//! freshly mounted editor happens to be focused.
//!
//! Run: deno run -A tests/browser/save.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const file = `${fx.roots}/proj/note.md`;
await Deno.writeTextFile(file, "on disk\n");
// Nested, so the breadcrumb below has a directory to show that the tab —
// which carries the basename alone — cannot.
const NESTED = "docs/deep/nested.md";
await Deno.mkdir(`${fx.roots}/proj/docs/deep`, { recursive: true });
await Deno.writeTextFile(`${fx.roots}/proj/${NESTED}`, "nested\n");

const roost = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/proj`);
  const { cmd, evalIn } = page;
  // The default headless window is 800x600, which is narrower than the left
  // (260px) and right (520px) panes together: the middle pane — the one every
  // assertion below looks at — collapses to nothing, and the layout assertion
  // in section 4 measures that instead of the editor. See the README's traps.
  await cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn(`typeof state !== "undefined" && !!(state && state.panes)`), 15, "workspace state");

  // Setup only — opening a tab is not what this file is testing.
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "note.md", mode: "Edit" } })`);
  ok(await until(() => evalIn(`!!document.querySelector("textarea.editor")`), 10, "editor"),
     "the file opens in an editor");

  // A real keystroke, not send(): a shortcut wired to nothing is the defect.
  // rawKeyDown with no `text` — a keyDown carrying text would type an "s"
  // into the buffer alongside the shortcut.
  const press = async (modifiers) => {
    for (const type of ["rawKeyDown", "keyUp"]) {
      await cmd("Input.dispatchKeyEvent",
        { type, modifiers, key: "s", code: "KeyS", windowsVirtualKeyCode: 83, nativeVirtualKeyCode: 83 });
    }
    await sleep(300);
  };
  const type = async (text) => {
    await evalIn(`document.querySelector("textarea.editor").focus()`);
    await cmd("Input.insertText", { text });
  };
  const onDisk = () => Deno.readTextFile(file);

  // --- 1. Ctrl-S with focus outside the editor: the reported case.
  await type("ctrl edit\n");
  await evalIn(`document.activeElement.blur(); document.body.focus()`);
  ok(await evalIn(`document.activeElement.tagName !== "TEXTAREA"`),
     "focus is outside the editor (without this the test proves nothing)");
  await press(2); // Ctrl
  ok(await until(async () => (await onDisk()).includes("ctrl edit"), 5, "ctrl-s save"),
     "ctrl-s saves while focus is on the body");

  // --- 2. Cmd-S, the same way: macOS sends Meta, and the reports came from it.
  await type("meta edit\n");
  await evalIn(`document.activeElement.blur(); document.body.focus()`);
  ok(await evalIn(`document.activeElement.tagName !== "TEXTAREA"`),
     "focus is outside the editor for the meta case too");
  await press(4); // Meta
  ok(await until(async () => (await onDisk()).includes("meta edit"), 5, "cmd-s save"),
     "cmd-s saves while focus is on the body");

  // --- 3. The focused editor must keep working — the path that already did.
  await type("focused edit\n");
  ok(await evalIn(`document.activeElement.tagName === "TEXTAREA"`), "focus is back in the editor");
  await press(2);
  ok(await until(async () => (await onDisk()).includes("focused edit"), 5, "focused save"),
     "ctrl-s still saves from inside the editor");

  // --- 4. The editor carries the breadcrumb a preview does, so a file looks
  // like the same file in both modes.
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: ${JSON.stringify(NESTED)}, mode: "Edit" } })`);
  ok(await until(async () => (await evalIn(
    `(document.querySelector('.pane[data-pane="2"] .editwrap .path .rel') || {}).textContent || ""`)) === NESTED,
    10, "the breadcrumb"), `the editor names the file by its full path (${NESTED})`);
  // The half the breadcrumb can break: .editor is height:100%, so without the
  // flex wrapper above it the textarea keeps the pane's full height, starts
  // below the breadcrumb, and pushes its own last lines out through the
  // bottom (measured: 26px past, with .content grown a scrollbar).
  const box = JSON.parse(await evalIn(`JSON.stringify((() => {
    const c = document.querySelector('.pane[data-pane="2"] .content');
    const p = c.querySelector('.path'), t = c.querySelector("textarea.editor");
    const b = (e) => e.getBoundingClientRect();
    return { over: Math.round(b(t).bottom - b(c).bottom), gap: Math.round(b(t).top - b(p).bottom) };
  })())`));
  // Both ends: overshooting hides the last lines, undershooting would mean the
  // wrapper had swallowed the height instead of sharing it.
  ok(box.over === 0 && box.gap === 0,
     `and the textarea fills exactly the space under it (${box.over}px past the pane, ${box.gap}px gap)`);
  ok(await evalIn(`(() => { const b = document.querySelector('.pane[data-pane="2"] .savebtn');
       return !!b && b.hidden; })()`),
     "with autosave on there is no Save button, only the state");

  // --- 5. And the key is only taken when there is something to save, or the
  // workspace would swallow the browser's own shortcut on every other tab.
  await evalIn(`send({ t: "SetMode", rel: ${JSON.stringify(NESTED)}, mode: "Preview" })`);
  await evalIn(`send({ t: "SetMode", rel: "note.md", mode: "Preview" })`);
  await until(() => evalIn(`!document.querySelector("textarea.editor")`), 5, "preview");
  ok(await evalIn(`saveTarget() === null`),
     "with no editor open the keystroke is left to the browser");

  // --- 6. A second browser onto the same workspace sees the file, not a
  // blank editor. Its text comes from disk now that a clean buffer holds
  // none, so this is the assertion that the disk read happens at all.
  //
  // Section 5 left every editor in Preview, so note.md is reopened in Edit
  // here first — `open_buffer_for` re-reads the file and resets its buffer to
  // Content::Clean (see hub.rs), which is exactly the case the second
  // browser's connect-time replay has to serve from disk rather than from
  // an in-memory edit.
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "note.md", mode: "Edit" } })`);
  await evalIn(`send({ t: "SetMode", rel: "note.md", mode: "Edit" })`);
  ok(await until(async () =>
    ((await evalIn(`(document.querySelector("textarea.editor") || {}).value`)) || "").includes("focused edit"),
    10, "note.md reopened in Edit"),
    "note.md is back in Edit mode with its saved text, ready for the second browser");

  const second = await openPage(browser.port, `http://127.0.0.1:${roost.port}/proj`);
  try {
    // Same override as the first page and for the same reason (see the
    // README's traps): the default 800x600 window collapses the middle
    // pane, and this assertion depends on that pane's editor existing.
    await second.cmd("Emulation.setDeviceMetricsOverride",
      { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
    await until(() => second.evalIn(`typeof state !== "undefined" && !!(state && state.panes)`), 15, "state");
    ok(await until(async () =>
      ((await second.evalIn(`(document.querySelector("textarea.editor") || {}).value`)) || "").includes("focused edit"),
      10, "the second browser's editor"),
      "a second browser onto an open editor is served its text");
  } finally { second.close(); }
} finally {
  try { page?.close(); } catch {}
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
