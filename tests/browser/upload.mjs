//! Do dropped files actually reach the filesystem, and are the refusals real?
//!
//! `cargo test` cannot reach static/app.js, and the Rust upload tests post
//! their own multipart by hand — so the client's half (FormData, the XHR, the
//! part cap checked at drop time, the folder refusal, the delegated listener
//! surviving a tree refresh) is entirely untested without this.
//!
//! The two refusal assertions check that **no request was made**, not merely
//! that an error appeared. A client that uploaded twenty files and then
//! complained would pass a message-only assertion while doing exactly the thing
//! the cap exists to prevent.
//!
//! Run: deno run -A tests/browser/upload.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`);
  const { evalIn } = page;
  await until(() => evalIn("typeof uploadFiles === 'function' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");

  // Counts XHR sends so a refusal can be shown to have refused *before* the
  // network, which is the whole point of checking the cap at drop time.
  await evalIn(`window.__sends = 0;
    const __send = XMLHttpRequest.prototype.send;
    XMLHttpRequest.prototype.send = function (...a) { window.__sends++; return __send.apply(this, a); };
    window.__errors = [];
    const __showError = showError;
    window.showError = (m) => { window.__errors.push(m); return __showError(m); };
    window.__file = (name, body) => new File([body], name, { type: "text/plain" });`);

  // ---- 1. A real upload lands on disk -------------------------------------
  await evalIn(`uploadFiles([__file("dropped.txt", "hello from the browser")], "")`);
  const landed = `${fx.roots}/${fx.project}/dropped.txt`;
  await until(async () => {
    try { return (await Deno.readTextFile(landed)) === "hello from the browser"; }
    catch { return false; }
  }, 20, "dropped.txt on disk");
  ok(true, "a file uploaded through the client's own path lands on disk with the right bytes");

  // ---- 2. The tree refreshes without a reload ------------------------------
  // Not cosmetic: the upload's own TreeChanged replaces the tree fragment, and
  // this is what proves the delegated listener outlives that replacement.
  await until(() => evalIn(`!!document.querySelector('[data-rel="dropped.txt"]')`), 20, "tree row");
  ok(true, "the new file appears in the tree with no reload");

  // ---- 3. The part cap refuses before sending ------------------------------
  const before = await evalIn("window.__sends");
  await evalIn(`window.__errors = [];
    uploadFiles(Array.from({length: 20}, (_, i) => __file("f" + i + ".txt", "x")), "")`);
  const after = await evalIn("window.__sends");
  const capMsg = await evalIn("window.__errors.join(' | ')");
  ok(after === before, `20 files refused with no request sent (sends ${before} -> ${after})`);
  ok(/limit 16/.test(capMsg), `the refusal names the limit: ${capMsg}`);

  // ---- 4. A dropped folder is refused by name ------------------------------
  // webkitGetAsEntry is what distinguishes a folder; a synthesised item reports
  // isDirectory the same way a real one does, which is the branch under test.
  const sendsBeforeDrop = await evalIn("window.__sends");
  await evalIn(`window.__errors = [];
    (() => {
    const row = document.querySelector('[data-rel]');
    const dt = new DataTransfer();
    // A real folder drag carries a zero-length File *and* sets types to
    // include "Files"; only webkitGetAsEntry distinguishes it. Synthesising
    // just the entry made this test miss that the handler now gates on types.
    dt.items.add(new File([], "src", { type: "" }));
    const ev = new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: dt });
    Object.defineProperty(ev.dataTransfer, "items", {
      value: [{ webkitGetAsEntry: () => ({ isDirectory: true, name: "src" }) }],
    });
    row.dispatchEvent(ev);
    })();`);
  const dropMsg = await evalIn("window.__errors.join(' | ')");
  const sendsAfterDrop = await evalIn("window.__sends");
  ok(sendsAfterDrop === sendsBeforeDrop, "a dropped folder sent no request");
  ok(/folders are not uploaded \(src\)/.test(dropMsg), `the refusal names the folder: ${dropMsg}`);

  // ---- 5. A misplaced file drag never reaches the browser ------------------
  // Found by hand, not by this file: dragover only called preventDefault over a
  // [data-rel] row, so every other pixel fell through to the browser, which
  // navigates to file:/// and throws the workspace away. Asserting
  // defaultPrevented is the only way to see that from inside the page — the
  // navigation itself would just destroy the test's own context.
  const outside = await evalIn(`(() => {
    const dt = new DataTransfer();
    dt.items.add(new File(["x"], "stray.txt", { type: "text/plain" }));
    const ev = new DragEvent("dragover", { bubbles: true, cancelable: true, dataTransfer: dt });
    document.body.dispatchEvent(ev);
    return ev.defaultPrevented;
  })()`);
  ok(outside === true, "a file dragged outside the tree is still intercepted, not left to the browser");

  const sendsBeforeStray = await evalIn("window.__sends");
  await evalIn(`window.__errors = [];
    (() => {
      const dt = new DataTransfer();
      dt.items.add(new File(["x"], "stray.txt", { type: "text/plain" }));
      document.body.dispatchEvent(new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: dt }));
    })();`);
  const strayMsg = await evalIn("window.__errors.join(' | ')");
  ok(
    (await evalIn("window.__sends")) === sendsBeforeStray,
    "a drop outside the tree uploaded nothing",
  );
  ok(/Files pane/.test(strayMsg), `it says where files go instead: ${strayMsg}`);

  // ---- 6. Pane whitespace counts as the project root -----------------------
  const paneDrop = await evalIn(`(() => {
    const pane = [...document.querySelectorAll(".pane")].find((p) => p.querySelector("ul.tree"));
    return pane ? uploadTargetDir(pane.querySelector(".content")) : null;
  })()`);
  ok(paneDrop === "", `empty space in the Files pane resolves to the project root (got ${JSON.stringify(paneDrop)})`);

  // ---- 7. A screenshot pasted on a terminal reaches /paste -----------------
  // The bug this pins, found by hand: xterm's own paste handler calls
  // stopPropagation() on every paste over a terminal and reads only
  // text/plain, so a bubble-phase listener never runs where it matters most.
  // The stand-in below reproduces that exactly — a listener on the inner
  // element that stops propagation — so this test fails if the capture flag is
  // ever dropped, which is the whole point of it existing.
  await evalIn(`(() => {
    const host = document.createElement("div");
    host.className = "termhost";
    host.dataset.session = "faketerm";
    const ta = document.createElement("textarea");
    host.appendChild(ta);
    document.body.appendChild(host);
    ta.addEventListener("paste", (e) => e.stopPropagation()); // xterm's behaviour
    ta.focus();
    window.__ta = ta;
  })()`);

  await evalIn(`window.__pasteUrl = null;
    const __open = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function (m, u, ...r) { window.__pasteUrl = u; return __open.call(this, m, u, ...r); };`);

  await evalIn(`(() => {
    const dt = new DataTransfer();
    dt.items.add(new File([new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10])], "shot.png", { type: "image/png" }));
    window.__ta.dispatchEvent(new ClipboardEvent("paste", { bubbles: true, cancelable: true, clipboardData: dt }));
  })()`);

  await until(() => evalIn("window.__pasteUrl !== null"), 10, "a paste request");
  const pasteUrl = await evalIn("window.__pasteUrl");
  ok(
    /\/paste\/[^/]+\/faketerm$/.test(pasteUrl),
    `a pasted image posts to that terminal's session even though xterm stops propagation (${pasteUrl})`,
  );

  // ---- 8. An image dropped on a terminal goes to that terminal -------------
  await evalIn(`window.__pasteUrl = null;
    (() => {
      const dt = new DataTransfer();
      dt.items.add(new File([new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10])], "shot.png", { type: "image/png" }));
      document.querySelector(".termhost").dispatchEvent(
        new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: dt }));
    })()`);
  await until(() => evalIn("window.__pasteUrl !== null"), 10, "a drop-to-paste request");
  const dropUrl = await evalIn("window.__pasteUrl");
  ok(
    /\/paste\/[^/]+\/faketerm$/.test(dropUrl),
    `an image dropped on a terminal posts to that session (${dropUrl})`,
  );

  await evalIn(`document.querySelector(".termhost").remove()`);

  // ---- 9. A collision is reported per file, not as a blanket error ---------
  // Waits for the error *about this file* rather than for any error at all.
  // The fake-terminal posts above are answered asynchronously with "no such
  // session", and one of those landed here instead — a race this test lost only
  // once the surrounding timing changed, which is the kind of flake that reads
  // as a merge breaking something.
  await evalIn(`window.__errors = [];
    uploadFiles([__file("dropped.txt", "second attempt")], "")`);
  await until(
    () => evalIn(`window.__errors.some((m) => m.includes("dropped.txt"))`),
    20,
    "the per-file error for dropped.txt",
  );
  const collideMsg = await evalIn(`window.__errors.filter((m) => m.includes("dropped.txt")).join(' | ')`);
  ok(/dropped\.txt.*already exists/.test(collideMsg), `the collision names the file: ${collideMsg}`);
  ok(
    (await Deno.readTextFile(landed)) === "hello from the browser",
    "the colliding upload left the original bytes untouched",
  );
} catch (e) {
  ok(false, `threw: ${e.message}`);
} finally {
  try { page?.close(); } catch { /* closing a dead page is not a failure */ }
  browser.close();
  await resh.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nall ok" : `\n${fail} FAILED`);
Deno.exit(fail === 0 ? 0 : 1);
