// Firefox's backend. Firefox has no debugger API (bug 1316741), so where
// Chrome's backend speaks the DevTools protocol this one uses what every
// extension has: scripting.executeScript to run roostPage in the tab, and
// tabs.captureTab to screenshot it — which, unlike Chrome's
// captureVisibleTab, takes any tab, background ones included.
//
// What that costs, said in each result's `note` so Claude knows when to
// suspect it: clicks and keys are dispatched events, untrusted, so a page
// that checks isTrusted ignores them; a button that opens a file picker
// cannot be caught, so an upload needs the <input type=file> itself; and the
// console is collected from the first chrome_console on, per page load.
"use strict";

const ALL_SITES = { origins: ["<all_urls>"] };
const UNTRUSTED_NOTE = "Firefox: this was a dispatched (untrusted) event; if nothing happened, the page may ignore synthetic input";

/// Every tool here runs code in, or captures, someone else's page, which in
/// Firefox needs the optional <all_urls> grant. Asked for from the consent
/// window's Allow button (a click in an extension page is the user gesture
/// permissions.request requires) and from the options page.
async function needSites() {
  if (await chrome.permissions.contains(ALL_SITES)) return;
  throw new Error("Firefox has not given roost bridge access to websites — allow it in the consent window, or in the extension's options (Access to websites)");
}

/// roostPage(op, a) in the tab's top frame. `world` MAIN is the page's own
/// JavaScript world: for the console, which must wrap the page's own, and
/// for files, which should be the page's own objects.
async function inPage(tab, op, a = {}, world = "ISOLATED") {
  await needSites();
  let res;
  try {
    [res] = await chrome.scripting.executeScript({ target: { tabId: tab.id }, func: roostPage, args: [op, a], world });
  } catch (e) {
    throw new Error(String((e && e.message) || e).replace(/^Error: /, ""));
  }
  if (!res) throw new Error("the page gave no answer");
  if (res.error) throw new Error(String((res.error && res.error.message) || res.error).replace(/^Error: /, ""));
  return res.result;
}

/// Device pixels per CSS pixel in a chrome_screenshot: the screen's own,
/// capped so the long side stays within SHOT_LONG_SIDE. A point on the image
/// divided by it is a CSS pixel in the viewport.
async function pageShotScale(tab) {
  const v = await inPage(tab, "viewport");
  return Math.min(v.dpr, SHOT_LONG_SIDE / Math.max(v.w, v.h));
}

/// Tabs whose console is being collected. The hook lives in the page, so a
/// new page load needs it again, as early as an extension can get in.
async function watched() { return session("consoleTabs", {}); }

if (!chrome.debugger) {
  chrome.tabs.onUpdated.addListener(async (tabId, info) => {
    if (info.status !== "loading") return;
    if (!(await watched())[tabId]) return;
    chrome.scripting.executeScript({
      target: { tabId }, func: roostPage, args: ["consoleHook", {}], world: "MAIN", injectImmediately: true,
    }).catch(() => {});
  });
  chrome.tabs.onRemoved.addListener(async (tabId) => {
    const w = await watched();
    if (!(tabId in w)) return;
    delete w[tabId];
    await setSession("consoleTabs", w);
  });
}

// eslint-disable-next-line no-unused-vars
const pageDrive = {
  name: "page",

  read(tab, max) {
    return inPage(tab, "read", { max });
  },

  async click(tab, args) {
    if (Number.isFinite(args.x) && Number.isFinite(args.y)) {
      const k = await pageShotScale(tab);
      const r = await inPage(tab, "click", { x: args.x / k, y: args.y / k });
      return { what: `${r.what} at point ${args.x},${args.y} on the screenshot`, note: UNTRUSTED_NOTE };
    }
    const r = await inPage(tab, "click", { ref: args.ref, selector: args.selector });
    return { what: r.what, note: UNTRUSTED_NOTE };
  },

  async type(tab, args) {
    const r = await inPage(tab, "type", { ref: args.ref, selector: args.selector, text: args.text, submit: !!args.submit });
    return { what: r.what, note: UNTRUSTED_NOTE };
  },

  async upload(tab, args, files) {
    const r = await inPage(tab, "upload", { ref: args.ref, selector: args.selector, files }, "MAIN");
    return { what: r.what };
  },

  async screenshot(tab) {
    await needSites();
    const scale = await pageShotScale(tab);
    for (const quality of [70, 50, 35]) {
      const url = await withTimeout(chrome.tabs.captureTab(tab.id, { format: "jpeg", quality, scale }), 15000, "the screenshot");
      const data = url.slice(url.indexOf(",") + 1);
      if (data.length <= MAX_SHOT_B64) return data;
    }
    throw new Error("the screenshot is too large even at low quality");
  },

  async console(tab) {
    const w = await watched();
    if (!w[tab.id]) { w[tab.id] = true; await setSession("consoleTabs", w); }
    const hook = await inPage(tab, "consoleHook", {}, "MAIN");
    const r = await inPage(tab, "consoleRead", {}, "MAIN");
    return {
      messages: r.messages || [],
      fresh: hook.fresh,
      note: "Firefox: collected from the page itself, from the first chrome_console on and again from the start of each page load; messages from the first moments of a load can be missed",
    };
  },
};
