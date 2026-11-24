"use strict";
const $ = (id) => document.getElementById(id);
const say = (t) => { $("msg").textContent = t; };
// Firefox match patterns take no port (bugs 1362809, 1468162): access to
// http://127.0.0.1:8444 is asked for as http://127.0.0.1/*.
const FIREFOX = chrome.runtime.getURL("").startsWith("moz-extension:");
const originPattern = (o) => {
  const u = new URL(o);
  return FIREFOX ? `${u.protocol}//${u.hostname}/*` : `${o}/*`;
};
const ALL_SITES = { origins: ["<all_urls>"] };

async function load() {
  const s = await chrome.storage.local.get({ origins: [], key: "", unsafe: false, deny: [], follow: true });
  const ul = $("origins");
  ul.replaceChildren();
  for (const o of s.origins) {
    const li = document.createElement("li");
    li.textContent = o + " ";
    const b = document.createElement("button");
    b.textContent = "Remove";
    b.addEventListener("click", async () => {
      const cur = (await chrome.storage.local.get({ origins: [] })).origins.filter((x) => x !== o);
      await chrome.storage.local.set({ origins: cur });
      // Another address on the same host shares the pattern in Firefox, so
      // the grant goes only when nothing left needs it.
      if (!cur.some((x) => originPattern(x) === originPattern(o))) chrome.permissions.remove({ origins: [originPattern(o)] }).catch(() => {});
      load();
    });
    li.append(b);
    ul.append(li);
  }
  $("keystate").textContent = s.key ? "A key is set." : "No key yet — calls will be refused.";
  $("deny").value = s.deny.join("\n");
  $("unsafe").checked = s.unsafe;
  $("follow").checked = s.follow;
  if (FIREFOX) {
    $("sites").hidden = false;
    const ok = await chrome.permissions.contains(ALL_SITES);
    $("sitestate").textContent = ok
      ? "Allowed. roost bridge can act in the pages a Claude session you allowed asks for."
      : "Not allowed yet. Firefox runs roost bridge's actions as scripts in the page, which needs this; until then every Claude request fails and says so.";
    $("allowsites").hidden = ok;
  }
}

$("allowsites").addEventListener("click", async () => {
  const ok = await chrome.permissions.request(ALL_SITES).catch(() => false);
  say(ok ? "Access to websites allowed." : "Firefox did not grant access to websites.");
  load();
});

$("add").addEventListener("click", async () => {
  let origin;
  try { origin = new URL($("origin").value.trim()).origin; } catch { return say("That is not an address."); }
  if (origin === "null") return say("That is not an address.");
  // Asked from a click, which is what permissions.request needs. Loopback is
  // already granted by the manifest; anything else is asked for here.
  const ok = await chrome.permissions.request({ origins: [originPattern(origin)] }).catch(() => false);
  if (!ok) return say(`The browser did not grant access to ${origin}.`);
  const cur = (await chrome.storage.local.get({ origins: [] })).origins;
  if (!cur.includes(origin)) cur.push(origin);
  await chrome.storage.local.set({ origins: cur });
  $("origin").value = "";
  say(`Added ${origin}. Open (or reload) roost there.`);
  load();
});

$("savekey").addEventListener("click", async () => {
  const key = $("key").value.trim().toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(key)) return say("A roost key is 64 hex characters — paste the whole line `roost browser-key` printed.");
  await chrome.storage.local.set({ key });
  $("key").value = "";
  say("Key saved.");
  load();
});

$("savedeny").addEventListener("click", async () => {
  const deny = $("deny").value.split("\n").map((l) => l.trim().toLowerCase()).filter(Boolean);
  await chrome.storage.local.set({ deny });
  say("Saved.");
});

$("unsafe").addEventListener("change", async (e) => {
  if (e.target.checked && !confirm("Unsafe mode lets every roost Claude act in this browser, with your logins, without asking. Turn it on?")) {
    e.target.checked = false;
    return;
  }
  await chrome.storage.local.set({ unsafe: e.target.checked });
  say(e.target.checked ? "Unsafe mode is ON." : "Unsafe mode is off.");
});

$("follow").addEventListener("change", async (e) => {
  await chrome.storage.local.set({ follow: e.target.checked });
  say(e.target.checked ? "Claude's tabs will come to the front." : "Claude's tabs will open behind yours.");
});

load();
