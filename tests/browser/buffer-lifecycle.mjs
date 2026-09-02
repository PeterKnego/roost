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
import { disableAutosave, fixture, freePort, openPage, profileDir, sleep, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const file = `${fx.roots}/proj/watched.rs`;
await Deno.writeTextFile(file, "fn main() {}\n");

// A second project, autosave off, for section C alone (same trick
// autosave.mjs uses for its "noauto" fixture). Sections A and B stay on the
// autosave-on project deliberately: A's "nothing was written" is only
// meaningful because autosave had the full 1s window to fire and still
// didn't, and B follows it in the same buffer. C is different — do_save
// resets *any* successfully-saved buffer to Content::Clean regardless of why
// it was dirty, so with autosave on, a long-enough wait after the undo lets
// autosave itself clean up a wrongly-dirtied buffer and mask a broken hash
// rule. Turning autosave off for C removes the race instead of narrowing a
// window to dodge it — nothing but the hash rule can make that buffer clean.
await disableAutosave(`${fx.roots}/noauto`);
const file2 = `${fx.roots}/noauto/watched.rs`;
await Deno.writeTextFile(file2, "fn main() {}\n");

const roost = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page, page2;

// Opens `rel` in an editor tab on `project` and returns real-key-event
// helpers bound to that page. Not send(): the whole point of this file is
// exercising the client's own event listeners, not the server directly.
const openEditor = async (project, rel) => {
  const p = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${project}`);
  const { cmd, evalIn } = p;
  // The default headless window is 800x600, narrower than the left (260px)
  // and right (520px) panes together, which collapses the middle pane this
  // whole test depends on — see the README's traps.
  await cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn(`typeof state !== "undefined" && !!(state && state.panes)`), 15, "workspace state");
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: ${JSON.stringify(rel)}, mode: "Edit" } })`);
  ok(await until(() => evalIn(`!!document.querySelector("textarea.editor")`), 10, "editor"),
     `${project}/${rel} opens in an editor`);
  return {
    ...p,
    press: async (modifiers) => {
      for (const t of ["rawKeyDown", "keyUp"]) {
        await cmd("Input.dispatchKeyEvent",
          { type: t, modifiers, key: "s", code: "KeyS", windowsVirtualKeyCode: 83, nativeVirtualKeyCode: 83 });
      }
      await sleep(300);
    },
    type: async (text) => {
      await evalIn(`document.querySelector("textarea.editor").focus()`);
      await cmd("Input.insertText", { text });
    },
    dirty: () => evalIn(`!!(state.buffers.find((b) => b.rel === ${JSON.stringify(rel)}) || {}).dirty`),
  };
};

try {
  page = await openEditor("proj", "watched.rs");
  const { cmd, press, dirty } = page;

  const mtimeBefore = (await Deno.stat(file)).mtime.getTime();

  console.log("A. navigating does not make a file dirty");
  // Real key events, not send(): what this is testing is which browser event
  // the client listens on. The input event fires only when the value changes,
  // so none of these must reach EditBuffer.
  for (const [key, code, vk] of [["ArrowDown","ArrowDown",40], ["End","End",35],
                                 ["PageDown","PageDown",34], ["ArrowRight","ArrowRight",39]]) {
    for (const t of ["rawKeyDown", "keyUp"]) {
      await cmd("Input.dispatchKeyEvent", { type: t, key, code, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk });
    }
  }
  await sleep(1500); // past the 1s autosave timer — autosave is ON in "proj"
  ok(!(await dirty()), "arrows, End and PageDown leave the buffer clean");
  ok((await Deno.stat(file)).mtime.getTime() === mtimeBefore, "and nothing was written");

  console.log("B. ⌘S on an untouched file writes nothing");
  await press(2); // ctrl
  await sleep(500); // margin past press()'s own 300ms for the round trip
  ok((await Deno.stat(file)).mtime.getTime() === mtimeBefore, "the file is untouched");

  console.log("C. typing a character and deleting it comes back clean");
  // The "noauto" project: see the header comment for why this section needs
  // autosave off rather than a narrow timing window.
  page2 = await openEditor("noauto", "watched.rs");
  // Proof the fixture took, before anything depends on it. Without this the
  // section is silently self-defeating: with autosave on, do_save cleans the
  // buffer for its own reasons and "comes back clean" passes with the hash rule
  // broken — the exact masking the header comment is about. Verified by
  // commenting the key out of `disableAutosave`: every other test that leans on
  // autosave being off failed, and this one stayed green.
  ok(await page2.evalIn(`AUTOSAVE === false`), "the noauto project really has autosave off");
  await page2.type("x");
  ok(await until(async () => await page2.dirty(), 5, "dirty"), "typing marks it dirty");
  await page2.cmd("Input.dispatchKeyEvent", { type: "rawKeyDown", key: "Backspace", code: "Backspace",
                                        windowsVirtualKeyCode: 8, nativeVirtualKeyCode: 8 });
  await page2.cmd("Input.dispatchKeyEvent", { type: "keyUp", key: "Backspace", code: "Backspace",
                                        windowsVirtualKeyCode: 8, nativeVirtualKeyCode: 8 });
  // A generous window like every other assertion in this suite: with
  // autosave off in this project, nothing but the hash rule can clean this
  // buffer up, so there is no timer to race and no reason to narrow it.
  ok(await until(async () => !(await page2.dirty()), 5, "clean again"), "undoing it comes back clean");
} finally {
  try { page?.close(); } catch {}
  try { page2?.close(); } catch {}
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
