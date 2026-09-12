//! A code file's editor and preview carry line numbers in a gutter.
//!
//! Issue #11 item 2. The interesting half is not drawing numbers, it is that
//! this editor soft-wraps: `.editwrap code-input` sets `white-space: pre-wrap`
//! (static/style.css) because "long lines are not worth a horizontal scroll in
//! a 600px pane", so one logical line is not one visual row, and a gutter of
//! 1, 2, 3… drifts out of alignment at the first long line. Numbering visual
//! rows instead would disagree with every other line number in the system —
//! the compiler's, the terminal's, RevealLine's.
//!
//! Two things make this hard to test honestly, and both shaped this file:
//!
//! A pseudo-element has no getBoundingClientRect, `getComputedStyle(el,
//! "::before").content` hands back the literal `counter(ln)` rather than the
//! numeral, and the accessibility tree does not carry it either (both checked
//! here, not assumed). So section B measures the ::before through CDP's
//! DOM.getBoxModel, which can address a pseudo-element node directly.
//!
//! And geometry alone would still pass with `content` emptied — the box would
//! be there, blank. That is CLAUDE.md's strip-test failure ("no glyph
//! assertion, so swapping ● and ○ left them all green") waiting to happen, so
//! section B also screenshots the gutter column and finds the bands of ink in
//! it. A gutter that numbered visual rows puts those bands in the wrong
//! places; a gutter that numbers nothing has none.
//!
//! Run: deno run -A tests/browser/gutter.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture({ autosave: false });
const MAIN = `fn main() {\n    let mut n = 0;\n    // a comment\n    println!("hello");\n}\n`;
await Deno.writeTextFile(`${fx.roots}/proj/main.rs`, MAIN);
// Line 2 is far wider than the pane and must wrap; lines 3-5 follow it, so a
// gutter that counted visual rows would misplace every one of them.
const WIDE = `fn main() {\n    // ${"a very long trailing comment ".repeat(14)}\n    let x = 1;\n    let y = 2;\n}\n`;
await Deno.writeTextFile(`${fx.roots}/proj/wide.rs`, WIDE);
// Past 99 lines, so the gutter has to widen for a third digit.
await Deno.writeTextFile(`${fx.roots}/proj/many.rs`, "let a = 0;\n".repeat(120));
await Deno.writeTextFile(`${fx.roots}/proj/view.rs`, MAIN);
await Deno.writeTextFile(`${fx.roots}/proj/notes.md`, "# heading\n\nprose\n\n```rust\nfn f() {}\nfn g() {}\n```\n");
// Past MAX_HIGHLIGHT_BYTES: a plain textarea, with no layer to number.
await Deno.writeTextFile(`${fx.roots}/proj/big.rs`, "// filler line\n".repeat(22000));

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { cmd, evalIn } = page;
  await cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await cmd("DOM.enable", {});
  await cmd("Page.enable", {});
  await until(() => evalIn(`typeof state !== "undefined" && !!(state && state.panes)`), 15, "workspace state");

  const PANE = '.pane[data-pane="2"]';
  const open = async (rel, mode) => {
    await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: ${JSON.stringify(rel)}, mode: ${JSON.stringify(mode)} } })`);
    // An editor's breadcrumb is `.path > .rel` (mountEditor builds it); a
    // preview's is the bare `.path` div render::file_fragment emits. Waiting
    // on only the first silently never fires for a Preview tab.
    return await until(async () => (await evalIn(
      `((document.querySelector('${PANE} .path .rel') || document.querySelector('${PANE} .path')) || {}).textContent`
    )) === rel, 10, rel);
  };
  const count = (sel) => evalIn(`document.querySelectorAll('${PANE} ${sel}').length`).then(Number);
  // Always wait for the exact count, never for "more than none": the
  // highlighted layer passes through a one-line state on its way up, and a
  // `> 0` wait catches that instead of the finished editor.
  const lines = (sel, n) => until(() => count(sel).then((c) => c === n), 10, `${n} spans in ${sel}`);

  /// Where each line's number is really painted, measured through CDP because
  /// a pseudo-element is invisible to every geometry API the page itself has.
  /// One row per `.ln`: the span's own bounding top — which for a wrapped
  /// inline span is its FIRST visual row — and its ::before's top, in the
  /// same coordinate space.
  const numberBoxes = async (sel) => {
    const { result: doc } = await cmd("DOM.getDocument", { depth: -1 });
    const { result: found } = await cmd("DOM.querySelectorAll",
      { nodeId: doc.root.nodeId, selector: `${PANE} ${sel} .ln` });
    const rows = [];
    for (const nodeId of found.nodeIds) {
      const { result: desc } = await cmd("DOM.describeNode", { nodeId });
      const before = (desc.node.pseudoElements || []).find((p) => p.pseudoType === "before");
      let spanTop = null, numTop = null;
      try { spanTop = (await cmd("DOM.getBoxModel", { nodeId })).result.model.content[1]; } catch { /* not laid out */ }
      if (before) {
        try { numTop = (await cmd("DOM.getBoxModel", { backendNodeId: before.backendNodeId })).result.model.content[1]; }
        catch { /* no box at all */ }
      }
      rows.push({ spanTop, numTop });
    }
    return rows;
  };

  /// The horizontal bands of ink in the gutter column, as `{top, height}`
  /// relative to the top of the element — i.e. where numerals are actually
  /// painted. Screenshotted through CDP and decoded on a canvas in the page,
  /// because nothing short of the pixels can tell a drawn number from an
  /// empty box of the right size.
  const inkBands = async (sel) => {
    const clip = JSON.parse(await evalIn(`JSON.stringify((() => {
      const el = document.querySelector('${PANE} ${sel}');
      if (!el) return null;
      const r = el.getBoundingClientRect();
      // The gutter is the left padding of whichever box actually carries it:
      // the <code-input> host has padding 0 !important from the vendor
      // stylesheet and pushes --padding-left down onto its layers, while a
      // preview's <pre> carries its own. Clipping that strip takes in the
      // numbers and the 10px gap, and stops short of the first glyph.
      const inner = el.querySelector('code');
      const pad = Math.max(parseFloat(getComputedStyle(el).paddingLeft) || 0,
                           inner ? parseFloat(getComputedStyle(inner).paddingLeft) || 0 : 0);
      return { x: Math.round(r.left), y: Math.round(r.top), width: Math.round(pad),
               height: Math.round(Math.min(r.height, 600)) }; })())`));
    if (!clip || !clip.width) return [];   // no gutter reserved: nothing to look at
    const raw = await cmd("Page.captureScreenshot",
      { format: "png", clip: { ...clip, scale: 1 }, fromSurface: true, captureBeyondViewport: false });
    if (!raw.result) throw new Error("captureScreenshot: " + JSON.stringify(raw).slice(0, 300) + " clip=" + JSON.stringify(clip));
    const shot = raw.result;
    const bands = await evalIn(`(async () => {
      const img = new Image();
      img.src = "data:image/png;base64,${shot.data}";
      await img.decode();
      const c = document.createElement("canvas");
      c.width = img.width; c.height = img.height;
      const g = c.getContext("2d");
      g.drawImage(img, 0, 0);
      const d = g.getImageData(0, 0, c.width, c.height).data;
      const bg = [d[0], d[1], d[2]];                     // the gutter's own ground
      const rows = [];
      for (let y = 0; y < c.height; y++) {
        let n = 0, lo = 1e9, hi = -1;
        for (let x = 0; x < c.width; x++) {
          const i = (y * c.width + x) * 4;
          if (Math.abs(d[i] - bg[0]) + Math.abs(d[i + 1] - bg[1]) + Math.abs(d[i + 2] - bg[2]) > 60) {
            n++; if (x < lo) lo = x; if (x > hi) hi = x;
          }
        }
        rows.push({ n, lo, hi });
      }
      const out = [];
      let start = -1, lo = 1e9, hi = -1;
      for (let y = 0; y <= rows.length; y++) {
        const on = y < rows.length && rows[y].n > 0;
        if (on) { if (start < 0) start = y; lo = Math.min(lo, rows[y].lo); hi = Math.max(hi, rows[y].hi); }
        if (!on && start >= 0) {
          // The ink's horizontal extent is how many digits were drawn, which
          // is the only thing here that can tell 1, 2, 3 apart from 0, 0, 0.
          out.push({ top: start, height: y - start, width: hi - lo + 1 });
          start = -1; lo = 1e9; hi = -1;
        }
      }
      return JSON.stringify(out); })()`);
    return JSON.parse(bands);
  };

  // A file of N lines that ends in a newline has N+1 rows in the textarea —
  // the caret can sit on that last empty one — so it has N+1 numbers. The
  // extra "\n" code-input appends to the value before highlighting is a
  // different thing and is deliberately NOT numbered (wrapLines drops a
  // trailing chunk with no text in it), or every file would show a phantom.
  const ROWS = 6;

  console.log("A. a code file's editor gets one number per logical line");
  ok(await open("main.rs", "Edit"), "main.rs opens in an editor");
  ok(await lines("code-input pre .ln", ROWS), `its five lines and its last empty row are numbered (${ROWS} spans)`);
  // Not just "spans exist": each one has to hold its own line. Splitting on
  // the wrong boundary, or dropping hljs's re-opened tags, shows up here.
  const texts = JSON.parse(await evalIn(`JSON.stringify(
    [...document.querySelectorAll('${PANE} code-input pre .ln')].map((e) => e.textContent))`));
  ok(JSON.stringify(texts) === JSON.stringify(MAIN.split("\n").map((l, i, a) => i < a.length - 1 ? l + "\n" : "\n")),
     "and each span holds exactly its own line, newline included");
  ok(await count("code-input pre .hljs-keyword") >= 2,
     "hljs's own tokens survived being split across lines");

  console.log("\nB. the numbers follow wrapped lines, not visual rows");
  ok(await open("wide.rs", "Edit"), "wide.rs opens");
  ok(await lines("code-input pre .ln", ROWS), "five logical lines and a last row are numbered");
  // Every read past this point tolerates a missing gutter and reports a
  // failure, rather than throwing and taking the remaining sections with it —
  // which is exactly what a run against the un-fixed code has to do to be
  // worth reading.
  const rows2 = Number(await evalIn(
    `(document.querySelector('${PANE} code-input pre .ln:nth-child(2)') || { getClientRects: () => [] })
       .getClientRects().length`));
  ok(rows2 > 1, `line 2 really does wrap (it occupies ${rows2} visual rows)`);
  const lh = Number(await evalIn(
    `parseFloat(getComputedStyle(document.querySelector('${PANE} code-input pre')).lineHeight)`));
  const boxes = await numberBoxes("code-input pre");
  const deltas = boxes.map((b) => (b.spanTop == null || b.numTop == null ? null : Math.round(b.numTop - b.spanTop)));
  ok(deltas.length === ROWS && deltas.every((d) => d !== null && Math.abs(d) <= 4),
     `every number sits on its own line's first row (offsets ${JSON.stringify(deltas)})`);
  // The discriminating measurement. Numbering visual rows would put line 3's
  // number one row under line 2's, while line 2's text runs a dozen rows
  // further down — so this gap has to clear the whole wrapped line.
  const at = (i) => (boxes[i] && boxes[i].numTop != null ? boxes[i].numTop : null);
  const gap = at(2) != null && at(1) != null ? at(2) - at(1) : null;
  ok(gap !== null && gap > lh * 1.5,
     `line 3's number clears the whole of wrapped line 2 (${gap === null ? "no numbers" : Math.round(gap) + "px"} vs one row of ${Math.round(lh)}px)`);
  const step = at(3) != null && at(2) != null ? at(3) - at(2) : null;
  ok(step !== null && Math.abs(step - lh) <= 2,
     "while two unwrapped lines stay exactly one row apart");
  // And the numerals are actually painted, not merely positioned.
  const bands = await inkBands("code-input pre");
  ok(bands.length === ROWS, `${ROWS} numerals are painted in the gutter (found ${bands.length})`);
  const bandGap = bands.length > 2 ? bands[2].top - bands[1].top : 0;
  ok(bandGap > lh * 1.5,
     `and the painted ink skips the wrapped rows too (${Math.round(bandGap)}px between the 2nd and 3rd)`);

  console.log("\nC. the gutter never reaches the bytes");
  ok(await open("main.rs", "Edit"), "back to main.rs");
  await lines("code-input pre .ln", ROWS);
  // The exact invariant scrollEditorTo gates on: code-input appends one "\n"
  // to the value before highlighting, so a synced layer is one character
  // longer. Wrapping lines in spans must not disturb it, or every RevealLine
  // into a fresh editor retries until its 4-second banner fires.
  const [preLen, taLen] = JSON.parse(await evalIn(`JSON.stringify((() => {
    const el = document.querySelector('${PANE} code-input');
    return [el.querySelector("pre").textContent.length, el.querySelector("textarea").value.length]; })())`));
  ok(preLen === taLen + 1, `the highlighted layer still holds exactly the file's text (${preLen} = ${taLen} + 1)`);
  ok(await evalIn(`scrollEditorTo(document.querySelector('${PANE} code-input textarea'), 20)`) === true,
     "so scrollEditorTo's sync guard passes instead of asking to be retried");
  // And the bytes themselves, through the real save path.
  await evalIn(`(() => { const t = document.querySelector('${PANE} code-input textarea');
                         t.focus(); t.setSelectionRange(0, 0); })()`);
  await cmd("Input.insertText", { text: "// typed\n" });
  await sleep(400);
  await evalIn(`saveNow("main.rs")`);
  ok(await until(async () => (await Deno.readTextFile(`${fx.roots}/proj/main.rs`)) === "// typed\n" + MAIN, 10, "the saved file"),
     "a save writes the file with no trace of the gutter in it");

  console.log("\nD. the gutter is sized to the file");
  const gutterOf = () => evalIn(
    `getComputedStyle(document.querySelector('${PANE} code-input')).getPropertyValue("--ln-gutter").trim()`);
  ok(await open("many.rs", "Edit"), "many.rs (120 lines) opens");
  await lines("code-input pre .ln", 121);
  const three = await gutterOf();
  ok(await open("wide.rs", "Edit"), "and wide.rs (5 lines)");
  await lines("code-input pre .ln", ROWS);
  const one = await gutterOf();
  ok(three !== one && three.includes("3ch") && one.includes("2ch"),
     `a three-digit file reserves more room than a one-digit one (${three} vs ${one})`);
  // The numerals have to *differ*, not merely exist. Dropping
  // `counter-increment` paints a 0 on every line: the same count of bands in
  // the same places, all one digit wide. Widths are what sees it.
  ok(await open("many.rs", "Edit"), "many.rs again, where the numbers pass 9");
  await lines("code-input pre .ln", 121);
  const widths = [...new Set((await inkBands("code-input pre")).map((b) => b.width))].sort((a, b) => a - b);
  ok(widths.length >= 2,
     `the painted numbers grow as they count (ink widths ${JSON.stringify(widths)})`);
  // Both layers, or the colours walk off the caret.
  const pads = JSON.parse(await evalIn(`JSON.stringify((() => {
    const el = document.querySelector('${PANE} code-input');
    return [getComputedStyle(el.querySelector("textarea")).paddingLeft,
            getComputedStyle(el.querySelector("pre code")).paddingLeft]; })())`));
  ok(pads[0] === pads[1] && parseFloat(pads[0]) > 10,
     `the textarea and the highlighted layer moved together (${pads.join(" / ")})`);

  console.log("\nE. only the surfaces that have lines get numbered");
  ok(await open("view.rs", "Preview"), "view.rs in Preview");
  // The same count as the editor gives the same file, which is the point:
  // code-input appends a newline of its own and a preview does not, so
  // without gutterFor accounting for it these two disagree by one and the
  // last number changes as you switch modes.
  ok(await lines("pre.codeview .ln", ROWS), `a code preview is numbered too, and identically (${ROWS})`);
  ok(await open("notes.md", "Preview"), "notes.md in Preview");
  await until(() => evalIn(`!!document.querySelector('${PANE} article.markdown-body')`), 10, "markdown");
  await sleep(300);
  ok(await count("pre code .ln") === 0,
     "a markdown preview's fenced block is not — its numbers would be nobody's line numbers");
  ok(await open("big.rs", "Edit"), "big.rs (over the highlight cap) opens");
  await until(() => count("textarea.editor").then((n) => n === 1), 10, "a plain editor");
  ok(await count("code-input") === 0, "a file too large to highlight stays a plain textarea");
  ok(await count(".ln") === 0, "and gets no gutter, rather than one that cannot line up");
} finally {
  page?.close();
  browser.close();
  await roost.close();
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
