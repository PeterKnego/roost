//! Are terminal links marked only when the user asks for them?
//!
//! No Rust test reaches static/app.js, so the matchers, the modifier gate and
//! everything a click does live entirely outside `cargo test`.
//!
//! The trap this file is written against: asserting that a mouse event was
//! accepted rather than that a link exists. xterm returns link ranges from a
//! provider; "the provider ran" is true with the gate deleted. So this drives
//! the registered providers themselves and asserts on the ranges they hand
//! back — and it asserts the seeded row was actually found first, because
//! "zero links" is also what an off-screen row produces.
//!
//! The gate is armed with a real CDP key event, never by assigning
//! `linksArmed`: the keydown listener is half of what this task delivers, and
//! setting the flag by hand would leave it untested. (Verified empirically on
//! this host: `e.ctrlKey` IS true on the Control keydown itself when CDP is
//! given `modifiers: 2`, so `linkModifier(e)` arms on the very first event.)
//!
//! Row lookup is bottom-up and matches the row's *whole* trimmed text, not a
//! substring. Seeding by running a command puts the needle on screen twice —
//! once in the echoed command line, once in its output — and a first-match
//! substring search finds the command line, so the assertions would be made
//! against a row that also contains `printf`, quotes and other paths.
//!
//! Revert-the-fix, each one applied, run, watched fail, then restored. Counts
//! re-measured after sections I and J (fix round 1's own additions) were
//! added, so they are what this file does today, not what an earlier draft
//! of it did:
//!   0. Deleted the `registerTermLinks(term, entry)` call in ensureTerm —
//!      the state of the tree before this task. Fourteen failed (eleven
//!      before section L existed, ten before I and J, eight before that —
//!      see below), among them:
//!        FAIL  two link providers are registered on the terminal (got 0)
//!        FAIL  a path is offered as a link while the modifier is held (got 0: [])
//!        FAIL  arming alone marked the path under the resting pointer (got null)
//!        FAIL  the link is still marked with the application holding the mouse
//!        FAIL  modifier+click on a real path opened docs/backlog.md
//!        FAIL  the refusal flashed on the terminal that was actually clicked
//!        FAIL  modifier+click opened the file even with mouse reporting on
//!        FAIL  CONTROL: the very same click, with the modifier on the
//!        event, does open it
//!      Both "no link offered" assertions (1 and 4) went on passing, since
//!      no providers and a closed gate look identical from outside. That is
//!      exactly why the registration guard and the row-on-screen guards are
//!      here: without them this whole file would be green against a tree
//!      with the feature deleted. Section H's own "flashed the refusal"
//!      assertion also went on passing here — with no providers at all,
//!      openTermPath is never called and pendingLink stays null, so
//!      flashText() staying "" satisfies nothing rather than exposing this.
//!      That is what the tab-count and modifier+click assertions are for.
//!      Section L behaves the same way and is guarded the same way: with no
//!      providers, "a plain click stayed with the application" is true
//!      because nothing could ever have been offered, and "the modifier
//!      click is reported to the application" is true because that report is
//!      xterm's own and owes resh nothing. Its two modifier-click
//!      assertions are what fail, and the mouse-report CONTROL beside them
//!      goes on passing legitimately — the click really did land on the row.
//!      Section I's second terminal has no providers either, so its click
//!      never activates and its own refusal-flash assertion fails the same
//!      way — but "and not on the other terminal" still passes, vacuously:
//!      neither terminal ever flashes anything. Section J is unaffected: it
//!      calls openTermPath directly, never through a provider.
//!   1. Deleted `if (!linksArmed) return cb(undefined);` from matchProvider.
//!      Three failed — the direct one plus both of section F's disarmed
//!      states, which is the same property seen through xterm:
//!        FAIL  no link is offered with the modifier up (got 1: ["docs/backlog.md"])
//!        FAIL  resting on the path marks nothing while disarmed
//!        FAIL  and releasing unmarked it, again with no mouse movement
//!      Sections H, I and J are unaffected: every click there already holds
//!      the modifier, so a gate that fails open changes nothing it clicks.
//!   2. Swapped the two entries of the `providers` array in
//!      registerTermLinks, so the path provider registers first. Two failed,
//!      one from each precedence assertion:
//!        FAIL  https://example.com/a/b is one whole-URL link, not a path
//!        link over its tail (got ["/example.com/a/b"])
//!        FAIL  xterm's own precedence marks the whole URL, not the path in
//!        its tail (got "/example.com/a/b")
//!      The first version of that second assertion did NOT fail here — it
//!      hovered column 4, inside `https:`, which only the URL matcher
//!      reaches, so xterm marked the URL either way. Hence URL_COL and the
//!      both-matchers-claim-this-cell guard beside it.
//!   3. Changed PATH_RE's `(?:[\w.@+-]+\/)+` to `(?:[\w.@+-]+\/)*`, allowing
//!      zero directory segments. One failed:
//!        FAIL  a bare filename with no directory offers no link (got 1: ["backlog.md"])
//!   4. Put nudgeLinks' synthetic mousemove back on the `.termhost` element
//!      instead of `.xterm-screen`. Five failed, unchanged by I and J, all
//!      of them hover-path:
//!        FAIL  arming alone marked the path under the resting pointer (got null)
//!        FAIL  xterm's own precedence marks the whole URL, not the path in
//!        its tail (got null)
//!        FAIL  the link is still marked with the application holding the mouse
//!        FAIL  modifier+click on a real path opened docs/backlog.md
//!        FAIL  modifier+click opened the file even with mouse reporting on
//!      That fourth one — new when section H was added — was not obvious in
//!      advance: clickLink's own mouseMoved is a real CDP event dispatched
//!      straight at xterm's screen element, so it looked independent of
//!      nudgeLinks entirely. Measured instead of assumed, with logging added
//!      and then removed: xterm's Linkifier caches its answer per line
//!      (`_askForLink`'s useLinkCache branch — see the comment above `away`
//!      below), and PATH's row was last queried, stale and unarmed, back in
//!      section G. A move that lands on the *same line* — which clickLink's
//!      does, since it goes straight to the target — reuses that stale
//!      cached answer instead of re-asking the providers; the currentLink
//!      stayed null after a real, correctly-armed mouseMoved, confirmed with
//!      `__t().term._core.linkifier.currentLink`. nudgeLinks' own
//!      off-line-then-back detour is what invalidates that cache before a
//!      click's own move ever runs, and section H's click depends on it
//!      exactly as much as F's pure-hover assertions do — this revert is
//!      what proved that, not the design intent going in. Section I stays at
//!      four rather than growing a sixth: ALSO_MISSING's row on terminal 2
//!      was never queried unarmed the way PATH's was, so there is no stale
//!      cache entry for clickLink's move to inherit there. Section L splits
//!      along the same line: its PATH2 click inherits section G's stale
//!      cache and fails, while its PATH3 hover further down lands on a row
//!      nothing ever queried unarmed and goes on passing.
//!   5. Put nudgeLinks' detour back to a sideways one — a different column on
//!      the same line, off the real `cols` — instead of a different row. The
//!      same five failed, identically, for the same reason as 4: a same-line
//!      detour does not change `_activeLine` either, so the cache is never
//!      invalidated and clickLink's own move inherits the stale answer.
//!      4 and 5 are both bugs this task shipped and then measured out; see
//!      task-4-report.md. Sections B-E stay green through both, because they
//!      ask the providers directly and never go near the hover path — which
//!      is exactly why F is here.
//!   6. Made nudgeLinks' events `bubbles: true` again. Section G alone
//!      failed — still just the one after section L, which was re-measured
//!      rather than assumed, since L is the one place after G that turns
//!      mouse reporting back on. Section H disables it before H, I, J and K
//!      run, so a bubbling nudge has no listener downstream to leak to
//!      there; and L's own two PTY assertions look for a *button* report
//!      (SGR button 0, and 0|16 for the modifier), which a phantom motion
//!      report (button 35) neither satisfies nor spoils. The phantom motion
//!      reports spelled out:
//!        FAIL  arming and disarming sent nothing to the PTY
//!        ("[<35;5;1M[<35;5;4M[<35;5;1M[<35;5;4M";
//!        after arming alone: "[<35;5;1M[<35;5;4M")
//!      Four reports per chord, two at the detour row (;1) and two back at
//!      the resting row (;5) — exactly the jump a 1003-mode TUI would show.
//!      (Row 5, not 4: this run's shell prompt landed one line later than
//!      whichever run first recorded this string — an artifact of exactly
//!      how many lines preceded it, not of anything this revert changed.)
//! All restored afterwards; the run passes clean again (see task-5-report.md
//! for the exact terminal output).
//!
//! Section H (task 5: what a click does) has its own two reverts, each
//! applied, run, watched fail, then restored. Both counts grew once sections
//! I and J existed, since openTermPath and the PathRefused handler are the
//! same code every one of those sections exercises:
//!   1. Changed the `PathRefused` case in onEvent to call `showError(ev.msg)`
//!      instead of `termFlash(pendingLink.entry, ev.msg)`. Three failed (one
//!      before I and J existed):
//!        FAIL  and flashed the refusal in the terminal that was clicked
//!        FAIL  the refusal flashed on the terminal that was actually clicked
//!        FAIL  a mismatched refusal in between did not swallow the next
//!        click's own reply
//!      showError still ran — the refusal itself was not lost — but it went
//!      to the workspace banner, not the terminal, which is exactly the
//!      wrong-shape failure this design exists to avoid: a refusal with no
//!      way back to which terminal, or which click, produced it.
//!   2. Had openTermPath send `{ t: "OpenTab", pane: 2, tab: { k: "File",
//!      rel: raw, mode: "Preview" } }` directly instead of `{ t: "OpenPath",
//!      text: raw }`. Four failed (two before I and J existed):
//!        FAIL  a path that does not resolve added no tab
//!        FAIL  and flashed the refusal in the terminal that was clicked
//!        FAIL  the refusal flashed on the terminal that was actually clicked
//!        FAIL  a mismatched refusal in between did not swallow the next
//!        click's own reply
//!      The first is the whole reason `do_open_path` resolves before
//!      touching the layout (see hub.rs): a raw, optimistic OpenTab for
//!      nope/missing.rs landed in pane 2 as a dead tab, in every connected
//!      browser's window, for a path that was never real.
//!
//! One regex worth flagging for the next reader: the refusal text produced
//! by resolve_terminal_path for a missing file is "not found: No such file
//! or directory (os error 2)" (capital N, confirmed by printing it), so the
//! assertion below matches case-insensitively (/i) — a case-sensitive
//! /cannot read|no such file/ never matches that string and would fail
//! against a correct implementation, not just a broken one. It also does not
//! include the path itself: two different refused paths refuse with the
//! *identical* string, which is exactly why section I settles terminal 1's
//! flash to a known "" baseline before taking one, rather than comparing
//! against whatever it happened to be showing.
//!
//! Fix round 1 added sections I and J and fixed pendingLink being cleared
//! unconditionally on a refusal, whether or not it matched the click still
//! in flight — a defect in the original brief's own code, not introduced by
//! task 5. Two reverts, each applied, run, watched fail, then restored:
//!   1. Restored the unconditional clear (moved `pendingLink = null;` back
//!      outside the `if (pendingLink && pendingLink.text === ev.text)`
//!      block, so it runs on every PathRefused, matched or not). Section J
//!      exists for exactly this case — a mismatched refusal arriving while a
//!      different click is still in flight — and it failed:
//!        (timed out waiting for the real click's own refusal flash)
//!        FAIL  a mismatched refusal in between did not swallow the next
//!        click's own reply
//!      The mismatched refusal cleared pendingLink before the real click's
//!      own reply ever arrived, so that reply found an empty slot and
//!      dropped silently to console.warn — the exact silent failure the fix
//!      exists to prevent, and precisely the common case PATH_RE creates: it
//!      marks ordinary prose, so arming links over a paragraph and clicking
//!      two different, both-refusing spans before the first reply lands is
//!      not an edge case.
//!   2. Made openTermPath capture the wrong entry — `pendingLink = { entry:
//!      [...terms.values()][0], text: raw }`, always terminal 1, regardless
//!      of which terminal's provider actually called it. Two of section I's
//!      three assertions failed, plus one collateral failure in J (the same
//!      wrong-entry bug corrupts everything downstream of it, not a second,
//!      independent defect):
//!        (timed out waiting for a refusal flash on either terminal)
//!        FAIL  the refusal flashed on the terminal that was actually clicked
//!        FAIL  and not on the other terminal, which was never clicked
//!        FAIL  terminal 1 starts this section with no flash pending
//!      Clicking the refused path in terminal 2 flashed terminal 1 instead.
//!      The first version of section I's "not on the other terminal" check
//!      compared against a `before1` snapshot taken right after section H's
//!      own click, without settling it first — since resolve_terminal_path's
//!      refusal text carries no path (see above), section H's leftover
//!      flash and a genuinely misrouted one are textually identical, and a
//!      20-second wait on terminal 2 alone let a misrouted flash on terminal
//!      1 fade (termFlash: 1600ms) before that check ever looked — so this
//!      revert's second assertion passed even with the bug live, measured
//!      rather than assumed. Fixed by settling terminal 1 to a known ""
//!      baseline first, and by polling *both* terminals together on a short
//!      timeout so a misrouted flash is caught before it can decay.
//! Both restored afterwards; the run passes clean again (see
//! task-5-report.md's fix-round-1 addendum for the exact terminal output).
//!
//! Section K (task 6: OSC 8) has its own two reverts. Neither shifted any
//! count above: revert 0 was re-measured with K present and is still eleven,
//! and K stays green under it, because an OSC 8 link comes from xterm's own
//! OscLinkProvider, which is registered at construction and owes nothing to
//! registerTermLinks. Both of K's own reverts, though, first passed:
//!   1. Set `linkHandler` back to `null`. The count assertion the task was
//!      written around — "an OSC 8 link is offered with no modifier held" —
//!      went on passing, because OscLinkProvider returns its ranges either
//!      way; the option only decides who is told about an activation. Worse,
//!      the run then *hung* rather than failing: xterm's fallback activate
//!      calls `confirm()`, and a native dialog with no CDP dialog handler
//!      wedges the renderer, so Input.dispatchMouseEvent never returned.
//!      Both are fixed here — confirm() is stubbed in the initial block, and
//!      the assertions that carry this revert are the click's, not the
//!      count's. The harness now also rejects any CDP command that goes
//!      unanswered for 30s, so the *next* file to find a way to wedge the
//!      renderer fails legibly instead of hanging; re-running this revert
//!      with the confirm() stub removed proves it, ending in 43s with
//!        error: Uncaught (in promise) Error: CDP command
//!        Input.dispatchMouseEvent (id 128) got no reply in 30s
//!      rather than the >10-minute hang that had to be killed by hand.
//!      With the stub in place, three failed:
//!        FAIL  CONTROL: clicking a plain https OSC 8 link actually reached
//!        window.open
//!        FAIL  window.open was asked for the URL the application declared
//!        (got [])
//!        FAIL  resh's own handler took the activation, not xterm's
//!        confirm() fallback
//!   2. Deleted `if (!SAFE_URL.test(u)) return;` from openUrl. The obvious
//!      assertion — "a javascript: OSC 8 destination opened nothing" — also
//!      went on passing, and would have shipped as this project's third
//!      vacuous javascript: check (mdlinks.mjs note 6 was the first). The
//!      reason is two layers away from resh: the vendored OscLinkProvider
//!      itself runs `new URL(uri)` and refuses to *offer* any link whose
//!      protocol is not http(s), so no OSC 8 payload can carry a hostile
//!      scheme as far as openUrl at all. That assertion is kept, and paired
//!      with one naming who actually refuses it; the allowlist gets its own
//!      reachable test by driving openUrl directly. Two failed:
//!        FAIL  openUrl refuses a javascript: destination outright
//!        FAIL  and any other scheme off the allowlist, not just javascript:
//! Both restored afterwards; the run passes clean again.
//!
//! Section L (task 7) is the one the design was written around and could
//! not answer on paper: does a link survive an application that has taken
//! the mouse? Measured, and the answer is yes — a modifier+click opens the
//! file with mode 1003 live, so nothing needed implementing and the brief's
//! capture-phase fallback stayed unwritten. Two independent reasons, both
//! read out of static/vendor/xterm.js after the measurement, not before:
//! the Linkifier binds its mousedown/mouseup to `screenElement`
//! (`.xterm-screen`), while the core's mouse-reporting handler binds to
//! `element` (`.xterm`) — an ancestor, so the Linkifier fires first no
//! matter what the core then does; and the core's `cancel(e)` is
//! `if (this.options.cancelEvents || t) ...`, so with the default
//! `cancelEvents: !1` and no second argument it never calls stopPropagation
//! at all. Its unconditional `e.preventDefault()` does not stop listeners.
//!
//! What the same measurement also showed, and what no amount of reading
//! would have: the application gets the click *too*. A modifier+click emits
//! `CSI < 16 ; col ; row M` (left button | the control bit) and its release,
//! alongside opening the file. Whether an application does anything visible
//! with a ctrl+click is the by-hand half, left open deliberately — see
//! task-7-report.md's checklist. If it ever proves to be a problem, the
//! brief's capture-phase fallback is also the remedy: stopping the event
//! ahead of both handlers suppresses the report as well.
//!
//! Section L's own two reverts, each applied, run, watched fail, restored.
//! They exist because the gate has two halves and the first version of this
//! section could only reach one of them:
//!   L1a. Deleted the modifier re-check inside matchProvider's activate
//!        callback (`activate: (ev) => { if (linkModifier(ev)) ... }`),
//!        leaving the provideLinks gate intact. Against the first draft of
//!        section L this changed NOTHING — the whole file stayed green,
//!        because with the gate intact there is never a link under an
//!        unmodified click to activate, so the re-check is unreachable
//!        through any ordinary click. The missed-keyup assertions at the end
//!        of section L were added for exactly that, and now one fails:
//!          FAIL  a click with no modifier on a still-marked link opens nothing
//!   L1b. Deleted both halves — the provideLinks gate and the activate
//!        re-check — which is the modifier gate gone entirely, and the state
//!        in which resh would steal a plain click from a running
//!        application. Five failed:
//!          FAIL  no link is offered with the modifier up (got 1: ["docs/backlog.md"])
//!          FAIL  resting on the path marks nothing while disarmed
//!          FAIL  and releasing unmarked it, again with no mouse movement
//!          FAIL  a plain click on a path stayed with the application and opened nothing
//!          FAIL  a click with no modifier on a still-marked link opens nothing
//!        Note which one is missing from revert 1's own list above: deleting
//!        the provideLinks gate alone does NOT fail assertion 10, because
//!        the activate re-check still refuses the click. Two independent
//!        guards, and the file now names them separately rather than
//!        crediting one for the other's work.
//! Both restored afterwards; the run passes clean again.
//!
//! Run: deno run -A tests/browser/termlinks.mjs
import { fixture, freePort, openPage, profileDir, sleep, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };
// Section H's assertions are about a websocket round trip (OpenPath ->
// PathRefused, or the State broadcast an accepted OpenTab produces), so the
// condition itself is async, unlike the direct provider calls sections B-G
// assert on with plain ok().
const assert = async (m, fn) => ok(await fn(), m);

const PATH = "docs/backlog.md";
const URL_ = "https://example.com/a/b";
const BARE = "backlog.md";
// Never created on disk — the fixture only ever seeds docs/backlog.md — so
// clicking this exercises the refusal path, which task 4's review
// established is the *common* case: PATH_RE matches any slash, and most of
// what it matches in ordinary prose (and/or, w/o, 24/7...) resolves to
// nothing on disk.
const MISSING = "nope/missing.rs";
// A second file that really exists and that nothing above ever opens. Section
// L needs one: it clicks the same path twice, plain then with the modifier,
// and PATH is already an open tab by then — so "the modifier click opened it"
// would be true before section L ran a single event.
const PATH2 = "docs/notes.md";
// A third one, for the missed-keyup case: a link that is still marked when a
// click arrives without the modifier on it. See section L's own comment.
const PATH3 = "docs/todo.md";

// The rels currently open as File tabs in one pane, read straight off the
// State snapshot rather than the DOM — the tab strip renders asynchronously
// off the same data, so reading state is what actually distinguishes "opened"
// from "about to be opened".
async function openTabRels(page, pane) {
  return JSON.parse(await page.evalIn(
    `JSON.stringify((state.panes[${pane}] ? state.panes[${pane}].tabs : [])
       .filter((t) => t.k === "File").map((t) => t.rel))`));
}

// What one terminal is currently flashing, "" if nothing. `session` targets
// a specific terminal (section I, where two are on screen at once and
// __t() — first-inserted-into-the-Map — cannot tell them apart); omitted, it
// reads __t()'s own node, matching every section before I where only one
// terminal exists. This is the same entry openTermPath flashes into, so a
// refusal that landed on a *different* terminal's element (a wrong `entry`
// captured in pendingLink, say) shows up here as "" or stale text, not as a
// false pass — proved, not assumed, by section I's revert below.
async function flashText(page, session) {
  const termExpr = session ? `terms.get(${JSON.stringify(session)})` : "__t()";
  return await page.evalIn(`(() => { const e = ${termExpr}; return e ? (e.node.dataset.flash || "") : ""; })()`);
}

// Clicks the way a user actually does it: hover with the modifier held so
// xterm's linkifier captures currentLink, then a modifier-held
// mousedown/mouseup pair at the same point — that pairing is what xterm's
// own _handleMouseUp gates activation on (static/vendor/xterm.js), not a
// direct call into the provider's activate callback. Driving it through real
// CDP mouse events, not __resolve, is what actually exercises openTermPath
// and the PathRefused round trip end to end. `session` targets a specific
// terminal, as with flashText above; omitted, it defaults to __t().
async function clickLink(page, needle, { modifier = false, session } = {}) {
  const { evalIn, cmd } = page;
  const termExpr = session ? `terms.get(${JSON.stringify(session)})` : "__t()";
  const seat = await evalIn(`(() => {
    const entry = ${termExpr};
    if (!entry) return null;
    // Scoped to this terminal's own subtree: with two terminals mounted at
    // once (section I), an unscoped search could resolve to the wrong one
    // if their printed text ever collided.
    const rows = [...document.querySelectorAll(".xterm-rows div")]
      .filter((x) => entry.node.contains(x));
    const n = rows.filter((x) => x.textContent.trim() === ${JSON.stringify(needle)}).pop();
    if (!n) return null;
    const b = n.getBoundingClientRect();
    const scr = entry.node.querySelector(".xterm-screen").getBoundingClientRect();
    const cell = scr.width / entry.term.cols;
    return { x: Math.round(scr.left + 4.5 * cell), y: Math.round(b.top + b.height / 2) };
  })()`);
  if (!seat) return false;
  const mods = modifier ? 2 : 0;
  const ctrlKey = (type, m) => cmd("Input.dispatchKeyEvent", {
    type, key: "Control", code: "ControlLeft",
    windowsVirtualKeyCode: 17, nativeVirtualKeyCode: 17, modifiers: m,
  });
  if (modifier) { await ctrlKey("rawKeyDown", 2); await sleep(150); }
  await cmd("Input.dispatchMouseEvent", { type: "mouseMoved", x: seat.x, y: seat.y, buttons: 0 });
  await sleep(200);
  await cmd("Input.dispatchMouseEvent",
    { type: "mousePressed", x: seat.x, y: seat.y, button: "left", buttons: 1, clickCount: 1, modifiers: mods });
  await cmd("Input.dispatchMouseEvent",
    { type: "mouseReleased", x: seat.x, y: seat.y, button: "left", buttons: 0, clickCount: 1, modifiers: mods });
  await sleep(200);
  if (modifier) { await ctrlKey("keyUp", 0); await sleep(150); }
  return true;
}

// Types a command as pty input — the same route __t().term.input(...) calls
// elsewhere in this file use to seed a prompt — and presses Enter. Pulled
// into its own helper only for section K, whose OSC 8 payloads are full of
// literal backslash-escapes (\e, \\) that bash's own printf must see intact;
// JSON.stringify is what carries those through this template without a
// second round of JS string-escaping mangling them, which plain command
// text elsewhere in this file never has to worry about.
async function typeInTerm(page, cmd) {
  await page.evalIn(`__t().term.input(${JSON.stringify(cmd + "\r")})`);
}

// How many links xterm's own OscLinkProvider (plus resh's two) offer over
// the row holding `needle` — see the __oscLinksAt comment above for why this
// has to reach past entry.linkProviders to ask. -1 means it could not look
// at all; callers must not read that as zero.
async function linksAt(page, needle) {
  return await page.evalIn(`__oscLinksAt(${JSON.stringify(needle)})`);
}

// What window.open has been asked for since the stub was installed (or
// since the caller last cleared window.__opens) — see the stub's own
// comment in the initial evalIn block for why arguments, not a popup, are
// the thing worth reading.
async function windowOpenCalls(page) {
  return await page.evalIn("window.__opens");
}

// Clears both stubs' logs, so the next assertion reads only what it caused.
async function clearOpens(page) {
  await page.evalIn("window.__opens = []; window.__confirms = []; 0");
}

const fx = await fixture();
// The fixture project holds only hello.md. Task 5 resolves a clicked path
// against the real project, so the path this file prints is a file that
// actually exists — seeded before the server starts, so its tree is right.
await Deno.mkdir(`${fx.base}/roots/proj/docs`, { recursive: true });
await Deno.writeTextFile(`${fx.base}/roots/proj/${PATH}`, "# backlog\n");
await Deno.writeTextFile(`${fx.base}/roots/proj/${PATH2}`, "# notes\n");
await Deno.writeTextFile(`${fx.base}/roots/proj/${PATH3}`, "# todo\n");

const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`);
  const { evalIn, cmd } = page;
  // Section F measures real pointer geometry, and the default 800x600 headless
  // window collapses the middle column (README, trap 5).
  await cmd("Emulation.setDeviceMetricsOverride",
            { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
  await evalIn(`// Stubbed once, here, before any section runs — so section K's click
    // is never the first thing to touch window.open, and nothing added later
    // in this file gets a chance to redefine it out from under that section.
    // Records arguments rather than blocking the call: asserting on what was
    // *asked for* is what tells a correctly-refused javascript: destination
    // apart from one Chromium's own popup blocker silently ate regardless —
    // mdlinks.mjs's header note 6 hit the same trap one layer up, with
    // target="_blank" anchors instead of window.open.
    window.__opens = [];
    window.open = (...args) => { window.__opens.push(args); return null; };
    // confirm() is stubbed for two reasons, one of them load-bearing.
    // xterm's OscLinkProvider gives every OSC 8 link a *fallback* activate
    // when no linkHandler is configured, and that fallback calls confirm().
    // A native dialog in headless Chromium with no Page.javascriptDialog
    // handler wedges the renderer, so CDP's own dispatchMouseEvent never
    // returns: with linkHandler removed, section K hung forever instead of
    // failing. Recording the call and returning false turns that hang into
    // an assertion — "resh's handler ran, not xterm's fallback" — which is
    // the one thing in this section that actually discriminates linkHandler.
    //
    // Note for anyone extending this file: the stub is file-wide and always
    // answers false, so app.js's own confirms — deleting a file, closing a
    // dirty buffer, ending a session — are silently *declined* in every
    // section, including ones added after this one. That stops them wedging
    // a run, but it also means an assertion like "the tab was closed" would
    // be measuring the stub, not the feature. Read window.__confirms, or
    // override the stub for the duration of such a section.
    window.__confirms = [];
    window.confirm = (...args) => { window.__confirms.push(args); return false; };
    window.__t = () => [...terms.values()][0];
    window.__txt = () => { const b = __t().term.buffer.active; let s = "";
      for (let i = 0; i < b.length; i++) s += b.getLine(i).translateToString(true) + "\\n"; return s; };
    window.__last = () => __txt().split("\\n").filter((l) => l.trim()).pop() || "";
    // Bottom-up, whole-row equality: see this file's header on why a
    // substring search would find the echoed command line instead.
    window.__rowY = (needle) => { const b = __t().term.buffer.active;
      for (let i = b.length - 1; i >= 0; i--) { const l = b.getLine(i);
        if (l && l.translateToString(true).trim() === needle) return i + 1; }
      return -1; };
    // Session-scoped twins of __last/__rowY, for section I, where a second
    // terminal exists and __t() (first-inserted-into-the-Map) cannot tell
    // the two apart.
    window.__lastIn = (session) => { const e = terms.get(session); if (!e) return "";
      const b = e.term.buffer.active; let s = "";
      for (let i = 0; i < b.length; i++) s += b.getLine(i).translateToString(true) + "\\n";
      return s.split("\\n").filter((l) => l.trim()).pop() || ""; };
    window.__rowYIn = (session, needle) => { const e = terms.get(session); if (!e) return -1;
      const b = e.term.buffer.active;
      for (let i = b.length - 1; i >= 0; i--) { const l = b.getLine(i);
        if (l && l.translateToString(true).trim() === needle) return i + 1; }
      return -1; };
    // Asks the registered providers directly, then applies the same
    // first-provider-wins rule xterm's Linkifier applies in
    // _removeIntersectingLinks — which is where registration order does its
    // work, and which no public API exposes.
    window.__resolve = (needle) => new Promise((res) => {
      const y = __rowY(needle);
      const ps = __t().linkProviders || [];
      const replies = new Array(ps.length);
      const done = () => {
        const taken = new Set(); const kept = [];
        for (let i = 0; i < ps.length; i++) for (const l of (replies[i] || [])) {
          let clash = false;
          for (let x = l.range.start.x; x <= l.range.end.x; x++) if (taken.has(x)) clash = true;
          if (clash) continue;
          for (let x = l.range.start.x; x <= l.range.end.x; x++) taken.add(x);
          kept.push(l.text);
        }
        res({ y, links: kept, byProvider: replies.map((r) => (r || []).map((l) => l.text)) });
      };
      if (y < 0 || !ps.length) return done();
      ps.forEach((p, i) => p.provideLinks(y, (ls) => {
        replies[i] = ls || [];
        if (replies.filter((r) => r !== undefined).length === ps.length) done();
      }));
    });
    // Does each provider claim this 1-based column on this row? Section F's
    // precedence assertion is only meaningful over cells both of them want.
    window.__claimsCol = (needle, col) => new Promise((res) => {
      const y = __rowY(needle); const ps = __t().linkProviders || [];
      if (y < 0 || !ps.length) return res([]);
      const hits = new Array(ps.length).fill(false); let n = 0;
      ps.forEach((p, i) => p.provideLinks(y, (ls) => {
        hits[i] = (ls || []).some((l) => l.range.start.x <= col && col <= l.range.end.x);
        if (++n === ps.length) res(hits);
      }));
    });
    // __resolve above only asks entry.linkProviders — resh's own URL and
    // path matchers — because that is the pair section B-E's modifier gate
    // is about. Section K is about the *other* provider: xterm registers its
    // own OscLinkProvider on the terminal's private link-provider list ahead
    // of either of resh's, so an OSC 8 answer has to be read from there, not
    // from entry.linkProviders (which stays length 2 whether or not
    // linkHandler exists at all — that list is undiscriminating for this
    // section, which is exactly why this asks the terminal's full list
    // instead). No precedence merge is needed here the way __resolve needs
    // one for D: PATH_RE and URL_RE do not match plain link text like
    // "click me" or "bad", so there is nothing for them to contest.
    //
    // Returns -1, never 0, when it could not look: a missing row, or a
    // private xterm field this reaches past its API to read and which an
    // upgrade may rename. Folding that into "no links" is the conflation
    // CLAUDE.md opens with, and it bites asymmetrically here: a count of 1
    // fails safely, but a count of 0 ("never even offered the non-http
    // destination") would go green precisely when the probe had stopped
    // working. A sentinel the assertions cannot mistake for an answer is
    // the third outcome that rule asks for.
    window.__oscLinksAt = (needle) => new Promise((res) => {
      const y = __rowY(needle);
      const ps = (__t().term._core?._linkProviderService || {}).linkProviders;
      if (y < 0 || !Array.isArray(ps) || !ps.length) return res(-1);
      let n = 0, count = 0;
      ps.forEach((p) => p.provideLinks(y, (ls) => {
        count += (ls || []).length;
        if (++n === ps.length) res(count);
      }));
    });
    // Is ctrlKey set on the Control keydown itself? Recorded rather than
    // assumed, because the whole gate hangs off it.
    window.__ctrlOnDown = null;
    addEventListener("keydown", (e) => { if (e.key === "Control") window.__ctrlOnDown = e.ctrlKey; }, true);`);

  const key = (type, modifiers) => cmd("Input.dispatchKeyEvent", {
    type, key: "Control", code: "ControlLeft",
    windowsVirtualKeyCode: 17, nativeVirtualKeyCode: 17, modifiers,
  });
  // CDP's modifier bitmask: Alt 1, Ctrl 2, Meta 4, Shift 8.
  const armed = async (fn) => {
    await key("rawKeyDown", 2);
    await sleep(80);
    try { return await fn(); } finally { await key("keyUp", 0); await sleep(80); }
  };
  const resolve = (needle) => evalIn(`__resolve(${JSON.stringify(needle)})`);

  console.log("A. start a terminal and print something worth linking");
  const find = `(() => { for (let pi = 0; pi < state.panes.length; pi++) {
    const ti = state.panes[pi].tabs.findIndex((t) => t.k === "Terminal");
    if (ti >= 0) return { pi, ti, session: state.panes[pi].tabs[ti].session }; } return null; })()`;
  let loc = await evalIn(find);
  if (!loc) {
    await evalIn(`send({ t: "NewTerminal", pane: 0 })`);
    await until(async () => !!(loc = await evalIn(find)), 15, "a terminal tab");
  }
  await evalIn(`send({ t: "ActivateTab", pane: ${loc.pi}, idx: ${loc.ti} })`);
  await sleep(500);
  await evalIn(`send({ t: "StartTerminal", session: ${JSON.stringify(loc.session)} })`);
  ok(await until(() => evalIn("terms.size > 0 && !!__t().sock && __t().sock.readyState === 1"), 30, "socket"),
     "terminal socket open");
  // readline discards typeahead while initialising, so the first command
  // silently vanishes if it is typed before the prompt (README trap 3).
  await until(async () => (await evalIn("__last()")).trimEnd().endsWith("$"), 30, "shell prompt");
  await evalIn(`__t().term.input("printf '%s\\\\n' '${PATH}' '${URL_}' '${BARE}'\\r")`);
  await until(() => evalIn(`__rowY(${JSON.stringify(BARE)}) > 0`), 20, "the seeded rows");

  // Without this, "no links" below is indistinguishable from "no providers".
  ok(await evalIn("(__t().linkProviders || []).length") === 2,
     `two link providers are registered on the terminal (got ${await evalIn("(__t().linkProviders || []).length")})`);

  console.log("\nB. armed state is off by default");
  const r1 = await resolve(PATH);
  ok(r1.y > 0, `the seeded path row is on screen (row ${r1.y}) — guards both path assertions`);
  ok(await evalIn("linksArmed") === false, "the gate starts closed");
  ok(r1.links.length === 0,
     `no link is offered with the modifier up (got ${r1.links.length}: ${JSON.stringify(r1.links)})`);

  console.log("\nC. and on while the modifier is held");
  const r2 = await armed(async () => {
    ok(await evalIn("__ctrlOnDown") === true, "ctrlKey is set on the Control keydown itself");
    ok(await evalIn("linksArmed") === true, "and the keydown listener armed the gate");
    return await resolve(PATH);
  });
  ok(r2.links.length === 1 && r2.links[0] === PATH,
     `a path is offered as a link while the modifier is held (got ${r2.links.length}: ${JSON.stringify(r2.links)})`);
  ok(await evalIn("linksArmed") === false, "and the keyup disarmed it again");

  console.log("\nD. a URL wins over the path inside it");
  const r3 = await armed(() => resolve(URL_));
  ok(r3.y > 0, `the seeded URL row is on screen (row ${r3.y})`);
  // Control: if PATH_RE never matched inside a URL there would be no conflict
  // to resolve, and assertion 3 would pass with the ordering deleted.
  ok(r3.byProvider.some((ts) => ts.some((t) => t !== URL_ && t.includes("example.com"))),
     `both matchers do claim these cells, so ordering is what resolves it (${JSON.stringify(r3.byProvider)})`);
  ok(r3.links.length === 1 && r3.links[0] === URL_,
     `${URL_} is one whole-URL link, not a path link over its tail (got ${JSON.stringify(r3.links)})`);

  console.log("\nE. a bare filename is deliberately not a path");
  const r4 = await armed(() => resolve(BARE));
  ok(r4.y > 0, `the seeded bare-filename row is on screen (row ${r4.y}) — guards the assertion below`);
  ok(r4.links.length === 0,
     `a bare filename with no directory offers no link (got ${r4.links.length}: ${JSON.stringify(r4.links)})`);

  console.log("\nF. and xterm itself marks it, with the pointer already at rest");
  // Everything above talks to the providers directly, which is the right level
  // for the matchers but leaves nudgeLinks — the whole reason arming is
  // visible without moving the mouse — completely uncovered. This section
  // arms the gate with the pointer already parked over the path and asks
  // xterm what it thinks is under the cursor.
  //
  // `_core.linkifier.currentLink` is private, and reached deliberately: it is
  // what the renderer draws the underline from, and xterm publishes no way to
  // ask "what link is hovered right now". The alternative — reading underline
  // styling out of the DOM — would bind this to one of two renderers.
  // col is 0-based and matters: section D's overlap only exists over part of
  // the URL row, so a seat picked by eyeballing pixels can land outside it and
  // quietly stop testing precedence at all. Derived from the screen element's
  // real width over the terminal's real cols, never a guessed glyph width.
  const seatOf = (needle, col) => evalIn(`(() => {
    const rows = [...document.querySelectorAll(".xterm-rows div")];
    const n = rows.filter((x) => x.textContent.trim() === ${JSON.stringify(needle)}).pop();
    if (!n) return null;
    const b = n.getBoundingClientRect();
    const scr = __t().node.querySelector(".xterm-screen").getBoundingClientRect();
    const cell = scr.width / __t().term.cols;
    return { x: Math.round(scr.left + (${col} + 0.5) * cell), y: Math.round(b.top + b.height / 2), col: ${col} };
  })()`);
  const hovered = `(() => { const l = __t().term._core.linkifier.currentLink; return l ? l.link.text : null; })()`;
  const seat = await seatOf(PATH, 4);
  ok(!!seat, `the path row is rendered and hoverable (${JSON.stringify(seat)})`);
  if (seat) {
    await cmd("Input.dispatchMouseEvent", { type: "mouseMoved", x: seat.x, y: seat.y, buttons: 0 });
    await sleep(300);
    ok(await evalIn(hovered) === null, "resting on the path marks nothing while disarmed");
    // No mouse event between here and the assertion: only nudgeLinks can make
    // xterm re-ask, so this fails if the synthetic move misses its target.
    await key("rawKeyDown", 2);
    await sleep(400);
    const got = await evalIn(hovered);
    ok(got === PATH, `arming alone marked the path under the resting pointer (got ${JSON.stringify(got)})`);
    await key("keyUp", 0);
    await sleep(400);
    ok(await evalIn(hovered) === null, "and releasing unmarked it, again with no mouse movement");
  }

  // Section D applies the precedence rule itself, on the providers' raw
  // answers. This asks xterm to apply its own — _removeIntersectingLinks,
  // whose algorithm is not the one __resolve reimplements — over the same
  // cells, so the ordering claim rests on the real thing and not only on a
  // model of it.
  //
  // Column 12 (0-based) is inside `example.com`, which BOTH matchers claim —
  // the URL over cells 1..23, the path over 8..23. That is not a detail: at
  // the seat this section first used (column 4, in `https:`) only the URL
  // matcher reaches, so xterm marks the URL whichever order the providers
  // were registered in and the assertion proves nothing. Measured, not
  // reasoned: with the registration order reverted it passed anyway. The
  // guard below is what stops that returning.
  const URL_COL = 12;
  const urlSeat = await seatOf(URL_, URL_COL);
  ok(!!urlSeat, `the URL row is rendered and hoverable (${JSON.stringify(urlSeat)})`);
  if (urlSeat) {
    await cmd("Input.dispatchMouseEvent", { type: "mouseMoved", x: urlSeat.x, y: urlSeat.y, buttons: 0 });
    await sleep(200);
    await key("rawKeyDown", 2);
    await sleep(400);
    const claims = await evalIn(`__claimsCol(${JSON.stringify(URL_)}, ${URL_COL + 1})`);
    ok(claims.length === 2 && claims.every(Boolean),
       `the hovered cell is claimed by both matchers, so precedence is what decides it (${JSON.stringify(claims)})`);
    const gotUrl = await evalIn(hovered);
    ok(gotUrl === URL_,
       `xterm's own precedence marks the whole URL, not the path in its tail (got ${JSON.stringify(gotUrl)})`);
    await key("keyUp", 0);
    await sleep(300);
  }

  console.log("\nG. and arming must stay invisible to the application");
  // The nudge is a synthetic mousemove, and a synthetic MouseEvent has
  // buttons === 0 — exactly what xterm's own bindMouse forwards to the PTY
  // once an app turns on motion reporting (mode 1003). Bubbling, it reached
  // that listener on `.xterm` and reported four phantom motions per chord,
  // two of them at the detour row, so a TUI's hover highlight jumped away and
  // came back every time the user reached for the modifier.
  //
  // Asserted on the PTY bytes themselves via term.onData, which is what
  // xterm hands the socket — the same probe copyselect.mjs uses for OSC 52.
  await evalIn(`__t().__sent = ""; if (!__t().__hooked) { __t().__hooked = 1;
    __t().term.onData((d) => { __t().__sent += d; }); }`);
  await evalIn(`__t().term.input("printf '\\\\033[?1000h\\\\033[?1002h\\\\033[?1003h\\\\033[?1006h'; sleep 300\\r")`);
  await sleep(2000);
  ok(await evalIn("__t().term.modes.mouseTrackingMode") === "any",
     "mouse motion reporting is on, as a full-screen TUI leaves it");
  const reseat = await seatOf(PATH, 4) || seat;
  await cmd("Input.dispatchMouseEvent", { type: "mouseMoved", x: reseat.x, y: reseat.y, buttons: 0 });
  await sleep(300);
  // Cleared AFTER parking the pointer: that real move legitimately reports,
  // and it is the synthetic ones that must not.
  await evalIn(`__t().__sent = ""`);
  await key("rawKeyDown", 2);
  await sleep(400);
  const armSent = await evalIn(`__t().__sent`);
  ok(await evalIn(hovered) === PATH,
     "the link is still marked with the application holding the mouse");
  await key("keyUp", 0);
  await sleep(400);
  const bothSent = await evalIn(`__t().__sent`);
  ok(bothSent === "",
     `arming and disarming sent nothing to the PTY (${JSON.stringify(bothSent.slice(0, 60))}; after arming alone: ${JSON.stringify(armSent.slice(0, 60))})`);

  console.log("\nH. a click does what task 3's resolver decided");
  // Section G's `sleep 300` is still the shell's foreground process — its
  // input is buffered by the pty, not read, until that returns. Interrupt it
  // and turn mouse tracking back off, or the printf below sits queued
  // forever and every assertion in this section reads a stale screen.
  await evalIn(`__t().term.input("\\x03")`);
  ok(await until(async () => (await evalIn("__last()")).trimEnd().endsWith("$"), 20, "the prompt back after ^C"),
     "the shell prompt returned after ^C reclaimed it from section G's sleep 300");
  await evalIn(`__t().term.input("printf '\\\\033[?1000l\\\\033[?1002l\\\\033[?1003l\\\\033[?1006l'\\r")`);
  await sleep(200);
  await evalIn(`__t().term.input("printf '%s\\\\n' '${MISSING}'\\r")`);
  ok(await until(() => evalIn(`__rowY(${JSON.stringify(MISSING)}) > 0`), 20, "the seeded refusal row"),
     "the refused path's row is on screen — guards the assertions below");

  await clickLink(page, PATH, { modifier: true });
  // OpenPath is a websocket round trip; poll for the State broadcast it
  // produces rather than a fixed sleep.
  await until(() => evalIn(`state.panes[2].tabs.some((t) => t.rel === ${JSON.stringify(PATH)})`),
    20, "docs/backlog.md opened as a tab");
  await assert(
    "modifier+click on a real path opened docs/backlog.md",
    async () => (await openTabRels(page, 2)).includes(PATH),
  );

  // Asserting the tab count is unchanged, so "opened the wrong file" cannot
  // pass as "correctly refused".
  const before = (await openTabRels(page, 2)).length;
  // Makes the dependency the `until` below relies on explicit: a stale flash
  // left over from the click above would let that `until` latch onto old
  // text and return immediately, without ever having observed *this*
  // click's own refusal.
  ok(await flashText(page) === "", "no flash pending before clicking the refused path");
  await clickLink(page, MISSING, { modifier: true });
  // openTermPath only flashes before send() when the raw text carries a
  // `:line` suffix (see strip_line_suffix in projects.rs) — MISSING has
  // none, so any flash text observed here can only be the PathRefused
  // reply, not the pre-send line-number notice.
  await until(async () => (await flashText(page)) !== "", 20, "the refusal flash");
  await assert(
    "a path that does not resolve added no tab",
    async () => (await openTabRels(page, 2)).length === before,
  );
  await assert(
    "and flashed the refusal in the terminal that was clicked",
    async () => /cannot read|no such file/i.test(await flashText(page)),
  );

  console.log("\nI. a refusal reaches only the terminal that was clicked");
  // The same single-subscriber trap CLAUDE.md names for send_to vs.
  // broadcast: with exactly one terminal in the fixture, section H's own
  // "in the terminal that was clicked" assertion could not actually tell
  // "the right terminal" apart from "some terminal, any terminal" — there
  // was only ever one to check. A second, independent terminal is what
  // discriminates them: both still share the one page-wide `ctrl` websocket
  // (see connectControl), so pendingLink.entry is the only thing routing a
  // PathRefused back to the DOM node the click actually came from.
  //
  // resolve_terminal_path's ENOENT text does not include the path at all
  // (confirmed in the report) — MISSING and ALSO_MISSING refuse with the
  // *identical* string. So a stale flash from section H's own click could
  // not be told apart from a wrongly-routed one by content; it has to be
  // told apart by settling first. termFlash's fade is 1600ms — wait it out
  // before taking the baseline below, rather than race it.
  ok(await until(async () => (await flashText(page, loc.session)) === "", 5, "section H's flash to fade"),
     "terminal 1's own flash from section H settled before section I begins");
  const ALSO_MISSING = "also/missing.ts";
  await evalIn(`send({ t: "NewTerminal", pane: 0 })`);
  // Not the section-A `find`: that returns the *first* Terminal tab it
  // finds, which is still terminal 1. This wants whichever one is not that.
  const find2 = `(() => { for (let pi = 0; pi < state.panes.length; pi++) {
      for (let ti = 0; ti < state.panes[pi].tabs.length; ti++) {
        const t = state.panes[pi].tabs[ti];
        if (t.k === "Terminal" && t.session !== ${JSON.stringify(loc.session)}) return { pi, ti, session: t.session };
      } } return null; })()`;
  let loc2 = null;
  ok(await until(async () => !!(loc2 = await evalIn(find2)), 15, "a second terminal tab"),
     "a second, distinct terminal tab was created");
  await evalIn(`send({ t: "ActivateTab", pane: ${loc2.pi}, idx: ${loc2.ti} })`);
  await sleep(500);
  await evalIn(`send({ t: "StartTerminal", session: ${JSON.stringify(loc2.session)} })`);
  ok(await until(() => evalIn(
      `(() => { const e = terms.get(${JSON.stringify(loc2.session)}); return !!e && !!e.sock && e.sock.readyState === 1; })()`),
      30, "second socket"),
     "second terminal socket open");
  // Same readline-typeahead trap section A guards against: the prompt must
  // be up before anything is typed at it.
  ok(await until(async () => (await evalIn(`__lastIn(${JSON.stringify(loc2.session)})`)).trimEnd().endsWith("$"), 30, "second shell prompt"),
     "second terminal's shell prompt came up");
  await evalIn(`terms.get(${JSON.stringify(loc2.session)}).term.input("printf '%s\\\\n' '${ALSO_MISSING}'\\r")`);
  ok(await until(() => evalIn(`__rowYIn(${JSON.stringify(loc2.session)}, ${JSON.stringify(ALSO_MISSING)}) > 0`), 20, "second terminal's refusal row"),
     "the second terminal's own row is on screen");

  await clickLink(page, ALSO_MISSING, { modifier: true, session: loc2.session });
  // Whichever entry actually gets the flash — never assumed to be terminal
  // 2, since that assumption is exactly what a misrouted pendingLink would
  // violate. Polled together, on a short timeout, so a misroute is caught
  // before its own flash can auto-decay (termFlash fades in 1600ms) and
  // erase the evidence the very next assertion below is looking for; a
  // 20-second wait on terminal 2 alone would let a flash that landed on
  // terminal 1 instead fade to "" long before this file ever looked there.
  await until(async () => (await flashText(page, loc2.session)) !== "" || (await flashText(page, loc.session)) !== "",
    10, "a refusal flash on either terminal");
  await assert(
    "the refusal flashed on the terminal that was actually clicked",
    async () => /cannot read|no such file/i.test(await flashText(page, loc2.session)),
  );
  await assert(
    "and not on the other terminal, which was never clicked",
    async () => (await flashText(page, loc.session)) === "",
  );

  console.log("\nJ. a mismatched refusal in flight does not swallow the next one");
  // PATH_RE marks ordinary prose, so a user arming links over a paragraph
  // and clicking two different, both-refusing spans before the first reply
  // lands is the common case here, not an edge case. pendingLink is a single
  // slot: this proves a refusal for a click that is no longer pending — text
  // that does not match — leaves the slot alone rather than stranding it, so
  // the click that IS still in flight still gets its own reply flashed
  // rather than silently dropped to console.warn.
  //
  // Injected via onEvent directly rather than raced over the real socket:
  // openTermPath and the injected PathRefused run in the same synchronous
  // tick, before any real websocket message can arrive, which is
  // deterministic where a real second click's timing would not be.
  const STRAY = "stray/unrelated.rs";
  const REAL_TEXT = "second/real-click.rs";
  ok(await flashText(page, loc.session) === "", "terminal 1 starts this section with no flash pending");
  await evalIn(`openTermPath(__t(), ${JSON.stringify(REAL_TEXT)});
    onEvent({ t: "PathRefused", text: ${JSON.stringify(STRAY)}, msg: "unrelated" });`);
  await until(async () => (await flashText(page, loc.session)) !== "", 20, "the real click's own refusal flash");
  await assert(
    "a mismatched refusal in between did not swallow the next click's own reply",
    async () => /cannot read|no such file/i.test(await flashText(page, loc.session)),
  );

  console.log("\nK. an application's own hyperlink needs no modifier");
  // OSC 8: ESC ] 8 ; params ; URI ESC \ text ESC ] 8 ; ; ESC \. Written into
  // the real shell with printf, so xterm's own OSC parser handles it exactly
  // as it would from a real application (Claude Code prints these for file
  // paths and URLs alike) — never synthesised by calling a provider's
  // activate() directly, the way __resolve does for sections B-E, because
  // the whole point of this section is that no modifier gate sits in front
  // of it, and there is no gate to bypass by construction if the test skips
  // the real mouse event.
  const OSC_URL = "https://example.com/osc";
  await typeInTerm(page, String.raw`printf '\e]8;;${OSC_URL}\e\\click me\e]8;;\e\\\n'`);
  ok(await until(() => evalIn(`__rowY("click me") > 0`), 20, "the OSC 8 link's row"),
     "the hyperlinked row is on screen — guards the assertion below");
  // __rowY reads the buffer directly and is ahead of the DOM by up to a
  // render tick; seatOf below queries the rendered .xterm-rows divs, so it
  // needs the render to have actually landed, not just the buffer write.
  await sleep(250);
  await assert(
    "an OSC 8 link is offered with no modifier held",
    async () => (await linksAt(page, "click me")) === 1,
  );

  // Clicks land at the row's own middle character rather than clickLink's
  // fixed column 4 (picked for longer PATH/URL fixtures): "bad" below is
  // only 3 columns wide, and a fixed offset lands past the word entirely,
  // missing the link outright — which would make the javascript: assertion
  // below pass for the wrong reason (nothing was under the pointer to click)
  // rather than the right one (something was, and got refused). No modifier:
  // that asymmetry with clickLink's modifier-gated clicks is this section's
  // whole point.
  //
  // Row lookup normalises U+00A0 back to a plain space before comparing:
  // xterm renders the space *inside* "click me" as U+00A0 in the DOM (found
  // by dumping codePoints off a failed match — the buffer itself, which
  // __rowY reads via translateToString, keeps a real space), so a literal
  // needle with a plain space never matches textContent as-is. Same trap
  // this file's own header names for substring search, just a different
  // character than a reader would guess.
  const seatAt = (needle) => evalIn(`(() => {
    const norm = (s) => s.replace(/\\u00a0/g, " ").trim();
    const rows = [...document.querySelectorAll(".xterm-rows div")];
    const n = rows.filter((x) => norm(x.textContent) === ${JSON.stringify(needle)}).pop();
    if (!n) return null;
    const b = n.getBoundingClientRect();
    const scr = __t().node.querySelector(".xterm-screen").getBoundingClientRect();
    const cell = scr.width / __t().term.cols;
    const col = Math.floor(${JSON.stringify(needle)}.length / 2);
    return { x: Math.round(scr.left + (col + 0.5) * cell), y: Math.round(b.top + b.height / 2) };
  })()`);
  const clickWord = async (needle) => {
    const s = await seatAt(needle);
    if (!s) return false;
    await cmd("Input.dispatchMouseEvent", { type: "mouseMoved", x: s.x, y: s.y, buttons: 0 });
    await sleep(200);
    await cmd("Input.dispatchMouseEvent", { type: "mousePressed", x: s.x, y: s.y, button: "left", buttons: 1, clickCount: 1 });
    await cmd("Input.dispatchMouseEvent", { type: "mouseReleased", x: s.x, y: s.y, button: "left", buttons: 0, clickCount: 1 });
    await sleep(200);
    return true;
  };

  // Control for the stub: without this, "zero recorded calls" below would be
  // indistinguishable from "the stub never captured anything", which is
  // exactly the trap mdlinks.mjs's header note 6 names for the same
  // javascript: case one layer up (there, Chromium's popup blocker was the
  // thing standing in for a deleted guard).
  //
  // This click, not the link count above, is what covers `linkHandler`.
  // Measured, not assumed: with linkHandler set back to null the count
  // assertion above stayed green, because OscLinkProvider hands back its
  // ranges either way — the option only decides who gets told about the
  // activation. So the pair below is the discriminating half of section K.
  ok(await clickWord("click me"), "the OSC 8 link's row was clicked");
  ok(await until(async () => (await windowOpenCalls(page)).length > 0, 10, "window.open recorded a call"),
     "CONTROL: clicking a plain https OSC 8 link actually reached window.open");
  const legit = await windowOpenCalls(page);
  ok(legit.length === 1 && legit[0][0] === OSC_URL,
     `window.open was asked for the URL the application declared (got ${JSON.stringify(legit)})`);
  // xterm's no-linkHandler fallback asks the user "do you want to navigate
  // to …?" through a native confirm(). Its silence is the positive evidence
  // that resh's own handler took the activation, rather than resh merely
  // benefiting from a default that happens to open the same URL.
  ok((await evalIn("window.__confirms")).length === 0,
     "resh's own handler took the activation, not xterm's confirm() fallback");

  await clearOpens(page);
  await typeInTerm(page, String.raw`printf '\e]8;;javascript:alert(1)\e\\bad\e]8;;\e\\\n'`);
  ok(await until(() => evalIn(`__rowY("bad") > 0`), 20, "the javascript: link's row"),
     "the javascript:-hyperlinked row is on screen — guards the assertion below");
  ok(await clickWord("bad"), "the javascript: link's row was clicked");
  // No further click here to wait on: with the click already dispatched,
  // this just gives openUrl's synchronous check time to run (or not) before
  // reading window.__opens.
  await sleep(300);
  await assert(
    "a javascript: OSC 8 destination opened nothing",
    async () => (await windowOpenCalls(page)).length === 0,
  );
  // …but not because of anything resh does. Read the vendored provider: it
  // runs `new URL(uri)` and drops any link whose protocol is not http(s)
  // before it is ever offered, so there is nothing under the pointer to
  // activate and openUrl is never reached. Asserted rather than left
  // implicit, because the assertion above would otherwise be the third
  // variant of the same vacuous javascript: check this project has written
  // (mdlinks.mjs note 6 was the first) — passing on a refusal made two
  // layers away from the guard it claims to cover. Deleting SAFE_URL from
  // openUrl leaves both green.
  const badLinks = await linksAt(page, "bad");
  ok(badLinks === 0,
     `xterm's own provider never even offered the non-http destination (got ${badLinks}${badLinks === -1 ? ": could not look, NOT zero" : ""})`);

  // So the scheme allowlist gets its own, reachable, test. openUrl is what
  // every other link route in this file ends at — the matchers' clicks, the
  // OSC 8 handler above — and it is the only thing standing between a URL
  // chosen by whatever is running and window.open. Driven directly because
  // no OSC 8 payload can reach it with a hostile scheme; the control below
  // is what keeps "nothing recorded" from meaning "nothing ran".
  await clearOpens(page);
  await evalIn(`openUrl("https://example.com/direct")`);
  const direct = await windowOpenCalls(page);
  ok(direct.length === 1 && direct[0][0] === "https://example.com/direct",
     `CONTROL: openUrl passes an https destination through (got ${JSON.stringify(direct)})`);
  await clearOpens(page);
  await evalIn(`openUrl("javascript:alert(1)")`);
  await assert(
    "openUrl refuses a javascript: destination outright",
    async () => (await windowOpenCalls(page)).length === 0,
  );
  await clearOpens(page);
  await evalIn(`openUrl("file:///etc/passwd")`);
  await assert(
    "and any other scheme off the allowlist, not just javascript:",
    async () => (await windowOpenCalls(page)).length === 0,
  );

  console.log("\nL. an application that owns the mouse keeps a plain click and yields a modifier one");
  // The question the whole design was written around, and the one thing
  // reading the vendored bundle could not settle: xterm's Linkifier binds its
  // own mousedown to `.xterm-screen` with no mouse-mode check at all, while
  // the core's handler on `.xterm` calls cancel(e) as soon as
  // coreMouseService.areMouseEventsActive. Which of those two an activation
  // survives is a property of two listeners' targets and order, not something
  // resh can assert about itself — so it is measured here, against a terminal
  // that really has mouse reporting on and a path that really exists.
  ok(!(await openTabRels(page, 2)).includes(PATH2),
     `${PATH2} is not open yet, so assertion 11 below has something left to prove`);
  const scr = await evalIn(`(() => {
    const r = __t().node.querySelector(".xterm-screen").getBoundingClientRect();
    return { w: Math.round(r.width), h: Math.round(r.height) }; })()`);
  ok(scr.w > 0 && scr.h > 0,
     `terminal 1 is mounted with a real rect, so a seat means something (${JSON.stringify(scr)})`);
  // Both rows are printed before mouse reporting goes on: from there the
  // shell is inside a foreground `sleep` and anything typed at it is buffered
  // by the pty rather than run.
  await evalIn(`__t().term.input("printf '%s\\\\n' '${PATH2}' '${PATH3}'\\r")`);
  ok(await until(() => evalIn(`__rowY(${JSON.stringify(PATH3)}) > 0`), 20, "the second real path's row"),
     "two real, never-opened paths are on screen — guards every assertion below");
  await sleep(250);
  // Mouse reporting on, and held on by a foreground process. The `sleep` is
  // not padding: at a bash prompt readline reads the click reports below as
  // input and echoes garbage over the very rows this section clicks. Same
  // incantation copyselect.mjs section D uses, and the state Claude Code
  // leaves a terminal in.
  await evalIn(`__t().term.input("printf '\\\\033[?1000h\\\\033[?1002h\\\\033[?1003h\\\\033[?1006h'; sleep 300\\r")`);
  await sleep(2000);
  ok(await evalIn("__t().term.modes.mouseTrackingMode") === "any",
     "the application really holds the mouse before either click");
  await evalIn(`__t().__sent = ""; if (!__t().__hooked) { __t().__hooked = 1;
    __t().term.onData((d) => { __t().__sent += d; }); }`);

  // 10. a plain click still belongs to the running application
  const tabsBefore = (await openTabRels(page, 2)).length;
  ok(await clickLink(page, PATH2, { modifier: false }), "the path row was found and plain-clicked");
  // An SGR press report — CSI < 0 ; col ; row M — is button 0 going *down*;
  // motion under mode 1003 reports button 35, so this cannot be satisfied by
  // the pointer merely crossing the row. Without it, "no tab was added" is
  // equally true of a click that landed on nothing, of a terminal that was
  // never mounted, and of the whole feature deleted — the trap CLAUDE.md
  // names, and the reason this control sits between the click and its
  // assertion rather than being left to the reader's confidence.
  const plainSent = await evalIn(`__t().__sent`);
  ok(/\x1b\[<0;\d+;\d+M/.test(plainSent),
     `CONTROL: the plain click reached the application as a mouse report (${JSON.stringify(plainSent.slice(-40))})`);
  // An absence cannot be polled for. A round trip that was going to open a
  // tab has landed well inside this window — section H's own OpenPath is
  // observed in tens of milliseconds.
  await sleep(1500);
  await assert(
    "a plain click on a path stayed with the application and opened nothing",
    async () => {
      const rels = await openTabRels(page, 2);
      return rels.length === tabsBefore && !rels.includes(PATH2);
    },
  );

  // 11. and a modifier+click still opens, with the app holding the mouse
  ok(await evalIn("__t().term.modes.mouseTrackingMode") === "any",
     "and still holds it for the modifier click — not a mode the shell dropped in between");
  await evalIn(`__t().__sent = ""`);
  ok(await clickLink(page, PATH2, { modifier: true }), "the same path row was modifier-clicked");
  await until(() => evalIn(`state.panes[2].tabs.some((t) => t.rel === ${JSON.stringify(PATH2)})`),
    20, `${PATH2} opened as a tab`);
  await assert(
    "modifier+click opened the file even with mouse reporting on",
    async () => (await openTabRels(page, 2)).includes(PATH2),
  );
  // The half of "does the application also react?" that automation can
  // answer: the bytes leave. Recorded as an assertion rather than left
  // implicit, because it is a measurement and not a design choice — xterm
  // reports the click to the application *and* activates the link, and a
  // future xterm that stopped doing one of those should break a test here
  // rather than quietly change what a click means. SGR button 16 is
  // left-button-with-control (0 | 16); on macOS the modifier is Meta, which
  // this bundle reports as its own bit, so this number is Linux's.
  //
  // Whether an application then does anything visible with a ctrl+click is
  // the part no test can settle — see this file's header, and the by-hand
  // checklist in task-7-report.md.
  const modSent = await evalIn(`__t().__sent`);
  ok(/\x1b\[<16;\d+;\d+M/.test(modSent),
     `the modifier click is reported to the application as well as opening the file (${JSON.stringify(modSent)})`);

  // The gate has two halves, and only one of them was reachable until now:
  // matchProvider refuses to *offer* a link while disarmed, and its activate
  // callback re-checks the modifier on the event itself. Deleting the second
  // alone leaves every other assertion in this file green (measured — see the
  // header's revert L1a), because with the first intact there is never a link
  // under an unmodified click to activate. The state that separates them is
  // the one the re-check was written for: a stale underline left by a keyup
  // that never arrived — alt-tabbing away with the key down — where the link
  // is marked but the click carries no modifier.
  //
  // Reproduced by holding the key for the hover and clearing it on the mouse
  // event alone, which CDP can express and a real user cannot. Mouse
  // reporting is still on, so this doubles as the case that matters most: a
  // stale underline over an application that would otherwise have had the
  // click.
  await key("rawKeyDown", 2);
  await sleep(200);
  const seat3 = await seatOf(PATH3, 4);
  ok(!!seat3, `the third path's row is rendered and hoverable (${JSON.stringify(seat3)})`);
  await cmd("Input.dispatchMouseEvent",
    { type: "mouseMoved", x: seat3.x, y: seat3.y, buttons: 0, modifiers: 2 });
  await sleep(400);
  ok(await evalIn(hovered) === PATH3,
     "CONTROL: the link really is marked when the unmodified click below arrives");
  const before3 = (await openTabRels(page, 2)).length;
  const clickAt = async (s, modifiers) => {
    await cmd("Input.dispatchMouseEvent",
      { type: "mousePressed", x: s.x, y: s.y, button: "left", buttons: 1, clickCount: 1, modifiers });
    await cmd("Input.dispatchMouseEvent",
      { type: "mouseReleased", x: s.x, y: s.y, button: "left", buttons: 0, clickCount: 1, modifiers });
  };
  await clickAt(seat3, 0);
  await sleep(1500);
  await assert(
    "a click with no modifier on a still-marked link opens nothing",
    async () => {
      const rels = await openTabRels(page, 2);
      return rels.length === before3 && !rels.includes(PATH3);
    },
  );
  // Same seat, same armed state, same marked link — only the event's own
  // modifier differs. Without this, the assertion above is also satisfied by
  // a seat that missed the row entirely.
  await clickAt(seat3, 2);
  await until(() => evalIn(`state.panes[2].tabs.some((t) => t.rel === ${JSON.stringify(PATH3)})`),
    20, `${PATH3} opened as a tab`);
  await assert(
    "CONTROL: the very same click, with the modifier on the event, does open it",
    async () => (await openTabRels(page, 2)).includes(PATH3),
  );
  await key("keyUp", 0);
  await sleep(200);

  // Leaves the shell the way section H found it, so anything appended after
  // this section starts from a prompt rather than a wedged `sleep`.
  await evalIn(`__t().term.input("\\x03")`);
  await until(async () => (await evalIn("__last()")).trimEnd().endsWith("$"), 20, "the prompt back after ^C");

} finally {
  page?.close();
  browser.close();
  await resh.close();
  await fx.cleanup();
}

console.log(fail === 0 ? "\nALL PASS" : `\n${fail} FAILED`);
Deno.exit(fail === 0 ? 0 : 1);
