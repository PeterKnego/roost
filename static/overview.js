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
    // The way-in control is a real link and must be left alone.
    if (e.target.closest(".ovgo")) return;
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

  // Adding a root. The front page has no socket of its own, so the one write
  // it makes travels on /ws/_roots — one exchange, then closed. Everything
  // shown comes back through textContent: a path is data.
  function renderRoots(list) {
    const span = document.querySelector("header .roots");
    if (!span) return;
    span.replaceChildren();
    for (const r of list) {
      const s = document.createElement("span"); s.className = "root"; s.textContent = r;
      span.appendChild(s);
    }
    // The list shows as one truncated line; the whole of it is the tooltip,
    // exactly as the server renders it (render.rs's `roots_title`).
    span.title = list.join(":");
  }
  // One exchange per connection, whichever intent it carries.
  function sendRoots(intent) {
    return new Promise((resolve) => {
      const ws = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws/_roots`);
      let done = false;
      const finish = (v) => { if (!done) { done = true; resolve(v); } try { ws.close(); } catch {} };
      ws.onopen = () => ws.send(JSON.stringify(intent));
      ws.onmessage = (e) => { try { finish(JSON.parse(e.data)); } catch { finish({ t: "Error", msg: "unreadable reply" }); } };
      ws.onerror = () => finish({ t: "Error", msg: "could not reach roost" });
      ws.onclose = () => finish({ t: "Error", msg: "connection closed before a reply" });
    });
  }

  // The roots roost is serving, read back out of the header this page already
  // renders — the same list `renderRoots` writes, so there is no second copy
  // to drift. Used only to decide whether to *ask* which root; the server
  // re-validates whatever comes back, because the dialog that offered it is a
  // hint and not an authorisation.
  function currentRoots() {
    return Array.from(document.querySelectorAll("header .roots .root")).map((s) => s.textContent);
  }

  // What the user typed decides what happens: absolute means a root, anything
  // else means a project under one. A relative path has no reading as a root —
  // a root is a place on disk roost scans, and there is nothing for it to be
  // relative to — so the two cannot collide. `~` counts as absolute: the
  // server expands it (`config::expand_home`) and a person typing `~/work`
  // plainly means a place, not a project name.
  const looksAbsolute = (s) => s.startsWith("/") || s.startsWith("~");

  // `reason` is the previous attempt's refusal. It goes in the reopened
  // dialog's own label rather than a banner: the dialog is modal and covers
  // the banner (it cannot even be clicked), so a banner said why exactly
  // where it could not be read.
  async function addRootFlow(prefill = "", reason = "") {
    const label = reason
      ? `${reason} — try again`
      : "A name makes a project here; an absolute path adds a place to look for them";
    const text = await askText({ title: "New project", label, value: prefill, confirm: "Create" });
    if (!text) return;
    const input = text.trim();
    if (!input) return addRootFlow(text, "enter a name or a path");
    return looksAbsolute(input) ? addRoot(input) : makeProject(input);
  }

  async function addRoot(path, create = false) {
    const reply = await sendRoots({ t: "AddRoot", path, create });
    if (reply.t === "Roots") {
      renderRoots(reply.roots);
      const sel = selNow();
      refresh("proj", sel);
      refresh("sess", sel);
      return;
    }
    // The refusal the confirmation can answer — and the only one. Everything
    // else reopens the text dialog with the reason on its label, as before.
    if (reply.missing) {
      const yes = await askConfirm({
        title: "Create it?",
        lines: [`${path} does not exist.`, "Create the directory and add it as a project root?"],
        confirm: "Create",
      });
      if (!yes) return;
      return addRoot(path, true);
    }
    // A typo is one edit away: reopen with the text kept, and with the reason
    // on the label.
    return addRootFlow(path, reply.msg);
  }

  async function makeProject(rel) {
    const roots = currentRoots();
    let root = roots.length === 1 ? roots[0] : null;
    if (roots.length > 1) {
      // Never a silent choice about where a folder lands on disk. The paths
      // are the labels; there is nothing shorter that stays unambiguous.
      root = await askChoice({
        title: "Which project root?",
        lines: [`Make ${rel} in:`],
        choices: roots.map((r) => ({ id: r, label: r })),
      });
      if (!root) return;
    }
    const reply = await sendRoots({ t: "NewProject", rel, root });
    if (reply.t !== "Project") return addRootFlow(rel, reply.msg);
    // Refreshed and selected, not navigated to: creating a project and being
    // thrown out of the page you created it from is a bigger move than was
    // asked for, and the row is one click from opening.
    //
    // `select(key, true)` first, for the history entry and so a reload comes
    // back to it — the same thing a click on a row does. The projects pane is
    // then refetched with the new key as `sel`, because `select` only marks
    // rows that are already on screen and this one is not yet among them.
    select(reply.key, true);
    refresh("proj", reply.key);
  }

  document.addEventListener("click", (e) => {
    if (e.target.closest("#addroot, .addroot")) { e.preventDefault(); addRootFlow(); }
  });

  // Double-click opens, the way a double click has always meant "go in" —
  // the picker this page replaced used it to descend.
  document.addEventListener("dblclick", (e) => {
    const row = e.target.closest("#ovprojects .ovrow:not(.unreachable)");
    const go = row && row.querySelector(".ovgo");
    if (!go) return;
    e.preventDefault();
    location.href = go.getAttribute("href");
  });

  // Back/forward must move the panes, not just the address bar.
  addEventListener("popstate", () => select(selNow(), false));

  // The server treats a selected project as open, so seed the set to match —
  // otherwise the first caret click on it would send `open=` without it and
  // collapse what the page is already showing.
  if (selNow()) open.add(selNow());
})();
