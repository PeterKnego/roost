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
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until }
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

// Two real Shift keydowns on the document, not a call to openSearch(): an
// overlay wired to nothing is exactly the defect this file exists to catch.
const shiftTwice = `(() => {
  const k = () => document.dispatchEvent(new KeyboardEvent("keydown", { key: "Shift", bubbles: true }));
  k(); k();
  return !document.getElementById("searchoverlay").hidden;
})()`;

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

// The scrollable box a reveal actually moves: code-input's host when the
// file is highlighted, the bare textarea otherwise (revealInEditor's own
// `box` variable, mirrored here so a wrong scroll target fails this, not a
// coincidence in the arithmetic).
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

try {
  page1 = await openPage(browser.port, url);
  page2 = await openPage(browser.port, url);
  const { evalIn } = page1;
  await page1.cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await page2.cmd("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn("ctrl && ctrl.readyState === 1 && !!state"), 30, "page one's app");
  await until(() => page2.evalIn("ctrl && ctrl.readyState === 1 && !!state"), 30, "page two's app");

  console.log("A. the trigger");
  ok(await evalIn(shiftTwice), "⇧⇧ opens the overlay");
  ok(
    await evalIn(`(() => { closeSearch(); return document.getElementById("searchoverlay").hidden; })()`),
    "closeSearch() is callable as a global, and hides the overlay",
  );
  // A single Shift, and a Shift with a key between two Shifts, must not open it —
  // that is what stops typing "HI" from opening search.
  const notOpened = `(() => {
    const k = (key) => document.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
    k("Shift"); k("H"); k("Shift");
    return document.getElementById("searchoverlay").hidden;
  })()`;
  ok(await evalIn(notOpened), "an intervening keystroke resets the pending Shift, so ordinary typing cannot open it");
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
  const beforeG = await evalIn(boxScrollTop("src/long.rs"));
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
  // pre-overlay element — the SAME textarea, already focused, so no focus
  // event fires and revealInEditor's focus()-driven native scroll has
  // nothing to trigger it. Whether the viewport still moves rests entirely
  // on setSelectionRange scrolling an already-focused textarea. This is the
  // case with no arithmetic fallback at all.
  ok(
    await until(async () => Math.abs((await evalIn(boxScrollTop("src/long.rs"))) - beforeG) > 5, 15, "viewport moved"),
    `searching a line near the end of an already-open, already-focused file still moves the viewport ` +
      `(before=${beforeG})`,
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
    await until(async () => Math.abs((await page2.evalIn(boxScrollTop("src/long.rs"))) - before2) > 5, 15, "page two scrolled"),
    "a reveal driven by page one scrolls page two's mirrored editor too",
  );
  ok(
    await page2.evalIn(`document.activeElement.getAttribute("data-focusmark") === "yes"`),
    "…without moving page two's focus — a real defect here triggers an autosave via the other user's blur listener",
  );

  console.log("\nI. the honesty line, all four states");
  async function isBlocked(path) {
    try { for await (const _e of Deno.readDir(path)) { /* just probing */ } return false; }
    catch { return true; }
  }

  await freshSearch(evalIn, "zzzznotpresentzzzz");
  ok(
    await until(() => evalIn(`document.getElementById("searchnote").textContent === "no matches"`), 10, "no-matches note"),
    "(a) zero rows and nothing unreadable says exactly 'no matches'",
  );

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
} finally {
  try { page1 && page1.close(); } catch { /* already gone */ }
  try { page2 && page2.close(); } catch { /* already gone */ }
  browser.close();
  await resh.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nALL PASS" : `\n${fail} FAILED`);
Deno.exit(fail === 0 ? 0 : 1);
