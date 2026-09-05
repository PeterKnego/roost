// Modal dialogs. Replaces the native confirm/prompt/alert the workspace asked
// every question through: those cannot be themed, cannot disable a button, and
// cannot offer a menu — which is why the file menu spent so long as a numbered
// prompt().
//
// Built on <dialog>.showModal(), so focus trapping, Escape, the top layer and
// an inert background all come from the browser and none of them are
// implemented here. Being in the top layer is also why there is no z-index to
// coordinate with #searchoverlay (40) or body.searching header (41).
//
// Nothing here builds an HTML string. Every value that reaches the DOM goes
// through textContent or createElement, because a path is attacker-influenced:
// roost opens cloned repositories, and a repository may contain a file named
// `<img src=x onerror=...>`. Rendering that as markup would be script
// injection into the document that holds the websocket that spawns shells.
// Same rule as the notification centre and the markdown sanitizer — make the
// dangerous operation unreachable rather than remember to escape at each site.

// One dialog at a time. A second call resolves with the dismissal value rather
// than stacking: two modals over a live terminal is worse than a dropped stray
// event.
let openDlg = null;

// `fill` populates the shell and returns either nothing or a function to run
// once the dialog is actually laid out. Anything needing measurement or
// selection must go in that returned function — offsetWidth is 0 and
// setSelectionRange is unreliable while the dialog is still display:none.
function runDialog(el, fill, dismissed) {
  if (openDlg) return Promise.resolve(dismissed);
  const restore = document.activeElement;
  openDlg = el;
  return new Promise((resolve) => {
    let done = false;
    const finish = (v) => {
      if (done) return;          // Escape during a click handler, etc.
      done = true;
      openDlg = null;
      el.removeEventListener("cancel", onCancel);
      el.removeEventListener("click", onClick);
      el.close();
      // NOT insurance for roost's pooled xterm nodes — that reasoning does
      // not hold up. This line runs *after* el.close(), by which point the
      // platform's own focus restoration has already happened; a pooled
      // xterm moved between panes via appendChild is still connected to the
      // document throughout, so the platform restores focus to it exactly as
      // it would to any other still-attached element, and a node that really
      // had been removed could not be focused here either way. There is no
      // gap in either direction for this line to fill.
      //
      // Kept anyway, and cheap to keep: three lines in a try/catch. The real
      // reason is cross-browser, not xterm-specific — <dialog> focus
      // restoration has historically been less reliable outside Chromium
      // (Firefox, and Safari around 15.4), and roost runs in whatever browser
      // the user has, not only the one this test suite drives.
      //
      // Revert-check (2026-09-05): commenting out the next line did NOT make
      // assertion E — or any assertion — fail; `deno run -A
      // tests/browser/dialogs.mjs` still printed 14/14 ok and PASS. A
      // standalone probe (bare `<dialog>`, no app code: focus a button,
      // showModal(), close(), read document.activeElement) confirmed why:
      // Chromium's own <dialog> already restores focus to the element that
      // was focused before showModal() was called, with no JS involved at
      // all, for a plain still-attached element like #closeproj. That is the
      // only case dialogs.mjs exercises in Chromium, so it cannot
      // discriminate for this line as written — the line's justification is
      // the other engines this suite does not run against, not this one.
      try { if (restore && restore.focus) restore.focus(); } catch { /* gone */ }
      resolve(v);
    };
    // Escape arrives as `cancel`. preventDefault so the close path is the one
    // above and every exit resolves the promise exactly once.
    const onCancel = (e) => { e.preventDefault(); finish(dismissed); };
    // A click on the backdrop has the dialog element itself as its target.
    const onClick = (e) => { if (e.target === el) finish(dismissed); };
    el.addEventListener("cancel", onCancel);
    el.addEventListener("click", onClick);
    // A throw here (from `fill` or from `showModal`) would otherwise leave
    // `openDlg` set forever, since nothing past this point clears it: every
    // later `runDialog` call would then take the early-return path above and
    // silently resolve as dismissed — every confirmation answering "no" and
    // every menu doing nothing, for the rest of the page's life, with no
    // banner and no visible cause. A control that visibly does nothing is
    // indistinguishable from a broken one, which is worse than surfacing the
    // throw.
    try {
      const ready = fill(finish);
      el.showModal();
      if (ready) ready();
    } catch (err) {
      openDlg = null;
      resolve(dismissed);
      throw err;
    }
  });
}

function askConfirm({ title, lines = [], confirm = "OK", danger = false, blocked = "" }) {
  const el = document.getElementById("dlg-confirm");
  return runDialog(el, (finish) => {
    el.querySelector(".dlg-title").textContent = title;
    const body = el.querySelector(".dlg-body");
    body.replaceChildren();
    for (const line of lines) {
      const p = document.createElement("p");
      p.textContent = line;
      body.appendChild(p);
    }
    const why = el.querySelector(".dlg-blocked");
    why.textContent = blocked;
    why.hidden = !blocked;
    const okBtn = el.querySelector(".dlg-ok");
    const cancelBtn = el.querySelector(".dlg-cancel");
    okBtn.textContent = confirm;
    okBtn.disabled = !!blocked;
    okBtn.classList.toggle("danger", danger);
    okBtn.onclick = () => finish(true);
    cancelBtn.onclick = () => finish(false);
    // Destructive dialogs focus Cancel, so Enter cancels. Native confirm()
    // accepts on Enter; this deliberately does not, because the burden of
    // proof is on destroying, not on keeping.
    return () => (danger || blocked ? cancelBtn : okBtn).focus();
  }, false);
}

function askText({ title, label = "", value = "", confirm = "OK" }) {
  const el = document.getElementById("dlg-text");
  return runDialog(el, (finish) => {
    el.querySelector(".dlg-title").textContent = title;
    const lab = el.querySelector(".dlg-label");
    lab.textContent = label;
    lab.hidden = !label;
    const input = el.querySelector(".dlg-input");
    input.value = value;
    // Empty resolves null, never "": every caller guards with `if (name)`,
    // and an empty string would pass a truthiness check as a create or
    // rename of a path with no name.
    const take = () => finish(input.value.trim() || null);
    const okBtn = el.querySelector(".dlg-ok");
    okBtn.textContent = confirm;
    okBtn.disabled = false;
    okBtn.classList.remove("danger");
    okBtn.onclick = take;
    el.querySelector(".dlg-cancel").onclick = () => finish(null);
    // Enter confirms. A <form method="dialog"> would do this natively but
    // would also make the shell submit-shaped, and a stray Enter elsewhere in
    // the page then has a form to submit.
    input.onkeydown = (e) => { if (e.key === "Enter") { e.preventDefault(); take(); } };
    return () => {
      input.focus();
      // Select the basename only, so typing replaces the name and leaves the
      // directory. lastIndexOf returns -1 for a bare name, and -1 + 1 === 0
      // selects the whole thing, which is what a new file wants.
      input.setSelectionRange(value.lastIndexOf("/") + 1, value.length);
    };
  }, null);
}

// Several positive answers and a Cancel: "start in a new worktree / start
// here anyway / dismiss". Not a menu, because a menu is positioned at a
// pointer and has no title or body to say what is being asked; not a
// confirm, because a confirm has exactly one OK and the structural CSS locks
// that shape. Resolves the chosen id, or null for Cancel, Escape and the
// backdrop. The first choice takes focus, as a non-destructive confirm's OK
// does — unless `focus: "cancel"`, for a question where every answer destroys
// something (the save conflict: overwrite discards the disk's changes,
// discard-mine discards yours), so Enter destroys nothing.
//
// `detailHtml` is the one exception to "nothing here builds an HTML string":
// it is set as innerHTML, and the only caller passes the diff render.rs
// produced, which escapes every line before wrapping it. Never pass anything
// that came from the DOM or from a path here.
function askChoice({ title, lines = [], choices, detailHtml = "", focus = "first" }) {
  const el = document.getElementById("dlg-choice");
  return runDialog(el, (finish) => {
    el.querySelector(".dlg-title").textContent = title;
    const body = el.querySelector(".dlg-body");
    body.replaceChildren();
    for (const line of lines) {
      const p = document.createElement("p");
      p.textContent = line;
      body.appendChild(p);
    }
    const detail = el.querySelector(".dlg-detail");
    detail.innerHTML = detailHtml;
    detail.hidden = !detailHtml;
    const buttons = el.querySelector(".dlg-buttons");
    // Cancel is in the shell; the choices are rebuilt around it each time.
    buttons.querySelectorAll(".dlg-choice").forEach((b) => b.remove());
    const cancelBtn = el.querySelector(".dlg-cancel");
    cancelBtn.onclick = () => finish(null);
    let first = null;
    for (const c of choices) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "dlg-choice";
      b.dataset.choice = c.id;
      b.textContent = c.label;
      b.onclick = () => finish(c.id);
      buttons.appendChild(b);
      first = first || b;
    }
    return () => (focus === "cancel" ? cancelBtn : first || cancelBtn).focus();
  }, null);
}

function askMenu({ items, x, y }) {
  const el = document.getElementById("dlg-menu");
  return runDialog(el, (finish) => {
    const list = el.querySelector(".dlg-items");
    list.replaceChildren();
    for (const it of items) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "dlg-item";
      b.textContent = it.label;
      b.onclick = () => finish(it.id);
      list.appendChild(b);
    }
    // Focus IS the selection here — no separate highlight class. A class that
    // moves without focus leaves Enter activating whatever the browser still
    // considers focused, which is the wrong row.
    el.onkeydown = (e) => {
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      e.preventDefault();
      const btns = [...list.querySelectorAll(".dlg-item")];
      const i = btns.indexOf(document.activeElement);
      const n = (e.key === "ArrowDown" ? i + 1 : i - 1 + btns.length) % btns.length;
      btns[n].focus();
    };
    el.style.left = `${x}px`;
    el.style.top = `${y}px`;
    return () => {
      // Clamped after showModal: getBoundingClientRect reads 0 while the
      // dialog is still display:none, so measuring in fill() would clamp
      // against a zero-sized box and never move anything.
      //
      // Revert-check (2026-09-05): deleting the next two lines and re-running
      // `deno run -A tests/browser/dialogs.mjs` produced exactly one failure —
      // "FAIL  a menu at the viewport edge is clamped back on screen" — with
      // every other assertion (including the other menu ones) still ok,
      // confirming this clamp is what assertion L exercises and nothing else
      // depends on it.
      const r = el.getBoundingClientRect();
      if (r.right > innerWidth - 8) el.style.left = `${Math.max(8, innerWidth - r.width - 8)}px`;
      if (r.bottom > innerHeight - 8) el.style.top = `${Math.max(8, innerHeight - r.height - 8)}px`;
      const first = list.querySelector(".dlg-item");
      if (first) first.focus();
    };
  }, null);
}

// The settings dialog. Unlike the ask* shapes it stays open across several
// intents and snapshots, so it keeps its own state: which pane, which
// scope, what has been edited, and the theme the page opened with (for
// Cancel). `runDialog` still owns the modal mechanics. Everything rendered
// here comes from the snapshot through textContent/createElement — a hide
// entry or a root path is text from a config file in a cloned repository.
function openSettings(settings) {
  const el = document.getElementById("dlg-settings");
  const themeBefore = appliedTheme;
  let view = settings;
  let pane = "settings";
  let scope = "project";
  // key → { value, clear } for this scope only; reset when the scope changes.
  let edits = new Map();
  let previewTheme = null;

  const tabs = el.querySelector(".dlg-tabs");
  const scopeBar = el.querySelector(".dlg-scope");
  const rows = el.querySelector(".dlg-rows");
  const themes = el.querySelector(".dlg-themes");
  const okBtn = el.querySelector(".dlg-ok");
  const cancelBtn = el.querySelector(".dlg-cancel");

  const row = (k) => view.keys.find((r) => r.key === k);
  const inScope = (r) => (scope === "project" ? r.project : r.global);
  const fileName = () => (scope === "project" ? view.project_file : view.global_file);

  function renderTabs() {
    tabs.replaceChildren();
    for (const [id, label] of [["settings", "Settings"], ["theme", "Theme"]]) {
      const b = document.createElement("button");
      b.type = "button"; b.className = "dlg-tab"; b.dataset.tab = id; b.textContent = label;
      b.setAttribute("role", "tab"); b.setAttribute("aria-selected", String(pane === id));
      b.onclick = () => { pane = id; render(); };
      tabs.appendChild(b);
    }
  }
  function renderScope() {
    scopeBar.replaceChildren();
    const lab = document.createElement("span"); lab.textContent = "Scope:"; scopeBar.appendChild(lab);
    for (const [id, label] of [["project", "Project"], ["global", "Global"]]) {
      const b = document.createElement("button");
      b.type = "button"; b.dataset.scope = id; b.textContent = label;
      b.setAttribute("aria-pressed", String(scope === id));
      b.onclick = () => {
        if (scope === id) return;
        scope = id;
        edits = new Map();
        // A theme pick lives in `edits` like every other row, so the switch
        // discards it — but unlike the others it has already repainted the
        // page. Undo the paint here or Save writes nothing and the dialog
        // closes over a preview it never kept.
        if (previewTheme) { applyTheme(themeBefore); previewTheme = null; }
        render();
      };
      scopeBar.appendChild(b);
    }
    const f = document.createElement("span"); f.className = "file"; f.textContent = fileName(); scopeBar.appendChild(f);
  }
  function hintFor(r) {
    if (r.writable.length === 0) return "read-only — edit it by hand in the global config file";
    if (scope === "project" && !r.writable.includes("project")) return "global only";
    // Derive the source only from scopes this key is writable in: a key not
    // writable in project scope must never claim "from project" merely
    // because a project file happens to set it (a hand-edited or stale
    // value there is not what's actually in effect).
    const fromProject = r.writable.includes("project") && r.project !== null;
    const src = fromProject ? "from project" : r.global !== null ? "from global" : "default";
    const tail = r.reload ? " · other tabs pick this up on reload" : "";
    return `${src}${tail}`;
  }
  function control(r) {
    const cur = edits.has(r.key) ? edits.get(r.key).value : (inScope(r) ?? r.effective);
    if (r.kind === "bool") {
      const c = document.createElement("input"); c.type = "checkbox"; c.checked = cur === true;
      c.onchange = () => { edits.set(r.key, { value: c.checked, clear: false }); };
      return c;
    }
    if (r.kind === "list") {
      const t = document.createElement("textarea"); t.value = (Array.isArray(cur) ? cur : []).join("\n");
      t.oninput = () => { edits.set(r.key, { value: t.value.split("\n").map((s) => s.trim()).filter(Boolean), clear: false }); };
      return t;
    }
    const i = document.createElement("input"); i.type = "text"; i.value = String(cur ?? "");
    i.oninput = () => { edits.set(r.key, { value: i.value.trim(), clear: false }); };
    return i;
  }
  function renderRows() {
    rows.replaceChildren();
    for (const r of view.keys) {
      const div = document.createElement("div");
      div.className = "dlg-row"; div.dataset.key = r.key;
      const writable = r.writable.includes(scope);
      if (!writable) div.classList.add("disabled");
      const lab = document.createElement("label"); lab.textContent = r.key; div.appendChild(lab);
      if (r.writable.length === 0) {
        const ro = document.createElement("span"); ro.className = "ro";
        ro.textContent = Array.isArray(r.effective) ? r.effective.join(", ") : String(r.effective);
        div.appendChild(ro);
        div.appendChild(document.createElement("span"));
      } else {
        const c = control(r); c.disabled = !writable; div.appendChild(c);
        const side = document.createElement("span");
        if (writable && inScope(r) !== null && !(edits.get(r.key) || {}).clear) {
          const clr = document.createElement("button"); clr.type = "button"; clr.className = "clear"; clr.textContent = "Clear";
          clr.title = `remove ${r.key} from ${fileName()} so the inherited value applies`;
          clr.onclick = () => { edits.set(r.key, { value: null, clear: true }); render(); };
          side.appendChild(clr);
        }
        div.appendChild(side);
      }
      const hint = document.createElement("div"); hint.className = "hint";
      hint.textContent = (edits.get(r.key) || {}).clear ? "will be cleared on Save" : hintFor(r);
      div.appendChild(hint);
      rows.appendChild(div);
    }
  }
  function renderThemes() {
    themes.replaceChildren();
    const current = previewTheme || (row("theme") || {}).effective;
    for (const [kind, title] of [["roost", "roost"], ["daisy", "daisyUI"]]) {
      const h = document.createElement("h3"); h.textContent = title; themes.appendChild(h);
      const grid = document.createElement("div"); grid.className = "dlg-tiles";
      for (const t of view.themes.filter((x) => x.kind === kind)) {
        const b = document.createElement("button");
        b.type = "button"; b.className = "dlg-tile"; b.dataset.name = t.name;
        b.setAttribute("aria-pressed", String(t.name === current));
        if (kind === "daisy") b.dataset.theme = t.name;
        else { b.style.background = t.bg; b.style.color = t.fg; b.style.setProperty("--tile-accent", t.accent); }
        const name = document.createElement("span"); name.textContent = t.name; b.appendChild(name);
        const sw = document.createElement("span"); sw.className = "swatch"; b.appendChild(sw);
        b.onclick = () => { previewTheme = t.name; applyTheme(t.name); edits.set("theme", { value: t.name, clear: false }); renderThemes(); };
        grid.appendChild(b);
      }
      themes.appendChild(grid);
    }
    // daisyUI tiles resolve their colours from the vendored variables, which
    // are only linked when a daisyUI theme is active; make sure they exist.
    if (!document.getElementById("theme-daisy")) {
      const l = document.createElement("link"); l.id = "theme-daisy"; l.rel = "stylesheet"; l.href = "/static/vendor/daisyui-themes.css";
      document.head.insertBefore(l, document.head.firstChild);
    }
  }
  function render() {
    renderTabs(); renderScope();
    rows.hidden = pane !== "settings"; themes.hidden = pane !== "theme";
    if (pane === "settings") renderRows(); else renderThemes();
  }

  return runDialog(el, (finish) => {
    settingsOpen = {
      onSnapshot(s) {
        view = s;
        // Re-render only what is not being typed into: rows keep the
        // person's edits (they live in `edits`, re-applied by control()),
        // and hints/source labels are what a fresh snapshot changes.
        render();
      },
    };
    okBtn.textContent = "Save"; okBtn.disabled = false; okBtn.classList.remove("danger");
    const save = () => {
      for (const [key, e] of edits) {
        const r = row(key);
        if (!r || !r.writable.includes(scope)) continue;
        send({ t: "SetSetting", scope, key, ...(e.clear ? {} : { value: e.value }) });
      }
      // The theme is now what was previewed (or unchanged); the snapshot
      // that follows the write confirms it. Do not revert.
      settingsOpen = null;
      finish(true);
    };
    okBtn.onclick = save;
    // "Enter saves" (spec, *The dialog*): nothing in here destroys, so the
    // key that means "yes" everywhere else means it here too. Not from a
    // textarea, where Enter is the list separator the control is built
    // around, and not from a button, which the browser already activates on
    // Enter — routing those through Save would make Cancel save.
    const onKeydown = (e) => {
      if (e.key !== "Enter" || e.altKey || e.ctrlKey || e.metaKey) return;
      const t = e.target;
      if (t && (t.tagName === "TEXTAREA" || t.tagName === "BUTTON")) return;
      e.preventDefault();
      save();
    };
    el.addEventListener("keydown", onKeydown);
    cancelBtn.onclick = () => { settingsOpen = null; if (previewTheme) applyTheme(themeBefore); finish(false); };
    // Escape and the backdrop go through runDialog's own finish; hook the
    // revert onto the dialog's close so every exit restores the preview.
    el.addEventListener("close", function onClose() {
      el.removeEventListener("close", onClose);
      el.removeEventListener("keydown", onKeydown);
      if (settingsOpen) { settingsOpen = null; if (previewTheme) applyTheme(themeBefore); }
    });
    render();
    return () => tabs.querySelector(".dlg-tab").focus();
  }, false);
}
