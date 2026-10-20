//! Worktrees outside roost's roots (#188): a plain `git worktree add`
//! somewhere no root reaches is announced by a banner that stays until it is
//! dismissed, **Allow…** turns it into a page at the virtual URL
//! `<parent>/.claude/outside/<name>` whose terminals start in the real
//! directory, and **Revoke** takes that page away again.
//!
//! No Rust test reaches `static/app.js`, and every visible half of this
//! feature lives there: the `#appeared` container that reconciles banners
//! against each State, the Allow… / Revoke confirm flows, the `OutsideAllowed`
//! banner, and the `#outsidenote` header note.
//!
//! Scenarios, in order (each needs the one before):
//!   P. `git worktree add <outside>/wt -b side` (and `wt2 -b side2`) with the
//!      main page open. wt's banner reads "appeared, outside roost's roots"
//!      with an Allow… button; it is still there after `location.reload()`
//!      and still there 10 s after it first showed (an old-style banner was
//!      gone at 8 s).
//!   Q. Allow… in wt's banner, confirmed in the in-page dialog. The sender
//!      gets the "Allowed — open" banner, the switcher row turns from a dim
//!      span with Allow… into a link to `/proj/.claude/outside/wt` with
//!      Revoke, and that page carries the header note and a terminal whose
//!      `pwd -P` is the real directory.
//!   R. Dismiss on wt2's banner, with a second page open on the same
//!      project: both pages lose it, and a reload does not bring it back.
//!   S. Revoke from the switcher, confirmed. The row is dim with Allow…
//!      again, and the virtual URL answers 404 "no such project".
//!   T. The overview (`/`, `static/overview.js`, which is not app.js):
//!      select proj, Allow… on wt's dim child row, confirm — the row becomes
//!      a reachable link with Revoke and `/api/projects` stops listing the
//!      virtual URL as a project of its own; Revoke, confirm — dim with
//!      Allow… again, and the click did not select the row it sits in. Each
//!      one-shot exchange is waited out (its socket closed) before "no error
//!      dialog" is read, so a late Error cannot hide behind a fast assert.
//!
//! Every absence is paired with a positive check that the thing was visible
//! before on the same page: the dim row and its Allow… are asserted present
//! before Q makes them a link (and S's dimming is read on the same selector);
//! wt2's banner is asserted on *both* pages before R's Dismiss; the virtual
//! URL is a working workspace in Q before S's 404. A reload is proved to have
//! happened by a window marker vanishing, so no wait is satisfied by the
//! stale page. The terminal wait matches `^MARK1$` as a whole line, which the
//! echoed command line (`… pwd -P; echo MARK1`) cannot satisfy.
//!
//! Revert-check (CLAUDE.md: "would this fail if I deleted the code it
//! covers?"), applied, watched failing, restored:
//!   Task 2's arm in `projects::resolve_project_with` (the
//!   `if name.contains("/.claude/outside/") { … }` block) removed, binary
//!   rebuilt, so the virtual URL falls through to the ordinary checks and
//!   resolves to nothing. Observed: P, R and S green, Q fails 3 —
//!     ok    wt's own appeared banner now links there instead of offering Allow…
//!       (timed out waiting for app.js)
//!     FAIL  /proj/.claude/outside/wt loads as a workspace
//!     FAIL  its header note names the real directory (null)
//!     FAIL  a terminal opens there (null)
//!   Everything before the page load stays green, which is what says those
//!   assertions test the allow (record, reply, switcher row) and not the
//!   resolve: the registry builds the virtual URL from the record on its own.
//!   S stays green too, and should: with no arm the URL 404s whether or not
//!   the record exists, so S's 404 is meaningful only beside Q's load of the
//!   same URL, and its dimmed row is what speaks for the revoke itself.
//!   Restored with `cp`, rebuilt: 41 ok, 0 FAIL.
//!
//!   A second check (task 8, #188), covering S, which the one above never
//!   exercises (it stays green under that revert): `outside::revoke_in`
//!   made a no-op (`Ok(())`, backup via `cp`, never `git checkout`), binary
//!   rebuilt. Observed: P, Q and R green, S fails 5 —
//!     ok    the confirm dialog opens, its button reads "Revoke" (Revoke)
//!     FAIL  after the revoke, the row is dim with Allow… again
//!     FAIL  and no longer a link with Revoke
//!     FAIL  GET /proj/.claude/outside/wt answers 200 (not 404) — the row
//!       stayed a link and the URL kept resolving as a workspace
//!     FAIL  a browser opening it gets the same answer
//!     FAIL  and no workspace
//!   The record the hub asked to delete was never deleted, so the row the
//!   switcher renders from it never dims and the virtual URL keeps
//!   resolving — exactly what S exists to catch. Restored with `cp`,
//!   rebuilt: 41 ok, 0 FAIL.
//!
//!   T's checks (#188 final review), each applied, watched failing,
//!   restored with `cp`:
//!   - `do_revoke_outside` replying `Ok(None)` again (no `OutsideRevoked`):
//!     T fails 1 — `FAIL and no error dialog (Not revoked roost did not
//!     answer within 10 s …)`. The overview now waits on its own answer, so
//!     a missing one is an error the user sees, not a success inferred from
//!     somebody else's `ProjectsChanged`. A first draft counted
//!     `close()` calls, which `finish` makes twice per socket, and passed
//!     under this same revert; it counts distinct sockets now.
//!   - `registry::project_rows`' outside retain removed (I1): T fails 1 —
//!     `FAIL and /api/projects lists proj but not the allowed URL … (["proj",
//!     "proj%2F.claude%2Foutside%2Fwt"])`. The overview's own list stayed
//!     green: `build_overview_projects` drops a top-level copy under an
//!     expanded parent, which is why the check reads the API.
//!   - overview.js's allow sending `Noop` instead of `AllowOutsideWorktree`:
//!     T fails 3 — the error dialog (`Not allowed unknown variant `Noop``),
//!     the row never becoming reachable, and the API check; the run then
//!     throws on the missing Revoke button.
//!   - the `return` after `revokeOutside` in overview.js's click handler
//!     removed: T fails 1 — `FAIL the click on Revoke did not select the
//!     row it sits in`.
//!
//! Run: deno run -A tests/browser/outside.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };
const enc = new TextEncoder();
const dec = new TextDecoder();
const t0 = Date.now();
const stamp = () => `${((Date.now() - t0) / 1000).toFixed(1)}s`;

const fx = await fixture();
const projDir = fx.dir;
const git = async (...args) => {
  const o = await new Deno.Command("git", { args: ["-C", projDir, ...args], stdout: "piped", stderr: "piped" }).output();
  return { ok: o.success, out: dec.decode(o.stdout), err: dec.decode(o.stderr) };
};
await git("config", "user.email", "t@t"); await git("config", "user.name", "t");
await git("add", "."); await git("commit", "-qm", "init");

// Beside the roots, never under them: `fx.base/roots` is the only root.
// Canonical, because the allow refuses a path that is not its own
// canonicalisation, and the terminal's `pwd -P` is compared against it.
const outsideDir = `${await Deno.realPath(fx.base)}/outside`;
await Deno.mkdir(outsideDir, { recursive: true });
const wtPath = `${outsideDir}/wt`;
const wt2Path = `${outsideDir}/wt2`;
const virtual = "proj/.claude/outside/wt";

// `--noprofile --norc`, so the prompt is bash's own `…$ ` and nothing from
// the developer's dotfiles prints into the terminal being read.
const shell = `${fx.base}/shell`;
await Deno.writeFile(shell, enc.encode(`#!/bin/sh\nexec /bin/bash --noprofile --norc "$@"\n`), { mode: 0o755 });

const browser = await startBrowser(profileDir(repoRoot));
let page, page2, pO, pR, pV, roost;
// Joins soft-wrapped rows, so a long path is one line.
const helpers = `window.__txt = (s) => { const e = terms.get(s); if (!e) return ""; const b = e.term.buffer.active; let o = "";
    for (let i = 0; i < b.length; i++) { o += b.getLine(i).translateToString(true); const n = b.getLine(i + 1); if (!n || !n.isWrapped) o += "\\n"; } return o; };
  window.__sessions = (pi) => state.panes[pi].tabs.filter((t) => t.k === "Terminal").map((t) => t.session);
  window.__box = (p) => [...document.querySelectorAll("#appeared .appeared-banner")].find((e) => e.dataset.path === p) || null;
  window.__row = (sel, attr, v) => { const b = [...document.querySelectorAll("#wtpanel " + sel)].find((e) => e.dataset[attr] === v); return b ? b.closest(".wtrow") : null; };`;
const ready = (pg) => until(() => pg.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
const boxText = (pg, p) => pg.evalIn(`__box(${JSON.stringify(p)})?.querySelector("b")?.textContent ?? null`);
/// Waits for `re` in session `s`'s screen and returns its match, or null.
const waitFor = async (pg, s, re, secs, label) => {
  let m = null;
  await until(async () => { m = (await pg.evalIn(`__txt(${JSON.stringify(s)})`)).match(re); return !!m; }, secs, label);
  return m;
};
/// Opens a terminal with the pane's own + and returns its name once attached.
const openTerm = async (pg) => {
  const before = JSON.parse(await pg.evalIn(`JSON.stringify(__sessions(3))`));
  await pg.evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newterm').click()`);
  await until(async () => JSON.parse(await pg.evalIn(`JSON.stringify(__sessions(3))`)).some((s) => !before.includes(s)), 20, "a new terminal");
  const s = JSON.parse(await pg.evalIn(`JSON.stringify(__sessions(3))`)).find((x) => !before.includes(x));
  if (!s) return null;
  await until(() => pg.evalIn(`terms.has(${JSON.stringify(s)})`), 30, "attached");
  return s;
};
/// Answers the in-page confirm dialog with its OK button, after checking it
/// is the dialog expected (its OK label).
const confirmDialog = async (pg, label) => {
  const open = await until(() => pg.evalIn(`document.getElementById("dlg-confirm").open`), 10, "the confirm dialog");
  const okText = open ? await pg.evalIn(`document.querySelector("#dlg-confirm .dlg-ok").textContent`) : null;
  ok(open && okText === label, `the confirm dialog opens, its button reads "${label}" (${okText})`);
  if (open) await pg.evalIn(`document.querySelector("#dlg-confirm .dlg-ok").click()`);
};
/// Opens the header switcher (a fresh fetch, `&state=1`) if it is closed.
const openSwitcher = async (pg) => {
  if (await pg.evalIn(`document.getElementById("wtpanel").hidden`)) await pg.evalIn(`document.getElementById("wtbtn").click()`);
  return until(() => pg.evalIn(`!document.getElementById("wtpanel").hidden`), 5, "the switcher panel");
};
/// The dim, unclickable state of wt's row: a `.wt.unreachable` span, no
/// link, and an Allow… carrying its path.
const rowDim = (pg) => pg.evalIn(`(() => { const r = __row(".wtallow", "path", ${JSON.stringify(wtPath)});
  return !!r && !!r.querySelector(".wt.unreachable") && !r.querySelector("a") && !r.querySelector(".wtrevoke"); })()`);
const linkHref = (pg) => pg.evalIn(`(() => { const r = __row(".wtrevoke", "name", "wt");
  return r && r.querySelector(".wtrevoke").dataset.parent === "proj" ? r.querySelector("a.wt")?.getAttribute("href") ?? null : null; })()`);

try {
  roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort(), extraEnv: { SHELL: shell } });
  const base = `http://127.0.0.1:${roost.port}`;
  page = await openPage(browser.port, `${base}/proj`);
  ok(await ready(page), "main page loaded");
  await page.evalIn(helpers);
  // A live shell on main: the registry knows a project once it has one, and
  // the switcher lists a worktree family only for a project it knows.
  ok(!!await openTerm(page), "a terminal on main");
  // Before any appearance: the page has no banner for either path. Read
  // again below once the banner arrives — the pair for this absence.
  ok(await page.evalIn(`!__box(${JSON.stringify(wtPath)}) && !__box(${JSON.stringify(wt2Path)})`),
     "no appeared banner before the worktrees exist");
  // The hub seeds its known-worktree set on its own thread when the page
  // connects; a worktree added before that lands is "already known" and is
  // never announced (spec §3, Not covered). The terminal above took longer,
  // and this is margin.
  await sleep(1500);

  // ------------------------------------------------------------------ P
  console.log(`P. a plain \`git worktree add\` outside the roots: a banner that stays (${stamp()})`);
  const add1 = await git("worktree", "add", "-q", wtPath, "-b", "side");
  ok(add1.ok, `git worktree add ${wtPath} -b side ${add1.ok ? "" : add1.err}`);
  const add2 = await git("worktree", "add", "-q", wt2Path, "-b", "side2");
  ok(add2.ok, `git worktree add ${wt2Path} -b side2 ${add2.ok ? "" : add2.err}`);
  const wantText = "worktree wt (side) appeared, outside roost's roots — ";
  const shown = await until(async () => (await boxText(page, wtPath)) === wantText, 20, "wt's appeared banner");
  const tShown = Date.now();
  ok(shown, `the main page shows "${wantText}" (${await boxText(page, wtPath)})`);
  ok(await page.evalIn(`__box(${JSON.stringify(wtPath)})?.querySelector('button[data-role="allow"]')?.textContent === "Allow…"`),
     "with an Allow… button");
  ok(await until(async () => (await boxText(page, wt2Path)) === "worktree wt2 (side2) appeared, outside roost's roots — ", 10, "wt2's banner"),
     "and wt2 has its own banner beside it");
  ok(await page.evalIn(`!!document.querySelector("#appeared") && !document.querySelector("#appeared").closest(".error-banner")
       && ![...document.querySelectorAll(".error-banner")].some((e) => e.textContent.includes("appeared"))`),
     "both live in #appeared, not in an .error-banner");

  // A reload: the marker vanishing is what says the page really is new.
  await page.evalIn(`window.__stale = 1; 0`);
  await page.evalIn(`location.reload(); 0`);
  const reloaded = await until(() => page.evalIn(`typeof window.__stale === "undefined"`), 15, "the reload");
  ok(reloaded && await ready(page), "the main page reloaded");
  await page.evalIn(helpers);
  ok(await until(async () => (await boxText(page, wtPath)) === wantText, 15, "wt's banner after reload"),
     "after the reload, wt's banner is back — from the server's list, not this page's memory");
  const wait = 10_000 - (Date.now() - tShown);
  if (wait > 0) await sleep(wait);
  ok(Date.now() - tShown >= 10_000 && (await boxText(page, wtPath)) === wantText,
     `and still there ${((Date.now() - tShown) / 1000).toFixed(1)} s after it first showed (an 8 s banner would be gone)`);

  // ------------------------------------------------------------------ Q
  console.log(`\nQ. Allow… → a page at the virtual URL, its terminal in the real directory (${stamp()})`);
  ok(await openSwitcher(page), "the switcher opens");
  ok(await until(() => rowDim(page), 10, "wt's dim row"),
     "before the allow, wt's switcher row is dim, not a link, with Allow…");
  ok((await linkHref(page)) === null, "and there is no link or Revoke for it yet");
  await page.evalIn(`__box(${JSON.stringify(wtPath)}).querySelector('button[data-role="allow"]').click(); 0`);
  await confirmDialog(page, "Allow");
  const allowedHref = await (async () => {
    let h = null;
    await until(async () => {
      h = await page.evalIn(`(() => { const e = [...document.querySelectorAll(".error-banner")].find((x) => x.querySelector("b")?.textContent.startsWith("Allowed — "));
        return e ? e.querySelector("a")?.getAttribute("href") ?? "" : null; })()`);
      return h !== null;
    }, 10, "the Allowed banner");
    return h;
  })();
  ok(allowedHref === `/${virtual}`, `the sender gets "Allowed — open", linking to /${virtual} (${allowedHref})`);
  // The panel is still open from before the click; nothing re-opens it, so
  // the row changing is the server's ProjectsChanged reaching the strip.
  ok(await until(async () => (await linkHref(page)) === `/${virtual}`, 15, "wt's row to become a link"),
     `the switcher row becomes a link to /${virtual} with Revoke (${await linkHref(page)})`);
  ok(!await rowDim(page), "and is no longer the dim row with Allow…");
  ok(await until(() => page.evalIn(`__box(${JSON.stringify(wtPath)})?.querySelector("a")?.getAttribute("href") === ${JSON.stringify(`/${virtual}`)}`), 10, "the banner's link"),
     "wt's own appeared banner now links there instead of offering Allow…");

  pO = await openPage(browser.port, `${base}/${virtual}`);
  const oReady = await ready(pO);
  ok(oReady && await pO.evalIn(`PROJECT === ${JSON.stringify(virtual)}`), `/${virtual} loads as a workspace`);
  const note = oReady ? await pO.evalIn(`document.getElementById("outsidenote")?.textContent ?? null`) : null;
  ok(note === `outside roost's roots: ${wtPath}`, `its header note names the real directory (${note})`);
  let sO = null;
  if (oReady) {
    await pO.evalIn(helpers);
    sO = await openTerm(pO);
  }
  ok(!!sO, `a terminal opens there (${sO})`);
  if (sO) {
    // Typeahead before the prompt is discarded (README's traps).
    ok(!!await waitFor(pO, sO, /\$ $/m, 20, "the prompt"), "the shell prompts");
    await pO.evalIn(`terms.get(${JSON.stringify(sO)}).term.input("pwd -P; echo MARK1\\r")`);
    const m = await waitFor(pO, sO, /^([^\n]*)\nMARK1$/m, 15, "MARK1");
    ok(m?.[1] === wtPath, `its pwd -P is the real directory (${m?.[1]} vs ${wtPath})`);
  }

  // ------------------------------------------------------------------ R
  console.log(`\nR. Dismiss, with two pages on the project (${stamp()})`);
  await page.bringToFront();
  page2 = await openPage(browser.port, `${base}/proj`);
  ok(await ready(page2), "a second page on proj");
  await page2.evalIn(helpers);
  const w2 = "worktree wt2 (side2) appeared, outside roost's roots — ";
  ok(await until(async () => (await boxText(page2, wt2Path)) === w2, 10, "wt2 on page 2"), "page 2 shows wt2's banner");
  await page.bringToFront();
  ok((await boxText(page, wt2Path)) === w2, "and so does page 1");
  await page.evalIn(`__box(${JSON.stringify(wt2Path)}).querySelector('button[data-role="dismiss"]').click(); 0`);
  ok(await until(async () => (await boxText(page, wt2Path)) === null, 5, "page 1 to drop it"), "Dismiss removes it on page 1");
  ok(await until(async () => (await boxText(page2, wt2Path)) === null, 10, "page 2 to drop it"),
     "and page 2 loses it too — the server's broadcast, not page 1's DOM");
  await page.evalIn(`window.__stale = 1; 0`);
  await page.evalIn(`location.reload(); 0`);
  ok(await until(() => page.evalIn(`typeof window.__stale === "undefined"`), 15, "the reload") && await ready(page),
     "page 1 reloaded");
  await page.evalIn(helpers);
  // The positive half: a State carrying the list has arrived on the new
  // page (the field is always there), so the absence is read off it.
  ok(await until(() => page.evalIn(`Array.isArray(state.appeared)`), 10, "a State with the list"),
     "the reloaded page has the server's appeared list");
  ok(!await page.evalIn(`state.appeared.some((e) => e.path === ${JSON.stringify(wt2Path)})`) && (await boxText(page, wt2Path)) === null,
     "and wt2 is not on it — the dismissal was recorded, not just hidden");

  // ------------------------------------------------------------------ S
  console.log(`\nS. Revoke: the row dims, the URL stops resolving (${stamp()})`);
  ok(await openSwitcher(page), "the switcher opens");
  ok(await until(async () => (await linkHref(page)) === `/${virtual}`, 10, "the link row"),
     "before the revoke, wt's row is the link with Revoke");
  await page.evalIn(`__row(".wtrevoke", "name", "wt").querySelector(".wtrevoke").click(); 0`);
  await confirmDialog(page, "Revoke");
  ok(await until(() => rowDim(page), 15, "wt's row to dim"), "after the revoke, the row is dim with Allow… again");
  ok((await linkHref(page)) === null, "and no longer a link with Revoke");
  const r = await fetch(`${base}/${virtual}`);
  const body = await r.text();
  ok(r.status === 404 && body === "no such project", `GET /${virtual} answers ${r.status} "${body}" — where Q loaded a workspace`);
  pR = await openPage(browser.port, `${base}/${virtual}`);
  ok(await until(() => pR.evalIn(`document.readyState === "complete" && document.body.innerText.trim() === "no such project"`), 15, "the 404 page"),
     "a browser opening it gets the same answer");
  ok(await pR.evalIn(`typeof state === "undefined" && !document.getElementById("wtbtn")`), "and no workspace");

  // ------------------------------------------------------------------ T
  console.log(`\nT. the overview's Allow… and Revoke (${stamp()})`);
  pV = await openPage(browser.port, `${base}/`);
  const vKey = "proj%2F.claude%2Foutside%2Fwt";
  ok(await until(() => pV.evalIn(`!!document.querySelector('#ovprojects .ovrow[data-key="proj"]')`), 20, "the overview's proj row"),
     "the overview lists proj");
  // Every one-shot exchange ends in `finish`, which closes its socket —
  // on the answer, or on the 10 s timeout with an Error. Counting closed
  // sockets is what lets "no error dialog" be read after the reply, not
  // before it. Distinct sockets, not close() calls: `finish` closes again
  // from the socket's own onclose, so one exchange makes two calls, and a
  // call count let the revoke's wait be satisfied by the allow's socket —
  // the first revert-check of this section passed that way.
  await pV.evalIn(`window.__socks = new Set(); window.__closed = 0; { const c = WebSocket.prototype.close;
    WebSocket.prototype.close = function (...a) { __socks.add(this); window.__closed = __socks.size; return c.apply(this, a); }; } 0`);
  const ovRow = (pg) => pg.evalIn(`JSON.stringify((() => {
    const r = [...document.querySelectorAll('#ovprojects .ovrow.child[data-parent="proj"]')].find((e) =>
      e.querySelector('.wtallow')?.dataset.path === ${JSON.stringify(wtPath)} || e.querySelector('.wtrevoke')?.dataset.name === "wt");
    if (!r) return null;
    const a = r.querySelector("a.ovname");
    return { dim: r.classList.contains("unreachable"), allow: !!r.querySelector(".wtallow"), revoke: !!r.querySelector(".wtrevoke"),
             href: a ? decodeURIComponent(a.getAttribute("href")) : null };
  })())`).then(JSON.parse);
  const strayTop = (pg) => pg.evalIn(`!!document.querySelector('#ovprojects .ovrow:not(.child)[data-key="${vKey}"]')`);
  const errOpen = (pg) => pg.evalIn(`document.getElementById("dlg-choice").open ? document.getElementById("dlg-choice").textContent.trim() : ""`);
  // Revoked in S but its terminal from Q still running: the one top-level
  // row an outside URL may have (registry::project_rows), unreachable. The
  // positive half of the "not top-level once allowed" check below.
  ok(await until(() => strayTop(pV), 10, "the revoked-but-live row"),
     "before the allow, the revoked URL's live session is its own top-level row");
  // The same list over /api/projects, which has no expanded parent to fold
  // a duplicate into (build_overview_projects drops a top-level copy only
  // under a parent that is open, so the overview alone cannot show I1 once
  // proj is selected).
  const apiKeys = async () => (await (await fetch(`${base}/api/projects`)).json()).map((p) => p.key);
  let keys = await apiKeys();
  ok(keys.includes("proj") && keys.includes(vKey), `and /api/projects lists it beside proj (${JSON.stringify(keys)})`);
  await pV.evalIn(`document.querySelector('#ovprojects .ovrow[data-key="proj"]').click(); 0`);
  let rv = null;
  await until(async () => { rv = await ovRow(pV); return !!rv; }, 15, "wt's overview row");
  ok(rv?.dim && rv.allow && !rv.revoke && rv.href === null, `selecting proj lists wt dim, unlinked, with Allow… (${JSON.stringify(rv)})`);
  await pV.evalIn(`[...document.querySelectorAll('#ovprojects .wtallow')].find((b) => b.dataset.path === ${JSON.stringify(wtPath)}).click(); 0`);
  await confirmDialog(pV, "Allow");
  ok(await until(() => pV.evalIn(`window.__closed >= 1`), 15, "the allow's reply"), "the allow's one-shot socket got its answer");
  ok((await errOpen(pV)) === "", `and no error dialog (${await errOpen(pV)})`);
  await until(async () => { rv = await ovRow(pV); return !!rv && !rv.dim; }, 15, "wt's row to become reachable");
  ok(rv && !rv.dim && rv.revoke && !rv.allow && rv.href === `/${virtual}`,
     `after the allow, wt's overview row is reachable, links to /${virtual}, with Revoke (${JSON.stringify(rv)})`);
  keys = await apiKeys();
  ok(keys.includes("proj") && !keys.includes(vKey),
     `and /api/projects lists proj but not the allowed URL, a child and not a project (#188 I1) (${JSON.stringify(keys)})`);

  await pV.evalIn(`[...document.querySelectorAll('#ovprojects .wtrevoke')].find((b) => b.dataset.name === "wt").click(); 0`);
  await confirmDialog(pV, "Revoke");
  ok(await until(() => pV.evalIn(`window.__closed >= 2`), 15, "the revoke's reply"), "the revoke's one-shot socket got its answer");
  ok((await errOpen(pV)) === "", `and no error dialog (${await errOpen(pV)})`);
  ok(await pV.evalIn(`location.pathname === "/" && new URLSearchParams(location.search).get("sel") === "proj"`),
     "the click on Revoke did not select the row it sits in");
  await until(async () => { rv = await ovRow(pV); return !!rv && rv.dim; }, 15, "wt's row to dim");
  ok(rv?.dim && rv.allow && !rv.revoke && rv.href === null, `after the revoke, wt's overview row is dim with Allow… again (${JSON.stringify(rv)})`);
} finally {
  for (const p of [pV, pR, pO, page2, page]) { try { p?.close(); } catch {} }
  browser.close();
  await roost?.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
