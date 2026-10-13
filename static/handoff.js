// The handoff page: a log shared by whoever is in this browser (a person, or
// a browser agent such as Claude in Chrome) and the Claude running in roost
// for this project. Everything travels over the project's workspace socket;
// see render::handoff_page for why the page is this plain.
"use strict";

const PROJECT = document.body.dataset.project;
const log = document.getElementById("log");
const status = document.getElementById("status");
const form = document.getElementById("send");
const box = document.getElementById("message");
const shown = new Set();
let sock = null;
let tries = 0;

const wsUrl = (p) => `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}${p}`;

function render(m) {
  if (shown.has(m.id)) return;
  shown.add(m.id);
  const li = document.createElement("li");
  li.dataset.id = m.id;
  li.dataset.from = m.from;
  const who = document.createElement("div");
  who.className = "who";
  const when = new Date(m.at * 1000).toLocaleTimeString();
  // The id stays in data-id: it starts at the server's start time in
  // milliseconds, which is a number for `since`, not one for a person.
  who.textContent = `${m.from === "browser" ? "browser" : m.from.replace(/^claude:/, "roost Claude in ")} · ${when}`;
  const pre = document.createElement("pre");
  pre.textContent = m.text;
  li.append(who, pre);
  log.append(li);
  li.scrollIntoView({ block: "nearest" });
}

function connect() {
  const s = new WebSocket(wsUrl(`/ws/${PROJECT}/_workspace`));
  sock = s;
  s.onopen = () => {
    if (sock !== s) return;
    tries = 0;
    status.textContent = "connected";
    s.send(JSON.stringify({ t: "HandoffSync" }));
  };
  s.onmessage = (e) => {
    if (sock !== s) return;
    const ev = JSON.parse(e.data);
    if (ev.t === "Handoff") {
      for (const m of ev.messages) render(m);
    } else if (ev.t === "Error") {
      status.textContent = `not sent: ${ev.msg}`;
    }
  };
  s.onclose = () => {
    if (sock !== s) return;
    status.textContent = "reconnecting…";
    setTimeout(connect, Math.min(500 * 2 ** tries++, 8000));
  };
}

form.addEventListener("submit", (e) => {
  e.preventDefault();
  const text = box.value;
  if (!text.trim()) return;
  if (!sock || sock.readyState !== 1) {
    status.textContent = "not connected — the message was not sent";
    return;
  }
  sock.send(JSON.stringify({ t: "HandoffPost", text }));
  box.value = "";
  status.textContent = "sent";
});

// Enter in the box sends, Shift+Enter is a newline — what an agent typing
// into a textarea and pressing Enter expects.
box.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    form.requestSubmit();
  }
});

connect();
