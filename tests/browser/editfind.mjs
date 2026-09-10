//! Find within the buffer, and go to a line.
//!
//! Two more vendored `code-input` plugins. The thing worth testing beyond
//! "the dialog opened" is the pair of claims the issue makes about *why* an
//! in-buffer find is needed at all:
//!
//!   - ⇧⌘F / ⇧⌃F searches files on disk. This searches the buffer.
//!   - **It sees unsaved changes, which project search cannot.** That is the
//!     assertion that distinguishes this feature from the one that already
//!     existed, and section C is it.
//!
//! Traps this file is written against (see README):
//!
//!   - **Section B proves the two searches stay apart**, in both directions:
//!     the shifted chord must still open the project overlay and must *not*
//!     open the find dialog, and the unshifted one the reverse. A test that
//!     only checked "Ctrl+F opens find" would not notice it having eaten the
//!     project search.
//!   - **Section C edits without saving and asserts the match count.** A find
//!     that quietly read the file from disk would return the old count and
//!     look like it worked.
//!   - **Section D asserts the caret moved to the right line**, not that a
//!     dialog appeared. Go-to-line that opens a prompt and does nothing is
//!     the failure worth catching.
//!   - **Neither plugin may bind on `document`.** Ctrl+F and Ctrl+G are
//!     readline bindings, and a terminal pane must keep them. Section E sends
//!     both to a terminal and asserts no dialog appears.
//!
//! Run: deno run -A tests/browser/editfind.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture({ autosave: false });
// Three occurrences on disk, on known lines, plus enough filler that a line
// number is worth asking for.
const LINES = [];
for (let i = 1; i <= 40; i++) LINES.push(`fn filler_${i}() {}`);
LINES[4] = "let needle_z9 = 1;";   // line 5
LINES[19] = "let needle_z9 = 2;";  // line 20
LINES[34] = "let needle_z9 = 3;";  // line 35
await Deno.writeTextFile(`${fx.roots}/proj/find.rs`, LINES.join("\n") + "\n");

const port = await freePort();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port });
const browser = await startBrowser(profileDir(repoRoot));
let page;
try {
  page = await openPage(browser.port, `http://127.0.0.1:${port}/proj`);
  const { cmd, evalIn } = page;
  await cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn("typeof send === 'function' && !!state"), 20, "app.js");

  const TA = `document.querySelector('.pane[data-pane="2"] code-input textarea')`;
  // "Open" is not "present". Neither plugin removes its dialog from the DOM
  // when it closes — both mark it `inert` and `aria-hidden` and leave it
  // there. An earlier draft tested for the element's existence, which is true
  // forever after the first open: it reported the find dialog as open in a
  // terminal pane that had never opened one, and made section E fail against
  // correct code.
  const dialogOpen = async (cls) => await evalIn(
    `(() => { const d = document.querySelector(".${cls}");
      return !!d && !d.hasAttribute("inert"); })()`);
  const findDialogOpen = () => dialogOpen("code-input_find-and-replace_dialog");
  const gotoDialogOpen = () => dialogOpen("code-input_go-to-line_dialog");
  // Both halves of the keystroke. The close handlers listen on `keyup`, so a
  // keyDown-only Escape leaves the dialog open and every later assertion
  // measures a dialog that should have gone.
  const key = async (opts) => {
    const base = {
      key: opts.key, code: opts.code,
      windowsVirtualKeyCode: opts.kc, nativeVirtualKeyCode: opts.kc,
      modifiers: (opts.ctrl ? 2 : 0) | (opts.shift ? 8 : 0),
    };
    await cmd("Input.dispatchKeyEvent", { type: "keyDown", ...base });
    await cmd("Input.dispatchKeyEvent", { type: "keyUp", ...base });
  };
  const focusEditor = async () => await evalIn(`(() => { ${TA}.focus(); return 0; })()`);

  console.log("\nA. the plugins are loaded and the editor is a code-input");
  ok(await evalIn(`!!(window.codeInput && codeInput.plugins && codeInput.plugins.FindAndReplace)`),
    "the find-and-replace plugin is registered");
  ok(await evalIn(`!!(window.codeInput && codeInput.plugins && codeInput.plugins.GoToLine)`),
    "the go-to-line plugin is registered");
  await evalIn(`send({ t: "OpenTab", pane: 2,
    tab: { k: "File", rel: "find.rs", mode: "Edit" } }); 0`);
  ok(await until(async () => await evalIn(`!!${TA}`), 10, "editor"), "find.rs opens as a code editor");

  console.log("\nB. the two searches stay apart");
  await focusEditor();
  await key({ key: "f", code: "KeyF", kc: 70, ctrl: true });
  ok(await until(findDialogOpen, 5, "find dialog"), "Ctrl+F opens find within the buffer");
  ok(!(await evalIn(`!document.getElementById("searchoverlay").hidden`)),
    "and does not open the project search overlay");
  // Close it before the next chord, or the assertion below cannot tell a
  // dialog that stayed open from one that opened.
  await key({ key: "Escape", code: "Escape", kc: 27 });
  ok(await until(async () => !(await findDialogOpen()), 5, "find closed"), "Escape closes it");

  await focusEditor();
  await key({ key: "F", code: "KeyF", kc: 70, ctrl: true, shift: true });
  // Focus, not the overlay. `openSearch()` deliberately leaves the panel
  // hidden for an empty query — "a chord into an empty field never opens it"
  // — so asserting on `#searchoverlay.hidden` measures a thing the chord is
  // not supposed to do, and fails against correct code. What the chord does
  // is put the caret in the project search field.
  ok(
    await until(async () => await evalIn(
      `document.activeElement === document.getElementById("searchinput")`), 5, "search focused"),
    "⇧⌃F still takes the caret to the project search field",
  );
  ok(!(await findDialogOpen()), "and does not open the in-buffer find");
  await key({ key: "Escape", code: "Escape", kc: 27 });

  console.log("\nC. find sees unsaved changes — what project search cannot");
  // autosave is off in this fixture, so the buffer really does differ from
  // disk. Without that this section would prove nothing: a find reading the
  // file would agree with a find reading the buffer.
  await focusEditor();
  await evalIn(`(() => { const t = ${TA};
    t.selectionStart = t.selectionEnd = t.value.length; return 0; })()`);
  await cmd("Input.insertText", { text: "let needle_z9 = 4;\n" });
  await until(async () => (await evalIn(`${TA}.value`)).split("needle_z9").length - 1 === 4,
    5, "fourth occurrence typed");
  const onDisk = (await Deno.readTextFile(`${fx.roots}/proj/find.rs`)).split("needle_z9").length - 1;
  ok(onDisk === 3, `setup: the file on disk still holds three occurrences (${onDisk})`);

  await key({ key: "f", code: "KeyF", kc: 70, ctrl: true });
  ok(await until(findDialogOpen, 5, "find dialog"), "find opens over the edited buffer");
  await evalIn(`(() => {
    const i = document.querySelector(".code-input_find-and-replace_dialog input");
    i.focus(); i.value = "needle_z9";
    i.dispatchEvent(new Event("input", { bubbles: true }));
    return 0; })()`);
  const count = await until(async () => {
    const t = await evalIn(`(() => { const d = document.querySelector(".code-input_find-and-replace_dialog");
      return d ? d.textContent : ""; })()`);
    return /\b4\b/.test(t);
  }, 5, "four matches");
  const dialogText = await evalIn(`(() => { const d = document.querySelector(".code-input_find-and-replace_dialog");
    return d ? d.textContent.replace(/\\s+/g, " ").trim() : ""; })()`);
  ok(count, `it counts the unsaved fourth occurrence, not the three on disk — dialog says ${JSON.stringify(dialogText)}`);
  await key({ key: "Escape", code: "Escape", kc: 27 });

  console.log("\nD. go to a line actually moves the caret");
  await focusEditor();
  await key({ key: "g", code: "KeyG", kc: 71, ctrl: true });
  ok(await until(gotoDialogOpen, 5, "goto dialog"), "Ctrl+G asks for a line");
  await evalIn(`(() => {
    const i = document.querySelector(".code-input_go-to-line_dialog input");
    i.focus(); i.value = "20";
    i.dispatchEvent(new Event("input", { bubbles: true }));
    i.dispatchEvent(new KeyboardEvent("keyup", { key: "Enter", bubbles: true }));
    return 0; })()`);
  // The assertion is where the caret is, not that a dialog appeared: a prompt
  // that opens and does nothing is the failure worth catching.
  const caretLine = await until(async () => {
    const l = await evalIn(`(() => { const t = ${TA};
      return t.value.slice(0, t.selectionStart).split("\\n").length; })()`);
    return l === 20;
  }, 5, "caret on line 20");
  const got = await evalIn(`(() => { const t = ${TA};
    return t.value.slice(0, t.selectionStart).split("\\n").length; })()`);
  ok(caretLine, `the caret lands on line 20 — got line ${got}`);

  console.log("\nE. a terminal keeps Ctrl+F and Ctrl+G");
  // Both are readline bindings. The plugins bind on the code-input's own
  // textarea, never on document; this is what proves it.
  // A real terminal, started here rather than assumed: the fixture's default
  // layout carries a Terminal *tab* but no running session, so the pane holds
  // a "start a terminal" placeholder and there is no xterm to focus. An
  // earlier draft focused `.termhost .xterm-helper-textarea`, found nothing,
  // silently left focus in the editor, and then reported that Ctrl+F "opened
  // a find dialog in a terminal" — about a terminal that did not exist.
  //
  // Dispatching at `document.body` instead would be the simpler substitution
  // and the wrong test: the claim is that a *terminal* keeps these keys, and
  // CLAUDE.md's dev/prod table is a list of times a simpler stand-in hid the
  // real behaviour.
  await evalIn(`send({ t: "NewTerminal", pane: 0 }); 0`);
  ok(
    await until(async () => await evalIn(`(() => {
      const t = document.querySelector(".termhost .xterm-helper-textarea");
      if (t) t.focus();
      return !!t && document.activeElement === t; })()`), 20, "terminal focused"),
    "setup: a real terminal is running and has focus",
  );
  ok(!(await findDialogOpen()), "setup: no find dialog is open going in");
  await key({ key: "f", code: "KeyF", kc: 70, ctrl: true });
  await key({ key: "g", code: "KeyG", kc: 71, ctrl: true });
  await sleep(300);
  ok(!(await findDialogOpen()), "Ctrl+F in a terminal opens no find dialog");
  ok(!(await gotoDialogOpen()), "and Ctrl+G opens no go-to-line dialog");

  console.log("\nF. on a Mac the chord follows the platform");
  // `alwaysCtrl: false` is the whole reason this feature does not collide
  // with roost's ⇧⌘F project search on macOS, and on this Linux host both
  // settings produce the identical Ctrl+F — so flipping it to `true` left
  // every assertion above green. `navigator.platform` is what the plugin
  // reads, and CDP can set it, which makes the decision testable instead of
  // merely commented.
  await cmd("Emulation.setUserAgentOverride", {
    userAgent: await evalIn(`navigator.userAgent`),
    platform: "MacIntel",
  });
  ok(await evalIn(`navigator.platform === "MacIntel"`), "setup: the page now reports a Mac");
  await focusEditor();
  await key({ key: "f", code: "KeyF", kc: 70, ctrl: true });
  await sleep(250);
  ok(!(await findDialogOpen()), "plain Ctrl+F does nothing on a Mac — ⌘ is the modifier there");
  await focusEditor();
  await cmd("Input.dispatchKeyEvent", {
    type: "keyDown", key: "f", code: "KeyF",
    windowsVirtualKeyCode: 70, nativeVirtualKeyCode: 70, modifiers: 4, // Meta
  });
  await cmd("Input.dispatchKeyEvent", {
    type: "keyUp", key: "f", code: "KeyF",
    windowsVirtualKeyCode: 70, nativeVirtualKeyCode: 70, modifiers: 4,
  });
  ok(await until(findDialogOpen, 5, "cmd-f find"), "⌘F opens find within the buffer");
} finally {
  if (page) page.close();
  browser.close();
  await roost.close();
}
console.log(fail ? `\n${fail} FAILED` : "\nALL PASS");
Deno.exit(fail ? 1 : 0);
