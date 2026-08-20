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

const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/proj`);
  const { cmd, evalIn } = page;
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

  // --- 4. And the key is only taken when there is something to save, or the
  // workspace would swallow the browser's own shortcut on every other tab.
  await evalIn(`send({ t: "SetMode", rel: "note.md", mode: "Preview" })`);
  await until(() => evalIn(`!document.querySelector("textarea.editor")`), 5, "preview");
  ok(await evalIn(`saveTarget() === null`),
     "with no editor open the keystroke is left to the browser");
} finally {
  try { page?.close(); } catch {}
  browser.close();
  await resh.close();
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
