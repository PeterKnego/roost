//! Project search: the ⇧⇧ overlay, its results, and landing on a line.
//!
//! Every line of the trigger, the overlay and the scroll lives in
//! static/app.js, where `cargo test` cannot reach — the same reason
//! dotfiles.mjs and paneicons.mjs exist. The server half is covered by Rust
//! tests; all of it can be correct while the overlay never opens, the rows
//! render as markup, or the editor opens at the top of the file.
//!
//! Two code reviews found seven-plus defects in this feature by reading
//! alone (a sequence-guard race on erase, scroll code that targeted an
//! element that never scrolls, a focus leak into a mirroring browser…). Every
//! section below exists because one of those was a real, specific way the
//! code could be wrong — not because it looked thorough to include.
//!
//! Run: deno run -A tests/browser/search.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

// --- fixture ---------------------------------------------------------------

const fx = await fixture();
await Deno.mkdir(`${fx.roots}/proj/src`, { recursive: true });

// A basic content hit, matching the plan's own worked example: query
// "marker" finds exactly one line, at line 3.
await Deno.writeTextFile(
  `${fx.roots}/proj/src/needle.rs`,
  "one\ntwo\nlet marker = 1;\nfour\n",
);

// XSS, both halves. A path and a matched line that are both markup: if either
// is interpolated rather than set as text, this file renders an element
// instead of characters — CLAUDE.md records an escaping test whose fixture
// had no metacharacter and so could not have caught this.
await Deno.writeTextFile(`${fx.roots}/proj/src/<img src=x onerror=1>.txt`, "hello\n");
await Deno.writeTextFile(`${fx.roots}/proj/src/xss.txt`, "before\n<img src=x onerror=1>\nafter\n");

// A long file with three widely separated, uniquely-named markers, used by
// the scroll sections below. Filler lines carry none of the marker
// substrings, so each query hits exactly one line.
const longLines = [];
for (let i = 1; i <= 260; i++) longLines.push(`fn filler_${i}() {}\n`);
longLines[149] = "let mirrortarget_9f3 = 150;\n"; // line 150
longLines[199] = "let farline_9f3 = 200;\n";      // line 200
longLines[244] = "let editortarget_9f3 = 245;\n"; // line 245
await Deno.writeTextFile(`${fx.roots}/proj/src/long.rs`, longLines.join(""));

// A second long file, opened only via a terminal-link-style OpenPath, so the
// preview-scroll section always exercises a first-ever fetch rather than a
// repeat click of an already-mounted tab.
const previewLines = [];
for (let i = 1; i <= 300; i++) previewLines.push(`// filler ${i}\n`);
await Deno.writeTextFile(`${fx.roots}/proj/src/preview.rs`, previewLines.join(""));

// A deliberately deep path, so the path column must truncate something. The
// directory half is 38 characters against a 22ch column; the filename and
// `:1` are 9 and must survive whole.
await Deno.mkdir(`${fx.roots}/proj/src/very/deeply/nested/directory/tree`, { recursive: true });
await Deno.writeTextFile(
  `${fx.roots}/proj/src/very/deeply/nested/directory/tree/deep.rs`,
  "let deepneedle = 1;\n",
);

const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
const url = `http://127.0.0.1:${resh.port}/proj`;
let page1, page2;

// A query typed into #searchinput, dispatched as a real input event.
const setQuery = (q) => `(() => {
  const i = document.getElementById("searchinput");
  i.value = ${JSON.stringify(q)};
  i.dispatchEvent(new Event("input", { bubbles: true }));
})()`;

// Two Shift keydowns dispatched on the document. Kept for the *later* sections,
// which only need the overlay open and do not care how it got there — but note
// what it does not prove: `document.dispatchEvent` calls document listeners
// directly, so it exercises neither the bubble path a real key takes nor the
// double-tap window, since both calls land in the same millisecond. Section A
// uses `realShiftTwice` for exactly that reason.
const shiftTwice = `(() => {
  const k = () => document.dispatchEvent(new KeyboardEvent("keydown", { key: "Shift", bubbles: true }));
  k(); k();
  return !document.getElementById("searchoverlay").hidden;
})()`;

/// One real Shift press-and-release, through the browser's own input pipeline.
async function realShift(page) {
  const k = { key: "Shift", code: "ShiftLeft", windowsVirtualKeyCode: 16, nativeVirtualKeyCode: 16 };
  await page.cmd("Input.dispatchKeyEvent", { type: "rawKeyDown", modifiers: 8, ...k });
  await page.cmd("Input.dispatchKeyEvent", { type: "keyUp", modifiers: 0, ...k });
}

/// Two of them `gap` ms apart, answering whether the overlay opened.
async function realShiftTwice(page, gap) {
  await realShift(page);
  await sleep(gap);
  await realShift(page);
  await sleep(150);
  return !(await page.evalIn(`document.getElementById("searchoverlay").hidden`));
}

// Closes whatever is open (idempotent) and reopens with a fresh query, so
// each section starts from a known state instead of layering onto whatever
// the previous one left behind.
async function freshSearch(evalIn, q) {
  await evalIn(`closeSearch()`);
  await evalIn(shiftTwice);
  await evalIn(setQuery(q));
}

// The element mounted for an already-open editor tab showing `rel`, found by
// its breadcrumb — the only place the rel exists once the textarea replaces
// the fetched fragment.
const editorMountedFor = (rel) => `(() => {
  const c = [...document.querySelectorAll(".pane .content")].find((c) => {
    const n = c.querySelector(".editwrap .path .rel");
    return n && n.textContent === ${JSON.stringify(rel)};
  });
  return !!(c && c.querySelector("textarea.editor"));
})()`;

// The scrollable box a reveal actually moves: code-input's host (see
// scrollEditorTo's own `host` in static/app.js) when the file is
// highlighted, the bare textarea otherwise — mirrored here so a wrong scroll
// target fails this, not a coincidence in the arithmetic. Returns null when
// the pane or the editor isn't there at all; callers comparing this against
// a baseline must check for null explicitly; Math.abs(null - x) is a real
// (positive) number in JS, not a signal that something is missing.
const boxScrollTop = (rel) => `(() => {
  const c = [...document.querySelectorAll(".pane .content")].find((c) => {
    const n = c.querySelector(".editwrap .path .rel");
    return n && n.textContent === ${JSON.stringify(rel)};
  });
  if (!c) return null;
  const ta = c.querySelector("textarea.editor");
  if (!ta) return null;
  const box = ta.closest("code-input") || ta;
  return box.scrollTop;
})()`;

// Whether the character at the start of `line` in `rel`'s *highlighted* file
// is currently within its scrollable host's viewport — an absolute check,
// not a before/after delta: a delta can tell "moved" from "didn't move" but
// not "moved to the right place" from "moved to the top of the file", and a
// regression that zeroes the scroll can still produce a large delta from a
// nonzero starting point. Reuses app.js's own caretRect() (a bare top-level
// function, reachable the same way send() and onEvent() are elsewhere in
// this file) rather than reimplementing the walk, so this can't drift from
// what scrollEditorTo() itself measures — and it returns null, not a
// numeric guess, while the highlight is still catching up, exactly
// mirroring scrollEditorTo's own "not yet" signal.
const lineInView = (rel, line) => `(() => {
  const c = [...document.querySelectorAll(".pane .content")].find((c) => {
    const n = c.querySelector(".editwrap .path .rel");
    return n && n.textContent === ${JSON.stringify(rel)};
  });
  if (!c) return null;
  const ta = c.querySelector("textarea.editor");
  if (!ta) return null;
  const host = ta.closest("code-input");
  const pre = host && host.querySelector("pre");
  if (!pre || pre.textContent.length < ta.value.length) return null;
  const lines = ta.value.split("\\n");
  const upto = lines.slice(0, ${line} - 1).join("\\n").length + (${line} > 1 ? 1 : 0);
  const rect = caretRect(pre, upto);
  if (!rect) return null;
  const hostRect = host.getBoundingClientRect();
  return rect.top >= hostRect.top && rect.bottom <= hostRect.bottom;
})()`;

try {
  page1 = await openPage(browser.port, url);
  page2 = await openPage(browser.port, url);
  const { evalIn } = page1;
  await page1.cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await page2.cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn("ctrl && ctrl.readyState === 1 && !!state"), 30, "page one's app");
  await until(() => page2.evalIn("ctrl && ctrl.readyState === 1 && !!state"), 30, "page two's app");

  console.log("A. the trigger");
  // Real key events, through CDP's input pipeline — NOT
  // `document.dispatchEvent`. This section used to do the latter, which invokes
  // document listeners directly and therefore proves only that a listener is
  // registered: it travels no capture/bubble path and, because the two calls
  // land microseconds apart, it cannot see the double-tap window at all. It
  // passed green while ⇧⇧ was unusable in a real browser at a human tapping
  // speed, which is how the bug was reported rather than caught here.
  ok(await realShiftTwice(page1, 90), "⇧⇧ (real key events, fast tap) opens the overlay");
  await evalIn(`closeSearch()`);
  // The gap a person actually produces when they mean it, rather than the
  // hurried one. 400 ms used to be the window and this failed at 450.
  ok(await realShiftTwice(page1, 520), "…and at a deliberate 520 ms tap, not just a hurried one");
  ok(
    await evalIn(`(() => { closeSearch(); return document.getElementById("searchoverlay").hidden; })()`),
    "closeSearch() is callable as a global, and hides the overlay",
  );
  // A Shift with a key between two Shifts must not open it — that is what stops
  // typing "HI" from opening search. Real events again: the whole question is
  // whether the H reaches the same listener the Shifts do.
  await realShift(page1);
  await page1.cmd("Input.dispatchKeyEvent", { type: "keyDown", key: "H", code: "KeyH", text: "H", windowsVirtualKeyCode: 72 });
  await page1.cmd("Input.dispatchKeyEvent", { type: "keyUp", key: "H", code: "KeyH", windowsVirtualKeyCode: 72 });
  await realShift(page1);
  await sleep(150);
  ok(
    await evalIn(`document.getElementById("searchoverlay").hidden`),
    "an intervening keystroke resets the pending Shift, so ordinary typing cannot open it",
  );

  // ⌘⇧F / Ctrl+Shift+F — the second way in, added because ⇧⇧ was reported as
  // not working in a real browser for reasons still unknown. Real key events
  // again: a synthetic dispatch would not exercise the modifier state at all.
  const chordF = async (mods) => {
    await evalIn(`closeSearch()`);
    await sleep(100);
    for (const type of ["keyDown", "keyUp"]) {
      await page1.cmd("Input.dispatchKeyEvent", {
        type, key: "F", code: "KeyF", windowsVirtualKeyCode: 70, nativeVirtualKeyCode: 70, modifiers: mods,
      });
    }
    await sleep(150);
    return !(await evalIn(`document.getElementById("searchoverlay").hidden`));
  };
  // CDP modifier bits: 1 alt, 2 ctrl, 4 meta/cmd, 8 shift.
  // Both modifiers, on every platform: the handler accepts either, so a
  // browser whose `navigator.platform` misreports cannot silently lose the
  // shortcut. Asserting only the "right" one for this host would not have
  // caught the platform-sniff gate this replaced.
  ok(await chordF(2 | 8), "Ctrl+Shift+F opens the overlay");
  ok(await chordF(4 | 8), "…and \u2318\u21e7F does too, without depending on platform detection");
  // The unshifted chord must NOT be bound: that is the whole reason for
  // choosing the shifted one. A terminal encodes Ctrl-F and Ctrl-Shift-F
  // identically, so leaving plain Ctrl-F alone is what keeps readline's
  // forward-char working at a shell prompt. If this ever opens the overlay,
  // ^F has been taken from every terminal in the app.
  ok(
    !(await chordF(2)) && !(await chordF(4)),
    "…and the UNshifted chord is left alone, so ^F still reaches the shell",
  );
  ok(await evalIn(`document.getElementById("searchoverlay").hidden`), "…and the overlay is still closed after that");

  console.log("\nB. Escape restores focus");
  await evalIn(`(() => {
    const el = document.createElement("input");
    el.id = "__focusprobe";
    document.body.appendChild(el);
    el.focus();
  })()`);
  const focusedBefore = await evalIn(`document.activeElement.id === "__focusprobe"`);
  ok(focusedBefore, "setup: the probe element holds focus before the overlay opens");
  await evalIn(shiftTwice);
  ok(await evalIn(`document.activeElement.id === "searchinput"`), "opening moves focus into the search box");
  await evalIn(`document.activeElement.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }))`);
  ok(await evalIn(`document.getElementById("searchoverlay").hidden`), "Escape closes the overlay");
  ok(
    await evalIn(`document.activeElement.id === "__focusprobe"`),
    "…and gives focus back to exactly the element that had it before — not merely 'somewhere other than the input'",
  );

  // The overlay has exactly one focusable element, so focus leaves it on one
  // Tab or on any click that is neither a row nor the backdrop. With the key
  // handler bound to #searchoverlay, Escape and ↑/↓ went with it, and
  // openSearch()'s `!ov.hidden` early return meant ⇧⇧ could not recover:
  // the modal was stranded open. Reproduced here by blurring rather than by
  // a synthetic Tab, because a dispatched KeyboardEvent does not move focus —
  // a Tab-based version of this test would pass with the bug fully present.
  await evalIn(shiftTwice);
  ok(await evalIn(`document.activeElement.id === "searchinput"`), "setup: the overlay is open with focus in its input");
  await evalIn(`document.getElementById("searchinput").blur()`);
  ok(
    await evalIn(`!document.getElementById("searchoverlay").contains(document.activeElement)`),
    "setup: focus is now outside the overlay entirely — the state one Tab or a stray click produces",
  );
  await evalIn(`document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }))`);
  ok(
    await evalIn(`document.getElementById("searchoverlay").hidden`),
    "Escape still closes the overlay once focus has left it — otherwise only a backdrop click can, and ⇧⇧ cannot reopen",
  );

  console.log("\nC0. a content hit lands on its line");
  await freshSearch(evalIn, "marker");
  ok(
    await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length > 0`), 10, "marker results"),
    "a content query returns rows",
  );
  const openedLine = `(() => {
    const rows = [...document.querySelectorAll("#searchresults .searchrow")];
    const hit = rows.find((n) => n.textContent.includes("src/needle.rs:3"));
    if (!hit) return "no content hit for needle.rs:3";
    hit.click();
    return "clicked";
  })()`;
  ok(await evalIn(openedLine) === "clicked", "the row naming needle.rs:3 exists and is clickable");
  ok(
    await until(() => evalIn(
      `(() => { const ta = document.querySelector("textarea.editor");
         if (!ta) return false;
         const upto = ta.value.slice(0, ta.selectionStart).split("\\n").length;
         return upto === 3; })()`,
    ), 15, "line 3 selected"),
    "the editor opens with line 3 selected, not the top of the file",
  );

  console.log("\nC. XSS (both halves) and index alignment across group headers");
  await evalIn(`(() => {
    if (!window.__origSend) window.__origSend = window.send;
    window.__sent = [];
    window.send = (m) => { window.__sent.push(m); return window.__origSend(m); };
  })()`);
  await freshSearch(evalIn, "onerror");
  ok(
    await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length >= 2`), 10, "onerror rows"),
    "the query matches both a file name and a content line",
  );
  ok(
    await evalIn(`document.querySelectorAll("#searchresults .searchgroup").length >= 2`),
    "…rendered as two separate groups (Files and Contents)",
  );
  ok(
    await evalIn(`document.querySelectorAll("#searchresults img").length === 0`),
    "markup in a path or a matched line renders as text, never as an element",
  );
  ok(
    await evalIn(`[...document.querySelectorAll("#searchresults .searchrow")]
       .some((n) => n.textContent.includes("<img src=x onerror=1>"))`),
    "…and the characters are actually visible, proving the row rendered at all rather than being dropped",
  );
  // The first row of the SECOND group, not the first row overall: a group
  // header is a child of #searchresults too, and an off-by-one caused by
  // counting it is invisible if a test only ever clicks the first row.
  const secondGroupClick = await evalIn(`(() => {
    const headers = [...document.querySelectorAll("#searchresults .searchgroup")];
    if (headers.length < 2) return "not enough groups";
    const row = headers[1].nextElementSibling;
    if (!row || !row.classList.contains("searchrow")) return "no row after the second header";
    row.click();
    return "clicked";
  })()`);
  ok(secondGroupClick === "clicked", "the second group's first row is a real, clickable row");
  const sent = JSON.parse(await evalIn(`JSON.stringify(window.__sent)`));
  const lastOpenAtLine = [...sent].reverse().find((m) => m.t === "OpenAtLine");
  ok(
    !!lastOpenAtLine && lastOpenAtLine.rel === "src/xss.txt" && lastOpenAtLine.line === 2,
    `clicking the second group's first row carries THAT row's target, not some other row's ` +
      `(got ${JSON.stringify(lastOpenAtLine)})`,
  );
  ok(
    await until(() => evalIn(
      `(() => { const ta = document.querySelector("textarea.editor");
         if (!ta) return false;
         const upto = ta.value.slice(0, ta.selectionStart).split("\\n").length;
         return upto === 2; })()`,
    ), 15, "xss.txt line 2 selected"),
    "…and the editor actually lands on that line",
  );

  console.log("\nD. the sequence guard's erase path");
  await freshSearch(evalIn, "marker");
  ok(
    await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length > 0`), 10, "baseline results"),
    "baseline: a query in flight gets a real reply",
  );
  const seqAtSend = await evalIn(`searchSeq`);
  await evalIn(setQuery(""));
  ok(
    await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length === 0`), 5, "cleared"),
    "clearing the query empties the result list",
  );
  // Simulate the "ma" query's reply landing late, after the user already
  // cleared the box. If the erase path didn't bump searchSeq (or the guard in
  // onEvent's SearchResults case were removed), this would silently repaint
  // the list the user already emptied.
  const afterStaleReply = await evalIn(`(() => {
    onEvent({
      t: "SearchResults",
      seq: ${JSON.stringify(seqAtSend)},
      results: { files: [{ rel: "src/needle.rs" }], lines: [], sessions: [], outcome: { state: "Complete" }, unreadable: 0 },
    });
    return document.querySelectorAll("#searchresults .searchrow").length;
  })()`);
  ok(afterStaleReply === 0, "a stale reply for the query the user already cleared is ignored, not repainted");
  await evalIn(`closeSearch()`);

  console.log("\nE. Preview scroll targets .content, not the <pre>");
  await evalIn(`send({ t: "OpenPath", text: "src/preview.rs:250" })`);
  ok(
    await until(() => evalIn(`(() => {
      const c = [...document.querySelectorAll(".pane .content")]
        .find((c) => c.dataset.url && c.dataset.url.includes("path=src%2Fpreview.rs"));
      return !!(c && c.querySelector("pre.codeview"));
    })()`), 15, "preview.rs fetched and mounted"),
    "the terminal-link-style open fetches and mounts the file fresh — the first-open path, not a repeat click",
  );
  ok(
    await until(() => evalIn(`(() => {
      const c = [...document.querySelectorAll(".pane .content")]
        .find((c) => c.dataset.url && c.dataset.url.includes("path=src%2Fpreview.rs"));
      return c && c.scrollTop > 0;
    })()`), 15, "content scrolled"),
    "the pane's .content actually scrolled",
  );
  ok(
    await evalIn(`(() => {
      const c = [...document.querySelectorAll(".pane .content")]
        .find((c) => c.dataset.url && c.dataset.url.includes("path=src%2Fpreview.rs"));
      const pre = c.querySelector("pre.codeview");
      return pre.scrollTop === 0;
    })()`),
    "…and not the <pre> itself, which has auto height and can never scroll vertically",
  );

  console.log("\nF. Editor scroll from a content hit — first open of a long file");
  // A background tab's requestAnimationFrame is throttled to the point of
  // never firing in this headless Chromium — measured directly: with two
  // pages open (as this file needs for section H below), the non-active one
  // never got past code-input's initial, unhighlighted <pre>, no matter how
  // long the test waited. code-input's own highlighting runs entirely off
  // its own perpetual rAF loop (confirmed by reading code-input.min.js), so
  // scrollEditorTo's measurement — and a real backgrounded browser tab, for
  // that matter — has nothing to measure until the tab is actually visible.
  // This is not a workaround for a test artifact: it is what the browser
  // does to any inactive tab, and a real user driving page one is looking at
  // it, so it is foreground for them by definition.
  await page1.cmd("Page.bringToFront");
  await freshSearch(evalIn, "farline_9f3");
  ok(
    await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length > 0`), 10, "farline row"),
    "the far-line marker is found",
  );
  await evalIn(`document.getElementById("searchoverlay")
    .dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }))`);
  ok(await until(() => evalIn(editorMountedFor("src/long.rs")), 15, "long.rs editor mount"),
     "long.rs opens in the editor");
  ok(
    await until(async () => (await evalIn(boxScrollTop("src/long.rs"))) > 0, 15, "scrolled"),
    "the scrollable host (code-input, for a highlighted file) actually scrolled to bring line 200 into view",
  );

  console.log("\nG. THE case: searching within a file already open AND focused in Edit");
  await page1.cmd("Page.bringToFront"); // see F's comment; still page one throughout this section
  await evalIn(`(() => {
    const c = [...document.querySelectorAll(".pane .content")].find((c) => {
      const n = c.querySelector(".editwrap .path .rel");
      return n && n.textContent === "src/long.rs";
    });
    c.querySelector("textarea.editor").focus();
  })()`);
  ok(await evalIn(editorMountedFor("src/long.rs")) && await evalIn(`document.activeElement.classList.contains("editor")`),
     "setup: long.rs's textarea is open and focused before the search");
  // Line 245 is not in view yet: section F left the host showing roughly
  // line 200, a third of the way down its viewport, and 245 is well past
  // that window. Asserted as a setup fact, not assumed, so the assertion
  // below can only pass because a scroll actually happened.
  ok(!(await evalIn(lineInView("src/long.rs", 245))), "setup: line 245 is not yet in view");
  // ⇧⇧ dispatched on the focused textarea itself, exactly as a real keypress
  // would arrive, bubbling up to the document-level listener.
  await evalIn(`(() => {
    const ta = document.activeElement;
    const k = () => ta.dispatchEvent(new KeyboardEvent("keydown", { key: "Shift", bubbles: true }));
    k(); k();
  })()`);
  ok(await evalIn(`!document.getElementById("searchoverlay").hidden`), "⇧⇧ opens search from inside a focused editor");
  await evalIn(setQuery("editortarget_9f3"));
  ok(
    await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length > 0`), 10, "editortarget row"),
    "the near-the-end marker is found",
  );
  await evalIn(`document.getElementById("searchoverlay")
    .dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }))`);
  // activateSearchRow calls closeSearch() first, which restores focus to the
  // pre-overlay element — the SAME textarea, already focused, so ta.focus()
  // fires no focus event here. scrollEditorTo() (static/app.js) does not
  // need one: it measures the highlighted <pre>'s own rendered text and
  // scrolls code-input's host directly, regardless of focus state. This
  // section is what proves that — before scrollEditorTo existed, this case
  // had no mechanism left to move the viewport at all once native
  // focus-driven scrolling turned out to require an actual focus
  // transition.
  ok(
    await until(() => evalIn(lineInView("src/long.rs", 245)), 15, "line 245 in view"),
    "searching a line near the end of an already-open, already-focused file scrolls it into view",
  );

  console.log("\nH. mirroring: a reveal on page one scrolls page two, without stealing its focus");
  await page2.evalIn(`document.activeElement.setAttribute("data-focusmark", "yes")`);
  const before2 = await page2.evalIn(boxScrollTop("src/long.rs"));
  ok(before2 !== null, "setup: page two already mirrors long.rs in its editor pane");
  await freshSearch(evalIn, "mirrortarget_9f3");
  ok(
    await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length > 0`), 10, "mirrortarget row"),
    "the mirror-target marker is found on page one",
  );
  await evalIn(`document.getElementById("searchoverlay")
    .dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }))`);
  // Page two is the one being measured now, so it needs to be the visible
  // tab for its own code-input highlighting to run — see F's comment. This
  // also means the mirrored scroll genuinely depends on page two's *own*
  // rendering, not on whatever page one already computed, which is the
  // point of a mirroring test.
  await page2.cmd("Page.bringToFront");
  ok(
    await until(async () => {
      // Bound once and checked explicitly: boxScrollTop() returns null while
      // the pane or its editor is missing (mountTab deletes and rebuilds
      // .content on every remount, and the State this reveal broadcasts
      // remounts page two's pane), and Math.abs(null - before2) is a real,
      // positive number in JS — enough to satisfy a bare `> 5` check with no
      // editor present on page two at all.
      const v = await page2.evalIn(boxScrollTop("src/long.rs"));
      return v !== null && Math.abs(v - before2) > 5;
    }, 15, "page two scrolled"),
    "a reveal driven by page one scrolls page two's mirrored editor too",
  );
  ok(
    await page2.evalIn(`document.activeElement.getAttribute("data-focusmark") === "yes"`),
    "…without moving page two's focus — a real defect here triggers an autosave via the other user's blur listener",
  );

  console.log("\nI. the honesty line — every outcome renderSearch() can build a note from");
  async function isBlocked(path) {
    try { for await (const _e of Deno.readDir(path)) { /* just probing */ } return false; }
    catch { return true; }
  }

  await freshSearch(evalIn, "zzzznotpresentzzzz");
  ok(
    await until(() => evalIn(`document.getElementById("searchnote").textContent === "no matches"`), 10, "no-matches note"),
    "(a) zero rows and nothing unreadable says exactly 'no matches'",
  );

  // renderSearch() switches on `results.outcome.state` (static/app.js), and
  // Failed/Truncated are real server outcomes (src/search.rs's Outcome enum)
  // that no suite anywhere else can reach — the Rust side has its own tests
  // for producing them, but turning them into this specific text is
  // JS-only. Injected the same way section D's stale-reply check is: real
  // rows are fetched and then cleared first, so the seq this lands on has no
  // real request in flight to race against (the erase path never sends one),
  // and every injection below reuses that same seq since nothing here types
  // again to bump it.
  await freshSearch(evalIn, "marker");
  ok(
    await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length > 0`), 10, "warm seq"),
    "setup: a real query round-trips before the injected ones, so the seq below is a real one",
  );
  await evalIn(setQuery(""));
  ok(
    await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length === 0`), 5, "cleared"),
    "setup: cleared without sending a request, so the current seq has none in flight",
  );
  const honestySeq = await evalIn(`searchSeq`);

  await evalIn(`onEvent({ t: "SearchResults", seq: ${JSON.stringify(honestySeq)}, results: {
    files: [], lines: [], sessions: [], outcome: { state: "Failed", msg: "disk gremlins" }, unreadable: 0,
  } })`);
  ok(
    await evalIn(`document.getElementById("searchnote").textContent`) === "search failed: disk gremlins",
    "(d) a Failed outcome reports the server's own message, verbatim",
  );

  await evalIn(`onEvent({ t: "SearchResults", seq: ${JSON.stringify(honestySeq)}, results: {
    files: [{ rel: "a.rs" }], lines: [], sessions: [], outcome: { state: "Truncated", reason: "stopped after 1500 ms" }, unreadable: 0,
  } })`);
  ok(
    await evalIn(`document.getElementById("searchnote").textContent`) === "partial results — stopped after 1500 ms",
    "(e) a Truncated outcome names the cap that fired, alongside the rows it did find",
  );

  // The case a regression that always appends *something* to the note would
  // not be caught by any of the above: real rows, nothing wrong at all.
  await evalIn(`onEvent({ t: "SearchResults", seq: ${JSON.stringify(honestySeq)}, results: {
    files: [{ rel: "a.rs" }], lines: [], sessions: [], outcome: { state: "Complete" }, unreadable: 0,
  } })`);
  ok(
    await evalIn(`document.getElementById("searchnote").textContent`) === "",
    "(f) rows present and nothing wrong leaves the note empty, not padded with a spurious caveat",
  );

  // Contents are not searched below three characters (wsconn.rs sets
  // Query::contents from `q.chars().count() >= 3`). That is a decision, not a
  // result, and the server has no way to report it: it returns Complete,
  // truthfully, for the categories it did search. Without the client-side
  // note the UI asserts completeness over a category nobody opened — the same
  // class of lie as (a)-(f) above, on the "chose not to look" side rather
  // than the "could not look" side. These use real typed queries, not
  // injected events: the note reports what the server did for the query the
  // client actually sent, so it is built from the query recorded at send time
  // (`searchSentQuery`). An injected SearchResults carries no query and would
  // be described by whatever the last real send happened to leave behind.
  const SHORT = "contents searched from 3 characters";
  const noteText = `document.getElementById("searchnote").textContent`;

  await freshSearch(evalIn, "lo");   // matches src/long.rs by path
  ok(
    await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length > 0`), 10, "lo rows"),
    "(g) setup: a two-character query still answers paths, so the note below is not just 'no matches'",
  );
  ok(
    await until(async () => (await evalIn(noteText)) === SHORT, 10, "short-query note"),
    `(g) a two-character query says contents were not searched, and says only that — got ${JSON.stringify(await evalIn(noteText))}`,
  );

  await freshSearch(evalIn, "zz");   // matches nothing at all
  ok(
    await until(async () => (await evalIn(noteText)) === `no matches · ${SHORT}`, 10, "composed note"),
    `(h) it composes with the note that was already there rather than replacing it — got ${JSON.stringify(await evalIn(noteText))}`,
  );

  // The discriminating half: at exactly three characters contents *are*
  // searched, so the note must go away. Without this a regression that
  // appended the line unconditionally would pass (g) and (h) both.
  await freshSearch(evalIn, "mar");
  ok(
    await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length > 0`), 10, "mar rows"),
    "(i) setup: a three-character query answers, so the empty note below is a real answer and not a blank screen",
  );
  ok(
    await evalIn(noteText) === "",
    "(i) at three characters contents ARE searched, so the caveat is gone — the threshold is 3, not 'always'",
  );

  // (j) The note describes the query the SERVER ran, not whatever the box
  // holds by the time the answer lands. Typing races the 120 ms debounce:
  // a two-character query's results can arrive when the box already holds
  // three, and a note read from the live input then contradicts the rows
  // under it. Driven by setting the value with no `input` event — so no new
  // query is sent — and then delivering the reply the two-character query is
  // still waiting on. Reading the live input scores this as a 6-character
  // query and drops the caveat, so this fails without the fix.
  await freshSearch(evalIn, "lo");
  ok(
    await until(async () => (await evalIn(noteText)) === SHORT, 10, "two-char note"),
    "(j) setup: the two-character query's caveat is showing",
  );
  const raceSeq = await evalIn(`searchSeq`);
  await evalIn(`document.getElementById("searchinput").value = "marker"`);
  await evalIn(`onEvent({ t: "SearchResults", seq: ${JSON.stringify(raceSeq)}, results: {
    files: [{ rel: "src/long.rs" }], lines: [], sessions: [], outcome: { state: "Complete" }, unreadable: 0,
  } })`);
  ok(
    await evalIn(noteText) === SHORT,
    "(j) the caveat still describes the query the results came from, not the characters now in the box",
  );

  await evalIn(`closeSearch()`);

  const locked1 = `${fx.dir}/locked1`;
  const locked2 = `${fx.dir}/locked2`;
  await Deno.mkdir(locked1, { recursive: true });
  await Deno.mkdir(locked2, { recursive: true });
  try {
    await Deno.chmod(locked1, 0o000);
    if (!(await isBlocked(locked1))) {
      console.log("  SKIP  running as root — chmod 000 does not block reads, so (b) and (c) would be vacuous");
    } else {
      await freshSearch(evalIn, "needle");
      ok(
        await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow").length > 0`), 10, "needle rows"),
        "(b) setup: the query also returns real rows, not just the honesty line",
      );
      ok(
        await until(() => evalIn(`document.getElementById("searchnote").textContent.includes("1 place could not be read")`), 10, "unreadable=1 note"),
        "(b)+(c@1) a query with rows AND an unreadable place says both — singular 'place'",
      );

      await Deno.chmod(locked2, 0o000);
      if (!(await isBlocked(locked2))) {
        console.log("  SKIP  running as root — chmod 000 does not block reads, so (c@2) would be vacuous");
      } else {
        await freshSearch(evalIn, "needle");
        ok(
          await until(() => evalIn(`document.getElementById("searchnote").textContent.includes("2 places could not be read")`), 10, "unreadable=2 note"),
          "(c@2) two unreadable places — plural 'places'",
        );
      }
    }
  } finally {
    // Restore before cleanup, or the temp dir cannot be removed.
    try { await Deno.chmod(locked1, 0o755); } catch { /* best effort */ }
    try { await Deno.chmod(locked2, 0o755); } catch { /* best effort */ }
  }

// --- the selected row is visible on the panel it sits on --------------------
//
// Kept last, because it swaps the theme stylesheet out from under the page.
//
// This is here because reading the CSS did not reveal the bug and a review
// did not catch it: `.searchrow.sel` was `--tool`, a token mixed against
// `--bg`, while `.searchpanel` is `--bg2` — so the selection was painted
// *darker* than its own surface, 1.12:1 in the dark theme, and which row was
// selected became a guess. The obvious repair, `--tab-on`, is also wrong for
// the same reason (it is `--tool` plus 10% `--fg`): it measures 1.02:1 here.
//
// So the assertion has to be about the rendered colour, in both polarities,
// and not about which token is named. getComputedStyle returns an unresolved
// `oklab(...)` for a color-mix, so the value is rasterised through a canvas —
// the engine's own resolution to sRGB.
//
// Both wrong versions were applied and run, not reasoned about. `var(--tool)`
// (the shipped bug): 5 failures — four themes the wrong direction, and dark
// also under the floor at 1.125. `var(--tab-on)` (the repair the backlog
// prescribed): 7 failures, down to 1.023 in dark. Restored: ALL PASS.
//
// Note which one the light theme lets through: `--tool` mixes toward black,
// which in a light theme *is* toward --fg, so it passes there. That is why
// the loop covers every theme in static/themes/ rather than the default one —
// a single-theme version of this test would have shipped the bug green.
{
  const probe = `(() => {
    const cx = document.createElement("canvas").getContext("2d", { willReadFrequently: true });
    const srgb = (css) => { cx.clearRect(0,0,1,1); cx.fillStyle = "#000"; cx.fillStyle = css;
      cx.fillRect(0,0,1,1); const d = cx.getImageData(0,0,1,1).data; return [d[0],d[1],d[2]]; };
    const lum = (c) => { const [r,g,b] = c.map(v => { v/=255; return v<=0.04045 ? v/12.92 : Math.pow((v+0.055)/1.055,2.4); });
      return 0.2126*r + 0.7152*g + 0.0722*b; };
    const el = document.querySelector(".searchrow.sel");
    if (!el) return JSON.stringify({ err: "no selected row" });
    const row = lum(srgb(getComputedStyle(el).backgroundColor));
    const panel = lum(srgb(getComputedStyle(document.querySelector(".searchpanel")).backgroundColor));
    // The --fg token, not documentElement's \`color\`: the themes set the
    // custom property and colour the body, so \`color\` on the root reads as
    // the UA default and inverts this check on every dark theme.
    const fg = lum(srgb(getComputedStyle(document.documentElement).getPropertyValue("--fg").trim()));
    return JSON.stringify({
      ratio: +(((Math.max(row,panel)+0.05)/(Math.min(row,panel)+0.05)).toFixed(3)),
      // The rule that holds in a light theme as well as a dark one: the
      // highlight moves away from the surface, toward the text colour.
      towardFg: fg > panel ? row > panel : row < panel,
    });
  })()`;

  await freshSearch(evalIn, "marker");
  ok(await until(() => evalIn(`!!document.querySelector(".searchrow.sel")`), 10, "a selected row"),
     "a row is selected, so the colours below are read off a real selection");

  for (const theme of ["darcula", "dark", "light", "gruvbox", "solarized-dark"]) {
    await evalIn(`(() => { document.querySelector('link[href*="/static/themes/"]').href = "/static/themes/${theme}.css"; return 1; })()`);
    // The swapped sheet has to be applied before the pixels mean anything;
    // the panel's own colour changing is the signal that it is.
    await until(async () => {
      const r = JSON.parse(await evalIn(probe));
      return !r.err && r.ratio > 1;
    }, 10, `${theme} applied`);
    const r = JSON.parse(await evalIn(probe));
    ok(r.towardFg, `${theme}: selection moves toward --fg, not away from it (ratio ${r.ratio})`);
    // 1.20 is just above the app's own selected-tab step (--tab-on against
    // --tool measures 1.15-1.24); a modal's keyboard selection should not
    // read weaker than a tab.
    ok(r.ratio >= 1.20, `${theme}: selection is at least 1.20:1 against the panel (got ${r.ratio})`);
  }
}

// --- one left edge, and a path that truncates from the left ----------------
//
// The complaint this answers: a row used to run `path:line` then the text in
// one span, so the matched content began at a different x on every row —
// after `CLAUDE.md:148` on one, after a 60-character spec path on the next.
// There was nothing to run the eye down.
{
  await freshSearch(evalIn, "marker");
  ok(await until(() => evalIn(`document.querySelectorAll("#searchresults .searchrow .what").length > 0`), 10, "rows"),
     "setup: rows render with a .what cell");

  // Revert-and-observe (CLAUDE.md's testing discipline): with
  // `grid-template-columns: auto 1fr` — the pre-fix sizing — this query
  // still printed "ok every result's text starts at one x (saw [473])".
  // The fixture's "marker" query returns exactly one row (only needle.rs
  // contains it), so a single-row grid is trivially at one x regardless of
  // column sizing; this assertion does not by itself discriminate the fix.
  // What DID fail under `auto 1fr` was the dirClipped assertion below:
  // "FAIL  the directory half of a long path is the part that truncates" —
  // with an auto column, `.dir` grows to fit its own content instead of
  // being squeezed by a fixed track, so it never clips. That is the
  // assertion this section actually depends on to catch a regression.
  const lefts = JSON.parse(await evalIn(`JSON.stringify(
    [...document.querySelectorAll("#searchresults .searchrow .what")]
      .map(n => Math.round(n.getBoundingClientRect().left)))`));
  ok(new Set(lefts).size === 1,
     `every result's text starts at one x (saw ${JSON.stringify([...new Set(lefts)])})`);

  // The long-path case: `.dir` may be clipped, `.base` may not — it carries
  // the filename and the line number, the only parts that identify the hit.
  await freshSearch(evalIn, "deepneedle");
  await until(() => evalIn(`!!document.querySelector("#searchresults .searchrow .base")`), 10, "a deep row");
  // `.base` is `flex: none`, so it is sized to its own content by
  // definition — `base.scrollWidth <= base.clientWidth` can never be false,
  // which is why the original brief's `baseWhole` probe (that comparison)
  // was rejected: it cannot fail no matter what the CSS does. This checks
  // geometry against `.at` (the fixed column) instead, plus the literal
  // text, so a `.base` that overflowed the column would actually be caught.
  const deep = JSON.parse(await evalIn(`(() => {
    const r = document.querySelector("#searchresults .searchrow");
    const at = r.querySelector(".at"), dir = r.querySelector(".dir"), base = r.querySelector(".base");
    return JSON.stringify({ dirClipped: dir.scrollWidth > dir.clientWidth,
                            baseInside: base.getBoundingClientRect().right <= at.getBoundingClientRect().right + 1,
                            baseText: base.textContent });
  })()`));
  ok(deep.dirClipped, "the directory half of a long path is the part that truncates");
  ok(deep.baseInside && deep.baseText === "deep.rs:1",
     `the filename and line survive intact and inside the column (got "${deep.baseText}")`);
}

} finally {
  try { page1 && page1.close(); } catch { /* already gone */ }
  try { page2 && page2.close(); } catch { /* already gone */ }
  browser.close();
  await resh.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nALL PASS" : `\n${fail} FAILED`);
Deno.exit(fail === 0 ? 0 : 1);
