/* deadlight glue: terminal wiring, tabs, hash mirror, refresh.
   No pushState anywhere — the Back button must never traverse app flow. */
const PROJECT = document.body.dataset.project;
htmx.config.historyCacheSize = 0;

/* ---- in-page view state (mirrored to the hash, never to history) ---- */
let mode = document.body.dataset.defaultTab || "terminal";
let openRel = null;
const m = location.hash.match(/^#(terminal|files|changes)(?:\/(.*))?$/);
if (m) { mode = m[1]; openRel = m[2] ? decodeURIComponent(m[2]) : null; }

function mirror() {
  // encodeURI keeps "/" literal so the hash stays readable: #files/src/main.rs
  history.replaceState(null, "", "#" + mode + (openRel ? "/" + encodeURI(openRel) : ""));
}

/* ---- tabs ---- */
const termPane = document.getElementById("term-pane");
const viewer = document.getElementById("viewer");
function setMode(next, rel) {
  mode = next;
  openRel = rel ?? null;
  termPane.classList.toggle("hidden", mode !== "terminal");
  viewer.classList.toggle("hidden", mode === "terminal");
  for (const t of ["terminal", "files", "changes"])
    document.getElementById("tab-" + t).classList.toggle("active", mode === t);
  if (mode === "files") {
    htmx.ajax("GET", "/frag/" + PROJECT + "/tree" + (openRel ? "?open=" + encodeURIComponent(openRel) : ""), "#sidebar");
    if (openRel) htmx.ajax("GET", "/frag/" + PROJECT + "/file?path=" + encodeURIComponent(openRel), "#content");
    else document.getElementById("content").innerHTML = "";
  } else if (mode === "changes") {
    htmx.ajax("GET", "/frag/" + PROJECT + "/changes", "#sidebar");
    if (openRel) htmx.ajax("GET", "/frag/" + PROJECT + "/diff?path=" + encodeURIComponent(openRel), "#content");
    else document.getElementById("content").innerHTML = "";
  } else {
    fit();
    term.focus();
  }
  mirror();
}
document.getElementById("tab-terminal").onclick = () => setMode("terminal");
document.getElementById("tab-files").onclick = () => setMode("files", openRel);
document.getElementById("tab-changes").onclick = () => setMode("changes", openRel);

/* track the open file/diff for the hash + selection highlight */
document.body.addEventListener("click", (e) => {
  const a = e.target.closest("a[data-rel]");
  if (!a) return;
  openRel = a.dataset.rel || null;
  document.querySelectorAll("#sidebar a.sel").forEach((x) => x.classList.remove("sel"));
  if (a.dataset.rel) a.classList.add("sel");
  mirror();
});

/* highlight code after htmx swaps */
document.body.addEventListener("htmx:afterSwap", (e) => {
  e.target.querySelectorAll("pre.codeview code").forEach((b) => hljs.highlightElement(b));
});

/* refresh re-fetches the current panes; the terminal is never touched */
function refresh() {
  document.body.dispatchEvent(new Event("refresh")); // #gitinfo hx-trigger listens
  if (mode !== "terminal") setMode(mode, openRel);
}
document.getElementById("refresh").onclick = refresh;
document.addEventListener("keydown", (e) => {
  if (e.key === "r" && !e.metaKey && !e.ctrlKey && mode !== "terminal") refresh();
});

/* ---- terminal ---- */
const css = getComputedStyle(document.documentElement);
const term = new Terminal({
  fontSize: 14,
  theme: {
    background: css.getPropertyValue("--bg").trim(),
    foreground: css.getPropertyValue("--fg").trim(),
  },
});
const fitAddon = new FitAddon.FitAddon();
term.loadAddon(fitAddon);
term.open(document.getElementById("term"));
let ws = null;
let retry = 250;
function fit() {
  try { fitAddon.fit(); } catch {}
  if (ws && ws.readyState === 1) ws.send("resize:" + term.cols + "x" + term.rows);
}
function connect() {
  ws = new WebSocket(
    (location.protocol === "https:" ? "wss://" : "ws://") + location.host + "/ws/" + PROJECT
  );
  ws.binaryType = "arraybuffer";
  ws.onopen = () => {
    retry = 250;
    document.getElementById("term-overlay").classList.add("hidden");
    fit();
    if (mode === "terminal") term.focus();
  };
  ws.onmessage = (e) => term.write(new Uint8Array(e.data));
  ws.onclose = () => {
    document.getElementById("term-overlay").classList.remove("hidden");
    setTimeout(connect, retry);
    retry = Math.min(retry * 2, 5000);
  };
}
term.onData((d) => {
  if (ws && ws.readyState === 1) ws.send(new TextEncoder().encode(d));
});
new ResizeObserver(() => { if (mode === "terminal") fit(); }).observe(termPane);
window.addEventListener("focus", () => {
  if (ws && ws.readyState !== 1) { try { ws.close(); } catch {} }
});

setMode(mode, openRel);
connect();
