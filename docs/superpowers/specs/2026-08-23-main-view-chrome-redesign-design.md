# resh — main-view chrome redesign (Quiet IDE)

Restyles the workspace chrome — header, header popups' buttons, pane-header
icons — around the design the user picked on the mockup canvas
(<https://claude.ai/code/artifact/c8554d0e-db13-4bef-952b-0d3157db99a2>, page
"Main view"), and adds the one new behaviour the header gains: a
branch/worktree chip that opens a switcher for the current repository's
worktrees. Everything below the header keeps its exact current values.

## The problem this solves

The main view's *panes* were designed deliberately (cards on a window surface,
matched to JetBrains' New UI — `static/style.css` documents the measurements),
but the *chrome* around them accreted: the header is a flat row of glyph
buttons (`◆`, `🔔`, `⟳`, `✕ Close`) where `🔔` is an emoji that renders
differently per platform and nothing shares a visual language with the
stroke-drawn SVG icons the file tree and tabs already use. Three items from
`docs/backlog.md`'s "First things to do" also land in the header and have
nowhere to go in the current layout:

- *"handle worktree selection/switching: top-bar left we already have
  project+branch, we should add worktree+selector"* — built by this spec.
- *"all project search, ala Idea shift-shift"* — placeholder in this spec, its
  own spec later.
- *"settings system"* / *"theme selector"* — placeholder in this spec, its own
  spec later (a project/global settings split is already planned).

## What changes, in one sentence

`workspace_page`'s header is rebuilt as identity · switcher · search · actions
with a consistent stroke-SVG icon set, the branch text becomes a chip that
opens a worktree switcher backed by a new `/frag/_worktrees` fragment, and the
pane-header glyphs (`◌ ◍ ⌃ ⇄ ⤢ ⤡`) become SVG — nothing else in the layout
moves.

## What deliberately does not change

- **Panes, tabs, tree, editor, terminal** — every measurement, colour and
  behaviour below the header row stays as `style.css` has it today.
- **`⚠ config` and `⧉ sharing selection`** keep their exact text, colour and
  placement. The sharing indicator is deliberately loud (it is the only thing
  on the page saying a selection is leaving the machine — see the comment above
  `#sharing` in `style.css`); a chrome pass must not quiet it.
- **`+`** stays a text glyph, as do the tab-level `●` (dirty/attention), `⚠`
  (stale), `✎` and `×`. They are the app's existing vocabulary. The one glyph
  that *does* change is `✻` — see "Pane-header icons" below: the
  launch-Claude button becomes the official Claude mark.
- **The bottom status bar stays hidden.** Considered in the design round and
  dropped as redundant — the information it would carry (session liveness,
  save state) is already on the tabs and the `.path` bar. `#statusbar` remains
  built-but-hidden exactly as `render.rs`'s comment describes; this is a
  decision record so the next chrome pass knows it was weighed, not forgotten.
- **The running-projects panel** (`#projbtn`/`#projpanel`) keeps its behaviour,
  including `target="dl-{key}"` named-tab links: it answers "what is running
  everywhere", where one named tab per project is the point. Only the button's
  face changes (SVG diamond + count).

## The header, left to right

All markup is built in `render.rs::workspace_page`, escaped as everything there
is; styling extends the existing `header` rules in `style.css`. Header height
goes 34px → 38px to seat the chip and the search field; `--header-h` is already
measured at runtime (`app.js::render` reads `header.offsetHeight`), so the grid
follows without a further change.

1. **Identity.** `◆` home link (inline SVG diamond in `--accent`, replacing the
   text glyph) and the project name, unchanged in behaviour.
2. **Branch/worktree chip.** A bordered chip (`--tool` fill, `--border` stroke,
   6px radius, 24px tall) containing:
   - the branch SVG + `#gitinfo` exactly as today (`/frag/{proj}/status` htmx
     swap: branch name, change count in accent),
   - `<span id="wtlabel"></span>` — empty at page render, filled out-of-band by
     the worktrees fragment (below) with `· <worktree> ▾` once the fragment
     knows there is anything to switch to.
   The chip is the click target that opens the switcher panel.
3. **Search placeholder.** A centered, input-styled box (400px, `--tool` fill,
   magnifier SVG, "Search files, symbols, sessions", a `⇧ ⇧` key chip). It is
   a `<div>`, not an input: not focusable, no handler, and its tooltip says
   plainly that project-wide search is not implemented yet. It exists so the
   layout is final now and the search feature (its own spec) lands into a
   reserved slot instead of reopening the header.
4. **Actions, right-aligned:** running-projects (SVG diamond + `#projcount`),
   bell (stroke SVG replacing `🔔`; `#bellcount` badge and `#noticepanel`
   unchanged), settings gear placeholder — the conventional toothed cog
   (Feather's MIT-licensed "settings" shape), not a stylised stand-in
   (tooltip: settings are not implemented yet; no handler), refresh (SVG
   replacing `⟳`), a 1px divider, then `Close`
   as a bordered quiet button (SVG × + label; same handler and confirmation
   semantics as today).

Placeholders are the user's explicit choice for this round: visible, honest
about being inert (tooltip), and never styled as disabled-grey mystery — they
look like the controls they will become.

## The worktree switcher — the one new behaviour

**Data.** `registry::ProjectStatus` already carries everything needed
(`src/registry.rs:10-38`): `key`, `url`, `live`, `branch`, `parent` (the
repository's storage key for a linked worktree), `reachable`. No registry
changes.

**Fragment.** A new arm in `routes.rs` beside `["frag", "_projects"]`
(`src/routes.rs:72`): `["frag", "_worktrees"]` with `?current={qkey}`, calling
`registry::known_projects` and a new `render::worktrees_strip(current_key,
&ps)`. The *family* is resolved from `current`: if the current entry has
`parent: Some(k)`, the family root is `k`, else the current key; the family is
the root plus every entry whose `parent` is the root's key. Unlike
`projects_strip`, **no `live > 0` filter** — an idle worktree is exactly what
you switch to before starting work in it. If the current key resolves to no
entry (not yet in the registry, non-git project), the fragment returns the
empty label and no rows — the chip then shows branch only, no caret, and
clicking it opens an empty panel stating "no worktrees". That is the absent
case stated as absent, not an error swallowed.

**Response shape.** Two parts in one response:

- `<span id="wtlabel" hx-swap-oob="true">…</span>` — htmx swaps this into the
  chip out-of-band (the vendored `htmx.min.js` ships `hx-swap-oob`). Content:
  `· main worktree ▾` when current is the root, `· {name} ▾` when current is a
  linked worktree — and **empty when the family has one member**, so a repo
  with no worktrees shows today's plain branch chip with no caret.
- The row list, swapped into the panel target: one row per family member, in
  registry order (root first). Each row: `●`/`○` liveness mark, the worktree's
  display name (the root shows its `url`; a child shows its last path segment,
  full `url` in `title=`), and `⎇ branch` muted. The current row is marked as
  `.current` (same treatment as `.projstrip`). An unreachable row renders as a
  dimmed, inert `<span>` with the explanatory tooltip, exactly the
  `projects_strip` precedent.

**Click semantics** (the user's choice: both). Reachable rows are plain
anchors — `href="/{url}"` and **no `target` attribute**. Plain click navigates
this browser tab to the worktree (workspace state is server-side per project,
so nothing is lost); ⌘/ctrl-click opens a new tab through the browser's native
modifier handling. No JavaScript implements the modifier: the behaviour *is*
the absence of `target`, which is why a test must pin that absence (below).

**Panel wiring.** `#wtpanel` is a third header popup positioned and toggled
like `#projpanel` and `#noticepanel` (the two existing ones deliberately share
a pattern; this joins it), loaded with
`hx-get="/frag/_worktrees?current={qkey}"` on `load, refresh from:body,
projects from:body` — the same triggers as `#projstrip`, so the label and rows
follow branch switches and worktree creation as soon as the existing refresh
machinery notices them. Left-anchored under the chip rather than
right-anchored.

## Pane-header icons

`app.js::buildPaneIcons`'s `icon()` helper takes an SVG string (assigned via
`innerHTML`) instead of a text glyph for: dotfiles toggle (dashed/filled
circle), collapse-all (chevron), move-to-pane (double arrow), maximize/restore
(out/in arrows). All SVG is authored in the codebase, stroke-based on the
16px grid, `currentColor` so the existing `.paneicon` hover colours keep
working. The `innerHTML` here is constant markup with no interpolation — no
escaping surface is added; anything ever interpolated into an icon goes
through the existing escape paths.

**The launch-Claude button (`.newclaude`) drops the `✻` text glyph for the
official Claude mark** — the starburst logo as packaged by lobehub
(<https://lobehub.com/icons/claude>; path fetched 2026-08-23 from
`lobehub/lobe-icons` `static-svg/icons/claude-color.svg`), inlined as one SVG
constant in `app.js`, 13px on the 24-viewBox it ships with, filled the brand's
`#D97757` in every theme rather than `currentColor` — the point of using the
real mark is that it is recognisable, and it reads against all five shipped
theme backgrounds (the darkest, solarized's `#002b36`, included). Hover keeps
the existing background treatment; the fill does not change. The old comment
in `app.js` explaining the ✻-as-text choice is replaced by one recording this
decision and the asset's provenance.

## Non-goals

- Live project-wide search, the settings pane, theme switching — placeholders
  only; each is its own backlog item and future spec.
- Worktree *creation, removal, or branch checkout* — the switcher navigates
  between worktrees that exist; mutating them stays in the terminal.
- The popup/dialog/notification redesign (`docs/backlog.md`: "fix popup
  UX+design") — `#wtpanel` copies the existing popup pattern rather than
  pre-empting that pass.
- Mobile layout, per-theme favicons, and anything else in the backlog's UI
  list not named here.

## Testing

Unit tests (`render.rs`, `routes.rs`), each watched to fail with the code
reverted before it counts (per CLAUDE.md):

- `worktrees_strip` lists the whole family of the current key — root and
  children, **including idle ones** (the discriminating case against reusing
  `projects_strip`'s filter: fixture has a live root and an idle child, assert
  the idle child's name is present).
- Resolving family from a *child* current key yields the same rows as from the
  root.
- The current row carries `.current`; only one row does.
- A reachable row has `href` **and no `target=` substring**; the assertion
  pairs presence of `href` with absence of `target` so it cannot pass on an
  empty string. An unreachable row has neither `href` nor `target` and keeps
  its tooltip.
- `wtlabel` is empty for a one-member family, `· main worktree ▾` at the root
  of a multi-member family, `· {name} ▾` in a child; the OOB attribute is
  present.
- Names and branches containing `<`, `&`, `"` render escaped (fixture must
  actually contain a metacharacter — the vacuous-fixture trap is on record).
- A `frag_route` dispatch test for `_worktrees` beside the existing
  `_projects` ones (`routes.rs:799`'s helper).
- `workspace_page` wires the chip, `#wtpanel`, the placeholders and the SVG
  header (extend `workspace_page_wires_everything`).

Browser (`tests/browser/`, per CLAUDE.md nothing in `cargo test` reaches
`app.js`): chip click opens `#wtpanel`; a row click navigates the page to the
other worktree's URL and the workspace renders there; pane icons render and
the dotfiles toggle still drives its intent; the `.newclaude` button carries
the Claude-mark SVG and still starts a terminal running Claude. Mind the four traps in
`tests/browser/README.md`. Also verify by eye in a real browser and run the
suite on the Linux host — both have caught what the suites did not.

## Task order

1. `render::worktrees_strip` + `routes.rs` arm + unit tests (server side is
   complete and tested before any markup changes).
2. Header rebuild in `workspace_page` + `style.css`: chip, placeholders, SVG
   actions, `#wtpanel` markup; extend the wiring tests.
3. `app.js`: `#wtpanel` toggle (copy the `#projbtn` pattern), pane-icon SVG.
4. Browser tests + real-browser pass.

Review between tasks, per the project process.

## Open questions

- The search placeholder's exact copy ("Search files, symbols, sessions" is
  the mockup's; trim if it overpromises for the eventual first search spec).
- Whether the chip should also show ahead/behind once a git-log view exists —
  out of scope now, noted so the chip's layout leaves room.
