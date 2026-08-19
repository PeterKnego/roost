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
    const row = document.querySelector('[data-rel]');
    const ev = new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: new DataTransfer() });
    Object.defineProperty(ev.dataTransfer, "items", {
      value: [{ webkitGetAsEntry: () => ({ isDirectory: true, name: "src" }) }],
    });
    row.dispatchEvent(ev);`);
  const dropMsg = await evalIn("window.__errors.join(' | ')");
  const sendsAfterDrop = await evalIn("window.__sends");
  ok(sendsAfterDrop === sendsBeforeDrop, "a dropped folder sent no request");
  ok(/folders are not uploaded \(src\)/.test(dropMsg), `the refusal names the folder: ${dropMsg}`);

  // ---- 5. A collision is reported per file, not as a blanket error ---------
  await evalIn(`window.__errors = [];
    uploadFiles([__file("dropped.txt", "second attempt")], "")`);
  await until(() => evalIn("window.__errors.length > 0"), 20, "per-file error");
  const collideMsg = await evalIn("window.__errors.join(' | ')");
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
