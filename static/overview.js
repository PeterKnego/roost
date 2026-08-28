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

  function refresh(which, sel) {
    const node = el(panes[which]);
    if (!node || !window.htmx) return;
    htmx.ajax("GET", which === "proj" ? projectsUrl(sel) : sessionsUrl(sel), `#${panes[which]}`);
  }

  // The refresh loop lives here rather than in `hx-trigger="every 5s"`,
  // because htmx captures a polling element's URL when it processes the
  // node: rewriting `hx-get` afterwards does not reach the poll, so five
  // seconds after every click both panes refetched the URL the page was
  // opened with and threw the selection away — the tree appeared to switch
  // projects on its own. Driving it from here means each tick asks for
  // whatever is selected and open *now*.
  setInterval(() => {
    const sel = selNow();
    refresh("proj", sel);
    refresh("sess", sel);
  }, 5000);

  // Splice one project's worktrees in under its row, or take them out
  // again. Selecting or opening a project must never re-fetch the list it
  // was chosen from — the only thing the server can add is that project's
  // own children, so that is all that is asked for.
  async function setOpen(key, wanted) {
    const row = document.querySelector(`#ovprojects .ovrow[data-key="${CSS.escape(key)}"]`);
    if (!row) return;
    const caret = row.querySelector(".ovcaret:not(.placeholder)");
    document
      .querySelectorAll(`#ovprojects .ovrow[data-parent="${CSS.escape(key)}"]`)
      .forEach((r) => r.remove());
    if (!wanted) {
      open.delete(key);
      if (caret) caret.textContent = "\u25b8";
      return;
    }
    open.add(key);
    const url =
      `/frag/_overview_worktrees?project=${encodeURIComponent(key)}` +
      `&sel=${encodeURIComponent(selNow())}`;
    const html = await (await fetch(url)).text();
    // The row may have been swapped out by a poll while this was in flight.
    const live = document.querySelector(`#ovprojects .ovrow[data-key="${CSS.escape(key)}"]`);
    if (!live) return;
    if (html.trim()) {
      live.insertAdjacentHTML("afterend", html);
      const c = live.querySelector(".ovcaret:not(.placeholder)");
      if (c) c.textContent = "\u25be";
    } else if (caret) {
      // Opened and nothing came back: it is not an expander, and should not
      // keep inviting the click that taught us so.
      caret.classList.add("empty");
      caret.title = "no worktrees";
    }
  }

  function select(sel, push) {
    if (push) {
      const url = sel ? `${location.pathname}?sel=${encodeURIComponent(sel)}` : location.pathname;
      history.pushState({ sel }, "", url);
    }
    // The selection is a property of the list already on screen: mark it
    // here rather than asking the server to render the same rows again.
    document.querySelectorAll("#ovprojects .ovrow").forEach((r) => {
      r.classList.toggle("current", !!sel && r.dataset.key === sel);
    });
    refresh("sess", sel);
  }

  // Delegated: htmx replaces the panes wholesale, so nothing may be bound to
  // a row.
  document.addEventListener("click", (e) => {
    const caret = e.target.closest("#ovprojects .ovcaret:not(.placeholder):not(.empty)");
    if (caret) {
      e.preventDefault();
      const key = caret.closest(".ovrow").dataset.key;
      setOpen(key, !open.has(key));
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
      select(key, true);
      if (!row.classList.contains("child") && !open.has(key)) setOpen(key, true);
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
