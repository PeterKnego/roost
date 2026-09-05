//! The dialog primitive: askConfirm's four exits, and focus restoration.
//!
//! Traps this file is written against (see README):
//!   - Every "no intent was sent" assertion records intents FIRST and proves
//!     the recorder works by observing a real one, so a cancel assertion
//!     cannot pass because the hook was never wired.
//!   - Focus restoration is asserted against a specific element that was
//!     focused beforehand, not against "something has focus" — document.body
//!     always satisfies the weaker form.
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const port = await freePort();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port });
const browser = await startBrowser(profileDir(repoRoot));
let page;
try {
  page = await openPage(browser.port, `http://127.0.0.1:${port}/proj`);
  const evalIn = page.evalIn;
  await until(async () => await evalIn("typeof askConfirm === 'function'"), 10, "dialog.js loaded");

  // Record every intent, and prove the recorder works before relying on its
  // silence. Without this proof a "cancel sends nothing" assertion passes
  // just as well when send() was never wrapped at all.
  await evalIn(`window.__sent = []; const __s = window.send;
                window.send = (m) => { window.__sent.push(m); return __s(m); }; 0`);
  await evalIn(`send({ t: "ActivateTab", pane: 0, idx: 0 }); 0`);
  ok((await evalIn("window.__sent.length")) === 1, "the intent recorder itself records");
  await evalIn("window.__sent = []; 0");

  // A: Cancel resolves false.
  await evalIn(`window.__r = null;
     askConfirm({ title: "T", lines: ["L"], confirm: "Go", danger: true })
       .then((v) => { window.__r = v; }); 0`);
  ok(await evalIn(`document.getElementById("dlg-confirm").open`), "the dialog opened");
  ok(await evalIn(`document.activeElement === document.querySelector("#dlg-confirm .dlg-cancel")`),
     "danger focuses Cancel, so Enter cancels rather than destroys");
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click(); 0`);
  ok(await until(async () => (await evalIn("window.__r")) === false, 5, "cancel"),
     "Cancel resolves false");
  ok((await evalIn("window.__sent.length")) === 0, "and sends no intent");

  // B: Escape resolves false.
  await evalIn(`window.__r = null; askConfirm({ title: "T" }).then((v) => { window.__r = v; }); 0`);
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  ok(await until(async () => (await evalIn("window.__r")) === false, 5, "escape"),
     "Escape resolves false");
  ok(!(await evalIn(`document.getElementById("dlg-confirm").open`)), "and closes the dialog");

  // C: the confirm button resolves true.
  await evalIn(`window.__r = null; askConfirm({ title: "T", confirm: "Go" }).then((v) => { window.__r = v; }); 0`);
  await evalIn(`document.querySelector("#dlg-confirm .dlg-ok").click(); 0`);
  ok(await until(async () => (await evalIn("window.__r")) === true, 5, "confirm"),
     "Confirm resolves true");

  // D: `blocked` disables the confirm button and shows the reason.
  await evalIn(`window.__r = null;
     askConfirm({ title: "T", confirm: "Go", blocked: "Save first." }).then((v) => { window.__r = v; }); 0`);
  ok(await evalIn(`document.querySelector("#dlg-confirm .dlg-ok").disabled`),
     "blocked disables the confirm button");
  ok((await evalIn(`document.querySelector("#dlg-confirm .dlg-blocked").textContent`)) === "Save first.",
     "and states the reason");
  ok(!(await evalIn(`document.querySelector("#dlg-confirm .dlg-blocked").hidden`)),
     "with the reason actually visible");
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click(); 0`);
  await until(async () => (await evalIn("window.__r")) === false, 5, "blocked cancel");

  // E: focus returns to the exact element that had it, for a plain button.
  //
  // This proves the platform behaviour roost's `runDialog` depends on
  // (Chromium's own <dialog>.close() restores focus to the pre-showModal()
  // element) — it does NOT prove that dialog.js's own `restore.focus()` call
  // in `runDialog` does anything. Verified by reverting: commenting out that
  // line and re-running this file still printed every `ok` including this
  // one, because Chromium restores focus to a plain, still-attached element
  // like #closeproj on its own, with no application code involved (confirmed
  // separately with a bare `<dialog>` and no dialog.js at all). Keep the line
  // in dialog.js anyway — it exists for roost's terminals, which are pooled
  // DOM nodes moved between panes with appendChild, a case this assertion
  // does not exercise and where the native restoration's behaviour is
  // unverified. That case is a by-hand check in a later task, not an
  // automated one here, because it needs a real dtach session.
  await evalIn(`document.getElementById("closeproj").focus();
     window.__r = null; askConfirm({ title: "T" }).then((v) => { window.__r = v; }); 0`);
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click(); 0`);
  await until(async () => (await evalIn("window.__r")) === false, 5, "focus case");
  ok(await evalIn(`document.activeElement === document.getElementById("closeproj")`),
     "focus returns to the element that had it after the dialog closes (platform behaviour; does not by itself prove dialog.js's restore.focus() call does anything — see comment above)");

  // F: a value that would be markup if it were ever interpolated.
  await evalIn(`window.__r = null;
     askConfirm({ title: "T", lines: ['<img src=x onerror="window.__pwned=1">'] })
       .then((v) => { window.__r = v; }); 0`);
  ok((await evalIn(`document.querySelector("#dlg-confirm .dlg-body").querySelectorAll("img").length`)) === 0,
     "a path that looks like markup produces no element");
  ok((await evalIn("window.__pwned === undefined")), "and runs nothing");
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click(); 0`);

  // G: askText preselects the basename, so typing replaces the name and not
  // the directory. prompt() could not do this, and a rename that silently
  // ate the directory would be a data-loss bug, not a UX one.
  await evalIn(`window.__r = null;
     askText({ title: "Rename", value: "src/main.rs", confirm: "Rename" })
       .then((v) => { window.__r = v; }); 0`);
  ok((await evalIn(`document.getElementById("dlg-input").value`)) === "src/main.rs",
     "askText prefills the whole path");
  ok((await evalIn(`document.getElementById("dlg-input").selectionStart`)) === 4 &&
     (await evalIn(`document.getElementById("dlg-input").selectionEnd`)) === 11,
     "and selects only the basename");
  ok(await evalIn(`document.activeElement === document.getElementById("dlg-input")`),
     "with focus in the field");
  await evalIn(`document.getElementById("dlg-input").value = "src/other.rs";
     document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  ok(await until(async () => (await evalIn("window.__r")) === "src/other.rs", 5, "text ok"),
     "confirming resolves the typed value");

  // H: an empty field resolves null, never the empty string — every caller
  // guards with `if (name)`, and "" would slip through a truthiness check as
  // a create/rename of a path with no name.
  await evalIn(`window.__r = "unset"; askText({ title: "New file", value: "x" }).then((v) => { window.__r = v; }); 0`);
  await evalIn(`document.getElementById("dlg-input").value = "   ";
     document.querySelector("#dlg-text .dlg-ok").click(); 0`);
  ok(await until(async () => (await evalIn("window.__r")) === null, 5, "empty"),
     "a blank field resolves null, not an empty string");

  // I: Escape resolves null.
  await evalIn(`window.__r = "unset"; askText({ title: "T", value: "y" }).then((v) => { window.__r = v; }); 0`);
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  ok(await until(async () => (await evalIn("window.__r")) === null, 5, "text escape"),
     "Escape resolves null");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
