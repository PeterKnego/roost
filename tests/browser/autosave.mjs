//! Autosave: the editor writes a buffer out on its own, and says so.
//!
//! Every assertion here is about something no Rust test can see. The server
//! cannot tell an autosave from a ⌘S — both arrive as the same intent — so
//! "did a write happen without anyone pressing anything" is only answerable
//! from a browser, against a real file on disk.
//!
//! Two separate properties guard a concurrent writer, and the difference
//! matters when reading these assertions. That the file is *not overwritten*
//! comes from the server: autosave sends `force: false`, so the conflict guard
//! declines the write. Removing the client's pause does not break that — it
//! was verified, and every "still not overwritten" assertion here passed with
//! the pause deleted, which is why they are not the test of it.
//!
//! What the pause does is stop a timer from re-raising a conflict banner the
//! user never asked for, every second, forever. So the banner *count* is what
//! this file asserts on: zero from autosave, one from an explicit ⌘S.
//!
//! Run: deno run -A tests/browser/autosave.mjs
import { disableAutosave, fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const file = `${fx.roots}/proj/note.md`;
await Deno.writeTextFile(file, "start\n");

// A second project that turns autosave off, so the config cascade is exercised
// end to end rather than only in config.rs's own tests. Same server, because
// settings are re-read per request and never cached.
await disableAutosave(`${fx.roots}/noauto`);
const manualFile = `${fx.roots}/noauto/manual.md`;
await Deno.writeTextFile(manualFile, "start\n");
// Its own file, so section F's forced race cannot disturb the buffer D3 is
// still asserting about.
const slowFile = `${fx.roots}/proj/slowstate.md`;
await Deno.writeTextFile(slowFile, "start\n");

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page, manualPage;

// One page's editor: open a file, type into it, read its header state.
const wire = async (project, rel) => {
  const p = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${project}`);
  await until(() => p.evalIn(`typeof state !== "undefined" && !!(state && state.panes)`), 15, "state");
  await p.evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: ${JSON.stringify(rel)}, mode: "Edit" } })`);
  await until(() => p.evalIn(`!!document.querySelector("textarea.editor")`), 10, "editor");
  // A textarea existing is not the same as the file being in it. Entering Edit
  // makes the server read the file and push a BufferText with an empty origin,
  // and that push lands whenever it lands — on a loaded host, possibly after
  // the caller has already typed, in which case it overwrites what they typed
  // with the file's original content and every later assertion waits on an
  // edit that no longer exists. Measured: the editor came back holding its
  // original bytes, in a run where the keystrokes had demonstrably arrived.
  // `texts` is written by that same handler, so agreement between the two is
  // the signal that the push has been applied.
  await until(() => p.evalIn(
    `texts.has(${JSON.stringify(rel)}) && (document.querySelector("textarea.editor") || {}).value === texts.get(${JSON.stringify(rel)})`),
    10, "the editor to settle on the file");
  return {
    ...p,
    /// Types into the mounted editor, and confirms the keystrokes arrived.
    ///
    /// The confirmation is not ceremony. `Input.insertText` requires the
    /// textarea to be focused, and focusing it is a *separate* CDP round
    /// trip: on a loaded host the insert can land before the focus does, and
    /// the text then goes nowhere at all. Every assertion downstream — the
    /// file on disk, the dirty flag, the header — is then waiting on an edit
    /// that was never made, which reads as a slow autosave and was written
    /// off as one for a long time. Measured: the editor still held its
    /// original 6 bytes in every failing run.
    type: async (text) => {
      for (let i = 0; i < 3; i++) {
        await p.evalIn(`document.querySelector("textarea.editor").focus()`);
        await p.cmd("Input.insertText", { text });
        if (await until(async () =>
          ((await p.evalIn(`(document.querySelector("textarea.editor") || {}).value`)) || "").includes(text.trim()),
          3, i ? `the keystrokes to land (retry ${i})` : null)) return;
      }
      throw new Error(`typing never reached the editor: ${JSON.stringify(text)}`);
    },
    blur: () => p.evalIn(`document.activeElement.blur(); document.body.focus()`),
    dirty: () => p.evalIn(`!!(state.buffers.find((b) => b.rel === ${JSON.stringify(rel)}) || {}).dirty`),
    banners: () => p.evalIn(`document.querySelectorAll('.pane[data-pane="2"] .conflict').length`),
    editorText: () => p.evalIn(`(document.querySelector("textarea.editor") || {}).value`),
    discard: () => p.evalIn(
      `(() => { const b = [...document.querySelectorAll('.conflict button')]
          .find((x) => /discard/i.test(x.textContent)); if (!b) return false; b.click(); return true; })()`),
    savestate: () => p.evalIn(`(document.querySelector('.pane[data-pane="2"] .savestate') || {}).textContent || ""`),
    saveButtonShown: () => p.evalIn(
      `(() => { const b = document.querySelector('.pane[data-pane="2"] .savebtn'); return !!b && !b.hidden; })()`),
    clickSave: () => p.evalIn(
      `(() => { const b = document.querySelector('.pane[data-pane="2"] .savebtn');
         if (!b || b.hidden) return false; b.click(); return true; })()`),
    press: async (modifiers) => {
      for (const type of ["rawKeyDown", "keyUp"]) {
        await p.cmd("Input.dispatchKeyEvent",
          { type, modifiers, key: "s", code: "KeyS", windowsVirtualKeyCode: 83, nativeVirtualKeyCode: 83 });
      }
      await sleep(300);
    },
  };
};

try {
  page = await wire("proj", "note.md");

  console.log("\nA. a pause in typing is enough");
  await page.type("typed with no keystroke to save it\n");
  ok(await until(async () => (await Deno.readTextFile(file)).includes("no keystroke"), 5, "autosave"),
     "the file is written with nothing pressed");
  ok(await until(async () => (await page.savestate()) === "saved", 3, "saved state"),
     `the header says so (got ${JSON.stringify(await page.savestate())})`);

  console.log("\nB. leaving the editor does not wait for the timer");
  await page.type("flushed on blur\n");
  await page.blur();
  // Deliberately shorter than AUTOSAVE_MS: if this passes on the delay alone
  // rather than on the blur, the assertion proves nothing.
  ok(await until(async () => (await Deno.readTextFile(file)).includes("flushed on blur"), 0.5, "blur flush"),
     "blur writes immediately, well inside the 1s delay");

  console.log("\nC. autosave never wins a race with another writer");
  await page.type("mine\n");
  // Wait for the server to have the edit before diverging the file, so the
  // buffer is dirty when the external write lands — that is the only state in
  // which a conflict is possible at all. ~200ms (the EditBuffer debounce),
  // leaving most of the 1s autosave delay as margin.
  ok(await until(() => page.dirty(), 3, "dirty"), "the buffer is dirty before the file diverges");
  await Deno.writeTextFile(file, "written by somebody else\n");
  await sleep(2500); // longer than the autosave delay: it must fire and decline
  // The server's guard, asserted end to end. Not the test of the pause — this
  // holds with the pause deleted too, because the intent carries force:false.
  ok((await Deno.readTextFile(file)) === "written by somebody else\n",
     "the other writer's content is still on disk");
  ok((await page.savestate()).includes("changed on disk"),
     `the header says why nothing is being saved (got ${JSON.stringify(await page.savestate())})`);
  ok(await page.banners() === 0,
     `a save nobody asked for must not raise a banner (got ${await page.banners()})`);

  console.log("\nD. and it stays stopped until the person decides");
  await page.type("more typing\n");
  await sleep(2000);
  ok((await Deno.readTextFile(file)) === "written by somebody else\n", "still not overwritten");
  // The discriminating one: without the pause each pass of the timer raises
  // another banner, so this reads 1, then 2, then 3 as the test goes on.
  ok(await page.banners() === 0,
     `still no banner — autosave is not retrying the conflict (got ${await page.banners()})`);

  console.log("\nD2. but asking explicitly still shows the difference");
  await page.press(2); // ctrl-s: the person, not the timer
  ok(await until(async () => (await page.banners()) === 1, 3, "banner"),
     `⌘S raises the conflict banner exactly once (got ${await page.banners()})`);

  console.log("\nD3. discarding shows the file, not an empty editor");
  ok(await page.discard(), "the banner offers a discard button");
  ok(await until(async () => (await page.editorText()) === "written by somebody else\n", 5, "reload"),
     `the editor now shows what is on disk (got ${JSON.stringify(await page.editorText())})`);
  ok(await until(async () => (await page.savestate()) === "saved", 3, "saved"),
     "and the header agrees there is nothing outstanding");
  // The bug this replaced left no buffer at all, which only *looked* fine
  // until a reload — so reload and check the editor is not blank.
  const reloaded = await wire("proj", "note.md");
  try {
    ok(await until(async () => (await reloaded.editorText()) === "written by somebody else\n", 10, "reloaded"),
       `a fresh page shows the file too (got ${JSON.stringify(await reloaded.editorText())})`);
  } finally { try { reloaded.close(); } catch {} }
  // The header wording is one symptom; this is the behaviour behind it. A
  // buffer that stayed paused would never autosave again for the rest of the
  // session, and nothing else in this file would notice.
  await page.type("typing again after the discard\n");
  // 20s is generous rather than necessary. An earlier round read this
  // assertion's failures as a slow host and raised the budget from 5s, but
  // its own measurement said otherwise: still 1 failure in 12 at *30s*. A
  // write that has not landed in thirty seconds is not a slow write.
  //
  // The two real causes are fixed above, in `wire`. Under load the keystrokes
  // could miss the textarea entirely (`Input.insertText` needs focus, and
  // focusing is a separate round trip), or land and then be overwritten by
  // the initial BufferText the server pushes when a tab enters Edit. Either
  // way this waited on an edit that did not exist. With both closed, five
  // consecutive fully-saturated runs pass; the budget stays wide because
  // nothing is bought by tightening it.
  // A genuine failure to resume still fails here; it just has to be a real
  // one rather than a slow machine.
  ok(await until(async () => (await Deno.readTextFile(file)).includes("typing again"), 20, "resumed"),
     "autosave works again once the divergence is resolved");

  console.log("\nF. a slow State does not cost the edit that was typed under it");
  // The defect this pins: autosaveNow's third guard asks "is there anything to
  // save?" by looking at `pendingEdits` (cleared when the 200ms debounce
  // fires) and at the server's `dirty` flag (true only once its State has
  // arrived). Between those two moments the honest answer is "I cannot tell
  // yet" — and the guard read it as "nothing to do", returned, and left no
  // timer behind, so the edit was never written at all. Not late: never,
  // until the user happened to type again or blur the window.
  //
  // Forced rather than waited for. On an idle box the State beats the 1s
  // timer and the gap is invisible; on a loaded one it does not, which is why
  // this looked like a flake in D3 for a long time and why raising that
  // budget could not fix it — the write does not land late, it does not land.
  const slow = await wire("proj", "slowstate.md");
  try {
    await slow.evalIn(`(() => {
      const real = window.onEvent;
      window.onEvent = (ev) => {
        if (ev.t === "State") { setTimeout(() => real(ev), 1500); return; }
        real(ev);
      };
    })()`);
    await slow.type("typed under a slow State\n");
    ok(await until(async () => (await Deno.readTextFile(slowFile)).includes("typed under a slow State"), 15, "the write"),
       "an edit typed while State is in flight is still autosaved");
    // The buffer really was dirty and really did have no timer left: without
    // that, the assertion above could pass for the boring reason that the
    // race never happened in this run.
    ok(await slow.evalIn(`autosaveTimers.size === 0`),
       "and it did not simply get rescued by a still-pending timer");
  } finally { try { slow.close(); } catch {} }

  console.log("\nE. a project can turn it off");
  manualPage = await wire("noauto", "manual.md");
  await manualPage.type("not to be autosaved\n");
  await sleep(2000);
  ok((await Deno.readTextFile(manualFile)) === "start\n", "nothing is written on its own");
  ok((await manualPage.savestate()).includes("⌘S"),
     `and the breadcrumb advertises the shortcut (got ${JSON.stringify(await manualPage.savestate())})`);
  // The button exists only where it is the thing that writes the file. With
  // autosave on it is hidden — asserted in save.mjs, on the other config, so
  // neither half can pass by the button simply never being built.
  ok(await manualPage.saveButtonShown(), "the breadcrumb offers a Save button");
  ok(await manualPage.clickSave(), "the button is clickable");
  ok(await until(async () => (await Deno.readTextFile(manualFile)).includes("not to be autosaved"), 5, "button save"),
     "clicking it writes the file");
  // Distinct text, so this is the keystroke's own evidence rather than the
  // button's write still sitting on disk.
  await manualPage.type("and by keystroke\n");
  await manualPage.press(2); // ctrl-s
  ok(await until(async () => (await Deno.readTextFile(manualFile)).includes("and by keystroke"), 5, "manual save"),
     "an explicit save still works there");
} finally {
  try { page?.close(); } catch {}
  try { manualPage?.close(); } catch {}
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
