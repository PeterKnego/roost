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
      // Explicit rather than trusting the browser's own focus restoration:
      // roost's terminals are pooled DOM nodes moved between panes with
      // appendChild, and how showModal() interacts with a focused xterm is not
      // something to assume. tests/browser/dialogs.mjs asserts this.
      //
      // Revert-check (2026-09-05): commenting out the next line did NOT make
      // assertion E — or any assertion — fail; `deno run -A
      // tests/browser/dialogs.mjs` still printed 14/14 ok and PASS. A
      // standalone probe (bare `<dialog>`, no app code: focus a button,
      // showModal(), close(), read document.activeElement) showed why:
      // Chromium's own <dialog> already restores focus to the element that
      // was focused before showModal() was called, with no JS involved at
      // all. This line is redundant for a plain, still-attached element like
      // #closeproj, which is all dialogs.mjs's assertion E exercises — so
      // that assertion cannot discriminate for this line as written. The
      // call stays for the case the first comment above names — an xterm
      // node that was appendChild'd to a different pane while the dialog was
      // open — which is untested here and where the native mechanism's
      // behaviour was not verified.
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
    const ready = fill(finish);
    el.showModal();
    if (ready) ready();
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
