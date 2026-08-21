//! Moving around inside a file is not an edit.
//!
//! `input` fires on a textarea only when its value changes, so arrows, End,
//! PageDown and friends never reach `EditBuffer` at all — that is a property
//! of which DOM event the client listens on, and no Rust test can see it
//! because Rust never runs the browser's event dispatch. Real CDP key events
//! are used rather than `send()` for exactly that reason: `send()` would talk
//! to the server directly and prove nothing about the client's listener.
//!
//! The two paths that *do* reach the server on an unchanged file — ⌘S, and a
//! character typed then deleted — must be no-ops there too. That is the hash
//! rule in `Buffer::set_text` (workspace.rs): the buffer is only `Edited` when
//! the text actually differs from the base, so a save that changes nothing
//! writes nothing, and undoing an edit lands back on `Content::Clean` rather
//! than `Content::Edited(original_text)`.
//!
//! Run: deno run -A tests/browser/buffer-lifecycle.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const file = `${fx.roots}/proj/watched.rs`;
await Deno.writeTextFile(file, "fn main() {}\n");

const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/proj`);
  const { cmd, evalIn } = page;
  // The default headless window is 800x600, narrower than the left (260px)
  // and right (520px) panes together, which collapses the middle pane this
  // whole test depends on — see the README's traps.
  await cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn(`typeof state !== "undefined" && !!(state && state.panes)`), 15, "workspace state");

  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "watched.rs", mode: "Edit" } })`);
  ok(await until(() => evalIn(`!!document.querySelector("textarea.editor")`), 10, "editor"),
     "the file opens in an editor");

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
  const dirty = () => evalIn(`!!(state.buffers.find((b) => b.rel === "watched.rs") || {}).dirty`);

  const mtimeBefore = (await Deno.stat(file)).mtime.getTime();

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
  // Deliberately well under the 1s AUTOSAVE_MS, not the brief's original 5s:
  // do_save resets a buffer to Content::Clean on *any* successful write, so a
  // 5s window lets autosave fire and clean the buffer up on its own, masking
  // a broken hash rule the same way the pause-deletion trap masks itself in
  // autosave.mjs. Only a window that ends before autosave can have fired
  // proves the hash rule itself, not the write that comes after it.
  ok(await until(async () => !(await dirty()), 0.7, "clean again"), "undoing it comes back clean");
} finally {
  try { page?.close(); } catch {}
  browser.close();
  await resh.close();
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
