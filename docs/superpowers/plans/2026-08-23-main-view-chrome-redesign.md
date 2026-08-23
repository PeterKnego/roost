# Main-View Chrome Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild the workspace header (identity · branch/worktree chip · search placeholder · SVG actions), add a real worktree switcher backed by a new `/frag/_worktrees` fragment, and convert the pane-header glyphs and the launch-Claude button to SVG — changing nothing below the header row.

**Architecture:** Server-rendered HTML built in `render.rs` (everything interpolated goes through `esc`/`percent_encode`), one new stateless fragment routed in `routes.rs`, styling in `static/style.css` on the existing CSS variables, client wiring in `static/app.js` copying the existing header-popup pattern. No new dependencies, no protocol changes, no registry changes.

**Tech Stack:** Rust (no async runtime, hand-rolled HTTP), htmx (vendored — `hx-swap-oob` is used and is supported), plain JS, Deno browser tests over CDP.

**Spec:** `docs/superpowers/specs/2026-08-23-main-view-chrome-redesign-design.md` — read it first; every visual value below traces to it and to the approved canvas (Option A / "Main view" page).

## Global Constraints

- `cargo test`, never `cargo test --release`.
- **Build from this checkout only.** The shared cargo target-dir means a `cargo build` from any second checkout (worktree included) silently rewrites the baked asset table. Do not create a worktree for this work; if one was created anyway, recover with `cargo clean -p resh` (see CLAUDE.md "Build from one checkout").
- Escape everything interpolated into HTML (`render::esc`); project keys go through `crate::http::percent_encode` before landing in URLs/query strings.
- Stage explicit paths in every commit — never `git add -A` (the user's backlog notes have been swept into a commit that way before).
- Another session (`resh-f8`) may be editing `src/proto.rs`, `src/hub.rs`, `src/render.rs`, `static/app.js` on this branch. Before Task 3 (first `render.rs`/`app.js` edit), send it a heads-up via SendMessage and check `git status`/`git log` for its commits; rebase/pull before starting a task if the tree moved.
- Comments give rationale, not mechanics; new tests go in `#[cfg(test)] mod tests` at the bottom of the same file.
- Every new test must be watched to fail before it counts — the plan's test-first steps do this for unit tests; browser tests get explicit revert steps.
- Do not touch: `#statusbar` (stays hidden — spec records the decision), the `⧉ sharing selection` / `⚠ config` indicators' text and colour, anything below the header row, the two upload POST endpoints, any Origin check.

---

### Task 1: `render::worktrees_strip` — the switcher fragment's HTML

**Files:**
- Modify: `src/render.rs` (new pub fn near `projects_strip`, ~line 632; tests at the bottom of the file's `mod tests`)

**Interfaces:**
- Consumes: `crate::registry::ProjectStatus` (`src/registry.rs:10-38` — fields `key`, `url`, `live`, `oldest_age_secs`, `has_layout`, `branch`, `parent: Option<String>`, `reachable`), `render::esc`, `crate::http::percent_encode`.
- Produces: `pub fn worktrees_strip(current_key: &str, projects: &[crate::registry::ProjectStatus]) -> String` — Task 2 routes to it; Task 3's markup hosts its output. The response always begins with `<span id="wtlabel" hx-swap-oob="true">…</span>` followed by `<span class="wtstrip">…rows…</span>`.

- [ ] **Step 1: Write the failing tests**

Add at the bottom of `mod tests` in `src/render.rs`:

```rust
    /// Fixture for the worktree switcher. `url` is passed separately from
    /// `key` because a child's key is percent-encoded (`a%2Fb`) while its
    /// url keeps readable slashes (`a/b`) — conflating them in the fixture
    /// would hide exactly the encoding bugs the strip must not have.
    fn wt(key: &str, url: &str, parent: Option<&str>, live: usize, branch: &str, reachable: bool)
        -> crate::registry::ProjectStatus
    {
        crate::registry::ProjectStatus {
            key: key.into(), url: url.into(),
            live, oldest_age_secs: None, has_layout: true,
            branch: branch.into(),
            parent: parent.map(|s| s.to_string()),
            reachable,
        }
    }

    fn karpie_family() -> Vec<crate::registry::ProjectStatus> {
        vec![
            wt("karpie", "karpie", None, 1, "master", true),
            wt("karpie%2F.claude%2Fworktrees%2Ffeat", "karpie/.claude/worktrees/feat",
               Some("karpie"), 0, "feature-x", true),
            wt("unrelated", "unrelated", None, 3, "main", true),
        ]
    }

    /// The reason this is not a reuse of `projects_strip`: an idle worktree
    /// is exactly what you switch to before starting work in it, and
    /// `projects_strip` filters `live == 0` out. Same input to both — the
    /// idle child must appear here and must not appear there.
    #[test]
    fn worktrees_lists_the_idle_family_member_that_projects_strip_hides() {
        let ps = karpie_family();
        let wt_html = worktrees_strip("karpie", &ps);
        assert!(wt_html.contains("feat"), "{wt_html}");
        assert!(wt_html.contains("⎇ feature-x"), "{wt_html}");
        let proj_html = projects_strip("karpie", &ps);
        assert!(!proj_html.contains("feature-x"), "projects_strip must still hide idle: {proj_html}");
    }

    #[test]
    fn worktrees_excludes_projects_outside_the_family() {
        let h = worktrees_strip("karpie", &karpie_family());
        assert!(!h.contains("unrelated"), "{h}");
    }

    #[test]
    fn worktrees_family_from_a_child_matches_family_from_the_root() {
        let ps = karpie_family();
        let from_root = worktrees_strip("karpie", &ps);
        let from_child = worktrees_strip("karpie%2F.claude%2Fworktrees%2Ffeat", &ps);
        // Same rows either way; only the `current` marking and the label move.
        assert!(from_child.contains("feat") && from_child.contains("karpie"), "{from_child}");
        assert!(from_root.contains("feat"), "{from_root}");
    }

    #[test]
    fn worktrees_marks_exactly_one_row_current() {
        let h = worktrees_strip("karpie", &karpie_family());
        assert_eq!(h.matches(" current\"").count(), 1, "{h}");
        // and it is the root's row, not the child's
        assert!(h.contains(r#"class="wt live current" href="/karpie""#), "{h}");
    }

    /// The ⌘/ctrl-click behaviour IS the absence of `target=` on a plain
    /// href — so the test pins href-present AND target-absent as a pair
    /// (absence alone would pass on an empty string).
    #[test]
    fn a_reachable_row_links_without_target_and_an_unreachable_row_not_at_all() {
        let mut ps = karpie_family();
        ps.push(wt("karpie%2Fgone", "karpie/gone", Some("karpie"), 0, "old", false));
        let h = worktrees_strip("karpie", &ps);
        assert!(h.contains(r#"href="/karpie/.claude/worktrees/feat""#), "{h}");
        assert!(!h.contains("target="), "no row may carry target=: {h}");
        // unreachable: a span with the tooltip, no href anywhere near it
        assert!(h.contains("unreachable"), "{h}");
        assert!(h.contains("worktree outside resh's roots"), "{h}");
        assert!(!h.contains(r#"href="/karpie/gone""#), "{h}");
    }

    #[test]
    fn wtlabel_is_empty_alone_and_names_the_current_worktree_in_company() {
        // One-member family: no label, no caret — the chip stays plain.
        let alone = vec![wt("solo", "solo", None, 1, "main", true)];
        let h = worktrees_strip("solo", &alone);
        assert!(h.contains(r#"<span id="wtlabel" hx-swap-oob="true"></span>"#), "{h}");
        // Root of a real family:
        let h = worktrees_strip("karpie", &karpie_family());
        assert!(h.contains("· main worktree ▾"), "{h}");
        // A child names itself by its last path segment:
        let h = worktrees_strip("karpie%2F.claude%2Fworktrees%2Ffeat", &karpie_family());
        assert!(h.contains("· feat ▾"), "{h}");
    }

    /// The fixture must contain real metacharacters or this asserts nothing
    /// (the vacuous-fixture trap is on record in CLAUDE.md).
    #[test]
    fn worktrees_escape_names_and_branches() {
        let ps = vec![
            wt("a<b", "a<b", None, 1, "main", true),
            wt("a<b%2Fwt", "a<b/wt", Some("a<b"), 0, "dev<&>", true),
        ];
        let h = worktrees_strip("a<b", &ps);
        assert!(h.contains("a&lt;b"), "{h}");
        assert!(h.contains("dev&lt;&amp;&gt;"), "{h}");
        assert!(!h.contains("dev<&>"), "{h}");
    }

    /// "The current key resolves to no entry" is the absent case stated as
    /// absent — empty label, no rows, an explanatory line. Never an error.
    #[test]
    fn an_unknown_current_key_yields_no_worktrees_not_an_error() {
        let h = worktrees_strip("nosuch", &karpie_family());
        assert!(h.contains(r#"<span id="wtlabel" hx-swap-oob="true"></span>"#), "{h}");
        assert!(h.contains("no worktrees"), "{h}");
        assert!(!h.contains("href="), "{h}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail to compile (no such function)**

Run: `cargo test worktrees_ 2>&1 | tail -20`
Expected: compile error — `cannot find function worktrees_strip`.

- [ ] **Step 3: Implement `worktrees_strip`**

Insert directly after `projects_strip`'s closing brace in `src/render.rs` (before `fn plural`):

```rust
/// The worktree switcher: the header chip's label (out-of-band, so one
/// fragment feeds two places) plus one row per member of the current
/// repository's worktree family.
///
/// Deliberately NOT a reuse of `projects_strip`, and not filtered to
/// `live > 0`: that strip answers "what is running anywhere", this one
/// answers "where can I go in *this* repo" — and an idle worktree is
/// exactly what you switch to before starting work in it.
///
/// Reachable rows carry `href` and no `target` on purpose: plain click
/// navigates this tab (workspace state is server-side, nothing is lost),
/// and ⌘/ctrl-click opens a new tab through the browser's own modifier
/// handling. The absence of `target` is load-bearing and pinned by test.
pub fn worktrees_strip(current_key: &str, projects: &[crate::registry::ProjectStatus]) -> String {
    // Family root: the current entry's parent when it is a worktree, else
    // itself. An unknown current key means the registry has no entry for
    // this project (not yet opened, or not a git repo) — that is "cannot
    // list", stated as an empty panel, never guessed around.
    let root_key: Option<&str> = projects
        .iter()
        .find(|p| p.key == current_key)
        .map(|p| p.parent.as_deref().unwrap_or(p.key.as_str()));
    let family: Vec<&crate::registry::ProjectStatus> = match root_key {
        Some(root) => projects
            .iter()
            .filter(|p| p.key == root || p.parent.as_deref() == Some(root))
            .collect(),
        None => Vec::new(),
    };
    // The label renders only when there is something to switch to — with a
    // single member the chip stays today's plain branch text, no caret.
    let label = if family.len() >= 2 {
        match family.iter().find(|p| p.key == current_key) {
            Some(p) if p.parent.is_none() => "· main worktree ▾".to_string(),
            Some(p) => {
                let name = p.url.rsplit('/').next().unwrap_or(&p.url);
                format!("· {} ▾", esc(name))
            }
            None => String::new(),
        }
    } else {
        String::new()
    };
    let mut out = format!(
        "<span id=\"wtlabel\" hx-swap-oob=\"true\">{label}</span><span class=\"wtstrip\">"
    );
    if family.is_empty() {
        out.push_str("<span class=\"wt-empty\">no worktrees</span>");
    }
    for p in &family {
        let marker = if p.live > 0 { "●" } else { "○" };
        // The root shows its full url (it names the repo); a child shows its
        // last segment, with the full path in the tooltip.
        let name = if p.parent.is_none() {
            p.url.as_str()
        } else {
            p.url.rsplit('/').next().unwrap_or(&p.url)
        };
        let branch = if p.branch.is_empty() {
            String::new()
        } else {
            format!(" <span class=\"branch\">⎇ {}</span>", esc(&p.branch))
        };
        let mut cls = String::from("wt");
        if p.live > 0 {
            cls.push_str(" live");
        }
        if p.key == current_key {
            cls.push_str(" current");
        }
        if !p.reachable {
            cls.push_str(" unreachable");
            out.push_str(&format!(
                "<span class=\"{cls}\" title=\"worktree outside resh's roots — cannot be opened\">{marker} {}{branch}</span>",
                esc(name)
            ));
            continue;
        }
        out.push_str(&format!(
            "<a class=\"{cls}\" href=\"/{}\" title=\"{}\">{marker} {}{branch}</a>",
            crate::http::percent_encode(&p.url),
            esc(&p.url),
            esc(name),
        ));
    }
    out.push_str("</span>");
    out
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test worktrees_ wtlabel_ a_reachable_row an_unknown_current 2>&1 | tail -20` — then the whole file's suite: `cargo test render 2>&1 | tail -5`
Expected: all PASS, nothing else broken.

- [ ] **Step 5: Commit**

```bash
git add src/render.rs
git commit -m "render: worktrees_strip — the switcher rows and the chip's OOB label"
```

---

### Task 2: Route `/frag/_worktrees`

**Files:**
- Modify: `src/routes.rs` (new match arm directly after the `["frag", "_projects"]` arm at `src/routes.rs:72-76`; test in `mod tests` near the other `frag_route` tests)

**Interfaces:**
- Consumes: `render::worktrees_strip(current_key, &ps)` from Task 1; `registry::known_projects(roots) -> Vec<ProjectStatus>` (`src/registry.rs:798`); the `frag_route` test helper (`src/routes.rs:799`).
- Produces: `GET /frag/_worktrees?current=<qkey>` returning `text/html` — Task 3's `#wtstrip` htmx element requests it.

- [ ] **Step 1: Write the failing test**

Add near the other `frag_route` tests in `src/routes.rs`:

```rust
    /// Dispatch through the real router, like every frag test here (the
    /// helper exists because direct-call tests once stayed green while the
    /// router could never reach the handler). Before the arm exists this
    /// request falls through to the catch-all and is treated as a project
    /// named "frag/_worktrees" — so the assertions below cannot pass early.
    #[test]
    fn the_worktrees_fragment_is_routed() {
        let d = tempfile::tempdir().unwrap();
        let roots = vec![d.path().to_path_buf()];
        let out = frag_route(&roots, "/frag/_worktrees?current=nosuch");
        assert!(out.contains("id=\"wtlabel\""), "{out}");
        assert!(out.contains("no worktrees"), "{out}");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test the_worktrees_fragment_is_routed 2>&1 | tail -10`
Expected: FAIL — the response is the catch-all's output (a workspace page or 404), containing no `wtlabel`.

- [ ] **Step 3: Add the arm**

Directly after the `["frag", "_projects"]` arm's closing brace in `route()`:

```rust
        // Same shape as _projects above, same reason it sits before the
        // general frag arm. Unlike _projects this does not filter to live
        // projects — see worktrees_strip's doc comment.
        ["frag", "_worktrees"] => {
            let current = req.query.get("current").map(String::as_str).unwrap_or("");
            let ps = registry::known_projects(roots);
            http::html(w, &render::worktrees_strip(current, &ps));
        }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test the_worktrees_fragment_is_routed 2>&1 | tail -5`, then `cargo test routes 2>&1 | tail -5`
Expected: PASS; no other routes test broken.

- [ ] **Step 5: Commit**

```bash
git add src/routes.rs
git commit -m "routes: serve /frag/_worktrees beside /frag/_projects"
```

---

### Task 3: Header rebuild — markup in `workspace_page`, styling in `style.css`

**Files:**
- Modify: `src/render.rs` (`workspace_page`, currently `src/render.rs:747-856`; new SVG consts above it; extend `workspace_page_wires_everything` at `src/render.rs:1478`)
- Modify: `static/style.css` (the `header` block near the top, `#projbtn`/`#bell` and `#projpanel` rules near line 415-450; new `#wtbtn` / `#wtpanel` / `#searchbox` rules)

**Interfaces:**
- Consumes: `/frag/_worktrees?current={qkey}` from Task 2 (`{qkey}` is the existing percent-encoded key variable already computed in `workspace_page`).
- Produces: DOM ids Task 4's JS binds to: `#wtbtn` (the chip, a `<button>`), `#wtpanel` (hidden popup `<div>` containing `#wtstrip`), `#settings` (inert placeholder), `#searchbox` (inert placeholder). Existing ids `#gitinfo`, `#projbtn`, `#projcount`, `#bell`, `#bellcount`, `#refresh`, `#closeproj`, `#projpanel`, `#noticepanel` keep their names and handlers.

- [ ] **Step 1: Extend the wiring test (failing first)**

In `workspace_page_wires_everything` (`src/render.rs:1478`), add:

```rust
        // The chrome redesign: chip + switcher panel + honest placeholders.
        assert!(h.contains("id=\"wtbtn\""), "{h}");
        assert!(h.contains("hx-get=\"/frag/_worktrees?current=proj\""), "{h}");
        assert!(h.contains("id=\"wtpanel\""), "{h}");
        assert!(h.contains("id=\"wtlabel\""), "{h}");
        assert!(h.contains("id=\"searchbox\""), "{h}");
        assert!(h.contains("id=\"settings\""), "{h}");
        // Placeholders say plainly that they are inert.
        assert!(h.contains("not implemented yet"), "{h}");
        // The emoji bell and the glyph buttons are gone from the header.
        assert!(!h.contains("🔔"), "{h}");
        assert!(!h.contains(">⟳<"), "{h}");
        assert!(!h.contains("✕ Close"), "{h}");
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test workspace_page_wires_everything 2>&1 | tail -10`
Expected: FAIL on `id="wtbtn"`.

- [ ] **Step 3: Add the SVG consts and rebuild the header markup**

Above `workspace_page` in `src/render.rs`:

```rust
// Header iconography: stroke SVGs on the app's 16px grid, `currentColor`
// throughout so the existing hover rules recolour them (the gear ships on
// Feather's 24 grid — MIT — and scales down; the spec pins "the
// conventional toothed cog, not a stylised stand-in"). None of these are
// interpolated into; anything dynamic stays in the surrounding markup and
// goes through esc/percent_encode as ever.
const SVG_HOME: &str = r#"<svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M8 1.5l6.5 6.5L8 14.5 1.5 8z"/></svg>"#;
const SVG_DIAMOND: &str = r#"<svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M8 1.5l6.5 6.5L8 14.5 1.5 8z"/></svg>"#;
const SVG_BRANCH: &str = r#"<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2" aria-hidden="true"><circle cx="5" cy="3.6" r="1.7"/><circle cx="5" cy="12.4" r="1.7"/><circle cx="11.4" cy="3.6" r="1.7"/><path d="M5 5.3v5.4M11.4 5.3v1.5a2.6 2.6 0 0 1-2.6 2.6H6.6"/></svg>"#;
const SVG_SEARCH: &str = r#"<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" aria-hidden="true"><circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5L14 14"/></svg>"#;
const SVG_BELL: &str = r#"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" aria-hidden="true"><path d="M4 11V7.5a4 4 0 0 1 8 0V11l1 1.5H3z"/><path d="M6.5 13.5a1.5 1.5 0 0 0 3 0"/></svg>"#;
const SVG_GEAR: &str = r#"<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>"#;
const SVG_REFRESH: &str = r#"<svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M13 8a5 5 0 1 1-1.5-3.6"/><path d="M13 2.5v3h-3"/></svg>"#;
const SVG_X: &str = r#"<svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8"/></svg>"#;
```

In `workspace_page`'s big `format!`, replace the `<header>…</header>` block and the `#projpanel` line with (everything else in the template stays byte-identical):

```html
<header>
  <a class="home" href="/" title="all projects">{SVG_HOME}</a><span class="proj">{proj_txt}</span>
  <button id="wtbtn" title="branch and worktrees">{SVG_BRANCH}<span id="gitinfo" hx-get="/frag/{proj_url}/status" hx-trigger="load, refresh from:body"></span><span id="wtlabel"></span></button>
  {warn}
  {sharing_indicator}
  <div id="searchbox" title="project-wide search — not implemented yet">{SVG_SEARCH}<span class="hintline">Search files, symbols, sessions</span><kbd>⇧ ⇧</kbd></div>
  <button id="projbtn" title="running projects">{SVG_DIAMOND}<span id="projcount"></span></button>
  <button id="bell" title="notifications (n)">{SVG_BELL}<span id="bellcount"></span></button>
  <button id="settings" title="settings — not implemented yet">{SVG_GEAR}</button>
  <button id="refresh" title="refresh (r)">{SVG_REFRESH}</button>
  <span class="vsep"></span>
  <button id="closeproj" title="close project — ends all its terminal sessions">{SVG_X}<span>Close</span></button>
</header>
<div id="projpanel" hidden><span id="projstrip" hx-get="/frag/_projects?current={qkey}" hx-trigger="load, refresh from:body, projects from:body"></span></div>
<div id="wtpanel" hidden><span id="wtstrip" hx-get="/frag/_worktrees?current={qkey}" hx-trigger="load, refresh from:body, projects from:body"></span></div>
```

Notes for the implementer:
- The right-side DOM order changes (Close moves to the end, after a divider) but every id keeps its meaning, so no JS handler moves.
- `#wtlabel` inside the button is the OOB target Task 1's fragment fills; it starts empty and CSS hides it while empty.
- The SVG consts contain no `{`/`}`, so they interpolate into the `format!` safely as named captures.

- [ ] **Step 4: Restyle in `static/style.css`**

Replace the current `header .home` rule and extend the header block (keep the existing `header` background/comment; change only what is listed):

```css
header { display: flex; align-items: center; gap: 10px; height: 38px; padding: 0 10px; background: var(--window); flex: none; }
header .home { display: flex; align-items: center; color: var(--accent); }
header button svg { display: block; }
/* The branch/worktree chip. #gitinfo keeps its id (htmx swaps into it) but
   sits on the chip now, so its muted colour lifts to --fg here; the change
   count keeps the accent via the existing #badge rule. */
#wtbtn { display: flex; align-items: center; gap: 6px; height: 24px; padding: 0 8px 0 7px;
         border-radius: 6px; border: 1px solid var(--border); background: var(--tool);
         color: var(--fg); cursor: pointer; font: inherit; }
#wtbtn:hover { border-color: var(--accent); }
#wtbtn svg { color: var(--muted); }
#wtbtn #gitinfo { color: var(--fg); }
#wtlabel { color: var(--muted); }
#wtlabel:empty { display: none; }
/* Honest placeholder: looks like the control it will become, and the help
   cursor + tooltip say it is not one yet. A div, not an input — nothing to
   focus, nothing to submit. */
#searchbox { display: flex; align-items: center; gap: 8px; width: 400px; height: 26px;
             margin-left: auto; margin-right: auto; padding: 0 8px 0 9px; border-radius: 6px;
             border: 1px solid var(--border); background: var(--tool); color: var(--muted);
             cursor: help; }
#searchbox .hintline { flex: 1 1 auto; overflow: hidden; white-space: nowrap; }
#searchbox kbd { border: 1px solid var(--border); border-radius: 3px; padding: 0 4px;
                 font: 11px/16px var(--mono); }
header .vsep { width: 1px; height: 16px; background: var(--border); margin: 0 6px; }
#closeproj { display: flex; align-items: center; gap: 5px; border: 1px solid var(--border);
             border-radius: 6px; background: none; color: var(--muted); padding: 3px 10px;
             cursor: pointer; font: inherit; }
#closeproj:hover { color: var(--fg); }
/* The switcher panel: the third header popup, same pattern as #projpanel and
   #noticepanel — but left-anchored, under the chip that opens it. */
#wtpanel { position: absolute; top: var(--header-h, 40px); left: 8px; z-index: 20;
           min-width: 220px; max-width: 380px; max-height: 60vh; overflow-y: auto;
           background: var(--bg2); border: 1px solid var(--border);
           border-radius: 4px; padding: 4px; }
.wtstrip { display: flex; flex-direction: column; gap: 2px; }
.wtstrip .wt { display: block; padding: 4px 8px; border-radius: 3px; text-decoration: none;
               opacity: .6; white-space: nowrap; color: var(--fg); }
.wtstrip .wt:hover { background: var(--bg); }
.wtstrip .wt.live { opacity: 1; }
.wtstrip .wt.current { font-weight: 600; text-decoration: underline; }
.wtstrip .wt.unreachable { opacity: .35; cursor: default; }
.wtstrip .branch { color: var(--muted); font-size: 12px; }
.wtstrip .wt-empty { color: var(--muted); display: block; padding: 6px 8px; font-size: 12px; }
```

Also in `style.css`:
- Add `#settings` to the existing `#projbtn, #bell { … }` and `#projbtn:hover, #bell:hover { … }` selector lists (near line 431), so the placeholder gets the same quiet-button treatment. Then change its `cursor` for `#settings` alone: `#settings { cursor: help; }`.
- Delete the standalone `#refresh` rule near the top (line 42-43) or fold it into the same shared list — one rule for the four icon buttons, not two copies.
- The comment above `#projbtn { margin-left: auto; }` (line ~430) explains that `margin-left:auto` positions the right group; `#searchbox`'s double-auto margin now does the centering AND the pushing, so change `#projbtn`'s rule to drop `margin-left: auto` and update that comment to say the search placeholder is what separates left from right now.

- [ ] **Step 5: Run the wiring test and the full suite**

Run: `cargo test workspace_page 2>&1 | tail -10`, then `cargo test 2>&1 | tail -5`
Expected: all PASS. (Other `workspace_page` tests assert on `data-*` attributes and theme links, which this task does not touch.)

- [ ] **Step 6: Look at it in a real browser**

Run the dev binary (`cargo run` with this repo as a root, or the existing dev instance after rebuild) and open a project. Check against the canvas (Option A): chip renders with branch, search placeholder centered, right group ordered ◆ · bell · gear · refresh · │ · Close, popups still open, `⚠ config`/`⧉ sharing` unchanged when applicable. CLAUDE.md: a green suite does not cover this file's visual output.

- [ ] **Step 7: Commit**

```bash
git add src/render.rs static/style.css
git commit -m "header: identity chip, search placeholder, SVG actions, worktree panel markup"
```

---

### Task 4: Client wiring — `#wtpanel` toggle, SVG pane icons, the Claude mark

**Files:**
- Modify: `static/app.js` (`buildPaneIcons` ~line 607; the `.newclaude` block ~line 515-538; the header-popup wiring block ~line 1946-1980)
- Modify: `tests/browser/dotfiles.mjs` (five glyph assertions — see Step 4)

**Interfaces:**
- Consumes: `#wtbtn`/`#wtpanel` ids from Task 3; the existing `projBtn`/`projPanel` block as the pattern to copy.
- Produces: `PANE_ICONS` and `CLAUDE_MARK` string consts; `icon(svg, title, fn)` in `buildPaneIcons` now takes SVG markup. Browser tests keep selecting by `title` and by class — titles do not change.

- [ ] **Step 1: Add the icon constants and switch `icon()` to SVG**

Near the top of `static/app.js` (beside other module-level consts):

```js
// Pane-header and launcher icon set. Constant markup ONLY — nothing here is
// ever interpolated, which is what makes innerHTML safe; anything dynamic
// stays in text nodes and dataset attributes as everywhere else.
const PANE_ICONS = {
  dotsOff: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"><circle cx="8" cy="8" r="5" stroke-dasharray="2.2 2.2"/></svg>',
  dotsOn: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"><circle cx="8" cy="8" r="5" stroke-dasharray="2.2 2.2"/><circle cx="8" cy="8" r="2" fill="currentColor" stroke="none"/></svg>',
  collapse: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M4 10l4-4 4 4"/></svg>',
  move: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 5.5h10l-2.5-2.5M13.5 10.5h-10l2.5 2.5"/></svg>',
  maximize: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M9 2.5h4.5V7M7 11.5H2.5V7M13.5 2.5L9 7M2.5 13.5L7 9"/></svg>',
  restore: '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M13.5 7H9V2.5M2.5 9H7v4.5M9 7l4.5-4.5M7 9l-4.5 4.5"/></svg>',
};
// The official Claude mark (lobehub packaging of Anthropic's starburst,
// fetched 2026-08-23 from lobehub/lobe-icons static-svg/icons/claude-color.svg),
// brand-filled in every theme on purpose: the point of the real mark is that
// it is recognisable, so it does not take currentColor.
const CLAUDE_MARK = '<svg width="13" height="13" viewBox="0 0 24 24" fill="#D97757"><path d="M4.709 15.955l4.72-2.647.08-.23-.08-.128H9.2l-.79-.048-2.698-.073-2.339-.097-2.266-.122-.571-.121L0 11.784l.055-.352.48-.321.686.06 1.52.103 2.278.158 1.652.097 2.449.255h.389l.055-.157-.134-.098-.103-.097-2.358-1.596-2.552-1.688-1.336-.972-.724-.491-.364-.462-.158-1.008.656-.722.881.06.225.061.893.686 1.908 1.476 2.491 1.833.365.304.145-.103.019-.073-.164-.274-1.355-2.446-1.446-2.49-.644-1.032-.17-.619a2.97 2.97 0 01-.104-.729L6.283.134 6.696 0l.996.134.42.364.62 1.414 1.002 2.229 1.555 3.03.456.898.243.832.091.255h.158V9.01l.128-1.706.237-2.095.23-2.695.08-.76.376-.91.747-.492.584.28.48.685-.067.444-.286 1.851-.559 2.903-.364 1.942h.212l.243-.242.985-1.306 1.652-2.064.73-.82.85-.904.547-.431h1.033l.76 1.129-.34 1.166-1.064 1.347-.881 1.142-1.264 1.7-.79 1.36.073.11.188-.02 2.856-.606 1.543-.28 1.841-.315.833.388.091.395-.328.807-1.969.486-2.309.462-3.439.813-.042.03.049.061 1.549.146.662.036h1.622l3.02.225.79.522.474.638-.079.485-1.215.62-1.64-.389-3.829-.91-1.312-.329h-.182v.11l1.093 1.068 2.006 1.81 2.509 2.33.127.578-.322.455-.34-.049-2.205-1.657-.851-.747-1.926-1.62h-.128v.17l.444.649 2.345 3.521.122 1.08-.17.353-.608.213-.668-.122-1.374-1.925-1.415-2.167-1.143-1.943-.14.08-.674 7.254-.316.37-.729.28-.607-.461-.322-.747.322-1.476.389-1.924.315-1.53.286-1.9.17-.632-.012-.042-.14.018-1.434 1.967-2.18 2.945-1.726 1.845-.414.164-.717-.37.067-.662.401-.589 2.388-3.036 1.44-1.882.93-1.086-.006-.158h-.055L4.132 18.56l-1.13.146-.487-.456.061-.746.231-.243 1.908-1.312-.006.006z"/></svg>';
```

In `buildPaneIcons` (~line 607), change the helper and its call sites:

```js
  const icon = (svg, title, fn) => {
    const b = document.createElement("span");
    b.className = "paneicon";
    b.title = title;
    b.innerHTML = svg; // constant markup from PANE_ICONS only
    b.onclick = fn;
    host.appendChild(b);
    return b;
  };
  if (active && active.k === "Tree") {
    const hidden = showHidden();
    icon(hidden ? PANE_ICONS.dotsOn : PANE_ICONS.dotsOff,
         hidden ? "hide dotfiles" : "show dotfiles", () => {
      send({ t: "SetShowHidden", on: !showHidden() });
    });
    icon(PANE_ICONS.collapse, "collapse all", () => {
      content.querySelectorAll("details[open]").forEach((d) => { d.open = false; });
    });
  }
  // …the move icon: replace "⇄" with PANE_ICONS.move (same title text)…
  // …the maximize icon: replace on ? "⤡" : "⤢" with
  //    on ? PANE_ICONS.restore : PANE_ICONS.maximize (same titles)…
```

In the `.newclaude` block (~line 528), replace `star.textContent = "✻";` with `star.innerHTML = CLAUDE_MARK;` and replace the ✻-as-text comment above it with:

```js
    // The official Claude mark (see CLAUDE_MARK above), replacing the ✻ text
    // glyph this button used to carry. Same button as +, same behaviour: the
    // server allocates the name and types `claude` into the shell it spawns.
```

Add to `static/style.css` so the two strip buttons center their content:

```css
.tabstrip .newclaude svg { display: block; }
.paneicon svg { display: block; }
```

- [ ] **Step 2: Wire the `#wtpanel` toggle**

Directly after the `projBtn`/`projPanel` block (~line 1972), copying its pattern:

```js
// The worktree switcher popup: third of the header popups, same pattern as
// projpanel above and the bell below. Rows are plain anchors — plain click
// navigates this tab, ⌘/ctrl-click is the browser's own new-tab, no JS here.
const wtBtn = document.getElementById("wtbtn");
const wtPanel = document.getElementById("wtpanel");
if (wtBtn && wtPanel) {
  wtBtn.onclick = () => {
    wtPanel.hidden = !wtPanel.hidden;
    if (!wtPanel.hidden && window.htmx) htmx.trigger(document.body, "refresh");
  };
  // A plain click through to a worktree navigates away anyway; this is for
  // the ⌘-click case, which stays on this page with the panel open.
  wtPanel.onclick = (e) => { if (e.target.closest("a")) wtPanel.hidden = true; };
}
```

- [ ] **Step 3: Quick sanity in a real browser**

Rebuild, open a project: chip click toggles the panel; pane icons draw; the Claude button shows the coral mark and still opens a Claude terminal. Check all five themes' contrast with the mark (spec's flagged claim) by switching `theme =` in a scratch config or the theme query — worst case is solarized-dark.

- [ ] **Step 4: Update `tests/browser/dotfiles.mjs` to the SVG state marker**

The five assertions comparing `b.textContent` to `◌`/`◍` (lines ~86, 98, 105, 114, 132) can no longer see a glyph. Keep the explicit state assertion (the README's "strip tests with no glyph assertion" trap) by counting the SVG's circles — 1 while hidden entries are off, 2 when on. Change the probe (~line 51) to:

```js
      return JSON.stringify(b ? { circles: b.querySelectorAll("circle").length, title: b.title } : null); })()`,
```

and each glyph assertion accordingly, e.g.:

```js
    ok(t?.circles === 1, `it draws the hollow ring while hidden (got ${t?.circles} circles)`);
    // …later, after the toggle:
    ok(t?.circles === 2, `the marker fills in (got ${t?.circles} circles)`);
```

(Same discriminating property as before: swapping the two icon constants must fail these.)

- [ ] **Step 5: Run the affected browser suites**

Run:
```bash
deno run -A tests/browser/dotfiles.mjs
deno run -A tests/browser/paneicons.mjs
deno run -A tests/browser/claudeterm.mjs
```
Expected: all pass (`paneicons.mjs` and `claudeterm.mjs` select by title/class, which did not change).

- [ ] **Step 6: Watch them fail for the right reason**

Swap `PANE_ICONS.dotsOn` and `dotsOff` at their call site → `dotfiles.mjs` must fail its circle-count assertions; restore. Replace `CLAUDE_MARK` assignment with `star.textContent = ""` → `claudeterm.mjs` must still pass its class/title assertions (they never asserted the glyph) — note in the commit message that the mark itself is covered by Task 5's new test, not this suite. Restore.

- [ ] **Step 7: Commit**

```bash
git add static/app.js static/style.css tests/browser/dotfiles.mjs
git commit -m "app: worktree panel toggle, SVG pane icons, the Claude mark on .newclaude"
```

---

### Task 5: Browser test for the switcher, and the full verification pass

**Files:**
- Create: `tests/browser/worktrees.mjs`
- Modify: `tests/browser/README.md` (add the run line and the revert-check notes)

**Interfaces:**
- Consumes: the harness (`fixture, freePort, openPage, profileDir, startBrowser, startResh, until` from `tests/browser/harness.mjs` — same import line as `paneicons.mjs`); ids `#wtbtn`, `#wtpanel`, `#wtlabel`, rows `.wtstrip .wt` from Tasks 1-4.
- Produces: `deno run -A tests/browser/worktrees.mjs`.

- [ ] **Step 1: Write the test**

```js
//! The header's worktree switcher: the chip grows a label only when the repo
//! has somewhere to switch to, the panel lists idle worktrees, and a row
//! click navigates THIS tab (no target= — ⌘-click new-tab is the browser's).
//!
//! Run: deno run -A tests/browser/worktrees.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const git = async (cwd, ...args) => {
  const p = new Deno.Command("git", { args, cwd, stdout: "null", stderr: "piped" }).spawn();
  const st = await p.status;
  if (!st.success) throw new Error(`git ${args.join(" ")} failed in ${cwd}`);
};

const fx = await fixture();
const proj = `${fx.roots}/proj`;
await Deno.writeTextFile(`${proj}/README.md`, "hello\n");
await git(proj, "init", "-b", "master");
await git(proj, "-c", "user.email=t@t", "-c", "user.name=t", "add", "README.md");
await git(proj, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-m", "init");
// The path Claude Code itself uses for worktrees, which projects.rs vouches for.
await git(proj, "worktree", "add", "-b", "feature-x", ".claude/worktrees/feat");

const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/proj`);
  const { evalIn } = page;
  await until(() => evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "app");

  console.log("A. the chip knows it has company");
  ok(await until(() => evalIn(`document.getElementById("wtlabel").textContent.includes("main worktree")`),
     15, "wtlabel filled"), "the chip label names the main worktree");

  console.log("\nB. the panel lists the idle worktree");
  await evalIn(`document.getElementById("wtbtn").click()`);
  ok(await until(() => evalIn(`!document.getElementById("wtpanel").hidden`), 10, "panel open"),
     "clicking the chip opens the panel");
  ok(await until(() => evalIn(
      `[...document.querySelectorAll("#wtpanel .wt")].some((a) => a.textContent.includes("feat"))`),
     10, "worktree row"), "the idle worktree is listed");
  ok(await evalIn(
      `[...document.querySelectorAll("#wtpanel .wt")].some((a) => a.textContent.includes("feature-x"))`),
     "with its branch");
  // The ⌘-click behaviour IS the missing target= — pin the pair.
  ok(await evalIn(
      `[...document.querySelectorAll("#wtpanel a.wt")].every((a) => a.getAttribute("href") && !a.hasAttribute("target"))`),
     "rows have href and no target");
  ok(await evalIn(
      `document.querySelector("#wtpanel .wt.current").textContent.includes("proj")`),
     "the current row is the main worktree");

  console.log("\nC. a plain click switches this tab");
  await evalIn(`[...document.querySelectorAll("#wtpanel a.wt")].find((a) => a.textContent.includes("feat")).click()`);
  ok(await until(() => evalIn(`location.pathname.endsWith("/worktrees/feat")`), 15, "navigated"),
     "the same tab now shows the worktree's workspace");

  console.log("\nD. the launcher carries the Claude mark");
  await until(() => evalIn("typeof terms !== 'undefined' && !!state"), 30, "app again");
  ok(await until(() => evalIn(
      `!!document.querySelector('.tabstrip .newclaude svg path[fill], .tabstrip .newclaude svg[fill="#D97757"]')
       || [...document.querySelectorAll(".tabstrip .newclaude svg")].length > 0`),
     10, "mark"), "the ✻ button is an SVG mark now");
} finally {
  try { await page?.close(); } catch {}
  await browser.stop();
  await resh.stop();
  await fx.cleanup();
}
Deno.exit(fail ? 1 : 0);
```

Note: section D runs after a navigation; if the harness's `evalIn` binds to the original document only, re-open the page with `openPage(browser.port, …/proj)` before D rather than reusing the handle — `reconnect.mjs` shows the harness's post-navigation pattern if one exists. The assertions themselves stay as written.

- [ ] **Step 2: Run it**

Run: `deno run -A tests/browser/worktrees.mjs`
Expected: all `ok`. If Chromium is absent the run skips — then this task is **not verifiable on this machine**; say so rather than reporting it green.

- [ ] **Step 3: Watch it fail for the right reasons**

Three reverts, one at a time, re-running the test after each and restoring before the next:
1. Comment out the `["frag", "_worktrees"]` arm in `routes.rs` → sections A and B must fail (label never fills, no rows).
2. Comment out the `wtBtn.onclick` handler in `app.js` → section B's "panel opens" must fail while A still passes.
3. Add `target="dl-x"` to the row anchor in `worktrees_strip` → the href/target pair assertion must fail.
Record the three counts in the test's header comment (the README's convention).

- [ ] **Step 4: Full suites**

```bash
cargo test 2>&1 | tail -5
for t in tests/browser/*.mjs; do deno run -A "$t" || echo "FAILED: $t"; done
```
Expected: everything green; time the cargo run too (a deadlock hangs rather than fails).

- [ ] **Step 5: Update `tests/browser/README.md`**

Add to the run list:
```
deno run -A tests/browser/worktrees.mjs # the header's worktree switcher chip + panel
```
and, to the revert-check bullet list, the three counts recorded in Step 3.

- [ ] **Step 6: Commit**

```bash
git add tests/browser/worktrees.mjs tests/browser/README.md
git commit -m "browser test: the worktree switcher — label, rows, in-place navigation"
```

---

## Final acceptance (after all tasks)

- [ ] `cargo test` green on this Linux host (it is the deploy OS — no macOS substitution in play).
- [ ] Every browser suite green, including the three updated/new ones.
- [ ] A by-eye pass against the canvas (Option A, "Main view" page): header composition, chip, placeholders' tooltips, popup positions, the Claude mark's contrast on all five themes.
- [ ] `git log`/`git status` match what was reported; nothing outside the five named files plus the two test files changed.
- [ ] If deploying: follow `docs/deploy.md` and confirm the *running* binary changed.
