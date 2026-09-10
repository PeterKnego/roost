//! Editing keys in a code file: Tab indents, Enter keeps the indent, brackets
//! and quotes close themselves.
//!
//! These are two vendored `code-input` plugins, and nothing in Rust can see
//! any of it. What makes them worth a test beyond "does Tab work" is the
//! constraint the feature sits under: **save is conflict-guarded against a
//! hash of what was read from disk**, so a plugin that rewrote the buffer —
//! a whitespace normaliser, a line-ending fixer, an indentation converter —
//! would make a file conflict with itself or silently reformat on open.
//! Section E is that assertion, and it is the reason this file exists.
//!
//! Traps this file is written against (see README):
//!
//!   - **Every keystroke goes through `Input.dispatchKeyEvent`**, not a
//!     synthesised `KeyboardEvent`. The plugins listen on `keydown` and call
//!     `preventDefault`; a synthetic event cannot suppress the browser's own
//!     Tab handling, so a test built from one would assert about a focus move
//!     that never happened in a real browser, or miss one that did.
//!   - **Every assertion that a byte reached disk reads the file**, not the
//!     textarea. The textarea is where the plugin wrote; the whole question
//!     is whether that survives the debounce, `EditBuffer` and the save.
//!   - **Section E types nothing at all before its first check.** A
//!     byte-fidelity test that edits first cannot tell "the save path is
//!     faithful" from "the edit happened to cancel out the damage".
//!   - **Section D asserts the prose file's Tab moves focus**, which is the
//!     control for section B: without it, "Tab inserted an indent" is equally
//!     satisfied by a build where the plugin is applied to everything.
//!
//! Run: deno run -A tests/browser/editkeys.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();

// Deliberately mixed, and none of it is decoration. Tabs and spaces in the
// same file, a line with trailing whitespace, and no newline at the end: each
// is something a normaliser would "fix", and fixing any of them silently is
// the defect this file is guarding.
const MIXED = "fn a() {\n\tlet t = 1;\n}\n\nfn b() {\n    let s = 2;   \n}\n\nfn c() {}";
await Deno.writeTextFile(`${fx.roots}/proj/mixed.rs`, MIXED);
await Deno.writeTextFile(`${fx.roots}/proj/keys.rs`, "fn main() {\n}\n");
await Deno.writeTextFile(`${fx.roots}/proj/notes.md`, "# heading\n\nprose\n");

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
  const ANY_TA = `document.querySelector('.pane[data-pane="2"] textarea')`;

  // Waits for *this* file's editor, not for any textarea. An earlier draft
  // waited on `!!textarea`, which was already true from the previously opened
  // file — so the section that follows measured the wrong editor and reported
  // "not a code editor" about a markdown file it had never left.
  const open = async (rel) => {
    await evalIn(`send({ t: "OpenTab", pane: 2,
      tab: { k: "File", rel: ${JSON.stringify(rel)}, mode: "Edit" } }); 0`);
    return await until(async () => await evalIn(
      `(() => { const p = state.panes[2]; const t = p.tabs[p.active];
        return !!(t && t.k === "File" && t.rel === ${JSON.stringify(rel)}
          && document.querySelector('.pane[data-pane="2"] .editwrap textarea')); })()`),
      10, `${rel} editor`);
  };
  // One character, as a real keystroke. `Input.insertText` with a multi-char
  // string fires a single `input` event whose `data` is the whole string, and
  // the auto-close plugin tests `event.data in bracketPairs` — so a bracket
  // typed as part of a longer insertText is never seen. An earlier draft did
  // exactly that: nothing auto-closed, and the *next* assertion ("retyping
  // the closer steps over it") then passed against a bracket that had never
  // been closed at all.
  const type = async (ch) => {
    const base = { key: ch, text: ch, unmodifiedText: ch };
    await cmd("Input.dispatchKeyEvent", { type: "keyDown", ...base });
    await cmd("Input.dispatchKeyEvent", { type: "keyUp", key: ch });
  };
  // A real key event. `text` is what makes it produce a character; Tab and
  // Enter carry none, because the plugins insert on their behalf.
  const key = async (k, code, keyCode, text) => {
    const base = { key: k, code, windowsVirtualKeyCode: keyCode, nativeVirtualKeyCode: keyCode };
    await cmd("Input.dispatchKeyEvent", { type: text ? "keyDown" : "rawKeyDown", ...base, text });
    await cmd("Input.dispatchKeyEvent", { type: "keyUp", ...base });
  };
  const tab = () => key("Tab", "Tab", 9, undefined);
  const enter = () => key("Enter", "Enter", 13, "\r");
  const val = async () => await evalIn(`${TA}.value`);
  const caretToEnd = async () => await evalIn(
    `(() => { const t = ${TA}; t.focus(); t.selectionStart = t.selectionEnd = t.value.length; return 0; })()`);

  console.log("\nA. the plugins are actually loaded");
  // Asserted separately from any behaviour: if the vendored files did not
  // load, every section below would report "Tab did nothing" and none of them
  // would say why.
  ok(await evalIn(`!!(window.codeInput && codeInput.plugins && codeInput.plugins.Indent)`),
    "the indent plugin is registered");
  ok(await evalIn(`!!(window.codeInput && codeInput.plugins && codeInput.plugins.AutoCloseBrackets)`),
    "the auto-close-brackets plugin is registered");

  console.log("\nB. Tab indents a code file instead of leaving it");
  ok(await open("keys.rs"), "keys.rs opens");
  ok(await evalIn(`!!${TA}`), "as a code-input, which is what carries the plugins");
  await evalIn(`(() => { const t = ${TA}; t.focus();
    t.selectionStart = t.selectionEnd = t.value.indexOf("}"); return 0; })()`);
  await tab();
  ok(await until(async () => (await val()).includes("{\n\t}"), 5, "tab inserted"),
    `Tab inserts an indent — got ${JSON.stringify(await val())}`);
  ok(await evalIn(`document.activeElement === ${TA}`),
    "and focus stayed in the editor rather than moving on");
  // The edit has to travel the whole way, or the plugin has written somewhere
  // nothing reads.
  ok(
    await until(async () => (await Deno.readTextFile(`${fx.roots}/proj/keys.rs`)).includes("{\n\t}"), 10, "autosave"),
    "and the indent reaches disk through the ordinary save path",
  );

  console.log("\nC. Enter carries the indent, brackets close themselves");
  await caretToEnd();
  await cmd("Input.insertText", { text: "\nfn g() {" });
  await enter();
  ok(
    await until(async () => /fn g\(\) \{\n\t/.test(await val()), 5, "auto-indent"),
    `Enter after an opening brace indents the next line — got ${JSON.stringify((await val()).slice(-40))}`,
  );

  await caretToEnd();
  await cmd("Input.insertText", { text: "\nlet v = foo" });
  await type("(");
  ok(
    await until(async () => (await val()).endsWith("foo()"), 5, "auto-close"),
    `typing ( closes it — got ${JSON.stringify((await val()).slice(-20))}`,
  );
  // Retyping the closer must move the caret past it, not insert a second one.
  // Without this, auto-close makes every call site read `foo())`.
  await type(")");
  await sleep(150);
  ok(
    (await val()).endsWith("foo()") && !(await val()).endsWith("foo())"),
    `retyping the closing bracket steps over it — got ${JSON.stringify((await val()).slice(-20))}`,
  );

  console.log("\nD. prose is left alone — the control for B");
  ok(await open("notes.md"), "notes.md opens");
  ok(!(await evalIn(`!!${TA}`)), "as a plain textarea, so it carries no plugins");
  const before = await evalIn(`${ANY_TA}.value`);
  await evalIn(`(() => { const t = ${ANY_TA}; t.focus();
    t.selectionStart = t.selectionEnd = t.value.length; return 0; })()`);
  await tab();
  await sleep(200);
  ok(await evalIn(`${ANY_TA}.value`) === before,
    "Tab inserts nothing into prose");
  // The header calls this section the control for B, and B asserts focus
  // *stayed*. Without the matching assertion here, a build where Tab in prose
  // were swallowed outright — preventDefault, no insert, no focus move —
  // passes this section unchanged, which is the half that makes it a control.
  ok(await evalIn(`document.activeElement !== ${ANY_TA}`),
    "and focus really moved out of the textarea, as it always did");
  // Focus is put back deliberately: the Tab above moved it out of the
  // textarea, which is the point of that assertion — but it also means a
  // quote typed straight afterwards lands nowhere, and "the buffer contains
  // no `""`" then passes without a quote ever having been typed. Caught by
  // reading the value in the failure message and seeing the file unchanged.
  await evalIn(`(() => { const t = ${ANY_TA}; t.focus();
    t.selectionStart = t.selectionEnd = t.value.length; return 0; })()`);
  await type('"');
  await until(async () => (await evalIn(`${ANY_TA}.value`)).includes('"'), 5, "quote typed");
  ok(
    (await evalIn(`${ANY_TA}.value`)).endsWith('"') && !(await evalIn(`${ANY_TA}.value`)).endsWith('""'),
    `and a quote in prose does not close itself — got ${JSON.stringify((await evalIn(`${ANY_TA}.value`)).slice(-20))}`,
  );

  // The case the two assertions above cannot see. Markdown normally gets a
  // plain textarea, so "no code-input, therefore no plugins" holds whatever
  // the prose template's plugin list says — passing the plugins to it as well
  // changed nothing here, and this section claimed to be a control while
  // being unfalsifiable. Verified by making that change and watching the file
  // still print ALL PASS.
  //
  // With the non-ASCII overlay on, a prose file *does* mount a code-input, on
  // the `nonascii` template. That is the one configuration where a plugin
  // wired to the wrong template becomes visible.
  await evalIn(`setNonAsciiOn(true); 0`);
  await evalIn(`send({ t: "OpenTab", pane: 2,
    tab: { k: "File", rel: "keys.rs", mode: "Edit" } }); 0`);
  await until(async () => await evalIn(`!!${TA}`), 10, "away from notes.md");
  ok(await open("notes.md"), "notes.md reopens with the non-ASCII overlay on");
  ok(await evalIn(`!!${TA}`), "and now it really is a code-input, so a mis-wired plugin would apply");
  const proseBefore = await evalIn(`${TA}.value`);
  await evalIn(`(() => { const t = ${TA}; t.focus();
    t.selectionStart = t.selectionEnd = t.value.length; return 0; })()`);
  await tab();
  await sleep(250);
  ok(await evalIn(`${TA}.value`) === proseBefore,
    `Tab still inserts nothing into prose — got ${JSON.stringify(await evalIn(`${TA}.value`))}`);
  await evalIn(`(() => { const t = ${TA}; t.focus();
    t.selectionStart = t.selectionEnd = t.value.length; return 0; })()`);
  await type("(");
  await until(async () => (await evalIn(`${TA}.value`)).includes("("), 5, "paren typed");
  ok(
    !(await evalIn(`${TA}.value`)).includes("()"),
    `and a bracket in prose still does not close itself — got ${JSON.stringify((await evalIn(`${TA}.value`)).slice(-20))}`,
  );
  await evalIn(`setNonAsciiOn(false); 0`);

  console.log("\nD6. a missing plugin file cannot take the whole app down");
  // `codeInput.plugins` is a Proxy whose `get` trap throws ReferenceError for
  // any name it does not know, so a truthiness guard on a missing plugin does
  // not evaluate to false — it throws. `codePlugins()` runs at app.js's top
  // level, so that throw aborts the entire file: no send, no state, no
  // websocket, no editors, no terminals.
  //
  // This section pins the *reason* rather than the fix, and the difference is
  // worth stating: with every plugin file present, `codePlugins()` never
  // reaches an absent name, so swapping `in` back for a truthiness test still
  // passes here. Reproducing the failure needs a page served without one of
  // the `<script>` tags, which the fragment shape does not let a test arrange.
  // What these three assertions do guarantee is that the hazard is real, that
  // `in` is the operator that answers it, and that app.js evaluated to
  // completion — so if the guard is ever reverted, this comment is where the
  // next reader lands.
  const probe = await evalIn(`(() => {
    try { return codeInput.plugins.NoSuchPluginXyz ? "truthy" : "falsy"; }
    catch (e) { return "THREW:" + e.name; } })()`);
  ok(probe.startsWith("THREW"), `a truthiness test on an absent plugin throws — got ${probe}`);
  ok(
    await evalIn(`("NoSuchPluginXyz" in codeInput.plugins) === false`),
    "while `in` answers false, which is why the guard uses it",
  );
  // And the consequence the guard protects: app.js finished evaluating.
  ok(await evalIn(`typeof send === "function" && typeof state !== "undefined"`),
    "app.js evaluated to completion, so the workspace exists at all");

  console.log("\nD7. typing a closing brace does not duplicate it");
  // AutoCloseBrackets inserts the `}`, Indent.checkEnter splits it onto its
  // own line, and Indent.checkCloseBracket then dedents when the user types
  // `}` — while AutoCloseBrackets cannot step over a closer that is no longer
  // the next character. Measured before the fix:
  //   `if (x) ` → `{` → Enter → `}`  gave  `if (x) {\n}\n}`
  ok(await open("keys.rs"), "keys.rs reopens");
  await evalIn(`(() => { const t = ${TA}; t.focus();
    t.selectionStart = t.selectionEnd = t.value.length; return 0; })()`);
  await cmd("Input.insertText", { text: "\nif (x) " });
  await type("{");
  await enter();
  await type("}");
  await sleep(250);
  const braces = (await val()).slice(-14);
  ok(
    !/\}\s*\n\s*\}/.test(await val()),
    `no duplicated closing brace — tail was ${JSON.stringify(braces)}`,
  );

  console.log("\nE. the bytes are not touched — the constraint this all sits under");
  // Nothing is typed before this check. Opening a file must not change it,
  // and a test that edits first cannot tell a faithful save path from an edit
  // that happened to cancel out the damage.
  ok(await open("mixed.rs"), "a file mixing tabs, spaces, trailing blanks and no final newline opens");
  ok(await evalIn(`!!${TA}`), "as a code editor, so the plugins are in play");
  ok((await val()) === MIXED,
    `the buffer holds the file's exact bytes — got ${JSON.stringify(await val())}`);
  await sleep(1200); // long enough for autosave to have fired if it were going to
  ok(
    (await Deno.readTextFile(`${fx.roots}/proj/mixed.rs`)) === MIXED,
    "and opening it wrote nothing: the file on disk is byte-for-byte what it was",
  );

  // Now one edit, and only that edit. A tab is inserted into a file whose
  // other function is indented with spaces — the deliberate choice recorded
  // in codePlugins(): roost cannot know a file's convention, and a stray tab
  // is visible where stray spaces in a Makefile are not.
  await evalIn(`(() => { const t = ${TA}; t.focus();
    t.selectionStart = t.selectionEnd = t.value.indexOf("fn c()"); return 0; })()`);
  await tab();
  const expected = MIXED.replace("fn c()", "\tfn c()");
  ok(await until(async () => (await val()) === expected, 5, "single edit"),
    `exactly one tab is inserted and nothing else moves — got ${JSON.stringify(await val())}`);
  ok(
    await until(async () => (await Deno.readTextFile(`${fx.roots}/proj/mixed.rs`)) === expected, 10, "saved"),
    `and disk matches it byte for byte — got ${JSON.stringify(await Deno.readTextFile(`${fx.roots}/proj/mixed.rs`))}`,
  );
  // The half a "does it save" assertion misses: the file must still end
  // without a newline, still carry its trailing blanks, and still mix tabs
  // and spaces. A normaliser passes the equality above only if it changes
  // nothing, so this is really an explanatory assertion — but a failure here
  // names *which* property went, where the equality above only says "differs".
  const after = await Deno.readTextFile(`${fx.roots}/proj/mixed.rs`);
  ok(!after.endsWith("\n"), "no trailing newline was added");
  ok(after.includes("   \n"), "trailing whitespace on a line survived");
  ok(after.includes("\tlet t = 1;") && after.includes("    let s = 2;"),
    "and tabs and spaces both survived, unconverted");
} finally {
  if (page) page.close();
  browser.close();
  await roost.close();
}
console.log(fail ? `\n${fail} FAILED` : "\nALL PASS");
Deno.exit(fail ? 1 : 0);
