//! A terminal you are looking at has nothing unread (#129).
//!
//! Before this, notices were read only through `focusSession` — a tab click,
//! a notice click, a desktop-notification click. Coming back to the page with
//! the notifying terminal already in front of you, clicking into it from the
//! editor, or having a notice arrive while you typed in it all left it unread
//! (and the last one bannered). Only a browser can see any of that: the rule
//! is about visibility, window focus and keyboard focus.
//!
//! Two pages on one project. `A` is the page under test; `O` is the observer.
//! Every read-state assertion is made on `O`: after `MarkNoticeRead`,
//! wsconn.rs rebroadcasts `Notices` to every client of the project, so O's
//! array is the server's answer. A's own array may have been marked locally,
//! and asserting on it would pass with no intent sent (notices.mjs, section E).
//! O is itself a roost page and would read notices too if its keyboard focus
//! sat in a terminal, so it blurs whenever it comes forward and each section
//! asserts it holds no terminal focus before raising.
//!
//! Notices are raised the production way: OSC 777 printed by a real shell in
//! a real dtach session.
//!
//! Revert-the-fix, each applied alone, run, and restored (2026-09-29):
//!
//!   1. Removing the visibilitychange and window-focus listeners failed
//!      **nothing**, in five runs. Traced: in this harness a return delivers
//!      `vis:visible`, `focus`, then `focusin` on xterm's textarea, and the
//!      termhost's focusin listener reads the notice. A bare page in the same
//!      Chromium has also delivered `focus`, `focusin` while still hidden,
//!      then `vis:visible` — the order where only the listeners can read it.
//!      So they stay, and this file cannot tell them from the focusin path.
//!   2. `watchedSession` returning `lastFocusedSession`: failed 4 — B's
//!      "focus elsewhere" as designed, and also A's hidden arrival, because
//!      O's own lastFocusedSession is t, so the *observer* read it. Keyboard
//!      focus is what keeps a second window from reading for you.
//!   3. Dropping the getClientRects guard: failed nothing. Section D's pooled
//!      case is unreachable here — opening a terminal moves keyboard focus
//!      into it, so focus never stays in the host that went to the pool.
//!   4. Dropping `document.hasFocus()`: failed nothing. A tab switch hides
//!      the page, so the visibility guard answers first; section F's second
//!      window left A `visible/true`, not blurred. A known gap.
//!   5. Disabling the arrival branch in onNotice: failed 4 in C — read state
//!      and both banner counts.
import { attachTarget, fixture, freePort, openPage, profileDir, startBrowser, startRoost, sleep, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };
const note = (m) => console.log(`  --    ${m}`);

const fx = await fixture();
const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
const url = `http://127.0.0.1:${roost.port}/proj`;

const wire = (page) => {
  const { evalIn } = page;
  const q = JSON.stringify;
  const ready = async () => {
    const up = await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app");
    await evalIn(`window.__last = (s) => { const b = terms.get(s).term.buffer.active; let out = "";
      for (let y = 0; y <= b.baseY + b.cursorY; y++) out += b.getLine(y).translateToString(true) + "\\n"; return out; };`);
    return up;
  };
  const sessions = () => evalIn(`state.panes.flatMap((p) => p.tabs.filter((t) => t.k === "Terminal").map((t) => t.session))`);
  // A new terminal in `pane`, attached, at a live prompt. Returns its name.
  const openTerm = async (pane) => {
    const before = await sessions();
    await evalIn(`send({ t: "NewTerminal", pane: ${pane} })`);
    let s = null;
    await until(async () => {
      const now = await sessions();
      s = now.find((x) => !before.includes(x)) || null;
      return !!s;
    }, 15, `a new terminal in pane ${pane}`);
    const ti = await evalIn(`state.panes[${pane}].tabs.findIndex((t) => t.k === "Terminal" && t.session === ${q(s)})`);
    await evalIn(`send({ t: "ActivateTab", pane: ${pane}, idx: ${ti} })`);
    await sleep(400);
    await evalIn(`send({ t: "StartTerminal", session: ${q(s)} })`);
    await until(() => evalIn(`terms.has(${q(s)}) && !!terms.get(${q(s)}).sock && terms.get(${q(s)}).sock.readyState === 1`), 30, `socket ${s}`);
    // readline discards typeahead while it initialises (README trap 3).
    await until(async () => (await evalIn(`__last(${q(s)})`)).trimEnd().endsWith("$"), 30, `prompt ${s}`);
    return s;
  };
  const raise = async (s, title) => {
    await until(async () => (await evalIn(`__last(${q(s)})`)).trimEnd().endsWith("$"), 30, `prompt ${s}`);
    await evalIn(`terms.get(${q(s)}).term.input(${q(`printf '\\033]777;notify;${title};b\\007'\r`)})`);
  };
  const readState = (title) => evalIn(`(() => { const n = notices.find((x) => x.title === ${q(title)});
    return n ? (n.read ? "read" : "unread") : "absent"; })()`);
  const focusTerm = (s) => evalIn(`(terms.get(${q(s)}).term.focus(),
    (document.activeElement && document.activeElement.closest(".termhost") || {dataset: {}}).dataset.session || null)`);
  const termFocus = () => evalIn(`(document.activeElement && document.activeElement.closest(".termhost") || {dataset: {}}).dataset.session || null`);
  const blurAll = () => evalIn(`(document.activeElement && document.activeElement.blur(), document.activeElement === document.body)`);
  // Banner spy. canNotify() needs a secure context (127.0.0.1 is one) and
  // permission "granted"; this stubs the permission and the service worker,
  // so what is observed is roost's decision to banner, not Chrome's prompt.
  const spyBanners = () => evalIn(`(() => { window.__posts = [];
    Object.defineProperty(Notification, "permission", { get: () => "granted", configurable: true });
    swReg = { active: { postMessage: (m) => window.__posts.push(m) } }; return canNotify(); })()`);
  const banners = () => evalIn(`window.__posts.length`);
  const vis = () => evalIn(`document.visibilityState + "/" + document.hasFocus()`);
  const setting = () => evalIn(`(state.settings.keys.find((r) => r.key === "read_when_watching") || {}).effective`);
  return { ...page, evalIn, ready, openTerm, raise, readState, focusTerm, termFocus, blurAll, spyBanners, banners, vis, setting };
};

let A, O, win;
try {
  A = wire(await openPage(browser.port, url));
  ok(await A.ready(), "page A is up");
  const t = await A.openTerm(0);
  ok(!!t, `terminal t (${t}) in pane 0`);
  O = wire(await openPage(browser.port, url));
  ok(await O.ready(), "observer O is up (A is now behind it)");
  // Mounting a terminal tab focuses it in a requestAnimationFrame
  // (mountTab), and a hidden page runs none — so a layout change A made
  // while O was behind lands its focus() only once O comes forward. Let
  // those frames run before blurring, or O ends up watching a terminal.
  const oFront = async () => { await O.bringToFront(); await sleep(300); await O.blurAll(); };
  const seen = (title, want, secs = 10) => until(async () => (await O.readState(title)) === want, secs, `${title} ${want}`);

  // ---- A. Return ------------------------------------------------------
  // The reported bug: A's terminal holds keyboard focus, A goes behind, a
  // notice arrives, A comes back. Nothing but the page's own visibility
  // changed, so only the visibilitychange/focus listeners can read it.
  await A.bringToFront();
  ok(await A.focusTerm(t) === t, "A: keyboard focus is in t");
  await oFront();
  ok(await A.vis() === "hidden/false", `A is behind O (${await A.vis()})`);
  ok(await O.termFocus() === null, "precondition: O holds no terminal focus");
  await A.raise(t, "ret1");
  ok(await seen("ret1", "unread"), "A: a notice raised while hidden arrives unread");
  await sleep(1000);
  ok(await O.readState("ret1") === "unread", "and stays unread while A is hidden");
  ok(await A.termFocus() === t, "A kept its terminal focus while hidden (the probe's finding)");
  await A.bringToFront();
  ok(await seen("ret1", "read"), "A: returning to the page reads it");

  // ---- B. Not focused ----------------------------------------------------
  // The discriminator. A was last focused in t (lastFocusedSession === t),
  // but the keyboard is elsewhere when it returns. An implementation keyed on
  // lastFocusedSession passes A and fails here.
  ok(await A.focusTerm(t) === t && await A.blurAll(), "A: t was focused, then focus left it");
  await oFront();
  await A.raise(t, "ret2");
  ok(await seen("ret2", "unread"), "A: ret2 arrives unread");
  await A.bringToFront();
  await sleep(1500);
  ok(await O.readState("ret2") === "unread", "A: returning with focus elsewhere does not read it");
  await A.focusTerm(t);
  ok(await seen("ret2", "read"), "A: clicking into the already-active terminal reads it (focusin)");

  // ---- C. Arrival -------------------------------------------------------
  ok(await A.spyBanners(), "A: banner spy armed (canNotify() true)");
  ok(await A.focusTerm(t) === t, "A: typing in t");
  await A.raise(t, "arr1");
  ok(await seen("arr1", "read"), "A: a notice from the terminal being typed in is read on arrival");
  ok(await A.banners() === 0, `and raises no OS banner (${await A.banners()})`);
  const u = await A.openTerm(3);
  ok(!!u, `terminal u (${u}) in pane 3`);
  ok(await A.focusTerm(t) === t, "A: typing in t again");
  await A.raise(u, "arr2");
  ok(await seen("arr2", "unread"), "A: a notice from u, not the terminal being typed in, stays unread");
  await sleep(500);
  ok(await A.banners() === 1, `and does banner — the pair, since 0 alone passes with the spy dead (${await A.banners()})`);
  await oFront();
  ok(await O.termFocus() === null, "precondition: O holds no terminal focus");
  await A.raise(t, "arr3");
  ok(await seen("arr3", "unread"), "A: hidden, t's own notice stays unread even with t focused");
  await sleep(500);
  ok(await A.banners() === 2, `and banners (${await A.banners()})`);

  // ---- D. Two panes -----------------------------------------------------
  await A.bringToFront();
  ok(await seen("arr3", "read"), "A: back in front with t focused reads arr3");
  ok(await A.focusTerm(u) === u, "A: typing in u (pane 3)");
  ok(await seen("arr2", "read"), "A: focusing u reads u's notice");
  await A.raise(t, "two1");
  ok(await seen("two1", "unread"), "A: t visible in pane 0 but not focused — its notice is kept");
  await sleep(1500);
  ok(await O.readState("two1") === "unread", "and stays kept");
  // Pooled host: another client switches pane 3 away from u while A's
  // keyboard is in it.
  const before = await O.evalIn(`state.panes[3].tabs.length`);
  await O.evalIn(`send({ t: "NewTerminal", pane: 3 })`);
  await until(() => A.evalIn(`(() => { const p = state.panes[3]; return p.tabs.length > ${before} && p.tabs[p.active].session !== ${JSON.stringify(u)}; })()`), 15, "pane 3 switched away from u");
  await sleep(500);
  const still = await A.termFocus();
  const hostShown = await A.evalIn(`terms.get(${JSON.stringify(u)}).node.getClientRects().length`);
  note(`after the switch: A's terminal focus = ${still}, u's host rendered rects = ${hostShown}`);
  if (still === u && hostShown === 0) {
    await A.raise(u, "pool1");
    ok(await seen("pool1", "unread"), "A: focus in a pooled (not rendered) host is not watching");
    await sleep(1500);
    ok(await O.readState("pool1") === "unread", "and stays unread");
  } else {
    note("pool case unreachable here: focus did not stay in a hidden host — getClientRects guard not exercised");
  }

  // ---- E. Setting off ---------------------------------------------------
  await A.evalIn(`send({ t: "SetSetting", scope: "project", key: "read_when_watching", value: false })`);
  ok(await until(async () => (await A.setting()) === false, 10, "setting off"), "A: read_when_watching off, live (no reload)");
  await A.evalIn(`window.__posts = []`);
  ok(await A.focusTerm(t) === t, "A: typing in t");
  await A.raise(t, "off1");
  ok(await seen("off1", "unread"), "A: with the setting off, arrival is not read");
  await sleep(1500);
  ok(await O.readState("off1") === "unread", "and stays unread");
  ok(await A.banners() === 1, `and banners (${await A.banners()})`);
  await oFront();
  await A.bringToFront();
  await sleep(1500);
  ok(await O.readState("off1") === "unread", "A: with the setting off, returning does not read it");
  await A.evalIn(`send({ t: "SetSetting", scope: "project", key: "read_when_watching", value: null })`);
  ok(await until(async () => (await A.setting()) === true, 10, "setting back"), "A: cleared, back to the default on");

  // ---- F. Visible but unfocused (best effort) ---------------------------
  // A tab switch hides the page, so it cannot tell the hasFocus() guard from
  // the visibility guard. A second window might leave A visible and blurred.
  ok(await A.focusTerm(t) === t, "A: typing in t");
  const ver = await (await fetch(`http://127.0.0.1:${browser.port}/json/version`)).json();
  const b = await attachTarget(ver.webSocketDebuggerUrl);
  try {
    const r = await b.cmd("Target.createTarget", { url: "about:blank", newWindow: true });
    win = { b, id: r.targetId };
  } catch (e) {
    note(`F skipped: Target.createTarget failed (${e})`);
  }
  await sleep(1000);
  const v = await A.vis();
  note(`F: with a second window open, A reports ${v}`);
  if (v === "visible/false") {
    await A.raise(t, "unf1");
    ok(await seen("unf1", "unread"), "A: visible but unfocused is not watching");
    await sleep(1500);
    ok(await O.readState("unf1") === "unread", "and stays unread");
  } else {
    note("F: visible-but-unfocused not reproducible here — the hasFocus() guard is not exercised");
  }
} finally {
  try { if (win) await win.b.cmd("Target.closeTarget", { targetId: win.id }); } catch {}
  try { win?.b.close?.(); } catch {}
  try { await A?.close?.(); } catch {}
  try { await O?.close?.(); } catch {}
  try { browser.close(); } catch {}
  try { await roost.close(); } catch {}
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
