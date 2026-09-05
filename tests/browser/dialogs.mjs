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

  // Native dialogs are gone from app.js and must stay gone. A grep would
  // match comments and strings; this fails only if a code path actually calls
  // one. It cannot be a Rust test — no Rust test reaches static/app.js.
  //
  // Deliberately NOT applied in termlinks.mjs: xterm's own OSC 8 fallback
  // legitimately calls confirm(), and that file asserts on its silence as
  // positive evidence that roost's linkHandler took the activation instead.
  await evalIn(`window.__native = [];
     for (const k of ["confirm", "prompt", "alert"]) {
       window[k] = (...a) => { window.__native.push([k, ...a]); return null; };
     } 0`);

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
  // in dialog.js anyway, but not for the reason once given here: roost's
  // pooled xterm nodes are not a gap in Chromium's restoration either — they
  // stay connected to the document the whole time (moved between panes with
  // appendChild, never detached), so the platform restores focus to them the
  // same way it does to #closeproj here, and this assertion's inability to
  // discriminate is about the *browser engine*, not about xterm. The actual
  // reason to keep the line is cross-browser: <dialog> focus restoration has
  // historically been less reliable outside Chromium (Firefox, Safari around
  // 15.4), and this suite only drives Chromium, so it cannot see that case
  // either way.
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
     askText({ title: "Rename", label: "New path", value: "src/main.rs", confirm: "Rename" })
       .then((v) => { window.__r = v; }); 0`);
  ok((await evalIn(`document.getElementById("dlg-input").value`)) === "src/main.rs",
     "askText prefills the whole path");
  // Regression: DIALOG_STRUCTURAL_CSS once put .dlg-label in its `revert`
  // group alongside dialog.roost/.dlg-title/.dlg-blocked. `revert` discards
  // ALL author-origin declarations for the property, not just the theme's, so
  // it wiped out style.css's own `.dlg-label { display: block; }` too and
  // fell back to <label>'s UA default of `inline` — breaking the layout of
  // every askText dialog that passes a label (all three real call sites do).
  // Asserted on computed style, not a class or attribute, because a class
  // toggling correctly says nothing about what the structural CSS actually
  // resolves to in the cascade.
  ok((await evalIn(`getComputedStyle(document.querySelector("#dlg-text .dlg-label")).display`)) === "block",
     "the label keeps its block layout (not reverted to <label>'s inline default)");
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

  // J: the menu renders one button per item and resolves the chosen id.
  await evalIn(`window.__r = "unset";
     askMenu({ x: 40, y: 60, items: [{ id: "a", label: "Alpha" }, { id: "b", label: "Beta" }] })
       .then((v) => { window.__r = v; }); 0`);
  ok((await evalIn(`document.querySelectorAll("#dlg-menu .dlg-item").length`)) === 2,
     "one button per item");
  ok(await evalIn(`document.activeElement === document.querySelector("#dlg-menu .dlg-item")`),
     "the first item takes focus");
  // ArrowDown moves focus. Asserted on the focused element, not on a class:
  // a highlight class with no focus move leaves Enter activating the wrong row.
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyDown", key: "ArrowDown", code: "ArrowDown", windowsVirtualKeyCode: 40 });
  ok(await evalIn(`document.activeElement === document.querySelectorAll("#dlg-menu .dlg-item")[1]`),
     "ArrowDown moves focus to the next item");
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyDown", key: "ArrowUp", code: "ArrowUp", windowsVirtualKeyCode: 38 });
  ok(await evalIn(`document.activeElement === document.querySelectorAll("#dlg-menu .dlg-item")[0]`),
     "and ArrowUp back again");
  await evalIn(`document.querySelectorAll("#dlg-menu .dlg-item")[1].click(); 0`);
  ok(await until(async () => (await evalIn("window.__r")) === "b", 5, "menu pick"),
     "clicking an item resolves its id");

  // K: a label that would be markup if it were ever interpolated.
  await evalIn(`window.__r = "unset";
     askMenu({ x: 10, y: 10, items: [{ id: "x", label: '<b>bold</b>' }] })
       .then((v) => { window.__r = v; }); 0`);
  ok((await evalIn(`document.querySelectorAll("#dlg-menu .dlg-item b").length`)) === 0,
     "a menu label that looks like markup produces no element");
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  ok(await until(async () => (await evalIn("window.__r")) === null, 5, "menu escape"),
     "Escape resolves null");

  // L: a menu opened at the far edge is clamped back on screen. Without the
  // clamp the last item is unreachable, which is not visible in any test that
  // only asserts the menu opened.
  await evalIn(`window.__r = "unset";
     askMenu({ x: innerWidth - 4, y: innerHeight - 4, items: [{ id: "a", label: "Alpha" }] })
       .then((v) => { window.__r = v; }); 0`);
  ok(await evalIn(`(() => { const r = document.getElementById("dlg-menu").getBoundingClientRect();
     return r.right <= innerWidth && r.bottom <= innerHeight; })()`),
     "a menu at the viewport edge is clamped back on screen");
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await until(async () => (await evalIn("window.__r")) === null, 5, "menu edge escape");

  // N: askChoice — several positive answers plus Cancel. The ✻ prompt ("a
  // Claude is already here": new worktree / here anyway / dismiss) needs it;
  // askConfirm has one OK, and its .dlg-ok/.dlg-cancel are what the
  // structural CSS locks, so a third button cannot be bolted onto it.
  await evalIn(`window.__r = "unset";
     askChoice({ title: "T", lines: ["L"], choices: [{ id: "wt", label: "New worktree" }, { id: "here", label: "Here" }] })
       .then((v) => { window.__r = v; }); 0`);
  ok(await evalIn(`document.getElementById("dlg-choice").open`), "askChoice opens its own shell");
  ok((await evalIn(`[...document.querySelectorAll("#dlg-choice .dlg-choice")].map((b) => b.textContent).join("|")`)) === "New worktree|Here",
     "one button per choice, labelled, in order");
  ok(await evalIn(`document.activeElement === document.querySelector('#dlg-choice .dlg-choice[data-choice="wt"]')`),
     "the first choice has focus, so Enter takes it");
  ok((await evalIn(`document.querySelector("#dlg-choice .dlg-body").textContent`)) === "L", "the body carries the lines");
  await evalIn(`document.querySelector('#dlg-choice .dlg-choice[data-choice="here"]').click(); 0`);
  ok(await until(async () => (await evalIn("window.__r")) === "here", 5, "choice pick"), "clicking a choice resolves its id");
  ok(!(await evalIn(`document.getElementById("dlg-choice").open`)), "and closes the dialog");

  ok(await evalIn(`document.querySelector("#dlg-choice .dlg-detail").hidden`), "with no detail the slot is hidden");

  await evalIn(`window.__r = "unset"; askChoice({ title: "T", choices: [{ id: "a", label: "A" }] }).then((v) => { window.__r = v; }); 0`);
  await evalIn(`document.querySelector("#dlg-choice .dlg-cancel").click(); 0`);
  ok(await until(async () => (await evalIn("window.__r")) === null, 5, "choice cancel"), "Cancel resolves null");

  // A detail slot for server-rendered HTML (the save conflict's diff, which
  // render.rs escapes line by line), and focus on Cancel when every choice
  // destroys something.
  await evalIn(`window.__r = "unset";
     askChoice({ title: "T", detailHtml: '<div class="dl add">+x</div>', focus: "cancel",
                 choices: [{ id: "a", label: "A" }, { id: "b", label: "B" }] })
       .then((v) => { window.__r = v; }); 0`);
  ok((await evalIn(`document.querySelectorAll("#dlg-choice .dlg-detail .dl.add").length`)) === 1
     && !(await evalIn(`document.querySelector("#dlg-choice .dlg-detail").hidden`)),
     "detailHtml renders into the visible detail slot");
  ok(await evalIn(`document.activeElement === document.querySelector("#dlg-choice .dlg-cancel")`),
     "focus: 'cancel' puts focus on Cancel, so Enter destroys nothing");
  await evalIn(`document.querySelector("#dlg-choice .dlg-cancel").click(); 0`);
  await until(async () => (await evalIn("window.__r")) === null, 5, "detail cancel");

  await evalIn(`window.__r = "unset"; askChoice({ title: "T", choices: [{ id: "a", label: '<b>bold</b>' }] }).then((v) => { window.__r = v; }); 0`);
  ok((await evalIn(`document.querySelectorAll("#dlg-choice .dlg-choice b").length`)) === 0,
     "a choice label that looks like markup produces no element");
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await page.cmd("Input.dispatchKeyEvent",
    { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  ok(await until(async () => (await evalIn("window.__r")) === null, 5, "choice escape"), "Escape resolves null");

  // M: drive each converted entry point once, so the guard above is armed
  // against real call sites and not only against the primitive.
  await evalIn(`fileMenu({ preventDefault(){}, clientX: 30, clientY: 30 }, "a.txt"); 0`);
  ok(await evalIn(`document.getElementById("dlg-menu").open`), "the file menu opens in-page");
  await evalIn(`document.querySelector("#dlg-menu .dlg-item").click(); 0`);
  await until(async () => await evalIn(`document.getElementById("dlg-text").open`), 5, "text dialog");
  await evalIn(`document.querySelector("#dlg-text .dlg-cancel").click(); 0`);
  await evalIn(`document.getElementById("closeproj").click(); 0`);
  await until(async () => await evalIn(`document.getElementById("dlg-confirm").open`), 5, "close dialog");
  ok(true, "Close Project opens in-page");
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click(); 0`);
  await evalIn(`showClaudeHere(3, ["term1"]); 0`);
  ok(await until(async () => await evalIn(`document.getElementById("dlg-choice").open`), 5, "claude-here dialog"),
     "the ✻ 'a Claude is already here' prompt opens in-page");
  ok((await evalIn(`document.querySelector("#dlg-choice .dlg-body").textContent`)).includes("term1"), "and names the terminal");
  ok((await evalIn("window.__sent.length")) === 0, "and sends nothing until answered");
  await evalIn(`document.querySelector("#dlg-choice .dlg-cancel").click(); 0`);
  await evalIn(`showConflict({ rel: "a.txt", diff_html: '<div class="dl del">-old</div><div class="dl add">+new</div>' }); 0`);
  ok(await until(async () => await evalIn(`document.getElementById("dlg-choice").open`), 5, "conflict dialog"),
     "the save conflict opens in-page");
  ok((await evalIn(`document.querySelector("#dlg-choice .dlg-title").textContent`)).includes("a.txt"), "titled with the file");
  ok((await evalIn(`document.querySelectorAll("#dlg-choice .dlg-detail .dl").length`)) === 2, "showing the diff");
  ok(await evalIn(`document.activeElement === document.querySelector("#dlg-choice .dlg-cancel")`),
     "with focus on Cancel: both answers discard someone's work");
  ok((await evalIn("window.__sent.length")) === 0, "and sends nothing until answered");
  await evalIn(`document.querySelector("#dlg-choice .dlg-cancel").click(); 0`);
  await evalIn(`document.getElementById("bell").click(); 0`);
  await until(async () => await evalIn(`!document.getElementById("noticepanel").hidden`), 5, "notice panel");
  await evalIn(`document.querySelector("#noticepanel .hookrow button").click(); 0`);
  ok(await until(async () => await evalIn(`document.getElementById("dlg-confirm").open`), 5, "hook dialog"),
     "the bell's hook switch asks in-page");
  ok((await evalIn(`document.querySelector("#dlg-confirm .dlg-body").textContent`)).includes("settings.local.json"),
     "and names the file it would write");
  ok((await evalIn("window.__sent.length")) === 0, "and sends nothing until answered");
  await evalIn(`document.querySelector("#dlg-confirm .dlg-cancel").click(); 0`);
  await evalIn(`document.getElementById("noticepanel").hidden = true; 0`);

  ok((await evalIn("window.__native.length")) === 0,
     "no code path in this file reached a native browser dialog");
} finally {
  try { await page?.close(); } catch { /* already gone */ }
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nPASS" : `\nFAIL (${fail})`);
Deno.exit(fail === 0 ? 0 : 1);
