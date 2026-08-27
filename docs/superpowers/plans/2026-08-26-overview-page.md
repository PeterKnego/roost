# Overview Page Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the front page (a filesystem picker) with a two-pane overview — projects/worktrees on the left, live terminal/Claude sessions on the right, filtered by selection — with the picker still reachable behind `?at=`.

**Architecture:** `/` with no query renders `render::overview_page` (a shell); its two panes are htmx fragments (`/frag/_overview_projects`, `/frag/_overview_sessions`) polled on an interval, selection carried in `?sel=`, expansion client-side. Left reuses `registry::known_projects_with_state` (the shipped switcher data); right joins `session` live rows to a single `ps` ages snapshot and marks Claude on positive evidence only. Clicking a session opens `/<project>?focus=<session>`, which `app.js` consumes once via the existing `focusSession`.

**Tech Stack:** Rust (std, serde), server-rendered HTML in `render.rs`, htmx, plain JS (no framework), Deno + headless Chromium browser test.

**Spec:** `docs/superpowers/specs/2026-08-26-overview-page-design.md`

## Global Constraints

- `cargo test`, never `cargo test --release`. Build from this one checkout (shared target dir — `CLAUDE.md`).
- Every new test revert-checked: apply the broken version, run, read the failure, restore, and note it in the test's comment. Browser assertions revert-checked per `tests/browser/README.md`'s four traps, recorded in the file header.
- All HTML built in `render.rs`; every interpolated value escaped with `esc`.
- Never hold a session/registry lock across blocking I/O (the `ps` fork): `session::list_sessions`'s doc comment is the pattern — scan under the lock, drop it, then fork.
- Three-valued honesty: the Claude mark is shown only on *positive* evidence (launched-as-Claude this process, or IDE-connected); absence of evidence renders `○ shell`, never a guessed `●`.
- Route: no new reserved top-level path. `/` (no `at` key) → overview; `?at=` present (empty included) → picker. `req.query.get("at")` is `None` vs `Some("")`.
- Session names validated with `session::valid_name` before use in a URL or lookup.
- Commit with explicit paths (`git add <files>`), never `git add -A` — the tree has the user's uncommitted `build.rs` edit.
- Reuse, don't duplicate: the worktree state chips (✻/dirty/N▲) are factored into one helper shared by `worktrees_strip` and the left fragment.

---

## File map

| File | Change |
|---|---|
| `src/routes.rs` | `serve_index` branches on `at`; two new frag arms `_overview_projects` / `_overview_sessions` |
| `src/render.rs` | `overview_page()`; `overview_projects()`; `overview_sessions()`; `worktree_chips()` helper factored from `worktrees_strip` |
| `src/session.rs` | `live_rows(project)` (lock-only scan, no `ps`); `ages_snapshot()` + injectable `parse_ages()` |
| `static/overview.js` | new — expand/select, re-apply expansion after htmx swap |
| `static/app.js` | `?focus=<session>` consumed once after first `State`, via existing `focusSession` |
| `static/style.css` | two-pane grid, tree rows, session rows |
| `static/picker.js` | verify only — its `?at=` navigation is unchanged |
| `tests/browser/overview.mjs` | new browser test |
| `tests/browser/README.md` | one-line entry |

---

### Task 1: Route split + overview page shell
> **Integration fix (found by Task 6's browser test):** `overview_page` must take `sel: &str` and bake it into BOTH fragment hx-get URLs as `?sel={percent_encode(sel)}`, and `serve_index` must read `sel` from the query and pass it — otherwise `/?sel=key` renders identically to `/` and selection never filters. This is the sel-threading wiring; see Task 8 in the SDD ledger.


**Files:**
- Modify: `src/routes.rs:167-175` (`serve_index`)
- Modify: `src/render.rs` (add `overview_page`; `index_page` unchanged)
- Test: in `src/routes.rs` and `src/render.rs` test modules

**Interfaces:**
- Produces: `pub fn render::overview_page(roots_label: &str) -> String` — the two-pane shell; panes are empty htmx containers that load `/frag/_overview_projects` and `/frag/_overview_sessions`. `serve_index` renders it when `at` is absent.
- Consumes: nothing new.

- [ ] **Step 1: Write the failing tests**

`src/routes.rs` tests (near the existing `the_worktrees_fragment_is_routed`):

```rust
    #[test]
    fn root_with_no_at_serves_the_overview_and_at_serves_the_picker() {
        // Revert-checked: with serve_index always calling index_page, the
        // first assertion fails (no overview markup at `/`).
        let d = tempfile::tempdir().unwrap();
        let roots = vec![d.path().to_path_buf()];
        let overview = route_to_string(&roots, "/");
        assert!(overview.contains("id=\"overview\""), "no-at is the overview: {overview}");
        assert!(overview.contains("_overview_projects") && overview.contains("_overview_sessions"), "{overview}");
        let picker = route_to_string(&roots, "/?at=");
        assert!(picker.contains("id=\"picker\""), "?at= is the picker: {picker}");
    }
```

If a `route_to_string`/`frag_route`-style helper does not already exist for driving `route` in tests, use the same harness the `_worktrees` routing test uses; match its exact name (`grep -n "the_worktrees_fragment_is_routed" -A4 src/routes.rs`).

`src/render.rs` test:

```rust
    #[test]
    fn overview_page_wires_both_fragment_panes() {
        let h = overview_page("/home/claude/projects");
        assert!(h.contains("id=\"overview\""));
        assert!(h.contains("hx-get=\"/frag/_overview_projects\""), "{h}");
        assert!(h.contains("hx-get=\"/frag/_overview_sessions\""), "{h}");
        assert!(h.contains("/static/overview.js"), "{h}");
        // The picker entry point, not a new reserved path.
        assert!(h.contains("href=\"/?at=\""), "open-a-directory reaches the picker: {h}");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test routes::root_with_no_at render::overview_page_wires 2>&1 | grep -E "^error|test result" | head`
Expected: `overview_page` not found.

- [ ] **Step 3: Implement**

`src/render.rs` — add beside `index_page`:

```rust
/// The front page: a two-pane overview. Both panes are htmx fragments that
/// load on open and poll (see `overview.js` / the fragment routes); this
/// shell only lays them out. The picker still lives on `/`, reached by the
/// `?at=` query and the "Open a directory" button here — no new reserved
/// path, which would collide with a project of that name the way `static`
/// and `frag` already can.
pub fn overview_page(roots_label: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>resh</title>\
         <link rel=\"stylesheet\" href=\"/static/themes/darcula.css\">\
         <link rel=\"stylesheet\" href=\"/static/style.css\">\
         <script src=\"/static/vendor/htmx.min.js\"></script>\
         </head><body class=\"overview-body\">\
         <header><span class=\"proj\">resh</span>\
           <span class=\"roots\" title=\"{roots}\"></span>\
           <a class=\"openbtn\" href=\"/?at=\">＋ Open a directory</a>\
         </header>\
         <main id=\"overview\">\
           <section id=\"ovprojects\" hx-get=\"/frag/_overview_projects\" hx-trigger=\"load, every 5s\"></section>\
           <section id=\"ovsessions\" hx-get=\"/frag/_overview_sessions\" hx-trigger=\"load, every 5s\"></section>\
         </main>\
         <script src=\"/static/overview.js\"></script>\
         </body></html>",
        roots = esc(roots_label),
    )
}
```

The htmx path is `/static/vendor/htmx.min.js` (the workspace page uses it, `render.rs:1036`); it is already embedded via `static/vendor/`. Do not add a new asset.

`src/routes.rs` `serve_index`:

```rust
fn serve_index(w: &mut impl Write, req: &http::Request, roots: &[PathBuf]) {
    // `at` absent → the overview; `at` present (empty included) → the picker.
    if req.query.get("at").is_none() {
        let label = roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(":");
        return http::html(w, &render::overview_page(&label));
    }
    let requested = req.query.get("at").map(String::as_str).unwrap_or("");
    let (at, entries, refused) = match projects::list_dir(roots, requested) {
        Some(entries) => (requested, entries, false),
        None => ("", projects::list_dir(roots, "").expect("top level never fails to resolve"), true),
    };
    let ps = registry::known_projects(roots);
    http::html(w, &render::index_page(at, &entries, refused, &ps));
}
```

- [ ] **Step 4: Run** — `cargo test routes:: render::overview 2>&1 | grep "test result"` → ok.

- [ ] **Step 5: Revert-check** — make `serve_index` always call `index_page` (drop the `at.is_none()` branch); `root_with_no_at…` must fail on the overview assertion; restore.

- [ ] **Step 6: Commit**

```bash
git add src/routes.rs src/render.rs
git commit -m "overview: front page shell on / (no query); picker moves behind ?at="
```

---

### Task 2: Left fragment — the project/worktree tree

**Files:**
- Modify: `src/render.rs` (factor `worktree_chips`; add `overview_projects`), `src/render.rs:875-910` (`worktrees_strip` uses the helper)
- Modify: `src/routes.rs` (arm `["frag","_overview_projects"]`)

**Interfaces:**
- Produces: `pub fn render::overview_projects(sel: &str, projects: &[crate::registry::ProjectStatus]) -> String`; `fn worktree_chips(w: &crate::registry::WorktreeStatus, key: &str, live: usize) -> String` (the ✻/dirty/N▲[/✕] run, factored from `worktrees_strip`).
- Consumes: `registry::known_projects_with_state` (Task from the shipped feature).

- [ ] **Step 1: Write the failing tests** (`src/render.rs` tests)

```rust
    fn ps_row(key: &str, url: &str, live: usize, branch: &str, parent: Option<&str>, wt: Option<crate::registry::WorktreeStatus>) -> crate::registry::ProjectStatus {
        crate::registry::ProjectStatus { key: key.into(), url: url.into(), live, oldest_age_secs: None, has_layout: true, branch: branch.into(), parent: parent.map(str::to_string), reachable: true, wt }
    }

    #[test]
    fn overview_projects_nests_worktrees_under_their_parent_and_marks_selection() {
        use crate::claudes::ClaudeEvidence;
        let wt = crate::registry::WorktreeStatus { claude: ClaudeEvidence::Present(vec!["term".into()]), dirty: Some(true), ahead: Some(3), base: "main".into(), base_recorded: true };
        let ps = vec![
            ps_row("ultima", "ultima", 1, "main", None, None),
            ps_row("ultima%2F.claude%2Fworktrees%2Fclaude-1", "ultima/.claude/worktrees/claude-1", 1, "claude-1", Some("ultima"), Some(wt)),
        ];
        let out = overview_projects("ultima", &ps);
        // Parent row carries an expansion caret and is current; child row is present with its chips.
        assert!(out.contains("ovcaret") && out.contains("current"), "{out}");
        assert!(out.contains("data-key=\"ultima\""), "{out}");
        assert!(out.contains("data-parent=\"ultima\"") && out.contains("claude-1"), "child under parent: {out}");
        assert!(out.contains("✻") && out.contains("dirty") && out.contains("3 ahead"), "chips reused: {out}");
    }

    #[test]
    fn overview_projects_renders_an_unreachable_worktree_as_inert_text() {
        // Revert-checked: rendering it as an <a> fails the `!contains("<a")` on that row.
        let ps = vec![
            ps_row("repo", "repo", 0, "main", None, None),
            {
                let mut r = ps_row("x", "/outside/wt", 0, "feat", Some("repo"), Some(crate::registry::WorktreeStatus { claude: crate::claudes::ClaudeEvidence::Absent, dirty: None, ahead: None, base: "main".into(), base_recorded: false }));
                r.reachable = false; r
            },
        ];
        let out = overview_projects("", &ps);
        assert!(out.contains("unreachable"), "{out}");
    }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test render::overview_projects 2>&1 | grep -E "^error" | head`.

- [ ] **Step 3: Implement**

Factor the chip block out of `worktrees_strip` (currently `src/render.rs:880-910`) into:

```rust
/// The per-worktree state chips (Claude / dirty / ahead) and, when every
/// axis is positively clean, the remove control. Shared by the header
/// switcher and the overview's left pane so the two never drift. `None`
/// on any axis renders `?`, never "clean" — see the switcher's own comment.
fn worktree_chips(w: &crate::registry::WorktreeStatus, key: &str, live: usize) -> String {
    let claude = match &w.claude {
        crate::claudes::ClaudeEvidence::Present(_) => "<span class=\"wtf on\" title=\"a Claude is running here\">✻</span>".to_string(),
        crate::claudes::ClaudeEvidence::Absent => "<span class=\"wtf\" title=\"no Claude here\">—</span>".to_string(),
        crate::claudes::ClaudeEvidence::Unknown => "<span class=\"wtf\" title=\"IDE integration is off, so resh cannot tell\">?</span>".to_string(),
    };
    let dirty = match w.dirty {
        Some(true) => "<span class=\"wtf on\">dirty</span>".to_string(),
        Some(false) => "<span class=\"wtf\">clean</span>".to_string(),
        None => "<span class=\"wtf\" title=\"git did not answer (status)\">?</span>".to_string(),
    };
    let against = if w.base_recorded {
        format!("measured against {}, recorded when resh created this worktree", esc(&w.base))
    } else {
        format!("measured against {}, the main worktree's branch — resh did not create this worktree", esc(&w.base))
    };
    let ahead = match w.ahead {
        Some(n) => format!("<span class=\"wtf{}\" title=\"{against}. A squash-merged branch stays ahead forever; remove it by hand.\">{n} ahead</span>", if n > 0 { " on" } else { "" }),
        None => "<span class=\"wtf\" title=\"git did not answer (rev-list), or no base is known\">?</span>".to_string(),
    };
    let remove = if crate::registry::removable(w, live) {
        format!(" <button class=\"wtremove\" data-key=\"{}\" title=\"remove this worktree and its branch\">✕</button>", esc(key))
    } else {
        String::new()
    };
    format!("{claude} {dirty} {ahead}{remove}")
}
```

Rewrite `worktrees_strip`'s inlined block to call `worktree_chips(w, &p.key, p.live)` wrapped in its existing ` · …` separator, so its output is unchanged (its tests must still pass byte-for-byte behaviour).

Add `overview_projects`:

```rust
/// The overview's left pane: known projects, each expandable to its worktree
/// family. Rows are pre-ordered by `known_projects_with_state` (parent then
/// its children), so this renders in order and lets `parent` decide nesting.
/// A parent with children gets a caret; a worktree row carries `worktree_chips`.
/// Selection (`sel`, a storage key) marks the current row; expansion is the
/// client's job (`overview.js`). A reachable row is a link to `/<url>` (open
/// the project); an unreachable worktree is inert text.
pub fn overview_projects(sel: &str, projects: &[crate::registry::ProjectStatus]) -> String {
    let has_children: std::collections::HashSet<&str> =
        projects.iter().filter_map(|p| p.parent.as_deref()).collect();
    let mut out = String::from("<ul class=\"ovtree\">");
    for p in projects {
        let is_child = p.parent.is_some();
        let mut cls = String::from("ovrow");
        if is_child { cls.push_str(" child"); }
        if p.live > 0 { cls.push_str(" live"); }
        if p.key == sel { cls.push_str(" current"); }
        let marker = if p.live > 0 { "●" } else { "○" };
        let caret = if !is_child && has_children.contains(p.key.as_str()) {
            "<span class=\"ovcaret\" aria-hidden=\"true\">▸</span>"
        } else {
            "<span class=\"ovcaret placeholder\" aria-hidden=\"true\"></span>"
        };
        let name = if is_child { p.url.rsplit('/').next().unwrap_or(&p.url) } else { p.url.as_str() };
        let branch = if p.branch.is_empty() { String::new() } else { format!(" <span class=\"branch\">⎇ {}</span>", esc(&p.branch)) };
        let chips = match &p.wt { Some(w) => format!(" <span class=\"ovchips\">{}</span>", worktree_chips(w, &p.key, p.live)), None => String::new() };
        let parent_attr = p.parent.as_deref().map(|pk| format!(" data-parent=\"{}\"", esc(pk))).unwrap_or_default();
        if !p.reachable {
            out.push_str(&format!(
                "<li class=\"{cls} unreachable\" data-key=\"{}\"{parent_attr} title=\"worktree outside resh's roots — cannot be opened\">{caret}{marker} {}{branch}{chips}</li>",
                esc(&p.key), esc(name)));
            continue;
        }
        out.push_str(&format!(
            "<li class=\"{cls}\" data-key=\"{}\"{parent_attr}>{caret}<a href=\"/{}\">{marker} {}{branch}</a>{chips}</li>",
            esc(&p.key), crate::http::percent_encode(&p.url), esc(name)));
    }
    out.push_str("</ul>");
    out
}
```

`src/routes.rs`, beside the `_worktrees` arm:

```rust
        ["frag", "_overview_projects"] => {
            let sel = req.query.get("sel").map(String::as_str).unwrap_or("");
            let ps = registry::known_projects_with_state(roots);
            http::html(w, &render::overview_projects(sel, &ps));
        }
```

- [ ] **Step 4: Run** — `cargo test render:: routes:: 2>&1 | grep "test result"` → ok (the existing `worktrees_strip` tests must still pass, proving the factor is behaviour-preserving).

- [ ] **Step 5: Revert-check** — the unreachable test per its comment; restore.

- [ ] **Step 6: Commit**

```bash
git add src/render.rs src/routes.rs
git commit -m "overview: left pane — project/worktree tree, chips shared with the switcher"
```

---

### Task 3: Session rows without `ps`, and a batch ages snapshot

**Files:**
- Modify: `src/session.rs` (add `live_rows`, `ages_snapshot`, `parse_ages`)

**Interfaces:**
- Produces:
  ```rust
  /// (name, pid, attached) for a project's live sessions — the lock-only
  /// scan of `list_sessions`, with no `ps` fork. Sorted by name.
  pub fn live_rows(project: &str) -> Vec<(String, u32, usize)>
  /// pid → elapsed seconds, from ONE `ps -Aww -o pid=,etime=`. Empty on failure.
  pub fn ages_snapshot() -> std::collections::HashMap<u32, u64>
  pub fn parse_ages(out: &str) -> std::collections::HashMap<u32, u64>   // for tests
  ```

- [ ] **Step 1: Write the failing tests** (`src/session.rs` tests)

```rust
    #[test]
    fn parse_ages_reads_pid_and_etime_columns() {
        // `ps -o pid=,etime=` output: pid, whitespace, etime, one row per line.
        let out = "  123 05:10\n 4567 1-02:03:04\n89 00:42\n";
        let m = parse_ages(out);
        assert_eq!(m.get(&123), Some(&310));         // 5m10s
        assert_eq!(m.get(&4567), Some(&(93784)));    // 1d2h3m4s
        assert_eq!(m.get(&89), Some(&42));
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn parse_ages_skips_a_malformed_line_rather_than_panicking() {
        // Revert-checked: an `unwrap` on the split fails this instead of skipping.
        let m = parse_ages("nonsense\n  7 03:00\n");
        assert_eq!(m.get(&7), Some(&180));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn live_rows_lists_a_projects_sessions_without_forking_ps() {
        let _s = SESSION_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("RESH_CMD", "cat");
        let d = tempfile::tempdir().unwrap();
        let _a = attach("ovproj", "term", d.path()).unwrap();
        let _b = attach("ovproj", "term1", d.path()).unwrap();
        let rows = live_rows("ovproj");
        assert_eq!(rows.iter().map(|(n,_,_)| n.as_str()).collect::<Vec<_>>(), vec!["term", "term1"]);
        // The "no ps" property is structural (live_rows has no ps call); this
        // asserts the scan's data is real. Revert-check: make live_rows return
        // pid 0 for each row and this fails.
        assert!(rows.iter().all(|(_, pid, _)| *pid != 0), "live_rows must carry real pids: {rows:?}");
        kill_project("ovproj");
    }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test session::parse_ages session::live_rows 2>&1 | grep -E "^error" | head`.

- [ ] **Step 3: Implement** (reuse `parse_etime`, already in the file)

```rust
pub fn live_rows(project: &str) -> Vec<(String, u32, usize)> {
    let prefix = format!("{}/", crate::projects::storage_key(project));
    let mut found: Vec<(String, u32, usize)> = {
        let map = sessions().lock().unwrap_or_else(|e| e.into_inner());
        map.iter()
            .filter_map(|(k, s)| Some((k.strip_prefix(&prefix)?.to_string(), s.child_pid, s.subs.len())))
            .collect()
    };
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// One `ps` for the whole host, so the overview's all-projects view costs
/// one fork rather than one per session (see the overview spec). Failure is
/// an empty map — every age then renders "unknown", never `0`.
pub fn ages_snapshot() -> std::collections::HashMap<u32, u64> {
    match std::process::Command::new("ps").args(["-Aww", "-o", "pid=,etime="]).output() {
        Ok(o) if o.status.success() => parse_ages(&String::from_utf8_lossy(&o.stdout)),
        _ => std::collections::HashMap::new(),
    }
}

pub fn parse_ages(out: &str) -> std::collections::HashMap<u32, u64> {
    let mut m = std::collections::HashMap::new();
    for line in out.lines() {
        let t = line.trim();
        let Some((pid_s, etime_s)) = t.split_once(char::is_whitespace) else { continue };
        let (Ok(pid), Some(age)) = (pid_s.trim().parse::<u32>(), parse_etime(etime_s.trim())) else { continue };
        m.insert(pid, age);
    }
    m
}
```

- [ ] **Step 4: Run** — `cargo test session:: 2>&1 | grep "test result"` → ok.

- [ ] **Step 5: Revert-check** — `parse_ages_skips_a_malformed_line` per its comment (make the split `.unwrap()`); restore.

- [ ] **Step 6: Commit**

```bash
git add src/session.rs
git commit -m "session: live_rows (no ps) and a one-ps ages snapshot for the overview"
```

---

### Task 4: Right fragment — the session list

**Files:**
- Modify: `src/render.rs` (add `overview_sessions` + an input struct), `src/routes.rs` (arm `["frag","_overview_sessions"]`)

**Interfaces:**
- Produces:
  ```rust
  pub struct OvSession { pub project_url: String, pub name: String, pub is_claude: bool, pub age_secs: Option<u64>, pub attached: usize }
  pub fn render::overview_sessions(sel: &str, rows: &[OvSession]) -> String
  ```
  The route builds `rows`: for the projects in scope (`sel` empty → all `known_projects`; `sel` = a project key → that key and its `parent==key` children; `sel` = a worktree key → just it), call `session::live_rows`, join ages from one `session::ages_snapshot()`, set `is_claude` from `session::launched_names` ∪ `claudes::claude_evidence(project_url)` being `Present` with the name (or any Present when the terminal can't be named). Render is pure over `rows`, so the test injects them.
- Consumes: Task 3's `live_rows`/`ages_snapshot`; `claudes::claude_evidence`; `session::launched_names`.

- [ ] **Step 1: Write the failing tests** (`src/render.rs`)

```rust
    #[test]
    fn overview_sessions_marks_claude_only_on_evidence_and_shows_label_age_attached() {
        // Revert-checked: rendering every row as ● (dropping is_claude) fails
        // the shell assertion.
        let rows = vec![
            OvSession { project_url: "ultima".into(), name: "term".into(), is_claude: true, age_secs: Some(14400), attached: 1 },
            OvSession { project_url: "ultima/.claude/worktrees/claude-1".into(), name: "term".into(), is_claude: true, age_secs: Some(1200), attached: 0 },
            OvSession { project_url: "resh".into(), name: "shell".into(), is_claude: false, age_secs: None, attached: 0 },
        ];
        let out = overview_sessions("", &rows);
        assert!(out.contains("claude") && out.contains("✻"), "claude row: {out}");
        assert!(out.contains("shell") && out.contains("○"), "shell row marked ○: {out}");
        assert!(out.contains("4h") && out.contains("20m"), "coarse ages: {out}");
        assert!(out.contains("ultima/.claude/worktrees/claude-1"), "worktree label: {out}");
        assert!(out.contains("·1"), "attached count: {out}");
        // The click target: /<project>?focus=<session>, percent-encoded project.
        assert!(out.contains("?focus=term"), "{out}");
        // Unknown age is not 0.
        assert!(!out.contains(">0<") , "unknown age must not render as 0: {out}");
    }

    #[test]
    fn overview_sessions_empty_scope_says_so() {
        let out = overview_sessions("", &[]);
        assert!(out.contains("no sessions") || out.contains("nothing running"), "{out}");
    }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test render::overview_sessions 2>&1 | grep -E "^error" | head`.

- [ ] **Step 3: Implement**

```rust
pub struct OvSession {
    pub project_url: String,
    pub name: String,
    pub is_claude: bool,
    pub age_secs: Option<u64>,
    pub attached: usize,
}

/// The overview's right pane. Pure over `rows` so it is tested without a
/// real `ps` or IDE socket. Each row: ✻ Claude vs ○ shell (Claude only when
/// the caller had positive evidence), the project/worktree label, a coarse
/// age (never `0` for unknown — `—`), and the attached-browser count. The
/// row links to `/<project>?focus=<session>`.
pub fn overview_sessions(sel: &str, rows: &[OvSession]) -> String {
    let scope = if sel.is_empty() { "all active".to_string() } else { format!("in {}", esc(&crate::registry::decode_key(sel))) };
    let mut out = format!(
        "<div class=\"ovshead\">SESSIONS · {scope} <a class=\"ovall\" href=\"/\">All</a></div><ul class=\"ovsessions\">");
    if rows.is_empty() {
        out.push_str("<li class=\"ovempty\">no sessions running</li></ul>");
        return out;
    }
    for r in rows {
        let mark = if r.is_claude { "<span class=\"ovkind on\" title=\"Claude\">✻ claude</span>" } else { "<span class=\"ovkind\" title=\"shell\">○ shell</span>" };
        let age = match r.age_secs { Some(s) => coarse_age(s), None => "—".to_string() };
        let attached = if r.attached > 0 { format!(" <span class=\"ovatt\">·{}</span>", r.attached) } else { String::new() };
        let href = format!("/{}?focus={}", crate::http::percent_encode(&r.project_url), crate::http::percent_encode(&r.name));
        out.push_str(&format!(
            "<li class=\"ovsession\"><a href=\"{href}\">{mark} <span class=\"ovlabel\">{} · {}</span> <span class=\"ovage\">{age}</span>{attached}</a></li>",
            esc(&r.project_url), esc(&r.name)));
    }
    out.push_str("</ul>");
    out
}
```

If a coarse-age helper does not already exist (`grep -n "fn coarse_age\|fn ago\|fn plural" src/render.rs`), reuse the switcher's age formatting; otherwise add a small `fn coarse_age(secs: u64) -> String` matching the `20m`/`4h`/`1d` style used elsewhere and unit-test it alongside.

`src/routes.rs`:

```rust
        ["frag", "_overview_sessions"] => {
            let sel = req.query.get("sel").map(String::as_str).unwrap_or("");
            http::html(w, &render::overview_sessions(sel, &build_overview_sessions(roots, sel)));
        }
```

Add the builder in `routes.rs` (kept here, not in `render.rs`, because it does I/O):

```rust
/// The scope resolution + data gathering for the session pane. `sel` empty →
/// every known project; a project key → it and its worktree children; a
/// worktree key → just it. One ages snapshot for the whole set.
fn build_overview_sessions(roots: &[PathBuf], sel: &str) -> Vec<render::OvSession> {
    let all = registry::known_projects(roots);
    let in_scope: Vec<&registry::ProjectStatus> = if sel.is_empty() {
        all.iter().collect()
    } else {
        all.iter().filter(|p| p.key == sel || p.parent.as_deref() == Some(sel)).collect()
    };
    let ages = crate::session::ages_snapshot();
    let mut rows = Vec::new();
    for p in in_scope {
        let launched: std::collections::HashSet<String> =
            crate::session::launched_names(&p.url).into_iter().map(|(n, _)| n).collect();
        let evidence = crate::claudes::claude_evidence(&p.url);
        for (name, pid, attached) in crate::session::live_rows(&p.url) {
            let is_claude = launched.contains(&name)
                || matches!(&evidence, crate::claudes::ClaudeEvidence::Present(ts) if ts.iter().any(|t| t == &name));
            rows.push(render::OvSession { project_url: p.url.clone(), name, is_claude, age_secs: ages.get(&pid).copied(), attached });
        }
    }
    rows
}
```

- [ ] **Step 4: Run** — `cargo test 2>&1 | grep "test result"` → all ok.

- [ ] **Step 5: Revert-check** — the marking test per its comment; restore.

- [ ] **Step 6: Commit**

```bash
git add src/render.rs src/routes.rs
git commit -m "overview: right pane — sessions with a positive-evidence Claude mark, one ps for ages"
```

---

### Task 5: Client wiring — overview.js, ?focus, CSS

**Files:**
- Create: `static/overview.js`
- Modify: `static/app.js` (the `?focus` block, next to `pendingLaunch` at ~line 14 and the first-State block at ~line 248)
- Modify: `static/style.css`

**Interfaces:**
- Consumes: the fragment markup from Tasks 2/4 (`.ovrow[data-key][data-parent]`, `.ovcaret`, `.ovsession a`); `app.js`'s existing `focusSession(session)`.
- Produces: no Rust surface; verified by Task 6's browser test.

- [ ] **Step 1: `static/overview.js`**

```js
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
```

- [ ] **Step 2: `?focus` in `static/app.js`** — beside `pendingLaunch` (~line 14):

```js
const pendingFocus = (() => {
  const f = new URLSearchParams(location.search).get("focus");
  return f && /^[A-Za-z0-9_-]{1,32}$/.test(f) ? f : null;   // session-name shape
})();
let pendingFocusDone = false;
```

In the first-`State` block (~line 254, next to the `pendingLaunch` consumption):

```js
      // A row clicked on the overview arrives with ?focus=<session>; focus
      // that terminal once, after the first State (so its tab exists), then
      // strip it so a reload doesn't re-focus. Uses the same focusSession the
      // tab bar uses; a name the layout lacks is simply ignored.
      if (pendingFocus && !pendingFocusDone) {
        pendingFocusDone = true;
        if (state.panes.some((p) => p.tabs.some((t) => t.k === "Terminal" && t.session === pendingFocus))) {
          focusSession(pendingFocus);
        }
        history.replaceState(null, "", location.pathname);
      }
```

Confirm `focusSession` is defined and in scope at that point (`grep -n "function focusSession\|const focusSession" static/app.js`); if it is declared later in the file, function-hoisting covers a `function` declaration — verify it is one.

- [ ] **Step 3: CSS** — `static/style.css`:

```css
.overview-body { margin: 0; }
.overview-body header { display: flex; align-items: center; gap: 12px; padding: 6px 10px; }
.overview-body .openbtn { margin-left: auto; text-decoration: none; padding: 2px 8px; border: 1px solid var(--muted); border-radius: 3px; }
#overview { display: grid; grid-template-columns: minmax(260px, 30%) 1fr; gap: 0; height: calc(100vh - 40px); }
#ovprojects, #ovsessions { overflow: auto; padding: 8px; }
#ovprojects { border-right: 1px solid var(--bg2); }
.ovtree { list-style: none; margin: 0; padding: 0; }
.ovrow { padding: 3px 4px; border-radius: 3px; white-space: nowrap; }
.ovrow.child { padding-left: 22px; }
.ovrow.current { background: var(--bg2); font-weight: 600; }
.ovrow a { text-decoration: none; }
.ovrow.unreachable { opacity: .35; }
.ovcaret { display: inline-block; width: 1em; cursor: pointer; color: var(--muted); }
.ovcaret.placeholder { cursor: default; }
.ovchips .wtf { color: var(--muted); font-size: 12px; }
.ovchips .wtf.on { color: var(--fg); }
.ovshead { color: var(--muted); font-size: 12px; margin-bottom: 6px; }
.ovall { margin-left: 8px; }
.ovsessions { list-style: none; margin: 0; padding: 0; }
.ovsession { padding: 4px; border-radius: 3px; }
.ovsession a { text-decoration: none; display: flex; gap: 8px; align-items: baseline; }
.ovkind.on { color: var(--fg); }
.ovkind { color: var(--muted); }
.ovage, .ovatt { color: var(--muted); font-size: 12px; }
.ovempty { color: var(--muted); padding: 6px 4px; }
```

- [ ] **Step 4: Build check** — `cargo build 2>&1 | grep -c warning` (should be 0; JS/CSS aren't compiled but the `build.rs` asset table picks up the new `overview.js` — confirm it's embedded: `grep -c overview.js $(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/build/resh-*/out/assets_table.rs`). If 0, run `cargo clean -p resh && cargo build` so `build.rs` re-scans `static/`.

- [ ] **Step 5: Commit**

```bash
git add static/overview.js static/app.js static/style.css
git commit -m "overview: client wiring — expand/select, ?focus opens a session's tab"
```

---

### Task 6: Browser test

**Files:**
- Create: `tests/browser/overview.mjs`
- Modify: `tests/browser/README.md`

**Interfaces:**
- Consumes: the whole feature; the harness (`startResh`, `startBrowser`, `openPage`, `attachTarget`, `fixture`, `until`).

- [ ] **Step 1: Write the test** — `tests/browser/overview.mjs`

```js
//! The overview front page: it lists real sessions, filters by selection,
//! expands a project to its worktrees, and clicking a session opens that
//! terminal focused.
//!
//! Only a real browser + real dtach proves the session list reflects live
//! PTYs (the Claude/shell mark and attached count are exactly the kind of
//! thing a RESH_CMD=cat unit test renders without touching a real terminal).
//! Assertions read DOM/State, never event order (README trap 2), and check a
//! before/after the poll rather than a bare timeout (trap 1).
//!
//! Run: deno run -A tests/browser/overview.mjs
import { fixture, freePort, openPage, attachTarget, profileDir, startBrowser, startResh, until, sleep }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();               // creates a git project `proj` under fx.roots
const browser = await startBrowser(profileDir(repoRoot));
let page, ws, resh;
try {
  resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
  // Start a real terminal in `proj` via the workspace, so the overview has a session to show.
  ws = await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`);
  await until(() => ws.evalIn("typeof terms !== 'undefined' && ctrl && ctrl.readyState === 1 && !!state"), 30, "workspace app.js");
  await ws.evalIn(`window.__sessions = (pi) => state.panes[pi].tabs.filter((t)=>t.k==="Terminal").map((t)=>t.session);`);
  await ws.evalIn(`document.querySelector('.pane[data-pane="3"] .paneicons .newterm').click()`);
  await until(async () => JSON.parse(await ws.evalIn(`JSON.stringify(__sessions(3))`)).length > 0, 20, "a terminal");
  const sess = JSON.parse(await ws.evalIn(`JSON.stringify(__sessions(3))`))[0];
  await until(() => ws.evalIn(`terms.has(${JSON.stringify(sess)})`), 30, "attached");

  console.log("A. the overview lists the live session");
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/`);
  const sessionsText = () => page.evalIn(`document.getElementById("ovsessions")?.textContent || ""`);
  ok(await until(async () => (await sessionsText()).includes(fx.project) && (await sessionsText()).includes(sess), 15, "session row"),
     `the overview's right pane lists ${fx.project} · ${sess}`);

  console.log("B. clicking the session opens the workspace focused on it");
  const before = (await (await fetch(`http://127.0.0.1:${browser.port}/json/list`)).length) || 0;
  await page.evalIn(`document.querySelector('.ovsession a').click()`);
  ok(await until(async () => (await page.evalIn("location.pathname")).includes(fx.project), 15, "navigated to workspace"),
     "the row navigates to the project workspace");
  ok(await until(async () => (await page.evalIn("location.search")) === "", 10, "?focus consumed"),
     "?focus was stripped after focusing");

  console.log("C. Open a directory reaches the picker");
  const page2 = await openPage(browser.port, `http://127.0.0.1:${resh.port}/?at=`);
  ok(await until(() => page2.evalIn(`!!document.getElementById("picker")`), 10, "picker"), "?at= shows the picker");
  page2.close();
} finally {
  page?.close(); ws?.close();
  browser.close();
  if (resh) await resh.close();
  await fx.cleanup();
}
console.log(fail ? `\n${fail} FAILED` : "\nall ok");
Deno.exit(fail ? 1 : 0);
```

If `fixture()` does not already create a git repo for `proj` (check `tests/browser/harness.mjs`), the overview still lists it as a project with a session; the worktree-expansion assertion is omitted here deliberately (the shipped `worktree-launch.mjs` already covers creating one) — the overview's tree rendering is unit-tested in Task 2. State this omission in the header comment.

- [ ] **Step 2: Run it** — `deno run -A tests/browser/overview.mjs 2>&1 | tail -20`. If SKIP (no browser), install per README; Chromium is present on this host, so it should run to `all ok`.

- [ ] **Step 3: Revert-checks** (record in the header): (a) break `?focus` stripping (remove `history.replaceState`) → assertion "?focus was stripped" fails; (b) make `overview_sessions` always render `○` → not observable here (mark is text) so instead revert the route's `build_overview_sessions` scope to empty and confirm section A fails. Restore.

- [ ] **Step 4: Also run the neighbours** — `deno run -A tests/browser/worktree-launch.mjs` and `claudeterm.mjs` must still pass (this task touched `app.js`).

- [ ] **Step 5: README + commit**

```bash
git add tests/browser/overview.mjs tests/browser/README.md
git commit -m "overview: browser test — lists a live session, opens it focused, picker still reachable"
```

---

### Task 7: Manual check + docs

- [ ] **Step 1** — `cargo test` full suite (time it; a hang is a lock defect): `620+`-style green, no `--release`.
- [ ] **Step 2** — By hand in a real browser on a scratch resh (never the live instance — see the `scratch-resh-for-browser-checks` note): open `/`, confirm both panes populate; expand a project with a worktree; select it and confirm the right pane narrows to its family; click a session and land focused; "Open a directory" reaches the picker.
- [ ] **Step 3** — If `docs/deploy.md` or `README.md` describes the front page as a picker, update that one sentence to say the front page is the overview with the picker behind "Open a directory".
- [ ] **Step 4: Commit** any doc change with explicit paths.
