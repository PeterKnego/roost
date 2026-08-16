/* deadlight frontend: hash routing #/<mode>/<project>/<relpath> */
const $ = (s) => document.querySelector(s);
const api = async (path) => {
  const r = await fetch(path);
  const j = await r.json();
  if (j && j.error) throw new Error(j.error);
  return j;
};

let projects = [];
const state = { project: null, mode: "files", file: null, openDirs: new Set() };

function projRoot() { return projects.find((p) => p.name === state.project)?.path; }

function setHash() {
  location.hash = `#/${state.mode}/${state.project || ""}/${state.file || ""}`;
}

function fromHash() {
  const m = location.hash.match(/^#\/(files|changes)\/([^/]*)\/?(.*)$/);
  if (m) { state.mode = m[1]; state.project = m[2] || state.project; state.file = m[3] || null; }
}

async function loadProjects() {
  projects = await api("/api/projects");
  const sel = $("#project");
  sel.innerHTML = projects.map((p) => `<option>${p.name}</option>`).join("");
  if (!state.project || !projects.some((p) => p.name === state.project)) {
    state.project = projects[0]?.name;
  }
  sel.value = state.project;
}

/* ---------- file tree ---------- */
async function renderTree() {
  const root = projRoot();
  if (!root) return;
  const nav = $("#sidebar");
  nav.innerHTML = "";
  nav.appendChild(await treeLevel(root, ""));
}

async function treeLevel(root, rel) {
  const entries = await api(`/api/tree?path=${encodeURIComponent(root + (rel ? "/" + rel : ""))}`);
  const ul = document.createElement("ul");
  for (const e of entries) {
    const li = document.createElement("li");
    const erel = rel ? `${rel}/${e.name}` : e.name;
    const a = document.createElement("a");
    a.textContent = (e.dir ? "▸ " : "") + e.name;
    a.className = e.dir ? "dir" : "file" + (state.file === erel ? " sel" : "");
    a.href = "javascript:;";
    a.onclick = async () => {
      if (e.dir) {
        if (li.querySelector("ul")) { li.querySelector("ul").remove(); a.textContent = "▸ " + e.name; state.openDirs.delete(erel); }
        else { li.appendChild(await treeLevel(root, erel)); a.textContent = "▾ " + e.name; state.openDirs.add(erel); }
      } else {
        state.file = erel; setHash(); showFile();
        document.querySelectorAll("#sidebar a.sel").forEach((x) => x.classList.remove("sel"));
        a.classList.add("sel");
      }
    };
    li.appendChild(a);
    ul.appendChild(li);
    if (e.dir && state.openDirs.has(erel)) {
      a.textContent = "▾ " + e.name;
      li.appendChild(await treeLevel(root, erel));
    }
  }
  return ul;
}

/* ---------- changes ---------- */
async function renderChanges() {
  const root = projRoot();
  const nav = $("#sidebar");
  try {
    const st = await api(`/api/git/status?repo=${encodeURIComponent(root)}`);
    $("#branch").textContent = st.branch ? `⎇ ${st.branch}` : "";
    $("#change-count").textContent = st.changes.length ? `(${st.changes.length})` : "";
    if (state.mode !== "changes") return;
    const ul = document.createElement("ul");
    const all = document.createElement("li");
    all.innerHTML = `<a href="javascript:;" class="file"><b>— full diff —</b></a>`;
    all.firstChild.onclick = () => { state.file = null; setHash(); showDiff(null); };
    ul.appendChild(all);
    for (const c of st.changes) {
      const li = document.createElement("li");
      const a = document.createElement("a");
      a.href = "javascript:;";
      a.className = "file" + (state.file === c.path ? " sel" : "");
      a.innerHTML = `<span class="xy">${c.xy}</span> ${c.path}`;
      a.onclick = () => {
        state.file = c.path; setHash(); showDiff(c.path);
        document.querySelectorAll("#sidebar a.sel").forEach((x) => x.classList.remove("sel"));
        a.classList.add("sel");
      };
      li.appendChild(a);
      ul.appendChild(li);
    }
    nav.innerHTML = "";
    nav.appendChild(ul);
    if (!st.changes.length) nav.innerHTML = "<div class='hint'>working tree clean</div>";
  } catch (e) {
    if (state.mode === "changes") nav.innerHTML = `<div class='hint'>${e.message}</div>`;
    $("#branch").textContent = "";
    $("#change-count").textContent = "";
  }
}

/* ---------- content pane ---------- */
async function showFile() {
  if (!state.file) return;
  const c = $("#content");
  try {
    const f = await api(`/api/file?path=${encodeURIComponent(projRoot() + "/" + state.file)}`);
    const ext = state.file.split(".").pop().toLowerCase();
    if (ext === "md" || ext === "markdown") {
      c.innerHTML = `<article class="markdown-body">${marked.parse(f.content)}</article>`;
      c.querySelectorAll("pre code").forEach((b) => hljs.highlightElement(b));
    } else {
      const lang = hljs.getLanguage(ext) ? ext : "";
      const code = lang ? hljs.highlight(f.content, { language: lang }).value
                        : hljs.highlightAuto(f.content).value;
      c.innerHTML = `<div class="path">${state.file}</div><pre class="codeview"><code>${code}</code></pre>`;
    }
  } catch (e) {
    c.innerHTML = `<div class="hint">${e.message}</div>`;
  }
}

async function showDiff(path) {
  const c = $("#content");
  try {
    const d = await api(`/api/git/diff?repo=${encodeURIComponent(projRoot())}${path ? "&path=" + encodeURIComponent(path) : ""}`);
    if (!d.diff.trim()) { c.innerHTML = "<div class='hint'>no diff</div>"; return; }
    const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;");
    const html = d.diff.split("\n").map((l) => {
      let cls = "ctx";
      if (l.startsWith("+++") || l.startsWith("---") || l.startsWith("diff ")) cls = "meta";
      else if (l.startsWith("@@")) cls = "hunk";
      else if (l.startsWith("+")) cls = "add";
      else if (l.startsWith("-")) cls = "del";
      return `<div class="dl ${cls}">${esc(l) || " "}</div>`;
    }).join("");
    c.innerHTML = `<div class="path">${path || "all changes"}</div><div class="diffview">${html}</div>`;
  } catch (e) {
    c.innerHTML = `<div class="hint">${e.message}</div>`;
  }
}

/* ---------- wiring ---------- */
async function refresh() {
  await loadProjects();
  await renderChanges();               // updates badge + branch always
  if (state.mode === "files") await renderTree();
  if (state.file) state.mode === "files" ? showFile() : showDiff(state.file);
  $("#tab-files").classList.toggle("active", state.mode === "files");
  $("#tab-changes").classList.toggle("active", state.mode === "changes");
}

$("#project").onchange = (e) => { state.project = e.target.value; state.file = null; state.openDirs.clear(); setHash(); refresh(); };
$("#tab-files").onclick = () => { state.mode = "files"; state.file = null; setHash(); refresh(); };
$("#tab-changes").onclick = () => { state.mode = "changes"; state.file = null; setHash(); refresh(); };
$("#refresh").onclick = refresh;
document.addEventListener("keydown", (e) => { if (e.key === "r" && !e.metaKey && !e.ctrlKey && document.activeElement.tagName !== "SELECT") refresh(); });
window.addEventListener("focus", () => renderChanges());
window.addEventListener("hashchange", () => { fromHash(); refresh(); });

fromHash();
refresh();
