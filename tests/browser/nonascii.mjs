//! The editor's non-ASCII indicator and highlight toggle.
//!
//! Prose files (md, txt) pick up characters nobody typed: smart quotes, an
//! NBSP from a web page, an em dash from an LLM, a zero-width space with no
//! visible trace at all. The edit bar carries a button that is an indicator
//! first — accented, with a count, whenever the buffer holds anything outside
//! TAB, LF and 0x20–0x7E — and a toggle second: on, the editor becomes the
//! same <code-input> overlay a code file uses, with each non-ASCII run marked
//! instead of syntax-coloured.
//!
//! Browser test because all of it lives in app.js, and because the marks are
//! painted on a layer under the textarea: "there is a span" is not the same
//! claim as "it sits under the character it marks".
//!
//! Run: deno run -A tests/browser/nonascii.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
// Four offenders, one of each kind: a curly quote, an em dash, an NBSP (only
// visible by its width), and a zero-width space (not visible at all).
const TEXT = "it\u2019s fine\u2014mostly\u00a0so\u200bfar\nplain second line\n";
await Deno.writeTextFile(`${fx.roots}/proj/notes.txt`, TEXT);
await Deno.writeTextFile(`${fx.roots}/proj/clean.md`, "# heading\n\nall ascii here\n");
await Deno.writeTextFile(`${fx.roots}/proj/main.rs`, "fn main() {}\n");
// Past MAX_HIGHLIGHT_BYTES: the overlay is not worth repainting per keystroke
// there, so the toggle has to say so rather than silently do nothing.
await Deno.writeTextFile(`${fx.roots}/proj/big.txt`, "filler line — with a dash\n".repeat(5000));

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn(`typeof state !== "undefined" && !!(state && state.panes)`), 15, "workspace state");
  // The flag persists across page loads on purpose; a previous run must not
  // leak its final state into this one.
  await evalIn(`localStorage.removeItem("roost.nonascii")`);

  const P = '.pane[data-pane="2"]';
  const open = async (rel) => {
    await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: ${JSON.stringify(rel)}, mode: "Edit" } })`);
    return await until(async () => (await evalIn(
      `(document.querySelector('${P} .editwrap .path .rel') || {}).textContent`)) === rel, 10, rel);
  };
  const q = (sel) => evalIn(`!!document.querySelector('${P} ${sel}')`);
  const btn = () => evalIn(`JSON.stringify((() => {
    const b = document.querySelector('${P} .nonasciibtn');
    return b ? { has: b.classList.contains("has"), on: b.classList.contains("on"), text: b.textContent,
                 disabled: b.disabled, title: b.title } : null; })())`).then(JSON.parse);
  // Every character inside a mark, so a run of two counts as two.
  const marked = () => evalIn(`[...document.querySelectorAll('${P} code-input pre .nonascii')].map((e) => e.textContent).join("")`);
  const taValue = () => evalIn(`document.querySelector('${P} textarea.editor').value`);

  console.log("A. the button is an indicator before it is a toggle");
  ok(await open("notes.txt"), "notes.txt opens in an editor");
  ok(await q("textarea.editor") && !(await q("code-input")), "as a plain textarea, the toggle being off");
  ok(await until(async () => { const x = await btn(); return x && x.has; }, 10, "indicator"),
     "the edit bar carries the non-ASCII button, accented");
  let b = await btn();
  ok(b && /\b4\b/.test(b.text), `and it counts the four offenders (${JSON.stringify(b && b.text)})`);
  ok(b && !b.on, "while highlighting itself is still off");
  ok(await open("clean.md"), "clean.md opens");
  b = await btn();
  ok(b && !b.has && !/\d/.test(b.text), `a file with none shows the button muted and uncounted (${JSON.stringify(b && b.text)})`);

  console.log("\nB. toggling on paints marks under the offenders");
  ok(await open("notes.txt"), "back to notes.txt");
  await evalIn(`document.querySelector('${P} .nonasciibtn').click()`);
  ok(await until(() => q("code-input pre .nonascii"), 10, "marks"), "the editor becomes an overlay with .nonascii marks");
  ok((await btn()).on, "and the button shows it is on");
  ok((await marked()) === "\u2019\u2014\u00a0\u200b", `exactly the four offenders are marked (${JSON.stringify(await marked())})`);
  ok((await taValue()) === TEXT, "the textarea's text is untouched");
  // The overlay only works if both layers lay the glyphs out identically —
  // the same guard hledit.mjs has, because a mark that drifts off its
  // character is worse than no mark.
  const geom = JSON.parse(await evalIn(`JSON.stringify((() => {
    const el = document.querySelector('${P} code-input');
    const ta = el.querySelector("textarea"), pre = el.querySelector("pre code");
    const b = (e) => e.getBoundingClientRect();
    return { dx: Math.round(b(ta).left - b(pre).left), dy: Math.round(b(ta).top - b(pre).top),
             taH: ta.scrollHeight, codeH: pre.scrollHeight, ws: getComputedStyle(ta).whiteSpace };
  })())`));
  ok(geom.dx === 0 && geom.dy === 0 && geom.taH === geom.codeH,
     `both layers start at the same point and are the same height (${geom.dx},${geom.dy}; ${geom.taH} vs ${geom.codeH})`);
  ok(geom.ws === "pre-wrap", `and the editor still wraps (${geom.ws})`);
  // A zero-width character has no box to paint a background on. The mark has
  // to be visible anyway, and it must not gain width, or every glyph after
  // it walks off the caret.
  const zw = JSON.parse(await evalIn(`JSON.stringify((() => {
    const m = [...document.querySelectorAll('${P} code-input pre .nonascii')].find((e) => e.textContent === "\\u200b");
    if (!m) return null;
    const s = getComputedStyle(m);
    return { w: m.getBoundingClientRect().width, outline: s.outlineStyle, ow: parseFloat(s.outlineWidth) };
  })())`));
  ok(zw && zw.w === 0, `the zero-width space's mark takes no width (${zw && zw.w}px)`);
  ok(zw && zw.outline !== "none" && zw.ow > 0, `but draws an outline so it can be seen (${zw && zw.outline} ${zw && zw.ow}px)`);

  console.log("\nC. typing keeps the count and the marks current");
  await evalIn(`document.querySelector('${P} textarea.editor').focus()`);
  await cmd("Input.insertText", { text: "café " });
  ok(await until(async () => (await marked()).includes("é"), 10, "repaint"), "a typed é is marked");
  ok(await until(async () => /\b5\b/.test((await btn()).text), 10, "recount"), "and the count goes to five");
  ok(await until(async () => (await Deno.readTextFile(`${fx.roots}/proj/notes.txt`)).includes("café"), 10, "autosave"),
     "and autosave still writes the buffer, non-ASCII intact");

  console.log("\nD. the flag is one setting, shared and remembered");
  ok(await open("clean.md"), "clean.md opens");
  ok(await q("code-input"), "it too is an overlay now, the toggle being on");
  ok(!(await q("code-input pre .nonascii")), "with nothing marked");
  b = await btn();
  ok(b && b.on && !b.has, "so the button is on but unaccented: the toggle and the indicator are independent");
  ok(await open("main.rs"), "main.rs opens");
  ok(!(await q(".nonasciibtn")), "a code file has no such button — hljs owns that overlay");
  ok(await open("big.txt"), "a 130KB text file opens");
  b = await btn();
  ok(b && b.has, "past the cap the indicator still counts");
  ok(b && b.disabled && /\d+ ?KB|large|big/i.test(b.title), `but the toggle is disabled and says why (${JSON.stringify(b && b.title)})`);
  ok(!(await q("code-input")), "and the editor stays plain");
  await evalIn(`location.reload()`);
  await until(() => evalIn(`typeof state !== "undefined" && !!(state && state.panes)`), 15, "reloaded state");
  ok(await open("notes.txt"), "after a reload, notes.txt opens");
  ok(await until(() => q("code-input pre .nonascii"), 10, "marks after reload"), "still highlighted: the setting survived");

  console.log("\nE. toggling off is a plain textarea again, still counted");
  await evalIn(`document.querySelector('${P} .nonasciibtn').click()`);
  ok(await until(async () => (await q("textarea.editor")) && !(await q("code-input")), 10, "plain"), "back to a plain textarea");
  b = await btn();
  ok(b && b.has && !b.on && /\b5\b/.test(b.text), `the indicator keeps its accent and count (${JSON.stringify(b && b.text)})`);
  const visible = await evalIn(`getComputedStyle(document.querySelector('${P} textarea.editor')).color`);
  ok(!/rgba\(0, 0, 0, 0\)|transparent/.test(visible), `and its text is visible (${visible})`);

  console.log("\nF. a change on disk under a clean buffer recounts");
  // The third way the text under the button changes: not typed here, not
  // mounted, but pushed by the server as a BufferText when the file is
  // written from outside — a Claude editing it, say. The buffer is clean by
  // now (section C's autosave landed before section D), so this arrives as a
  // BufferText and not a BufferStale.
  ok(await until(() => evalIn(`!(state.buffers.find((x) => x.rel === "notes.txt") || {}).dirty`), 10, "clean"),
     "the buffer is clean before the file is touched");
  const onDisk = await Deno.readTextFile(`${fx.roots}/proj/notes.txt`);
  await Deno.writeTextFile(`${fx.roots}/proj/notes.txt`, onDisk + "\u00ab\u00bb\n");
  ok(await until(async () => (await taValue()).endsWith("\u00ab\u00bb\n"), 10, "external text"),
     "the editor picks up the text written from outside");
  ok(await until(async () => /\b7\b/.test((await btn()).text), 10, "external recount"),
     `and the count follows it to seven (${JSON.stringify((await btn()).text)})`);
} finally {
  page?.close();
  browser.close();
  await roost.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
