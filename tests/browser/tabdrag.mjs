//! Dragging a tab: between panes, and within one to reorder.
//!
//! Every line of this lives in static/app.js, where `cargo test` cannot
//! reach. The server needs no change at all — `Intent::MoveTab { from, idx,
//! to, at }` already carries a destination *index*, and the ⇄ button simply
//! always passes the destination's length — so the whole feature is the
//! client deciding `at` from where the pointer is. Nothing in Rust can verify
//! that decision.
//!
//! Traps this file is written against (see README):
//!
//!   - **`draggable` is asserted separately from the handlers.** The drops
//!     below are dispatched as real `DragEvent`s carrying a real
//!     `DataTransfer`, which exercises the handlers faithfully but would pass
//!     just as well against tabs the browser would never let you pick up.
//!     Section A asserts the attribute; the rest assert the behaviour.
//!   - **Every assertion is on *which* tab moved, never on a count.** A count
//!     is equally satisfied by moving the wrong tab, which is the whole
//!     failure mode of an off-by-one in `at`.
//!   - **The same-pane cases use three tabs, not two.** With two, "insert
//!     before" and "insert after" produce the same array whichever way the
//!     index adjustment goes, so a reorder test on two tabs cannot fail.
//!   - **Section D drives both drags over the same target, in both orders.**
//!     roost already uses drag-and-drop for file upload, and a tab handler
//!     that did not check the type would swallow a file drop — uploads would
//!     stop working with no error anywhere. Asserting only that a tab drag
//!     works would never see it.
//!
//! Revert-checked, each change applied and watched to fail: dropping the
//! same-pane index adjustment, ignoring the pointer position and always
//! appending, un-setting `draggable`, removing the drop indicator, and a tab
//! drop handler that claims every drop before checking the type.
//!
//! One reversion deliberately does *not* fail, and it is worth knowing why
//! rather than papering over: deleting the type check from the tab `dragover`
//! handler alone changes no behaviour, because `dragTabSource` is null during
//! a file drag and the handler returns before it prevents anything. That is
//! defence in depth rather than a gap in this file — the check that does
//! carry the weight is the one in `drop`, and R6 above pins it.
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
  for (const f of ["a.txt", "b.txt", "c.txt"]) {
    await Deno.writeTextFile(`${fx.roots}/proj/${f}`, `${f}\n`);
  }
  page = await openPage(browser.port, `http://127.0.0.1:${port}/proj`);
  const evalIn = page.evalIn;
  // The README's viewport trap, and this file walked straight into it. At the
  // default 800x600 the left (260px) and right (520px) panes leave the middle
  // column **8 pixels wide**: the tab strip is narrower than one tab, every
  // tab wraps onto its own row and overflows it, and a drop aimed at the
  // strip's right edge lands left of every tab's midpoint. `dropIndexIn`
  // then returned 0 for "past the end", the same-pane no-op guard fired, and
  // two assertions failed for a reason that had nothing to do with the code
  // under test.
  await page.cmd("Emulation.setDeviceMetricsOverride",
    { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(async () => await evalIn("typeof send === 'function'"), 10, "app.js loaded");
  // Asserted, not assumed: every coordinate below is meaningless if the strip
  // is narrower than the tabs in it, and that failure is silent.
  ok(
    await until(async () => await evalIn(
      `document.querySelector('.pane[data-pane="2"] .tabstrip').getBoundingClientRect().width > 300`),
      10, "the middle pane is wide enough to aim at"),
    "setup: the middle tab strip is wider than a tab, so drop coordinates mean something",
  );

  const MIDDLE = 2, RIGHT = 3;
  const relsIn = async (pi) => JSON.parse(await evalIn(
    `JSON.stringify(state.panes[${pi}].tabs.map((t) => t.rel ?? t.k))`));

  // Dispatches a real drag sequence, landing left of the tab at `beforeIdx`
  // in the destination strip, or past the end when it is null. A real
  // DataTransfer is used so `types` carries the private MIME the handlers key
  // off — the mechanism that keeps this drag and a file drag apart.
  //
  // Addressed by index, not by label. A Terminal tab's label is its *session
  // name*, so matching on `rel` (which is `"Terminal"` for one) silently
  // found nothing and fell through to the past-the-end branch — the drop
  // then appended, and the assertion that the tab lands where it was dropped
  // failed for a reason that had nothing to do with the code under test.
  const dragTab = async (fromPane, fromIdx, toPane, beforeIdx) => {
    await evalIn(`(() => {
      const src = document.querySelector('.pane[data-pane="${fromPane}"] .tabstrip')
        .querySelectorAll('.tab')[${fromIdx}];
      const strip = document.querySelector('.pane[data-pane="${toPane}"] .tabstrip');
      const before = ${beforeIdx === null ? "null" : `strip.querySelectorAll('.tab')[${beforeIdx}]`};
      if (!src) throw new Error("no source tab at index ${fromIdx}");
      const sr = strip.getBoundingClientRect();
      // Left of a target tab's midpoint lands before it; past the last tab's
      // right edge lands at the end. Both are what dropIndexIn() measures.
      const r = before ? before.getBoundingClientRect() : null;
      const x = r ? r.left + 2 : sr.right - 2;
      const y = r ? r.top + r.height / 2 : sr.top + 8;
      const dt = new DataTransfer();
      const ev = (type, node, extra) => node.dispatchEvent(
        new DragEvent(type, { dataTransfer: dt, bubbles: true, cancelable: true, ...extra }));
      ev("dragstart", src);
      ev("dragover", strip, { clientX: x, clientY: y });
      ev("drop", strip, { clientX: x, clientY: y });
      ev("dragend", src);
      return 0;
    })()`);
  };

  for (const f of ["a.txt", "b.txt", "c.txt"]) {
    await evalIn(`send({ t: "OpenTab", pane: ${MIDDLE},
      tab: { k: "File", rel: ${JSON.stringify(f)}, mode: "Edit" } }); 0`);
    await until(async () => (await relsIn(MIDDLE)).includes(f), 10, `${f} opened`);
  }

  console.log("\nA. tabs are actually draggable");
  // The attribute the DragEvents below cannot verify: dispatching a synthetic
  // dragstart works on any element, draggable or not, so without this the
  // whole file would pass against tabs no one can pick up.
  ok(
    await evalIn(`[...document.querySelectorAll('.pane[data-pane="2"] .tabstrip .tab')]
      .every((t) => t.draggable === true)`),
    "every tab in the strip carries draggable",
  );
  ok(
    await evalIn(`[...document.querySelectorAll('.pane[data-pane="2"] .tabstrip .tab')]
      .every((t) => t.dataset.pane === "2" && t.dataset.idx !== undefined)`),
    "and the pane/index the drop needs",
  );

  console.log("\nB. reordering inside one pane");
  ok(JSON.stringify(await relsIn(MIDDLE)) === '["a.txt","b.txt","c.txt"]',
    "setup: three tabs, in a known order");

  // c.txt onto a.txt — a drop to the LEFT of where it started, so the index
  // adjustment must NOT fire.
  await dragTab(MIDDLE, 2, MIDDLE, 0);   // c.txt, before a.txt
  ok(
    await until(async () => JSON.stringify(await relsIn(MIDDLE)) === '["c.txt","a.txt","b.txt"]',
      10, "c moved to the front"),
    `dropping c.txt before a.txt puts it first — got ${JSON.stringify(await relsIn(MIDDLE))}`,
  );

  // Now the direction that needs the adjustment: c.txt is at 0, dropped past
  // the end. workspace.rs removes before it inserts, so `at` indexes the
  // shortened list; passing the unadjusted position lands it one place too
  // far, which with three tabs is a visibly different array.
  await dragTab(MIDDLE, 0, MIDDLE, null); // c.txt (now first), past the end
  ok(
    await until(async () => JSON.stringify(await relsIn(MIDDLE)) === '["a.txt","b.txt","c.txt"]',
      10, "c moved to the end"),
    `dropping c.txt past the end puts it last — got ${JSON.stringify(await relsIn(MIDDLE))}`,
  );

  // The case that actually pins the index adjustment. Dropping *past the end*
  // does not: workspace.rs clamps `at` to the destination's length, so an
  // unadjusted 3 and an adjusted 2 both land the tab last, and the assertion
  // above passes with the adjustment deleted. Verified by deleting it and
  // watching this file still print ALL PASS.
  //
  // A drop into the middle, to the right of where the tab started, is below
  // the clamp and so cannot be masked: from ["a","b","c"], dragging a.txt to
  // before c.txt is at=2 unadjusted (giving ["b","c","a"]) and at=1 adjusted
  // (giving ["b","a","c"] — where it was actually dropped).
  ok(JSON.stringify(await relsIn(MIDDLE)) === '["a.txt","b.txt","c.txt"]',
    "setup: back to a known order before the case the clamp cannot hide");
  await dragTab(MIDDLE, 0, MIDDLE, 2);   // a.txt, before c.txt
  ok(
    await until(async () => JSON.stringify(await relsIn(MIDDLE)) === '["b.txt","a.txt","c.txt"]',
      10, "a moved between b and c"),
    `dropping a.txt before c.txt lands it between b and c, not after c — got ${JSON.stringify(await relsIn(MIDDLE))}`,
  );
  // Put the order back so the no-op case below reads against a known state.
  await dragTab(MIDDLE, 1, MIDDLE, 0);   // a.txt back to the front
  await until(async () => JSON.stringify(await relsIn(MIDDLE)) === '["a.txt","b.txt","c.txt"]',
    10, "order restored");

  // A drop back onto itself must do nothing at all, rather than shifting by
  // one — the most visible way an off-by-one shows up.
  const beforeNoop = JSON.stringify(await relsIn(MIDDLE));
  await dragTab(MIDDLE, 1, MIDDLE, 1);   // b.txt onto itself
  await new Promise((r) => setTimeout(r, 400));
  ok(JSON.stringify(await relsIn(MIDDLE)) === beforeNoop,
    `dropping a tab where it already is changes nothing — got ${JSON.stringify(await relsIn(MIDDLE))}`);

  console.log("\nC. moving between panes, at a position");
  const rightBefore = await relsIn(RIGHT);
  ok(rightBefore.length > 0, `setup: the right pane starts with ${JSON.stringify(rightBefore)}`);

  // Onto the FRONT of the right pane. The ⇄ button can only append, so a
  // landing at index 0 is the thing this feature adds that the button cannot
  // express at all — asserting on membership alone would not see it.
  //
  // The index is looked up rather than written in: section B reorders this
  // pane, so a hardcoded 1 here drags whichever tab that reorder happened to
  // leave in the middle.
  const bIdx = await evalIn(`state.panes[${MIDDLE}].tabs.findIndex((t) => t.rel === "b.txt")`);
  ok(bIdx >= 0, "setup: b.txt is still in the middle pane");
  await dragTab(MIDDLE, bIdx, RIGHT, 0);
  ok(
    await until(async () => (await relsIn(RIGHT))[0] === "b.txt", 10, "b landed first"),
    `b.txt lands at the position it was dropped, not appended — got ${JSON.stringify(await relsIn(RIGHT))}`,
  );
  ok(!(await relsIn(MIDDLE)).includes("b.txt"), "and it left the pane it came from");
  ok(
    await evalIn(`state.panes[${RIGHT}].tabs[state.panes[${RIGHT}].active].rel === "b.txt"`),
    "the dropped tab becomes active, matching what the ⇄ button already does",
  );

  console.log("\nD. the file-upload drag still works, over the same target");
  // The direction that matters. A tab-drop handler that does not check the
  // type swallows a file drop, and uploads stop working with no error
  // anywhere. Both drags are driven over the same tab strip, in both orders.
  const fileDragOverTabs = async () => await evalIn(`(() => {
    const strip = document.querySelector('.pane[data-pane="2"] .tabstrip');
    const dt = new DataTransfer();
    dt.items.add(new File(["hello"], "dropped.txt", { type: "text/plain" }));
    const r = strip.getBoundingClientRect();
    const ev = new DragEvent("dragover", { dataTransfer: dt, bubbles: true, cancelable: true,
      clientX: r.left + 4, clientY: r.top + 4 });
    strip.dispatchEvent(ev);
    // The document-level upload handler calls preventDefault on every file
    // drag; the tab handler must not have claimed this one first.
    return JSON.stringify({ types: [...dt.types], prevented: ev.defaultPrevented,
      marker: !!document.querySelector(".tabdrop") });
  })()`);

  let fd = JSON.parse(await fileDragOverTabs());
  ok(fd.types.includes("Files"), "setup: the synthetic file drag really carries Files");
  ok(fd.prevented, "a file drag over a tab strip is still claimed by the upload path");
  ok(!fd.marker, "and the tab drop indicator does not appear for it");

  // `defaultPrevented` alone proves nothing here — the upload handler
  // prevents every file drag, so it is true whether or not the tab handler
  // interfered. Verified by deleting the tab `dragover`'s type check and
  // watching this file still print ALL PASS.
  //
  // What discriminates is whether the file *drop* reaches the upload path at
  // all. A tab strip is not a valid upload target, so the upload handler
  // answers it with a specific refusal; if a tab handler had swallowed the
  // drop (a stopPropagation, or a preventDefault before checking the type)
  // no message would appear and uploads would fail silently — which is the
  // failure this whole section exists for.
  await evalIn(`window.__errors = []; const __se = showError;
    window.showError = (m) => { window.__errors.push(m); return __se(m); }; 0`);
  await evalIn(`(() => {
    const strip = document.querySelector('.pane[data-pane="2"] .tabstrip');
    const dt = new DataTransfer();
    dt.items.add(new File(["hello"], "dropped.txt", { type: "text/plain" }));
    const r = strip.getBoundingClientRect();
    strip.dispatchEvent(new DragEvent("drop", { dataTransfer: dt, bubbles: true,
      cancelable: true, clientX: r.left + 4, clientY: r.top + 4 }));
    return 0;
  })()`);
  const errs = JSON.parse(await evalIn(`JSON.stringify(window.__errors)`));
  ok(
    errs.some((m) => m.includes("Files pane")),
    `a file dropped on a tab strip still reaches the upload path — got ${JSON.stringify(errs)}`,
  );

  // Now a tab drag immediately after, over the same strip, to prove the file
  // handler did not poison the tab path either.
  await evalIn(`(() => {
    const strip = document.querySelector('.pane[data-pane="2"] .tabstrip');
    const src = strip.querySelector('.tab');
    const dt = new DataTransfer();
    const r = strip.getBoundingClientRect();
    src.dispatchEvent(new DragEvent("dragstart",
      { dataTransfer: dt, bubbles: true, cancelable: true }));
    strip.dispatchEvent(new DragEvent("dragover", { dataTransfer: dt, bubbles: true,
      cancelable: true, clientX: r.right - 2, clientY: r.top + 8 }));
    return 0;
  })()`);
  ok(await evalIn(`!!document.querySelector(".tabdrop")`),
    "a tab drag over the same strip still shows its drop indicator");
  ok(
    await evalIn(`(() => { const t = document.querySelector('.tabstrip .tab');
      t.dispatchEvent(new DragEvent("dragend", { bubbles: true }));
      return !document.querySelector(".tabdrop"); })()`),
    "and the indicator is cleared when the drag ends",
  );

  // And the reverse order, since a stale `dragTabSource` from the tab drag
  // above is exactly what would make the next file drag misbehave.
  fd = JSON.parse(await fileDragOverTabs());
  ok(fd.prevented && !fd.marker, "a file drag after a tab drag is still the upload path");
} finally {
  if (page) page.close();
  browser.close();
  await roost.close();
}
console.log(fail ? `\n${fail} FAILED` : "\nALL PASS");
Deno.exit(fail ? 1 : 0);
