//! A code file keeps its syntax highlighting while you edit it.
//!
//! Preview highlights (render::file_fragment hands hljs a <pre>), edit mode
//! did not: it was a bare textarea, so switching to edit turned the colours
//! off. The editor is now a <code-input> — the same textarea with a
//! highlighted <pre> painted under it — for code files only. Markdown has a
//! rendered preview and is edited as prose; plaintext has nothing to colour.
//!
//! What makes this worth a browser test rather than a unit test: the whole
//! feature is two stacked layers agreeing on where every glyph is. Nothing in
//! Rust can see that, and "it highlighted" is not the same claim as "the
//! colours land under the caret".
//!
//! Run: deno run -A tests/browser/hledit.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const CODE = `fn main() {\n    let mut n = 0;\n    // a comment\n    println!("hello");\n}\n`;
await Deno.writeTextFile(`${fx.roots}/proj/main.rs`, CODE);
await Deno.writeTextFile(`${fx.roots}/proj/notes.md`, "# heading\n\nprose\n");
// Past MAX_HIGHLIGHT_BYTES, where highlighting every keystroke would cost
// more than it is worth.
await Deno.writeTextFile(`${fx.roots}/proj/big.rs`, "// filler line\n".repeat(22000));
// A line far wider than the pane, plus one long unbroken token: the first
// must wrap, the second cannot, and the two layers have to agree either way.
await Deno.writeTextFile(`${fx.roots}/proj/wide.rs`,
  `fn main() {}\n// ${"a very long comment ".repeat(30)}\nconst B: &str = "${"QUJDRA".repeat(70)}";\n`);

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn(`typeof state !== "undefined" && !!(state && state.panes)`), 15, "workspace state");

  const open = async (rel) => {
    await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: ${JSON.stringify(rel)}, mode: "Edit" } })`);
    return await until(async () => (await evalIn(
      `(document.querySelector('.pane[data-pane="2"] .editwrap .path .rel') || {}).textContent`)) === rel, 10, rel);
  };
  const q = (sel) => evalIn(`!!document.querySelector('.pane[data-pane="2"] ${sel}')`);

  console.log("A. a code file is highlighted in the editor");
  ok(await open("main.rs"), "main.rs opens in an editor");
  ok(await q("code-input"), "the editor is a code-input");
  ok(await until(() => q("code-input pre .hljs-keyword"), 10, "highlighting"),
     "hljs has painted tokens under it (.hljs-keyword)");
  // Not just "some spans exist": the *right* text is coloured. Swapping the
  // language attribute for a wrong one leaves spans behind but not these.
  const tokens = JSON.parse(await evalIn(`JSON.stringify(
    [...document.querySelectorAll('.pane[data-pane="2"] code-input pre .hljs-keyword')].map((e) => e.textContent))`));
  ok(tokens.includes("fn") && tokens.includes("let"), `and they are Rust's (${JSON.stringify(tokens)})`);
  ok((await evalIn(`document.querySelector('.pane[data-pane="2"] code-input pre').textContent`)).includes("// a comment"),
     "the highlighted layer carries the file's own text");

  console.log("\nB. the two layers agree on where the glyphs are");
  // The failure this guards is the one that makes an overlay editor unusable:
  // colours drifting away from the caret. Both layers must lay text out
  // identically, which means the same font, size, line-height and padding —
  // so compare the boxes, not the CSS.
  // Against `pre code`, not `pre`: code-input puts the padding on the inner
  // code element, so that — not its wrapper — is the box the glyphs are laid
  // out in. Comparing the wrapper reports a 10px padding difference that is
  // not a defect.
  const geom = JSON.parse(await evalIn(`JSON.stringify((() => {
    const el = document.querySelector('.pane[data-pane="2"] code-input');
    const ta = el.querySelector("textarea"), pre = el.querySelector("pre code");
    const cs = (e) => { const s = getComputedStyle(e);
      return [s.fontFamily, s.fontSize, s.lineHeight, s.paddingLeft, s.paddingTop, s.tabSize, s.whiteSpace].join("|"); };
    const b = (e) => e.getBoundingClientRect();
    return { same: cs(ta) === cs(pre), ta: cs(ta), pre: cs(pre),
             dx: Math.round(b(ta).left - b(pre).left), dy: Math.round(b(ta).top - b(pre).top) };
  })())`));
  // What these two actually catch: the vendored stylesheet not being loaded
  // at all — the easy thing to forget when adding a vendored asset. Verified
  // by dropping its <link> from render.rs, which puts the layers 153px apart.
  // They do *not* catch a bad font override, because code-input forces
  // `font-size: inherit !important` on both layers; the assertion below is
  // what covers that.
  ok(geom.same, `both layers compute the same font metrics\n        textarea ${geom.ta}\n        pre      ${geom.pre}`);
  ok(geom.dx === 0 && geom.dy === 0, `and start at the same point (${geom.dx}px, ${geom.dy}px apart)`);
  // code-input's own default is bare `monospace`, so without this app's font
  // override a code editor would render in a different typeface from every
  // other editor and terminal in the window while still being self-consistent.
  const font = JSON.parse(await evalIn(`JSON.stringify((() => {
    const norm = (v) => v.replace(/\\s+/g, "");
    return { editor: norm(getComputedStyle(document.querySelector('.pane[data-pane="2"] code-input textarea')).fontFamily),
             app: norm(getComputedStyle(document.documentElement).getPropertyValue("--mono")) };
  })())`));
  ok(font.editor === font.app, `and use the app's own mono font (${font.editor.slice(0, 40)}…)`);

  console.log("\nC. typing repaints the colours");
  await evalIn(`document.querySelector('.pane[data-pane="2"] code-input textarea').focus()`);
  await cmd("Input.insertText", { text: "\nstruct Added;\n" });
  ok(await until(async () => (await evalIn(
    `document.querySelector('.pane[data-pane="2"] code-input pre').textContent`)).includes("struct Added"), 10, "repaint"),
     "text typed into the textarea reaches the highlighted layer");
  ok(await until(async () => JSON.parse(await evalIn(`JSON.stringify(
       [...document.querySelectorAll('.pane[data-pane="2"] code-input pre .hljs-keyword')].map((e) => e.textContent))`))
       .includes("struct"), 10, "new token"), "and is highlighted, not just copied");
  // The edit still has to travel: highlighting must not have swallowed the
  // input event the whole save path hangs off.
  ok(await until(async () => (await Deno.readTextFile(`${fx.roots}/proj/main.rs`)).includes("struct Added"), 10, "autosave"),
     "and autosave still writes it to disk");

  console.log("\nD. only code files, and only up to a size");
  ok(await open("notes.md"), "notes.md opens in an editor");
  ok(!(await q("code-input")), "markdown stays a plain textarea");
  ok(await q("textarea.editor"), "and it is still an editor");
  ok(await open("big.rs"), "a 330KB source file opens");
  ok(!(await q("code-input")), "past the cap it stays a plain textarea too");
  // The failure mode if that fallback ever went wrong: code-input paints the
  // textarea's own glyphs transparent, so a plain textarea that kept those
  // styles would be an invisible editor.
  const visible = await evalIn(
    `getComputedStyle(document.querySelector('.pane[data-pane="2"] textarea.editor')).color`);
  ok(!/rgba\(0, 0, 0, 0\)|transparent/.test(visible), `and its text is visible (${visible})`);
  console.log("\nE. long lines do not wrap, and both layers lay out identically");
  ok(await open("wide.rs"), "a file with lines wider than the pane opens");
  await until(() => q("code-input pre .hljs-keyword"), 10, "highlighting");
  const wrap = JSON.parse(await evalIn(`JSON.stringify((() => {
    const el = document.querySelector('.pane[data-pane="2"] code-input');
    const ta = el.querySelector("textarea"), code = el.querySelector("pre code");
    const pane = el.closest(".pane");
    const ln = el.querySelector("pre .ln");
    // Where this layer's first glyph actually lands, against where the
    // textarea's text starts. Widths cannot show this: the textarea stretches
    // to whatever the <pre> needs, so a shifted layer still matches in size.
    const tw = document.createTreeWalker(ln, NodeFilter.SHOW_TEXT);
    const t = tw.nextNode(), r = document.createRange();
    r.setStart(t, 0); r.setEnd(t, 1);
    const glyphX = r.getBoundingClientRect().left;
    const taTextX = ta.getBoundingClientRect().left + parseFloat(getComputedStyle(ta).paddingLeft);
    return { taW: ta.scrollWidth, codeW: code.scrollWidth,
             taH: ta.scrollHeight, codeH: code.scrollHeight,
             hostScrollW: el.scrollWidth, hostW: el.clientWidth,
             hostRight: el.getBoundingClientRect().right, paneRight: pane.getBoundingClientRect().right,
             ws: getComputedStyle(ta).whiteSpace,
             lnPos: ln ? getComputedStyle(ln, "::before").position : null, glyphX, taTextX,
             bg: getComputedStyle(code).backgroundColor, hostBg: getComputedStyle(el).backgroundColor };
  })())`));
  // Wrapped code broke at every hyphen and continued at column 0, reading as
  // a new statement; a hanging indent is impossible over a single textarea.
  // So a code file scrolls instead. Fails on the pre-wrap this replaced.
  ok(wrap.ws === "pre", `the editor does not wrap (white-space: ${wrap.ws})`);
  // The property everything else rests on: one `white-space` on the element
  // reaches both layers, because each takes it by inherit. If only one layer
  // wrapped, the colours would sit lines away from the text. Equal sizes
  // say nothing about the sticky line numbers, though: see the glyph check.
  ok(wrap.taH === wrap.codeH && wrap.taW === wrap.codeW,
     `both layers lay out identically (${wrap.taW}x${wrap.taH} vs ${wrap.codeW}x${wrap.codeH})`);
  // The host is code-input's only scroller, so it is the thing that must
  // overflow — not the pane, which a max-content layer once pushed to 4739px.
  ok(wrap.hostScrollW > wrap.hostW, `the editor scrolls sideways (${wrap.hostScrollW}px in a ${wrap.hostW}px editor)`);
  ok(wrap.hostRight <= wrap.paneRight + 1,
     `and stays inside its pane (editor right ${Math.round(wrap.hostRight)} vs pane right ${Math.round(wrap.paneRight)})`);
  ok(wrap.lnPos === "sticky", `line numbers stay in view while scrolled (::before position: ${wrap.lnPos})`);
  // The sticky number sits in the <pre>'s flow, so its margins must net its
  // inline advance to zero. Watched failing with margin-right raised by 36px:
  // every size check above stayed green, the text sat 36px right of the caret.
  ok(Math.abs(wrap.glyphX - wrap.taTextX) < 0.5,
     `and take no room from the text (first glyph at ${wrap.glyphX.toFixed(1)}, textarea text at ${wrap.taTextX.toFixed(1)})`);
  // A highlight.js theme brings its own background; over this app's it reads
  // as a second, slightly different shade that stops where the text stops.
  ok(/rgba\(0, 0, 0, 0\)|transparent/.test(wrap.bg),
     `and the theme paints no background of its own over the pane's (${wrap.bg} on ${wrap.hostBg})`);
} finally {
  page?.close();
  browser.close();
  await roost.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
