"use strict";
const id = new URLSearchParams(location.search).get("id");

function row(dl, k, v) {
  const dt = document.createElement("dt"); dt.textContent = k;
  const dd = document.createElement("dd"); dd.textContent = v;
  dl.append(dt, dd);
}

chrome.runtime.sendMessage({ type: "consent-info", id }).then((info) => {
  if (!info) { document.querySelector("main").textContent = "This request is no longer waiting."; return; }
  const dl = document.getElementById("who");
  row(dl, "roost", info.origin);
  row(dl, "project", info.project);
  row(dl, "terminal", info.session);
  row(dl, "Claude pid", String(info.pid));
  document.getElementById("what").textContent = `${info.tool} ${JSON.stringify(info.args, null, 2)}`;
});

// Firefox has no debugger API, so roost bridge acts in pages by running
// scripts in them, and that needs the optional access to all websites.
// permissions.request is honoured only from a user gesture in an extension
// page — which this Allow click is — and it must be the handler's first call:
// any await before it ends the gesture.
const FIREFOX = chrome.runtime.getURL("").startsWith("moz-extension:");

for (const answer of ["session", "once", "deny"]) {
  document.getElementById(answer).addEventListener("click", async () => {
    const asked = FIREFOX && answer !== "deny" ? chrome.permissions.request({ origins: ["<all_urls>"] }) : null;
    // The browser's own prompt opens in this window; closing it first would
    // take the prompt with it.
    if (asked) await asked.catch(() => false);
    await chrome.runtime.sendMessage({ type: "consent-answer", id, answer });
    window.close();
  });
}
