"use strict";
async function render() {
  const s = await chrome.storage.local.get({ origins: [], key: "", unsafe: false });
  const { grants = {} } = await chrome.storage.session.get({ grants: {} });
  const bits = [];
  if (s.unsafe) bits.push("⚠ UNSAFE MODE: every call is allowed without asking.");
  if (!s.key) bits.push("No key set — open Options.");
  if (!s.origins.length) bits.push("No roost address set — open Options.");
  if (!bits.length) bits.push(`Relaying for ${s.origins.join(", ")}.`);
  const st = document.getElementById("state");
  st.textContent = bits.join(" ");
  st.className = s.unsafe ? "warn" : "fine";
  const ul = document.getElementById("grants");
  ul.replaceChildren();
  const keys = Object.keys(grants);
  if (!keys.length) { const li = document.createElement("li"); li.textContent = "none"; ul.append(li); }
  for (const k of keys) {
    const g = grants[k];
    const li = document.createElement("li");
    li.textContent = `${g.project} · ${g.session} · pid ${g.pid} `;
    const b = document.createElement("button");
    b.textContent = "Revoke";
    b.addEventListener("click", async () => {
      const cur = (await chrome.storage.session.get({ grants: {} })).grants;
      delete cur[k];
      await chrome.storage.session.set({ grants: cur });
      render();
    });
    li.append(b);
    ul.append(li);
  }
}
document.getElementById("options").addEventListener("click", () => chrome.runtime.openOptionsPage());
render();
