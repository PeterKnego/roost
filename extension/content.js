// Injected into roost pages only — the origins listed in the options page,
// top frame only (registered with allFrames: false). It is a pipe between the
// roost page and the extension worker, and decides nothing: the worker
// verifies every call's signature and owns consent.
"use strict";

(() => {
  const post = (m) => window.postMessage({ roostBridge: 1, from: "ext", ...m }, location.origin);
  const alive = () => { try { return !!chrome.runtime && !!chrome.runtime.id; } catch { return false; } };

  // Once per page. The worker both registers this script and injects it into
  // roost tabs already open when the address list changes, and a page that
  // loads while that happens gets both: two copies forward every call, the
  // second is refused as a replay ("that call was already handled"), and that
  // refusal can reach roost before the real answer. Found by the Firefox
  // test, where it happened on almost every call. A copy orphaned by an
  // extension reload is not alive, and does not keep its successor out.
  if (typeof window.__roostBridgeAlive === "function" && window.__roostBridgeAlive()) return;
  window.__roostBridgeAlive = alive;
  let pending = 0;
  let keepalive = null;
  let dead = false;

  // Reloading or updating the extension orphans this script: the page would
  // stay registered as a relay with nobody behind it. Saying bye drops it.
  function orphaned() {
    if (dead) return;
    dead = true;
    clearInterval(keepalive);
    removeEventListener("message", onMessage);
    post({ type: "bye" });
  }

  function hello() {
    if (!alive()) return orphaned();
    post({ type: "hello", version: chrome.runtime.getManifest().version });
    tellWorker({ type: "roost-tab" });
  }

  function tellWorker(m) {
    if (!alive()) return orphaned();
    chrome.runtime.sendMessage(m).catch(() => {});
  }

  // An MV3 worker is stopped after ~30 s without events, consent window or
  // not. While a call is outstanding, a message every 20 s keeps it up.
  function holdWorker() {
    pending++;
    if (!keepalive) keepalive = setInterval(() => tellWorker({ type: "keepalive" }), 20000);
  }
  function releaseWorker() {
    pending--;
    if (pending <= 0) { pending = 0; clearInterval(keepalive); keepalive = null; }
  }

  async function onMessage(e) {
    // This window's own posts only — never an iframe's — and only the page's
    // side of the conversation (we hear our own posts too).
    if (e.source !== window || e.origin !== location.origin) return;
    const m = e.data;
    if (!m || m.roostBridge !== 1 || m.from !== "page") return;
    if (m.type === "probe") return hello();
    if (m.type !== "call") return;
    // Before anything else: tells the server this page is alive, so a call
    // is never left waiting on a tab that cannot answer.
    post({ type: "ack", call: m.call });
    if (!alive()) {
      post({ type: "result", call: m.call, ok: false, error: "the roost extension was reloaded — reload this roost tab" });
      return orphaned();
    }
    holdWorker();
    try {
      const r = await chrome.runtime.sendMessage({ type: "call", call: m.call, payload: m.payload, sig: m.sig });
      if (!r) throw new Error("no answer from the extension");
      post({ type: "result", call: m.call, ok: !!r.ok, result: r.result, error: r.error });
    } catch (err) {
      // A worker that restarted mid-call has lost it; that is a failure to
      // report now, never a hang until the server's timeout.
      const gone = !alive();
      post({
        type: "result", call: m.call, ok: false,
        error: gone ? "the roost extension was reloaded — reload this roost tab"
                    : `the roost extension stopped while handling the call (${err && err.message || err})`,
      });
      if (gone) orphaned();
    } finally {
      releaseWorker();
    }
  }

  addEventListener("message", onMessage);
  addEventListener("focus", () => tellWorker({ type: "roost-tab" }));
  chrome.runtime.onMessage.addListener((m, sender, reply) => {
    if (sender.id !== chrome.runtime.id || !m || m.type !== "send") return;
    post({ type: "send", text: m.text || "", image: m.image || null });
    reply({ ok: true });
  });
  hello();
})();
