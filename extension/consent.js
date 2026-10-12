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

for (const answer of ["session", "once", "deny"]) {
  document.getElementById(answer).addEventListener("click", async () => {
    await chrome.runtime.sendMessage({ type: "consent-answer", id, answer });
    window.close();
  });
}
