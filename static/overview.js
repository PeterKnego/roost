// The overview front page. Plain JS, no framework — same idiom as picker.js.
// Two htmx panes poll every few seconds; this keeps the client-only state
// (which projects are expanded) and re-applies it after each swap, and turns
// a row click into selection (a ?sel= navigation) or expansion (local).
(() => {
  const expanded = new Set();      // storage keys of expanded parents
  const projects = () => document.getElementById("ovprojects");

  function applyExpansion() {
    const root = projects();
    if (!root) return;
    root.querySelectorAll(".ovrow.child").forEach((li) => {
      const parent = li.dataset.parent;
      li.style.display = expanded.has(parent) ? "" : "none";
    });
    root.querySelectorAll(".ovrow:not(.child)").forEach((li) => {
      const caret = li.querySelector(".ovcaret");
      if (caret && !caret.classList.contains("placeholder")) {
        caret.textContent = expanded.has(li.dataset.key) ? "▾" : "▸";
      }
    });
  }

  // Delegated: the panes are replaced by htmx, so listen on a stable root.
  document.addEventListener("click", (e) => {
    const caret = e.target.closest(".ovcaret:not(.placeholder)");
    if (caret) {
      const li = caret.closest(".ovrow");
      const key = li.dataset.key;
      if (expanded.has(key)) expanded.delete(key); else expanded.add(key);
      applyExpansion();
      e.preventDefault();
      return;
    }
    // A plain click on a row selects it (filters the right pane) without
    // leaving the overview; ⌘/ctrl-click falls through to the row's <a>
    // (open the project in a new tab), the browser's own way.
    const row = e.target.closest("#ovprojects .ovrow:not(.unreachable)");
    if (row && !e.metaKey && !e.ctrlKey) {
      e.preventDefault();
      const url = new URL(location.href);
      url.searchParams.set("sel", row.dataset.key);
      location.href = url.pathname + "?" + url.searchParams.toString();
    }
  });

  // htmx swaps the left fragment on every poll — re-apply expansion after.
  document.body.addEventListener("htmx:afterSwap", (e) => {
    if (e.target && e.target.id === "ovprojects") applyExpansion();
  });
})();
