# Links and Images in Markdown Preview — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a markdown preview's own references work — a link to another
project file opens it as a tab instead of navigating the workspace away, and a
project image renders instead of showing a broken icon — and let a `.png` in the
tree be opened as a picture.

**Architecture:** One resolver (`resolve_dest`) classifies every link and image
destination in a markdown file; two emitters in `markdown_html` consume it, one
rewriting links to the tab-opening anchor markup the file tree already uses, the
other rewriting images to a new `/frag/<proj>/raw?path=` route that serves image
bytes under confinement and the existing 2 MB cap. Remote images are dropped to
their alt text; remote links are kept but retargeted so they cannot replace the
single-page workspace.

**Tech Stack:** Rust (no async), `pulldown-cmark` 0.13, hand-rolled HTTP, plain
JS with no framework, Deno + Chromium for browser tests.

**Spec:** `docs/superpowers/specs/2026-08-19-preview-links-and-images-design.md`

## Global Constraints

Copied from CLAUDE.md and the spec. Every task's requirements include these.

- **Every filesystem path is confined** before use — `projects::safe_resolve`
  for existing targets. The lexical normalization in `resolve_dest` is for
  correctness and is **not** the boundary.
- **2 MB cap** (`projects::MAX_FILE_BYTES`) on every project-controlled read.
  `metadata` first, then read — never `fs::read` then check.
- **HTTP stays GET-only** apart from the two existing POSTs. This adds a GET.
- **All HTML is built in Rust in `render.rs`; escape everything interpolated.**
- **URL values go through `crate::http::percent_encode`** — never a hand-rolled
  encoder. It is a strict allowlist emitting `%2B` for `+`; the matching
  `percent_decode` maps a bare `+` to a space, so any other encoder reproduces
  CLAUDE.md's defect #1 on a file named `gtk+.png`.
- **Module-level `//!` doc; `#[cfg(test)] mod tests` at the bottom of the same
  file.** Comments give rationale, not mechanics.
- **`cargo test`, never `cargo test --release`.**
- **Every new test gets the revert-the-fix check**: apply the broken version,
  run it, read the failure, restore — and record the failure mode in the test's
  own comment. A test that cannot fail is the dominant defect class here.
- **Confinement tests must create a real file at the escape target.** A test
  that errors with `ENOENT` before reaching the confinement check passes green
  and proves nothing; that is why a symlink escape once survived review.

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `src/render.rs` | Modify | `Dest`, `resolve_dest`, the two rewrite arms, `image_fragment` |
| `src/routes.rs` | Modify | `IMAGE_EXT`, `is_image`, `serve_raw`, the `raw` and `file` fragment arms |
| `src/workspace.rs` | Modify | Refuse `SetMode { Edit }` on an image |
| `src/http.rs` | Modify | `img-src` CSP on HTML responses |
| `static/app.js` | Modify | Widen the `data-rel` selector; hide ✎ on images |
| `static/style.css` | Modify | Inline styling for `.mdlink` / `.mdbroken`, `img` sizing |
| `tests/browser/mdlinks.mjs` | Create | The half no Rust test can reach |

---

### Task 1: The shared resolver

Pure function, no callers yet. It exists first because both rewrite arms depend
on it answering identically — splitting the question is how the two drift.

**Files:**
- Modify: `src/render.rs` (add above `markdown_html`, currently line 39)
- Test: `src/render.rs`, in the existing `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: nothing.
- Produces: `pub enum Dest { Remote, Data, Local(String), Passthrough, Broken }`
  and `pub fn resolve_dest(dest: &str, from_rel: &str) -> Dest`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/render.rs`:

```rust
#[test]
fn resolve_dest_classifies_every_destination_shape() {
    use Dest::*;
    // Relative resolves against the *file's* directory, which is the whole bug.
    assert_eq!(resolve_dest("cat.png", "docs/a.md"), Local("docs/cat.png".into()));
    assert_eq!(resolve_dest("../img/x.png", "docs/a.md"), Local("img/x.png".into()));
    assert_eq!(resolve_dest("./b.md", "docs/a.md"), Local("docs/b.md".into()));
    // A file at the root has no directory to prepend.
    assert_eq!(resolve_dest("b.md", "a.md"), Local("b.md".into()));
    // Absolute means project-root-relative, which is what repo authors mean.
    assert_eq!(resolve_dest("/x.png", "docs/a.md"), Local("x.png".into()));
    // Query and fragment are not part of the path.
    assert_eq!(resolve_dest("b.md#heading", "a.md"), Local("b.md".into()));

    assert_eq!(resolve_dest("https://e.com/x.png", "a.md"), Remote);
    assert_eq!(resolve_dest("http://e.com/x.png", "a.md"), Remote);
    assert_eq!(resolve_dest("//e.com/x.png", "a.md"), Remote);
    assert_eq!(resolve_dest("data:image/svg+xml,%3Csvg/%3E", "a.md"), Data);
    assert_eq!(resolve_dest("mailto:p@example.com", "a.md"), Passthrough);
    assert_eq!(resolve_dest("#section", "a.md"), Passthrough);
    assert_eq!(resolve_dest("", "a.md"), Passthrough);

    // Escaping the project is a broken reference, not a path to follow.
    assert_eq!(resolve_dest("../../../etc/passwd", "docs/a.md"), Broken);
    assert_eq!(resolve_dest("..", "a.md"), Broken);
}

/// A relative path containing a colon must not be read as a URL scheme.
/// `find(':')` alone would classify `notes:1.md` as Remote and silently stop
/// rewriting it.
#[test]
fn a_colon_in_a_filename_is_not_a_scheme() {
    assert_eq!(resolve_dest("notes:1.md", "a.md"), Dest::Local("notes:1.md".into()));
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test resolve_dest a_colon_in_a_filename`
Expected: FAIL to compile — `cannot find function resolve_dest`.

- [ ] **Step 3: Implement**

Add to `src/render.rs`, above `markdown_html`:

```rust
/// Where a markdown link or image destination points, once resolved against
/// the file it appeared in.
///
/// Links and images ask this question identically and differ only in what they
/// emit, so it is answered once. Two copies of this logic would drift, and the
/// drift would be silent: a link and an image to the same file would resolve
/// to different places with nothing to flag it.
#[derive(Debug, PartialEq, Eq)]
pub enum Dest {
    /// Off-origin: an `http`/`https` (or other non-`data`) scheme, or
    /// protocol-relative `//host/x`.
    Remote,
    /// A `data:` URI. Self-contained, and issues no request.
    Data,
    /// Project-relative and lexically normalized. **Not confined** — the
    /// server confines on use, and this must not be mistaken for the boundary.
    Local(String),
    /// `mailto:`, `#anchor`, empty. Not ours to rewrite.
    Passthrough,
    /// A relative path that climbs out of the project. A dead reference.
    Broken,
}

/// Collapses `.` and `..` lexically. Returns `None` if the path climbs above
/// the project root — clamping instead would silently retarget an escaping
/// reference at some unrelated file that happens to exist.
fn normalize_rel(p: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(out.join("/"))
}

pub fn resolve_dest(dest: &str, from_rel: &str) -> Dest {
    if dest.is_empty() || dest.starts_with('#') {
        return Dest::Passthrough;
    }
    if dest.starts_with("//") {
        return Dest::Remote;
    }
    // A URL scheme is `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":"`.
    // Testing for a bare ':' would misread `notes:1.md` — a legal filename —
    // as a scheme and stop rewriting it.
    if let Some(i) = dest.find(':') {
        let scheme = &dest[..i];
        let is_scheme = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        if is_scheme {
            return if scheme.eq_ignore_ascii_case("data") {
                Dest::Data
            } else if scheme.eq_ignore_ascii_case("mailto") || scheme.eq_ignore_ascii_case("tel") {
                Dest::Passthrough
            } else {
                Dest::Remote
            };
        }
    }
    // Query and fragment are not part of the path on disk.
    let path = dest.split(['?', '#']).next().unwrap_or(dest);
    let joined = match path.strip_prefix('/') {
        Some(abs) => abs.to_string(),
        None => match from_rel.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/{path}"),
            None => path.to_string(),
        },
    };
    match normalize_rel(&joined) {
        Some(p) => Dest::Local(p),
        None => Dest::Broken,
    }
}
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test resolve_dest a_colon_in_a_filename`
Expected: PASS.

- [ ] **Step 5: Prove the colon test can fail**

Replace the `is_scheme` computation with `let is_scheme = true;`, run
`cargo test a_colon_in_a_filename`, confirm it FAILS with
`Remote != Local("notes:1.md")`, then restore. Record the failure mode in the
test's comment.

- [ ] **Step 6: Commit**

```bash
git add src/render.rs
git commit -m "render: resolve a markdown destination against its own file"
```

---

### Task 2: The raw image route

Independent of Task 1 — nothing rewrites to it yet, so it is reviewable purely
as "does this serve the right bytes and refuse the right things".

**Files:**
- Modify: `src/routes.rs` (`FRAGMENT_KINDS` at line 277; new arm in `serve_frag`)
- Test: `src/routes.rs`, in the existing `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: `pub const IMAGE_EXT: &[&str]`, `pub fn is_image(rel: &str) -> bool`,
  and the route `GET /frag/<project>/raw?path=<rel>`.

- [ ] **Step 1: Write the failing tests**

The existing `frag_route` test helper in `src/routes.rs` builds a request and
returns the raw response string; follow the pattern already used by
`a_project_may_never_serve_code` around line 596.

```rust
/// The escape target must EXIST. A test pointing `path` at a file that is not
/// there passes on ENOENT without ever reaching the confinement check — the
/// exact hole that let a symlink escape survive review once already.
#[test]
fn raw_serves_an_image_and_refuses_everything_else() {
    let d = tempfile::tempdir().unwrap();
    let proj = d.path().join("p");
    std::fs::create_dir_all(proj.join("docs")).unwrap();
    // A one-pixel PNG: real bytes, so a content-type assertion means something.
    let png: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R',
    ];
    std::fs::write(proj.join("docs/cat.png"), png).unwrap();
    std::fs::write(proj.join("secret.rs"), "fn main() {}").unwrap();
    let roots = vec![d.path().to_path_buf()];

    let ok = frag_route(&roots, "/frag/p/raw?path=docs/cat.png");
    assert!(ok.starts_with("HTTP/1.1 200 OK"), "{ok}");
    assert!(ok.contains("Content-Type: image/png"), "{ok}");
    assert!(ok.contains("X-Content-Type-Options: nosniff"), "{ok}");
    assert!(ok.contains("Content-Security-Policy: sandbox"), "{ok}");

    // Deny-by-default: the file exists and is readable, and is still refused.
    let code = frag_route(&roots, "/frag/p/raw?path=secret.rs");
    assert!(code.starts_with("HTTP/1.1 404"), "{code}");
    assert!(code.contains("not an image"), "must refuse on class, not absence: {code}");

    let up = frag_route(&roots, "/frag/p/raw?path=../p/docs/cat.png");
    assert!(up.starts_with("HTTP/1.1 404"), "{up}");
}

#[test]
fn raw_refuses_an_oversize_image_and_a_symlink_out() {
    let d = tempfile::tempdir().unwrap();
    let proj = d.path().join("p");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("big.png"), vec![0u8; (crate::projects::MAX_FILE_BYTES + 1) as usize])
        .unwrap();
    // Outside the project, and REAL — see the comment on the test above.
    std::fs::write(d.path().join("outside.png"), b"not yours").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(d.path().join("outside.png"), proj.join("escape.png")).unwrap();
    let roots = vec![d.path().to_path_buf()];

    let big = frag_route(&roots, "/frag/p/raw?path=big.png");
    assert!(big.starts_with("HTTP/1.1 404"), "{big}");
    assert!(big.contains("asset too large"), "must refuse on size, not absence: {big}");

    #[cfg(unix)]
    {
        let esc = frag_route(&roots, "/frag/p/raw?path=escape.png");
        assert!(esc.starts_with("HTTP/1.1 404"), "a symlink leaving the project: {esc}");
        assert!(!esc.contains("not yours"), "and its bytes must not appear: {esc}");
    }
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test raw_serves_an_image raw_refuses_an_oversize`
Expected: FAIL — the `raw` path is not a fragment kind, so the response is a
404 without the asserted body, or a theme-asset 404.

- [ ] **Step 3: Implement**

In `src/routes.rs`, add near `content_type` (line 186):

```rust
/// Extensions the raw route serves, the `file` fragment renders as a picture,
/// and `SetMode { Edit }` refuses.
///
/// Deny-by-default, for the reason `assets::class_of` gives: an unrecognised
/// extension must fall outside this list, so widening it is an edit here and
/// never a side effect of an unfamiliar file appearing in a cloned repo.
///
/// `static/app.js` keeps a copy, for the ✎ toggle. Nothing checks the two
/// agree — the design doc records why neither direction of mismatch loses
/// data, because `workspace.rs` is the actual guard.
pub const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg", "ico"];

pub fn is_image(rel: &str) -> bool {
    IMAGE_EXT.contains(&crate::assets::ext_of(rel).as_str())
}

/// Image bytes out of a project, for `<img>` in a markdown preview and for an
/// image tab.
///
/// `metadata` before `read` is not a style preference: a bare `fs::read`
/// allocates the whole file — a size a cloned repo controls — on this
/// connection's thread before any cap could reject it.
fn serve_raw(w: &mut impl Write, dir: &Path, rel: &str) {
    let Some(rel) = crate::assets::normalize(rel) else {
        return http::not_found(w, "no such asset");
    };
    if !is_image(rel) {
        return http::not_found(w, "not an image");
    }
    let Ok(path) = projects::safe_resolve(dir, rel) else {
        return http::not_found(w, "no such asset");
    };
    match std::fs::metadata(&path) {
        Ok(meta) if meta.len() > projects::MAX_FILE_BYTES => http::not_found(w, "asset too large"),
        Ok(_) => match std::fs::read(&path) {
            Ok(body) => {
                http::respond_with(w, 200, "OK", content_type(rel), &[NOSNIFF, SANDBOX], &body)
            }
            Err(_) => http::not_found(w, "no such asset"),
        },
        Err(_) => http::not_found(w, "no such asset"),
    }
}
```

Add `"raw"` to `FRAGMENT_KINDS` (line 277):

```rust
const FRAGMENT_KINDS: &[&str] =
    &["tree", "file", "raw", "changes", "status", "diff", "theme.css"];
```

Add the arm in `serve_frag`, next to `["file"]`:

```rust
["raw"] => match req.query.get("path") {
    None => http::not_found(w, "missing path"),
    Some(rel) => serve_raw(w, &dir, rel),
},
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test raw_serves_an_image raw_refuses_an_oversize`
Expected: PASS.

- [ ] **Step 5: Prove the deny-by-default and symlink tests can fail**

Two reverts, each run and restored:

1. Delete the `if !is_image(rel)` guard → `raw_serves_an_image...` must FAIL on
   `secret.rs` returning 200.
2. Replace `projects::safe_resolve(dir, rel)` with `Ok(dir.join(rel))` →
   `raw_refuses_an_oversize_image_and_a_symlink_out` must FAIL with the
   symlink's bytes in the response.

Record both failure modes in the tests' comments.

- [ ] **Step 6: Commit**

```bash
git add src/routes.rs
git commit -m "routes: serve project image bytes under confinement and the cap"
```

---

### Task 3: Link rewriting, and the client wiring it needs

The deliverable is "clicking a link to another file opens it as a tab", which
needs both halves — the Rust rewrite and the one-selector client change — so
they land together.

**Files:**
- Modify: `src/render.rs` (`markdown_html` line 39, `file_fragment` line 53)
- Modify: `src/routes.rs:316` (the `file_fragment` call site)
- Modify: `static/app.js:583` (the `wireFileLinks` selector)
- Modify: `static/style.css`
- Test: `src/render.rs`, in `mod tests`

**Interfaces:**
- Consumes: `resolve_dest` and `Dest` from Task 1.
- Produces: `pub fn markdown_html(md: &str, project: &str, rel: &str) -> String`
  and `pub fn file_fragment(project: &str, rel: &str, content: &str) -> String`
  — both gain parameters, so every caller changes.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_local_link_becomes_a_tab_opening_anchor() {
    let h = markdown_html("see [the plan](plan.md)\n", "proj", "docs/a.md");
    assert!(h.contains(r#"<a class="mdlink" data-rel="docs/plan.md">"#), "{h}");
    assert!(h.contains("the plan</a>"), "the link text must survive: {h}");
    // No href at all: an href would let a click navigate the SPA away before
    // the handler ran, which is the bug this fixes.
    assert!(!h.contains(r#"href="plan.md""#), "{h}");
    // class="file" would style an inline reference as a tree row (icon,
    // indent guides, full-width hover). Asserting the absence, because a test
    // that only greps data-rel passes either way.
    assert!(!h.contains(r#"class="file""#), "{h}");
}

#[test]
fn a_remote_link_survives_but_cannot_replace_the_workspace() {
    let h = markdown_html("[docs](https://example.com/x)\n", "proj", "a.md");
    assert!(h.contains(r#"href="https://example.com/x""#), "{h}");
    assert!(h.contains(r#"target="_blank""#), "{h}");
    assert!(h.contains(r#"rel="noopener noreferrer""#), "{h}");
}

#[test]
fn an_anchor_link_is_left_alone_and_a_broken_one_is_inert() {
    let h = markdown_html("[top](#top) and [gone](../../../etc/passwd)\n", "proj", "a.md");
    assert!(h.contains(r##"href="#top""##), "{h}");
    assert!(h.contains(r#"<a class="mdbroken">"#), "{h}");
    assert!(!h.contains("etc/passwd"), "a dead reference must not stay clickable: {h}");
}

/// The link arm emits Event::Html, which sits in the same match as the arm
/// that turns repo-authored Html into text. If those are ever reordered, repo
/// HTML reaches the page.
#[test]
fn rewriting_links_did_not_reopen_the_raw_html_hole() {
    let h = markdown_html("hello <script>alert(1)</script>\n", "proj", "a.md");
    assert!(!h.contains("<script>"), "{h}");
}
```

Update the three existing tests that call the old signatures
(`markdown_renders_wrapped` line 560, `markdown_raw_html_is_neutralized` line
568, `file_fragment_md_vs_code` line 576) to pass `"proj"` and a rel.

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --lib render`
Expected: FAIL to compile — `markdown_html` takes 1 argument, 3 supplied.

- [ ] **Step 3: Implement the Rust half**

Replace `markdown_html` and `file_fragment` in `src/render.rs`:

```rust
/// The opening tag for a markdown link, by where it points.
///
/// Raw HTML is required here rather than rewriting the tag's `dest_url`,
/// because `data-rel` and `target` are attributes `Tag::Link` cannot carry.
/// Everything interpolated is escaped; the closing `</a>` comes from
/// `push_html`'s own handling of `TagEnd::Link`, which runs whether or not the
/// opening tag was ours.
fn link_open(dest: &str, from_rel: &str) -> String {
    match resolve_dest(dest, from_rel) {
        // Deliberately no href — `wireFileLinks` opens it as a tab, and an
        // href would race that handler by navigating the workspace away.
        Dest::Local(p) => format!("<a class=\"mdlink\" data-rel=\"{}\">", esc(&p)),
        // Kept, because a link is a deliberate click that shows its target,
        // unlike an image's automatic fetch. `_blank` stops it replacing the
        // workspace; `noopener` denies it `window.opener`; `noreferrer` keeps
        // the workspace URL out of the request.
        Dest::Remote => format!(
            "<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">",
            esc(dest)
        ),
        Dest::Data | Dest::Passthrough => format!("<a href=\"{}\">", esc(dest)),
        // Inert: no href, no data-rel, so neither the browser nor the client
        // will follow it.
        Dest::Broken => "<a class=\"mdbroken\">".to_string(),
    }
}

pub fn markdown_html(md: &str, project: &str, rel: &str) -> String {
    // TagEnd is not imported yet — the image arm in Task 4 adds it. Importing
    // it here would warn.
    use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag};
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let events = Parser::new_ext(md, opts).filter_map(|ev| match ev {
        // raw HTML from repo content must never reach the page: render it as
        // text. This arm must stay FIRST — the arms below emit Event::Html we
        // built ourselves from escaped values, and they are not re-examined
        // only because nothing downstream looks at them again.
        Event::Html(h) => Some(Event::Text(h)),
        Event::InlineHtml(h) => Some(Event::Text(h)),

        Event::Start(Tag::Link { ref dest_url, .. }) => {
            Some(Event::Html(CowStr::from(link_open(dest_url, rel))))
        }

        other => Some(other),
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    let _ = project; // used by the image arm in the next task
    format!("<article class=\"markdown-body\">{out}</article>")
}

pub fn file_fragment(project: &str, rel: &str, content: &str) -> String {
    let ext = rel.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if ext == "md" || ext == "markdown" {
        format!(
            "<div class=\"path\">{}</div>{}",
            esc(rel),
            markdown_html(content, project, rel)
        )
    } else {
        format!(
            "<div class=\"path\">{}</div><pre class=\"codeview\"><code class=\"language-{}\">{}</code></pre>",
            esc(rel),
            esc(&ext),
            esc(content)
        )
    }
}
```

Update the caller at `src/routes.rs:316`:

```rust
Ok(content) => http::html(w, &render::file_fragment(project, rel, &content)),
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test`
Expected: PASS, whole suite.

- [ ] **Step 5: Implement the client half**

In `static/app.js`, widen the selector on line 583:

```js
  // Any anchor carrying data-rel, not just tree rows: markdown previews emit
  // <a class="mdlink" data-rel> for links to project files, and they want the
  // identical open-as-tab and context-menu behaviour. A no-op for existing
  // markup — every data-rel anchor rendered today already has class="file"
  // (render.rs tree_level, changes_fragment) — and tree <details data-rel>
  // stays excluded because the selector still requires an `a`.
  root.querySelectorAll("a[data-rel]").forEach((a) => {
```

In `static/style.css`, add near the other `.markdown-body` rules:

```css
/* A markdown link is inline text. .content a.file styles a *tree row* — block
   padding scaled by --d, a file icon, indent guides, a full-width hover band —
   which is why these links carry their own class instead of reusing it. */
.markdown-body a.mdlink { color: var(--accent); cursor: pointer; text-decoration: underline; }
.markdown-body a.mdlink:hover { text-decoration: none; }
.markdown-body a.mdbroken { color: var(--muted); text-decoration: line-through; cursor: default; }
.markdown-body img { max-width: 100%; height: auto; }
```

- [ ] **Step 6: Prove the no-href assertion can fail**

In `link_open`, change the `Dest::Local` arm to
`format!("<a href=\"{}\" class=\"mdlink\" data-rel=\"{}\">", esc(dest), esc(&p))`,
run `cargo test a_local_link_becomes`, confirm it FAILS on the `href="plan.md"`
assertion, then restore. Record the failure mode in the test's comment.

- [ ] **Step 7: Commit**

```bash
git add src/render.rs src/routes.rs static/app.js static/style.css
git commit -m "preview: open a link to a project file as a tab"
```

---

### Task 4: Image rewriting, and the CSP backstop

**Files:**
- Modify: `src/render.rs` (`markdown_html`)
- Modify: `src/http.rs:130` (`html`)
- Test: `src/render.rs` and `src/http.rs`, each in `mod tests`

**Interfaces:**
- Consumes: `resolve_dest`/`Dest` (Task 1), the `raw` route's URL shape (Task 2),
  `markdown_html(md, project, rel)` (Task 3).
- Produces: no new signatures.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_local_image_points_at_the_raw_route() {
    let h = markdown_html("![a cat](cat.png)\n", "proj", "docs/a.md");
    assert!(h.contains(r#"src="/frag/proj/raw?path=docs/cat.png""#), "{h}");
    assert!(h.contains(r#"alt="a cat""#), "{h}");
}

/// Both halves are asserted. "No <img" alone passes if the image vanished
/// entirely; "alt text present" alone passes if the <img> is still there with
/// its alt attribute.
#[test]
fn a_remote_image_is_dropped_to_its_alt_text() {
    let h = markdown_html("text ![a *fancy* cat](https://e.com/b.png) after\n", "proj", "a.md");
    assert!(!h.contains("<img"), "{h}");
    assert!(!h.contains("e.com"), "{h}");
    assert!(h.contains("a <em>fancy</em> cat"), "alt renders as inline markdown: {h}");
}

#[test]
fn a_data_image_survives_untouched() {
    let h = markdown_html("![d](data:image/gif;base64,R0lGOD)\n", "proj", "a.md");
    assert!(h.contains("src=\"data:image/gif;base64,R0lGOD\""), "{h}");
}

/// CLAUDE.md's defect #1 in miniature: an encoder that leaves `+` alone pairs
/// with a decoder that reads `+` as a space, and the file silently "does not
/// exist". The round-trip is the only assertion that catches a plausible-
/// looking encoder swapped in for `percent_encode`.
#[test]
fn an_image_path_with_a_plus_and_a_space_round_trips() {
    // Angle brackets are required: CommonMark does not allow a bare space in a
    // destination, and `![x](my notes+drafts.png)` parses as no image at all —
    // verified before this plan was written.
    let h = markdown_html("![x](<my notes+drafts.png>)\n", "proj", "a.md");
    let start = h.find("path=").unwrap() + "path=".len();
    let end = h[start..].find('"').unwrap() + start;
    assert_eq!(crate::http::percent_decode(&h[start..end]), "my notes+drafts.png");
}
```

And in `src/http.rs`'s `mod tests`:

```rust
#[test]
fn an_html_page_blocks_off_origin_images() {
    let mut out = Vec::new();
    html(&mut out, "<p>hi</p>");
    let s = String::from_utf8(out).unwrap();
    // 'self' stops a remote image the render-side rewrite missed; data: is
    // required because the favicon is a data URI (app.js, faviconFor).
    assert!(s.contains("Content-Security-Policy: img-src 'self' data:"), "{s}");
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test a_local_image a_remote_image a_data_image an_image_path_with an_html_page_blocks`
Expected: FAIL — images still emit their original `src`, and no CSP header is
sent.

- [ ] **Step 3: Implement**

In `markdown_html`: add `TagEnd` to the `pulldown_cmark` import, delete the
`let _ = project;` line Task 3 left behind, declare the flag above the iterator,
and add the image arms above `other => Some(other)`:

```rust
    // Set while an image whose Start was dropped is still open, so its End is
    // dropped too. Images cannot nest, so one flag suffices. Dropping only the
    // Start would leave push_html emitting a stray `" />` into the document.
    let mut dropped_image = false;
    let events = Parser::new_ext(md, opts).filter_map(|ev| match ev {
        // ... the Html/InlineHtml and Link arms from Task 3 ...

        // Rewritten by editing the tag rather than by emitting raw HTML: the
        // alt text lives in the events BETWEEN Start and End, and only
        // push_html's own image handling collects them into the attribute.
        Event::Start(Tag::Image { link_type, dest_url, title, id }) => {
            match resolve_dest(&dest_url, rel) {
                Dest::Local(p) => {
                    let url = format!(
                        "/frag/{}/raw?path={}",
                        crate::http::percent_encode(project),
                        crate::http::percent_encode(&p)
                    );
                    Some(Event::Start(Tag::Image {
                        link_type,
                        dest_url: CowStr::from(url),
                        title,
                        id,
                    }))
                }
                Dest::Data => Some(Event::Start(Tag::Image { link_type, dest_url, title, id })),
                // Remote, Passthrough, Broken. Dropping the tag leaves the
                // events between it and its End to render as ordinary inline
                // markdown — so the fallback is the alt text with its
                // emphasis intact, and no placeholder markup is needed.
                _ => {
                    dropped_image = true;
                    None
                }
            }
        }
        Event::End(TagEnd::Image) if dropped_image => {
            dropped_image = false;
            None
        }

        other => Some(other),
    });
```

In `src/http.rs`, replace `html`:

```rust
/// Every HTML response, page or fragment.
///
/// The `img-src` policy is a backstop, not the mechanism: `render::markdown_html`
/// already drops off-origin images, which is precise, unit-testable without a
/// browser, and leaves readable alt text where CSP would leave a broken icon.
/// This catches what that misses. Only `img-src` is set — a fuller policy would
/// have to reason about the inline script and style this page serves.
///
/// It rides on every HTML response rather than only on the page, so a new page
/// cannot be added without it. A fragment response simply ignores it.
pub fn html(w: &mut impl Write, body: &str) {
    respond_with(
        w,
        200,
        "OK",
        "text/html; charset=utf-8",
        &[("Content-Security-Policy", "img-src 'self' data:")],
        body.as_bytes(),
    );
}
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test`
Expected: PASS, whole suite.

- [ ] **Step 5: Prove the remote-image and round-trip tests can fail**

Two reverts, each run and restored:

1. Change the catch-all image arm to
   `_ => Some(Event::Start(Tag::Image { link_type, dest_url, title, id }))` →
   `a_remote_image_is_dropped_to_its_alt_text` must FAIL on `<img` being present.
2. Replace `crate::http::percent_encode(&p)` with `p.replace(' ', "+")` →
   `an_image_path_with_a_plus_and_a_space_round_trips` must FAIL, decoding to
   `my notes drafts.png`.

Record both failure modes in the tests' comments.

- [ ] **Step 6: Commit**

```bash
git add src/render.rs src/http.rs
git commit -m "preview: render project images, and drop the ones that phone home"
```

---

### Task 5: Image tabs

**Files:**
- Modify: `src/render.rs` (add `image_fragment`)
- Modify: `src/routes.rs` (the `["file"]` arm, line 311)
- Modify: `src/workspace.rs` (`Intent::SetMode`, line 202)
- Modify: `static/app.js:241` (the ✎ toggle)
- Test: `src/render.rs`, `src/routes.rs`, `src/workspace.rs`

**Interfaces:**
- Consumes: `is_image` and the raw route (Task 2).
- Produces: `pub fn image_fragment(project: &str, rel: &str) -> String`.

- [ ] **Step 1: Write the failing tests**

In `src/routes.rs`'s `mod tests`:

```rust
/// The tree lists every file, so a .png can be clicked. Before this, the file
/// fragment read through read_text_file and answered "binary file" — the tree
/// offered a file it then refused to open.
#[test]
fn clicking_an_image_shows_a_picture_not_a_binary_error() {
    let d = tempfile::tempdir().unwrap();
    let proj = d.path().join("p");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("shot.png"), b"\x89PNG\x00\x01\x02").unwrap();
    let roots = vec![d.path().to_path_buf()];

    let out = frag_route(&roots, "/frag/p/file?path=shot.png");
    assert!(out.contains(r#"src="/frag/p/raw?path=shot.png""#), "{out}");
    assert!(!out.contains("binary file"), "{out}");
}
```

In `src/workspace.rs`'s `mod tests`:

```rust
/// Edit mounts a <textarea> seeded from `texts`, which the server cannot fill
/// for a binary file — so an image in Edit shows an empty editor over a real
/// file, and a save truncates it. app.js hides the toggle; this is the guard,
/// because the client is not a boundary and another browser may hold a tab
/// strip rendered before this shipped.
#[test]
fn an_image_tab_cannot_be_switched_to_edit() {
    let mut w = Workspace::default_layout();
    apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("shot.png") }).unwrap();
    apply_layout(&mut w, &Intent::SetMode { rel: "shot.png".into(), mode: Mode::Edit }).unwrap();
    assert_eq!(
        w.panes[proto::MIDDLE as usize].tabs[0],
        Tab::File { rel: "shot.png".into(), mode: Mode::Preview },
        "an image must stay in Preview"
    );
    // The same intent on a text file must still work, or this test would pass
    // just as well with SetMode broken outright — which is the failure mode
    // `set_mode_rewrites_the_matching_file_tab` would not catch either, since
    // it never opens an image.
    apply_layout(&mut w, &Intent::OpenTab { pane: proto::MIDDLE, tab: file("a.rs") }).unwrap();
    apply_layout(&mut w, &Intent::SetMode { rel: "a.rs".into(), mode: Mode::Edit }).unwrap();
    assert_eq!(
        w.panes[proto::MIDDLE as usize].tabs[1],
        Tab::File { rel: "a.rs".into(), mode: Mode::Edit }
    );
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test clicking_an_image an_image_tab_cannot`
Expected: FAIL — the file fragment answers `binary file`, and `SetMode` flips
the image tab to `Edit`.

- [ ] **Step 3: Implement**

In `src/render.rs`:

```rust
/// An image opened as a tab. Not `file_fragment`'s business, because that
/// function's whole contract is that it has already been handed the file's
/// text — which for an image does not exist.
pub fn image_fragment(project: &str, rel: &str) -> String {
    format!(
        "<div class=\"path\">{}</div><img class=\"imgview\" src=\"/frag/{}/raw?path={}\" alt=\"{}\">",
        esc(rel),
        crate::http::percent_encode(project),
        crate::http::percent_encode(rel),
        esc(rel)
    )
}
```

In `src/routes.rs`, replace the `["file"]` arm (line 311):

```rust
["file"] => match req.query.get("path") {
    None => http::html(w, &render::hint("missing path")),
    // Branch BEFORE read_text_file: it sniffs for NUL bytes and returns
    // "binary file" for every image, which is what made a .png in the tree
    // unopenable. safe_resolve still runs, so a path leaving the project
    // gets the standard hint rather than an <img> that would 404.
    Some(rel) if is_image(rel) => match projects::safe_resolve(&dir, rel) {
        Ok(_) => http::html(w, &render::image_fragment(project, rel)),
        Err(e) => http::html(w, &render::hint(&e)),
    },
    Some(rel) => match projects::safe_resolve(&dir, rel)
        .and_then(|p| projects::read_text_file(&p))
    {
        Ok(content) => http::html(w, &render::file_fragment(project, rel, &content)),
        Err(e) => http::html(w, &render::hint(&e)),
    },
},
```

In `src/workspace.rs`, at the top of the `Intent::SetMode` arm (line 202):

```rust
Intent::SetMode { rel, mode } => {
    // See the test: Edit over an image is a data-loss path, not a display
    // glitch. app.js hides the toggle; this is what actually stops it.
    if *mode == proto::Mode::Edit && crate::routes::is_image(rel) {
        return Ok(false);
    }
    let mut hit = false;
```

In `static/app.js`, near the top of the file:

```js
// Mirrors routes.rs IMAGE_EXT. Nothing checks the two agree; a mismatch hides
// or shows the ✎ toggle wrongly, but never loses data, because workspace.rs
// refuses the intent regardless.
const IMAGE_EXT = ["png", "jpg", "jpeg", "gif", "webp", "svg", "ico"];
const isImage = (rel) => IMAGE_EXT.includes((rel || "").split(".").pop().toLowerCase());
```

and at line 241:

```js
      if (t.k === "File" && !isImage(t.rel)) {
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test`
Expected: PASS, whole suite.

- [ ] **Step 5: Prove the SetMode test can fail**

Delete the `if *mode == proto::Mode::Edit && ...` guard, run
`cargo test an_image_tab_cannot`, confirm it FAILS with the tab in `Edit`, then
restore. Record the failure mode in the test's comment.

- [ ] **Step 6: Commit**

```bash
git add src/render.rs src/routes.rs src/workspace.rs static/app.js
git commit -m "preview: open an image as a tab, and refuse to edit it"
```

---

### Task 6: The browser test

`cargo test` cannot reach `static/app.js` or `static/style.css`. CLAUDE.md's
dev/prod substitution table records "no browser" as having hidden a completely
broken save path, so this is required, not optional.

**Files:**
- Create: `tests/browser/mdlinks.mjs`
- Modify: `tests/browser/README.md` (list the new test)

**Interfaces:**
- Consumes: everything from Tasks 1-5.
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Read the traps**

Read `tests/browser/README.md` — specifically the four ways a browser test
passes while asserting nothing — and `tests/browser/upload.mjs` for the harness
idiom.

- [ ] **Step 2: Write the test**

```js
//! Do a preview's own references actually work in a browser?
//!
//! No Rust test reaches static/app.js, so the selector that turns <a data-rel>
//! into an OpenTab intent, the ✎ suppression, and whether an <img> element's
//! bytes ever arrived are all untested without this.
//!
//! naturalWidth, not presence: an <img> exists in the DOM whether or not the
//! request succeeded, so `querySelector("img") !== null` is one of the four
//! traps README.md warns about — it passes with the route deleted.
//!
//! Run: deno run -A tests/browser/mdlinks.mjs
import { fixture, freePort, openPage, profileDir, startBrowser, startResh, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
// A real 1x1 PNG, so naturalWidth is meaningfully 1 rather than 0.
const PNG = Uint8Array.from(atob(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
), (c) => c.charCodeAt(0));
await Deno.mkdir(`${fx.roots}/${fx.project}/docs`, { recursive: true });
await Deno.writeFile(`${fx.roots}/${fx.project}/docs/shot.png`, PNG);
await Deno.writeTextFile(`${fx.roots}/${fx.project}/docs/other.md`, "# other\n");
await Deno.writeTextFile(
  `${fx.roots}/${fx.project}/docs/index.md`,
  "# index\n\n![local](shot.png)\n\n![remote](https://example.invalid/x.png)\n\n[to other](other.md)\n",
);

const resh = await startResh({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${resh.port}/${fx.project}`);
  const { evalIn } = page;
  await until(() => evalIn("ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");

  const urlBefore = await evalIn("location.href");
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "docs/index.md", mode: "Preview" } })`);
  await until(() => evalIn(`!!document.querySelector(".markdown-body a.mdlink")`), 15, "preview");

  // ---- 1. A project image renders -----------------------------------------
  await until(() => evalIn(
    `(() => { const i = [...document.querySelectorAll(".markdown-body img")]
        .find(x => x.src.includes("raw?path=")); return !!i && i.complete; })()`), 15, "image load");
  ok(await evalIn(
    `[...document.querySelectorAll(".markdown-body img")]
       .find(x => x.src.includes("raw?path=")).naturalWidth === 1`),
    "a project image actually loaded its bytes");

  // ---- 2. The remote image is gone, its alt text is not --------------------
  ok(await evalIn(`!document.body.innerHTML.includes("example.invalid")`),
    "no request to a remote image host");
  ok(await evalIn(`document.querySelector(".markdown-body").textContent.includes("remote")`),
    "the dropped image left its alt text behind");

  // ---- 3. A link opens a tab and does NOT navigate -------------------------
  await evalIn(`document.querySelector(".markdown-body a.mdlink").click()`);
  await until(() => evalIn(
    `state.panes.some(p => p.tabs.some(t => t.rel === "docs/other.md"))`), 15, "tab opened");
  ok(true, "clicking a local link opened it as a tab");
  ok(await evalIn("location.href") === urlBefore,
    "and the workspace page did not navigate away");

  // ---- 4. An image tab shows a picture and offers no editor ----------------
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "docs/shot.png", mode: "Preview" } })`);
  await until(() => evalIn(`!!document.querySelector("img.imgview")`), 15, "image tab");
  ok(await evalIn(
    `(() => { const i = document.querySelector("img.imgview"); return i.complete && i.naturalWidth === 1; })()`),
    "an image tab shows the picture, not a binary-file error");
  ok(await evalIn(
    `![...document.querySelectorAll(".tabstrip .tab")]
       .some(b => b.textContent.includes("shot.png") && b.querySelector("span.x[title*='edit']"))`),
    "an image tab offers no edit toggle");
} finally {
  try { await page?.close?.(); } catch {}
  try { browser.stop(); } catch {}
  try { resh.stop(); } catch {}
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
```

- [ ] **Step 3: Run it**

Run: `deno run -A tests/browser/mdlinks.mjs`
Expected: all assertions pass. It skips if no browser is present.

- [ ] **Step 4: Prove the image assertions can fail**

Two reverts, each run and restored:

1. Comment out the `["raw"]` arm in `serve_frag` → assertion 1 must FAIL with
   `naturalWidth === 0`, confirming it tests bytes and not DOM presence.
2. Restore the `if (t.k === "File")` condition at `app.js:241` → the last
   assertion must FAIL.

Record both in the test's header comment.

- [ ] **Step 5: Add it to the browser README**

List `mdlinks.mjs` alongside `reconnect.mjs`, `upload.mjs` and `paneicons.mjs`,
with one line on what it covers.

- [ ] **Step 6: Run the whole suite, on the Linux host too**

Run: `cargo test` and `deno run -A tests/browser/mdlinks.mjs`
Also `ssh` to the deploy host and run `cargo test` there — CLAUDE.md records
substitutions (FSEvents vs inotify, `RESH_CMD=cat` vs dtach) that a local green
suite has hidden before.

- [ ] **Step 7: Commit**

```bash
git add tests/browser/mdlinks.mjs tests/browser/README.md
git commit -m "test: drive markdown links and images through a real browser"
```

---

## Self-Review Notes

**Spec coverage.** Every section maps to a task: the resolver → 1; the raw route,
allowlist and SVG → 2; link rewriting, the `mdlink` class and the selector
widening → 3; image rewriting, encoding, the alt-text fallback and CSP → 4;
image tabs, the `IMAGE_EXT` mirror and the Edit refusal → 5; the browser half → 6.

**Deliberately deferred, from the spec's non-goals:** heading anchors, links to
directories, blocking remote links, a placeholder for a blocked image, a
per-project remote-image opt-in, raising the 2 MB cap, non-image embeds.

**Signature changes ripple.** `markdown_html` and `file_fragment` both gain
parameters in Task 3, which breaks three existing tests in `render.rs` and one
call site in `routes.rs`. Task 3 Step 1 names them; do not skip that edit and
expect Task 4 to compile.
