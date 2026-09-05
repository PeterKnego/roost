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
