//! The update mark, the dialog, Later, Skip, and the About row. #65 step 4.
//!
//! The harness's own `startRoost` writes `version_check = false` into its own
//! config so no other file here ever touches the network. This one needs the
//! opposite — the check *on*, so the client's real renderers see a real
//! `newer` status — so it points `ROOST_CONFIG` at its own file and plants a
//! *fresh* `check.json` naming 999.0.0 before roost ever starts: `stale()`
//! (version.rs) sees a same-second `checked_at` and skips the request
//! entirely. Section A asserts the planted file is byte-identical afterwards,
//! which is the only thing that says no request fired.
//!
//! The test binary is a checkout with no compiled-in key
//! (`keys/roost.pub` does not exist for a dev build), so the *real* `offer`
//! is false and the mark never appears on its own. Sections B-D drive the
//! real renderers (`renderUpdateMark`, `openUpdate`, `latestLabel`) against
//! `state.settings.build` mutated in place — about.mjs's technique — to make
//! the mark and the dialog's Update button appear as they would for a
//! replaceable copy. `Later`, `Skip` and the un-skip on a newer version go
//! through the real server: nothing about those three is faked.
//!
//! Section F is the controller-required proof that CLAUDE.md's "Destruction
//! requires positive evidence" calls for here: dialog.js flushes every
//! pending debounced edit before it sends `Update`, specifically because the
//! server execs over itself on a successful swap and anything still sitting
//! in the 200ms debounce would never reach disk. A checkout's Update is
//! refused instantly with no exec, so nothing here is destroyed either way —
//! the only thing that can tell the flushed case apart from the unflushed
//! one is *when* the edit reaches the server, not *whether* it eventually
//! does (the debounce's own timer is untouched by clicking Update, and fires
//! on its own 200ms later regardless). So the section types into an editor
//! and clicks Update in one synchronous browser-side call — leaving the edit
//! inside the 200ms debounce at the moment of the click — then asks a
//! *second*, independent page (already connected, never having typed
//! anything itself) whether it saw the new text within ~150ms. That second
//! page's own `texts` map can only be set by a `BufferText` the server
//! broadcast to it; it is not reachable from anything the typing page
//! believes locally, which is what makes it a check on the server and not on
//! the client's own optimistic cache.
//!
//! `openUpdate`'s `onProgress` hook re-flushes `pendingEdits` too, on *every*
//! phase it sees ("typing does not stop once Update is clicked") — and a
//! checkout's refusal is one round trip away, fast enough that this second,
//! entirely legitimate flush reaches the server well inside the window below
//! and rescues a missing pre-send flush for the wrong reason. First measured
//! directly: with only the pre-send flush deleted and `onProgress` left
//! alone, this section passed anyway, because the "refused" progress frame's
//! own reflush covered for it. Section F wraps `updateOpen.onProgress` (after
//! the dialog opens, before the click) to empty `pendingEdits` for the
//! instant the real handler runs and put it back after — the progress text
//! this section still reads keeps rendering, but that handler's own reflush
//! loop now iterates nothing. That isolates the property under test to the
//! one flush line that sits directly before `send({ t: "Update" })`.
//!
//! Revert-checked by deleting that line in `static/dialog.js`'s `ok.onclick`
//! (backed up with `cp`, restored the same way — never `git checkout`),
//! `cargo build`, and running this file again: section F's "saw it almost
//! immediately" assertion failed — `texts` on the second page still held the
//! original `"# hello\n"` at the ~150ms mark, because with the flush gone
//! (and the `onProgress` reflush neutered above) the edit does not leave the
//! browser until the debounce's own 200ms timer fires, which is on the far
//! side of the window this section checks. Every other assertion in the
//! section stayed green — including the refusal text, since `onProgress` is
//! wrapped, not replaced — which is what says the break is isolated to this
//! one property and not to Update or to editing in general. Restored and
//! rebuilt, the file returns to a clean PASS, 40/40.
//!
//! Run: deno run -A tests/browser/update.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
// The check is ON here, with a planted, fresh answer: status "newer" without
// a request. The harness's own config would say "off" and hide everything.
const globalToml = `${fx.base}/global.toml`;
await Deno.writeTextFile(globalToml, "version_check = true\n");
const checkFile = `${fx.stateDir}/update/check.json`;
const plant = async (latest) => {
  await Deno.mkdir(`${fx.stateDir}/update`, { recursive: true });
  await Deno.writeTextFile(checkFile,
    JSON.stringify({ latest, checked_at: Math.floor(Date.now() / 1000), failed_at: null }));
};
await plant("999.0.0");
const planted = await Deno.readTextFile(checkFile);

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(),
                                 extraEnv: { ROOST_CONFIG: globalToml } });
const browser = await startBrowser(profileDir(repoRoot));
let page, page2;

const readRows = (evalIn) => evalIn(`(() => {
  const out = {};
  for (const r of document.querySelectorAll("#dlg-settings .dlg-about .dlg-row")) {
    out[r.querySelector("label").textContent] = r.querySelector(".aboutval").textContent.trim();
  }
  return out; })()`);

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await page.cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  const { evalIn } = page;
  await until(() => evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"),
    30, "app.js");

  console.log("A. the server sent newer, from the planted file, without a request");
  const u = await evalIn(`state.settings.update`);
  ok(u && u.status === "newer" && u.latest === "999.0.0", `status=${u && u.status} latest=${u && u.latest}`);
  ok(u && u.offer === false, "a checkout is not offered the button");
  await sleep(1500);
  ok((await Deno.readTextFile(checkFile)) === planted, "check.json is byte-identical: no check ran");

  console.log("B. a checkout gets no mark; a replaceable copy does, and the dialog opens itself once");
  ok(await evalIn(`document.getElementById("updmark").hidden`), "no mark for a checkout");
  ok(!(await evalIn(`document.getElementById("dlg-update").open`)), "and no dialog");
  // The real renderer, on a build mutated in place — the about.mjs technique.
  await evalIn(`Object.assign(state.settings.build, { channel: "release", owner: "cargo-bin", replaceable: "yes" }); updateOffered = false; renderUpdateMark(); 0`);
  ok(!(await evalIn(`document.getElementById("updmark").hidden`)), "the mark shows for a replaceable copy");
  ok((await evalIn(`document.getElementById("updmark").textContent`)) === "↑ 999.0.0", "and names the version");
  ok(await until(() => evalIn(`document.getElementById("dlg-update").open`), 5, "auto-open"),
     "the dialog opened by itself");
  const title = await evalIn(`document.querySelector("#dlg-update .dlg-title").textContent`);
  const running = await evalIn(`state.settings.build.version`);
  ok(title === `roost 999.0.0 is available — you are running ${running}`, `the title names both versions (${title})`);
  ok(await evalIn(`document.querySelector("#dlg-update .dlg-ok").hidden`), "offer is false: no Update button");
  ok(!(await evalIn(`document.querySelector("#dlg-update .dlg-cmd").hidden`)), "the command is shown instead");
  ok((await evalIn(`document.querySelector("#dlg-update .dlg-skip").textContent`)) === "Skip 999.0.0", "Skip names the version");
  await evalIn(`renderUpdateMark(); 0`);
  ok(await evalIn(`document.getElementById("dlg-update").open`), "a second render does not open a second dialog");

  console.log("C. Later: the dialog closes, the mark stays, the choice is on disk and in every snapshot");
  await evalIn(`document.querySelector("#dlg-update .dlg-later").click()`);
  ok(await until(() => evalIn(`!document.getElementById("dlg-update").open`), 5, "close"), "the dialog closed");
  ok(await until(() => evalIn(`state.settings.update.deferred_until > 0`), 5, "deferred"),
     "the snapshot carries the deferral");
  const choices = JSON.parse(await Deno.readTextFile(`${fx.stateDir}/update/choices.json`));
  ok(choices.deferred_until > Math.floor(Date.now() / 1000), `choices.json holds it (${choices.deferred_until})`);
  await evalIn(`Object.assign(state.settings.build, { channel: "release", owner: "cargo-bin", replaceable: "yes" }); renderUpdateMark(); 0`);
  ok(!(await evalIn(`document.getElementById("updmark").hidden`)), "the mark stays after Later");
  ok(!(await evalIn(`document.getElementById("dlg-update").open`)), "and the dialog does not reopen by itself");

  console.log("D. Skip: the mark goes, the row says skipped, and a newer version brings it back");
  await evalIn(`document.getElementById("updmark").click()`);
  ok(await until(() => evalIn(`document.getElementById("dlg-update").open`), 5, "reopen"), "clicking the mark reopens the dialog");
  await evalIn(`document.querySelector("#dlg-update .dlg-skip").click()`);
  ok(await until(() => evalIn(`state.settings.update.skipped === "999.0.0"`), 5, "skipped"), "the snapshot carries the skip");
  await evalIn(`Object.assign(state.settings.build, { channel: "release", owner: "cargo-bin", replaceable: "yes" }); renderUpdateMark(); 0`);
  ok(await evalIn(`document.getElementById("updmark").hidden`), "the mark is gone for the skipped version");
  await evalIn(`document.getElementById("settings").click()`);
  ok(await until(() => evalIn(`document.getElementById("dlg-settings").open`), 5, "settings"), "settings opens");
  await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="about"]').click()`);
  await sleep(200);
  let rows = await readRows(evalIn);
  ok(rows.Latest === "999.0.0 skipped", `the row reads skipped (${JSON.stringify(rows.Latest)})`);
  await evalIn(`document.querySelector("#dlg-settings .dlg-cancel").click()`);

  // A newer release un-skips by itself. The check's answer is read from
  // disk once per process, so the new fact needs a restart to be seen.
  await plant("999.1.0");
  ok(await roost.restart(), "roost restarted on the same state dir");
  ok(await until(() => evalIn("ctrl && ctrl.readyState === 1 && state.settings.update.latest === '999.1.0'"), 40, "reconnect"),
     "the page reconnected and sees 999.1.0");
  ok((await evalIn(`state.settings.update.skipped`)) === "999.0.0", "the old skip is still recorded");
  await evalIn(`Object.assign(state.settings.build, { channel: "release", owner: "cargo-bin", replaceable: "yes" }); renderUpdateMark(); 0`);
  ok(!(await evalIn(`document.getElementById("updmark").hidden`)), "and the mark is back for the newer version");
  ok((await evalIn(`document.getElementById("updmark").textContent`)) === "↑ 999.1.0", "naming it");
  await evalIn(`document.getElementById("dlg-update").dispatchEvent(new Event("cancel", { cancelable: true })); 0`);

  console.log("E. the About button, and the row's three extra renderings");
  await evalIn(`state.settings.update.offer = true; 0`);
  await evalIn(`document.getElementById("settings").click()`);
  await until(() => evalIn(`document.getElementById("dlg-settings").open`), 5, "settings");
  await sleep(300); // let RequestState's answer land first: it replaces state.settings wholesale
  await evalIn(`state.settings.update.offer = true; 0`);
  await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="settings"]').click()`);
  await evalIn(`document.querySelector('#dlg-settings .dlg-tab[data-tab="about"]').click()`);
  await sleep(200);
  const btn = await evalIn(`(() => { const r = [...document.querySelectorAll("#dlg-settings .dlg-about .dlg-row")]
    .find((x) => x.querySelector("label").textContent === "Latest");
    const b = r && r.querySelector("button.dlg-upd"); return b ? b.textContent : null; })()`);
  ok(btn === "Update", `the Latest row carries the button when offered (${JSON.stringify(btn)})`);
  await evalIn(`[...document.querySelectorAll("#dlg-settings .dlg-about .dlg-row")]
    .find((x) => x.querySelector("label").textContent === "Latest").querySelector("button.dlg-upd").click()`);
  ok(await until(() => evalIn(`!document.getElementById("dlg-settings").open && document.getElementById("dlg-update").open`), 5, "swap"),
     "the button closes settings and opens the update dialog");
  ok(!(await evalIn(`document.querySelector("#dlg-update .dlg-ok").hidden`)), "which now has an Update button");

  // The real intent, from a checkout: refused, and the refusal reaches this
  // dialog as a progress line rather than a silent nothing.
  await evalIn(`document.querySelector("#dlg-update .dlg-ok").click()`);
  ok(await until(() => evalIn(`document.querySelector("#dlg-update .dlg-progress").textContent.startsWith("not updated: channel checkout")`), 5, "refused"),
     `a checkout's Update is refused by name (${await evalIn(`document.querySelector("#dlg-update .dlg-progress").textContent`)})`);
  await evalIn(`document.getElementById("dlg-update").dispatchEvent(new Event("cancel", { cancelable: true })); 0`);

  console.log("F. an edit typed just before Update reaches the server first, not 200ms later");
  // The buffer this section edits, and what a fresh tab on it should read.
  const REL = "hello.md";
  const ORIGINAL = "# hello\n";
  const MARKER = "# hello\ncontroller wrote this just before clicking Update\n";
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: ${JSON.stringify(REL)}, mode: "Edit" } })`);
  await until(() => evalIn(`!!document.querySelector("textarea.editor")`), 10, "an editor for hello.md");
  await until(() => evalIn(`texts.get(${JSON.stringify(REL)}) === ${JSON.stringify(ORIGINAL)}
    && (document.querySelector("textarea.editor") || {}).value === texts.get(${JSON.stringify(REL)})`),
    10, "the editor to settle on hello.md's own text");

  // A second, independent client — connected, but never having typed into
  // this file itself. Its own `texts` map can only move because the server
  // pushed a BufferText at it; nothing on the typing page can set it.
  page2 = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  await until(() => page2.evalIn("typeof state !== 'undefined' && !!state && ctrl && ctrl.readyState === 1"),
    30, "app.js on the second page");
  ok((await page2.evalIn(`texts.get(${JSON.stringify(REL)})`)) === ORIGINAL,
     "the second page's connect-time replay shows the file's own original text, not the marker");

  // Opening page2 put page1 in the background (visibilityState: "hidden"),
  // where timers and websockets keep running but rAF-driven work does not —
  // harness.mjs's own warning. This section goes on driving page1 (the
  // dialog, the click), so it has to come forward again first.
  await page.bringToFront();

  await evalIn(`Object.assign(state.settings.update, { status: "newer", latest: "999.1.0", offer: true, skipped: null, failure: null, deferred_until: 0 }); 0`);
  await evalIn(`openUpdate(state.settings); 0`);
  ok(await until(() => evalIn(`document.getElementById("dlg-update").open && !document.querySelector("#dlg-update .dlg-ok").hidden`), 5, "a fresh Update dialog"),
     "a fresh dialog opens with the Update button");

  // dialog.js's onProgress hook *also* re-flushes pendingEdits on every phase
  // it sees (its own comment: "typing does not stop once Update is
  // clicked"), and a checkout's refusal is one round trip away — fast enough
  // that it reaches here well inside this section's own window and would
  // rescue a missing pre-send flush for the wrong reason. Wrapped, not
  // replaced, so the progress text this section still reads keeps rendering:
  // only the reflush loop's view of pendingEdits is emptied for the instant
  // orig() runs, isolating the property under test to the one flush line
  // that sits directly before `send({ t: "Update" })`.
  await evalIn(`(() => {
    const orig = updateOpen.onProgress;
    updateOpen.onProgress = (ev) => {
      const saved = new Map(pendingEdits);
      pendingEdits.clear();
      orig(ev);
      for (const [k, v] of saved) pendingEdits.set(k, v);
    };
  })(); 0`);

  // Typed and clicked in one synchronous browser-side call: no round trip to
  // this test script separates the two, so the edit is still inside the
  // 200ms debounce (pendingEdits non-empty) at the instant Update is clicked
  // — exactly the state the flush line in dialog.js exists to catch.
  await evalIn(`(() => {
    const ta = document.querySelector("textarea.editor");
    ta.focus();
    ta.value = ${JSON.stringify(MARKER)};
    ta.dispatchEvent(new Event("input", { bubbles: true }));
    document.querySelector("#dlg-update .dlg-ok").click();
  })(); 0`);

  // ~150ms, in two polls at 0ms and 100ms (harness.until's own grain) — well
  // inside the 200ms debounce window, so landing here can only mean the
  // flush sent it, not the timer.
  const echoedFast = await until(
    () => page2.evalIn(`texts.get(${JSON.stringify(REL)}) === ${JSON.stringify(MARKER)}`),
    0.15, "the edit echoed to page2 inside the debounce window");
  ok(echoedFast,
     `a second, already-connected client saw the new text within ~150ms of the click (texts on page2: ${JSON.stringify(await page2.evalIn(`texts.get(${JSON.stringify(REL)})`))})`);

  // And Update itself was still handled — refused, since this is a checkout
  // — which is what says the edit arrived *before* Update was processed,
  // not that Update was never processed at all.
  ok(await until(() => evalIn(`document.querySelector("#dlg-update .dlg-progress").textContent.startsWith("not updated: channel checkout")`), 5, "refused"),
     `and Update was still handled, refused by name (${await evalIn(`document.querySelector("#dlg-update .dlg-progress").textContent`)})`);
  await evalIn(`document.getElementById("dlg-update").dispatchEvent(new Event("cancel", { cancelable: true })); 0`);

  const label = async (x) => await evalIn(`latestLabel(${JSON.stringify(x)})`);
  ok((await label({ status: "newer", latest: "1.0.0", skipped: "1.0.0" })) === "1.0.0 skipped", "skipped");
  ok((await label({ status: "newer", latest: "1.0.0", skipped: "0.9.0" })) === "1.0.0 available", "an older skip does not apply");
  ok((await label({ status: "newer", latest: "1.0.0", failure: "verify: signature did not verify" })) === "1.0.0 available (update failed: verify: signature did not verify)", "a failure is appended");
  ok((await label({ status: "up-to-date", latest: "1.0.0", installed: "1.0.0" })) === "restart roost to run 1.0.0", "installed wins over everything");
  ok((await label({ status: "off" })) === "version checks are off", "step 2's renderings are untouched");
} finally {
  try { await page2?.close(); } catch { /* already gone */ }
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
