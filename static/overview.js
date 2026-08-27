// The overview front page. Plain JS, no framework — same idiom as picker.js.
//
// Nothing here navigates. Selecting a project used to set `?sel=` on
// `location`, which reloaded the document: both panes emptied and refilled,
// so the list visibly blinked on every click. Instead the panes are swapped
// in place through htmx and the address bar is updated with `pushState`, so
// the URL stays shareable and Back still works — the page behaves like the
// workspace it sits in front of.
//
// Expansion is not client state. The server renders a project's worktrees
// only when its key is in `open`, and the arrow direction is read off the
// response (children present ⇒ expanded), so there is nothing to re-apply
// after a poll swaps the pane out. `open` is deliberately kept out of the
// address bar: it is view state, not a place.
(() => {
  const panes = { proj: "ovprojects", sess: "ovsessions" };
  const open = new Set(); // storage keys of expanded projects
  const el = (id) => document.getElementById(id);
  const selNow = () => new URLSearchParams(location.search).get("sel") || "";

  const projectsUrl = (sel) =>
    `/frag/_overview_projects?sel=${encodeURIComponent(sel)}` +
    (open.size ? `&open=${[...open].map(encodeURIComponent).join(",")}` : "");
  const sessionsUrl = (sel) => `/frag/_overview_sessions?sel=${encodeURIComponent(sel)}`;

  // Both panes poll on their own `hx-get`, so that attribute is the single
  // source of truth for what the next poll asks: refresh it before firing a
  // request, or the poll five seconds later would undo the click.
  function refresh(which, sel) {
    const node = el(panes[which]);
    if (!node) return;
    const url = which === "proj" ? projectsUrl(sel) : sessionsUrl(sel);
    node.setAttribute("hx-get", url);
    if (window.htmx) htmx.ajax("GET", url, `#${panes[which]}`);
  }

  function select(sel, push) {
    if (push) {
      const url = sel ? `${location.pathname}?sel=${encodeURIComponent(sel)}` : location.pathname;
      history.pushState({ sel }, "", url);
    }
    // Mark the row immediately rather than waiting for the round trip — the
    // click should feel answered even on a slow fetch.
    document.querySelectorAll("#ovprojects .ovrow").forEach((r) => {
      r.classList.toggle("current", !!sel && r.dataset.key === sel);
    });
    refresh("proj", sel);
    refresh("sess", sel);
  }

  // Delegated: htmx replaces the panes wholesale, so nothing may be bound to
  // a row.
  document.addEventListener("click", (e) => {
    const caret = e.target.closest("#ovprojects .ovcaret:not(.placeholder)");
    if (caret) {
      e.preventDefault();
      const key = caret.closest(".ovrow").dataset.key;
      if (open.has(key)) open.delete(key);
      else open.add(key);
      refresh("proj", selNow());
      return;
    }
    const all = e.target.closest(".ovall");
    if (all) {
      e.preventDefault();
      select("", true);
      return;
    }
    // A plain click selects (and opens the project's worktrees, which is
    // where the git cost is paid); ⌘/ctrl-click falls through to the row's
    // own <a> so the browser can open the project in a new tab.
    const row = e.target.closest("#ovprojects .ovrow:not(.unreachable)");
    if (row && !e.metaKey && !e.ctrlKey) {
      e.preventDefault();
      const key = row.dataset.key;
      if (!row.classList.contains("child")) open.add(key);
      select(key, true);
    }
  });

  // Most projects have no worktrees, so opening one would otherwise be a
  // click that visibly does nothing. The server cannot say so — it renders
  // rows, and there are none — but the client knows what it asked to open,
  // so an expander that produced no children says as much and stops
  // pretending it is an expander.
  document.body.addEventListener("htmx:afterSwap", (e) => {
    if (!e.target || e.target.id !== "ovprojects") return;
    for (const key of open) {
      const row = document.querySelector(`#ovprojects .ovrow[data-key="${CSS.escape(key)}"]`);
      const caret = row && row.querySelector(".ovcaret");
      if (!caret) continue;
      if (!document.querySelector(`#ovprojects .ovrow[data-parent="${CSS.escape(key)}"]`)) {
        caret.classList.add("empty");
        caret.title = "no worktrees";
      }
    }
  });

  // Back/forward must move the panes, not just the address bar.
  addEventListener("popstate", () => select(selNow(), false));

  // The server treats a selected project as open, so seed the set to match —
  // otherwise the first caret click on it would send `open=` without it and
  // collapse what the page is already showing.
  if (selNow()) open.add(selNow());
})();
