// Directory picker on `/`. Plain JS, no framework — same idiom as app.js.
// This page does a real navigation on every action (descend, go up, open),
// so all state here is just module-scoped, not persisted anywhere.
(() => {
  const picker = document.getElementById("picker");
  const openBtn = document.getElementById("openBtn");
  if (!picker || !openBtn) return; // nothing to wire if the picker markup isn't present

  const at = picker.dataset.at || "";
  const dirRows = () => Array.from(picker.querySelectorAll("li.dir"));
  let selected = null;

  function select(li) {
    if (selected) selected.classList.remove("sel");
    selected = li;
    if (selected) selected.classList.add("sel");
    // With a selection, Open acts on it. With none, Open acts on the
    // directory currently being browsed — but the synthetic top level
    // (both ROOTS merged) isn't itself a real, openable directory, so with
    // no selection there Open has nothing to do.
    openBtn.disabled = !selected && at === "";
  }

  // Browsing (single click / arrow keys / breadcrumb) stays inside the
  // picker via `?at=`, which must preserve `/` as a real path separator —
  // matching render.rs's own slash-preserving encode for this same query
  // param (see breadcrumb in render.rs), not app.js's plain
  // encodeURIComponent(rel) convention for opaque `path=`/`dir=` values.
  function descend(rel) {
    location.href = "/?at=" + rel.split("/").map(encodeURIComponent).join("/");
  }

  // Opening builds a real path, so every segment is encoded but `/` stays
  // a literal separator between them.
  function open(rel) {
    location.href = "/" + rel.split("/").map(encodeURIComponent).join("/");
  }

  function parentOf(rel) {
    const i = rel.lastIndexOf("/");
    return i === -1 ? "" : rel.slice(0, i);
  }

  // Files are listed (greyed out via CSS) but carry no click/dblclick
  // handler at all here — that omission, not just the styling, is what
  // makes them actually unselectable rather than merely muted-looking.
  dirRows().forEach((li) => {
    li.addEventListener("click", () => select(li));
    li.addEventListener("dblclick", () => descend(li.dataset.rel));
  });

  // The per-row git shortcut (render.rs's `a.git`) is a real `<a href>` so
  // the browser handles the actual navigation (and Enter/middle-click/
  // ctrl-click) on its own — this only has to stop the click/dblclick from
  // bubbling up to the row's own listeners just above, which would select
  // or descend the row in addition to the icon's own navigation.
  Array.from(picker.querySelectorAll("a.git")).forEach((a) => {
    a.addEventListener("click", (e) => e.stopPropagation());
    a.addEventListener("dblclick", (e) => e.stopPropagation());
  });

  openBtn.addEventListener("click", () => {
    if (selected) return open(selected.dataset.rel);
    if (at !== "") return open(at);
  });

  picker.addEventListener("keydown", (e) => {
    const rows = dirRows();
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (rows.length === 0) return;
      const i = selected ? rows.indexOf(selected) : -1;
      const next =
        e.key === "ArrowDown" ? rows[Math.min(i + 1, rows.length - 1)] : rows[Math.max(i - 1, 0)];
      select(next);
      next.scrollIntoView({ block: "nearest" });
    } else if (e.key === "Enter") {
      // Enter descends into the selection, mirroring double-click, rather
      // than duplicating the [Open] button: Backspace already goes back up
      // one level (below), so Enter/Backspace form one symmetric
      // keyboard-only up/down navigation pair — the file-manager convention
      // (Midnight Commander, Explorer's own list) — while [Open] stays the
      // single, deliberate "finalize" action, reachable by mouse or by
      // Tab-ing to its own button.
      if (selected) {
        e.preventDefault();
        descend(selected.dataset.rel);
      }
    } else if (e.key === "Backspace") {
      e.preventDefault();
      if (at !== "") descend(parentOf(at));
    }
  });

  // So arrow keys work immediately without requiring a click first.
  picker.focus();
  openBtn.disabled = !selected && at === "";
})();
