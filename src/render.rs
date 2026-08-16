//! All HTML generation. Plain string building, no template engine.
//! Fragments target htmx swap sites; pages are full documents.
use crate::config::Settings;
use crate::gitio::Status;
use crate::projects::Entry;
use std::path::Path;

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn hint(msg: &str) -> String {
    format!("<div class=\"hint\">{}</div>", esc(msg))
}

pub fn diff_html(diff: &str) -> String {
    diff.lines()
        .map(|l| {
            let cls = if l.starts_with("+++") || l.starts_with("---") || l.starts_with("diff ") {
                "meta"
            } else if l.starts_with("@@") {
                "hunk"
            } else if l.starts_with('+') {
                "add"
            } else if l.starts_with('-') {
                "del"
            } else {
                "ctx"
            };
            let body = if l.is_empty() { " ".to_string() } else { esc(l) };
            format!("<div class=\"dl {cls}\">{body}</div>")
        })
        .collect()
}

pub fn markdown_html(md: &str) -> String {
    use pulldown_cmark::{html, Event, Options, Parser};
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let events = Parser::new_ext(md, opts).map(|ev| match ev {
        // raw HTML from repo content must never reach the page: render it as text
        Event::Html(h) => Event::Text(h),
        Event::InlineHtml(h) => Event::Text(h),
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    format!("<article class=\"markdown-body\">{out}</article>")
}

pub fn file_fragment(rel: &str, content: &str) -> String {
    let ext = rel.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if ext == "md" || ext == "markdown" {
        format!("<div class=\"path\">{}</div>{}", esc(rel), markdown_html(content))
    } else {
        format!(
            "<div class=\"path\">{}</div><pre class=\"codeview\"><code class=\"language-{}\">{}</code></pre>",
            esc(rel),
            esc(&ext),
            esc(content)
        )
    }
}

// Whole-tree eager rendering (the old design) is what made this slow: a
// 41k-entry project produced 895 KB of HTML and still hit the 4,000-entry
// budget with ~90% of the tree missing. Instead we render one level at a
// time, IDE-style — only directories on the currently-open file's path are
// expanded inline; everything else renders closed and lazily fetches its
// own children (see the `dir` query param on the `tree` fragment endpoint
// in routes.rs) the first time the user expands it.
pub fn tree_fragment(project: &str, dir: &Path, open: &str, hide: &[String]) -> String {
    let mut out = String::from("<ul class=\"tree\">");
    tree_level(project, dir, "", open, hide, &mut out);
    out.push_str("</ul>");
    out
}

/// Renders the immediate children of `dir` as `<li>` items only (no `<ul>`
/// wrapper) — used both to seed `tree_fragment`'s top level and to answer a
/// lazy `?dir=` fetch for one previously-closed directory, so a fetched
/// subtree slots into its parent's `<ul>` the same way the initial render
/// built it. `rel` is `dir`'s path relative to the project root ("" at the
/// project root itself).
///
/// The 4,000 budget is applied fresh on every call, i.e. per directory
/// level rather than to the whole recursive walk: an ordinary large project
/// (many modest directories) never trips it, while one pathological
/// directory with thousands of direct entries still gets capped.
pub fn tree_level(project: &str, dir: &Path, rel: &str, open: &str, hide: &[String], out: &mut String) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| (e.path().is_file(), e.file_name().to_ascii_lowercase()));
    let mut budget = 4000usize;
    for e in entries {
        if budget == 0 {
            out.push_str("<li class=\"hint\">tree truncated (too many entries)</li>");
            break;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if crate::projects::SKIP_DIRS.contains(&name.as_str()) || hide.iter().any(|h| h == &name)
        {
            continue;
        }
        budget -= 1;
        let erel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if e.path().is_dir() {
            let is_open = open == erel || open.starts_with(&format!("{erel}/"));
            if is_open {
                // On the open file's path: expand inline, recursively, so
                // the file is visible on load with no extra round trip.
                out.push_str(&format!(
                    "<li><details open data-rel=\"{}\"><summary>{}</summary><ul>",
                    esc(&erel),
                    esc(&name)
                ));
                tree_level(project, &e.path(), &erel, open, hide, out);
                out.push_str("</ul></details></li>");
            } else {
                // Closed, with an empty <ul>: `data-rel` lets the client find
                // this node again (TreeChanged re-fetches whatever is
                // currently expanded), and the hx-get/hx-trigger pair fetches
                // this directory's children exactly once, on first expand.
                out.push_str(&format!(
                    "<li><details data-rel=\"{rel}\" hx-get=\"/frag/{proj}/tree?dir={qrel}\" hx-trigger=\"toggle once\" hx-target=\"find ul\"><summary>{name}</summary><ul></ul></details></li>",
                    rel = esc(&erel),
                    proj = crate::http::percent_encode(project),
                    qrel = crate::http::percent_encode(&erel),
                    name = esc(&name)
                ));
            }
        } else {
            // No hx-get here: the app wires file clicks itself (wireFragment
            // in app.js, via data-rel) rather than through htmx's own ajax
            // pipeline. Leaving hx-get on would make htmx.process() (now
            // called on tree content so lazy <details> bind — see app.js)
            // ALSO bind a real click handler on the same anchor, racing our
            // own and firing a pointless request at a #content target that
            // doesn't exist in the four-pane layout.
            let sel = if open == erel { " sel" } else { "" };
            out.push_str(&format!(
                "<li><a class=\"file{sel}\" data-rel=\"{}\">{}</a></li>",
                esc(&erel),
                esc(&name)
            ));
        }
    }
}

pub fn changes_fragment(project: &str, st: &Status) -> String {
    if st.changes.is_empty() {
        return hint("working tree clean");
    }
    let project_url = crate::http::percent_encode(project);
    let mut out = format!(
        "<ul class=\"changes\"><li><a class=\"file\" data-rel=\"\" hx-get=\"/frag/{project_url}/diff\" hx-target=\"#content\"><b>— full diff —</b></a></li>"
    );
    for c in &st.changes {
        out.push_str(&format!(
            "<li><a class=\"file\" data-rel=\"{}\" hx-get=\"/frag/{}/diff?path={}\" hx-target=\"#content\"><span class=\"xy\">{}</span> {}</a></li>",
            esc(&c.path),
            project_url,
            crate::http::percent_encode(&c.path),
            esc(&c.xy),
            esc(&c.path)
        ));
    }
    out.push_str("</ul>");
    out
}

pub fn status_fragment(st: &Status) -> String {
    format!(
        "<span id=\"branch\">{}</span><span id=\"badge\">{}</span>",
        if st.branch.is_empty() { String::new() } else { format!("⎇ {}", esc(&st.branch)) },
        if st.changes.is_empty() { String::new() } else { format!("({})", st.changes.len()) }
    )
}

/// Breadcrumb for the directory picker: "deadlight" always links back to the
/// top level (`at=""`), every segment but the last is a clickable link to
/// browsing that prefix, and the last segment is plain text (you're already
/// there — the picker doesn't render a `..` row, this is the way up).
fn breadcrumb(at: &str) -> String {
    if at.is_empty() {
        return "<nav class=\"crumbs\"><span class=\"crumb-current\">deadlight</span></nav>".to_string();
    }
    let mut out = String::from("<nav class=\"crumbs\"><a href=\"/\">deadlight</a>");
    let segs: Vec<&str> = at.split('/').collect();
    let mut acc = String::new();
    for (i, seg) in segs.iter().enumerate() {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(seg);
        out.push_str(" / ");
        if i + 1 == segs.len() {
            out.push_str(&format!("<span class=\"crumb-current\">{}</span>", esc(seg)));
        } else {
            // Slash-preserving encode, like tree_level's `dir=` query value
            // above — `acc` is itself a rel path, not an opaque token.
            out.push_str(&format!(
                "<a href=\"/?at={}\">{}</a>",
                crate::http::percent_encode(&acc),
                esc(seg)
            ));
        }
    }
    out.push_str("</nav>");
    out
}

/// One `<li>` per picker row. Directories are selectable (click/dblclick/
/// keyboard, wired client-side by `/static/picker.js` off `li.dir`); files
/// are rendered but carry no such hooks — `.file`'s CSS greys them out, and
/// the absence of any click handler is what makes them actually
/// unselectable, not just visually muted.
///
/// Git repos additionally get a `⎇` shortcut: a real `<a href>` straight to
/// the workspace URL, not a `<span>` with a JS click handler, so opening it
/// gets keyboard reachability (Tab + Enter), middle-click-for-new-tab, and
/// ctrl/cmd-click for free from the browser rather than reimplementing them.
/// picker.js still needs a couple of lines to stop this anchor's click/
/// dblclick from *also* bubbling to the row's own listeners below (which
/// would select or descend the row in addition to navigating).
fn picker_rows(entries: &[Entry]) -> String {
    entries
        .iter()
        .map(|e| {
            if e.is_dir {
                let git = if e.git {
                    format!(
                        " <a class=\"git\" href=\"/{href}\" title=\"open this repo\">⎇</a>",
                        href = crate::http::percent_encode(&e.rel)
                    )
                } else {
                    String::new()
                };
                format!(
                    "<li class=\"dir\" data-rel=\"{rel}\"><span class=\"name\">{name}</span>{git}</li>",
                    rel = esc(&e.rel),
                    name = esc(&e.name)
                )
            } else {
                format!(
                    "<li class=\"file\"><span class=\"name\">{}</span></li>",
                    esc(&e.name)
                )
            }
        })
        .collect()
}

/// The `/` directory picker (see routes::route's `?at=` handling). `at` is
/// the rel path currently being browsed ("" for the merged top level);
/// `entries` is its already-confined listing (`projects::list_dir`).
///
/// `refused` marks the case where the caller's `?at=` did not resolve
/// (missing, outside ROOTS, or otherwise rejected) and routes.rs silently
/// fell back to the top level rather than erroring. The rejected path
/// itself is never echoed here — it's query-string input the user can
/// already see in their own URL bar, and folding it into the message would
/// just be another string to escape for no benefit. That's a distinct
/// situation from `entries` being empty (a real, successfully-opened
/// directory with nothing in it), so the two get separate messages rather
/// than being collapsed into one "nothing to show" hint.
pub fn index_page(at: &str, entries: &[Entry], refused: bool) -> String {
    let notice = if refused { hint("no such directory — showing the top level") } else { String::new() };
    let rows_hint = if entries.is_empty() { hint("empty directory") } else { String::new() };
    let rows = if entries.is_empty() { String::new() } else { picker_rows(entries) };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>deadlight</title>\
         <link rel=\"stylesheet\" href=\"/static/themes/dark.css\">\
         <link rel=\"stylesheet\" href=\"/static/style.css\">\
         </head><body><header><span class=\"proj\">deadlight</span></header>\
         <main>{notice}{crumbs}\
         <ul class=\"picker\" id=\"picker\" data-at=\"{at_attr}\" tabindex=\"0\">{rows}</ul>\
         {rows_hint}<div class=\"pickerbar\"><button id=\"openBtn\" type=\"button\">Open</button></div>\
         </main>\
         <script src=\"/static/picker.js\"></script>\
         </body></html>",
        crumbs = breadcrumb(at),
        at_attr = esc(at),
    )
}

pub fn workspace_page(project: &str, s: &Settings, has_theme_css: bool) -> String {
    let warn = s
        .warning
        .as_deref()
        .map(|w| format!("<span class=\"warn\" title=\"{}\">⚠ config</span>", esc(w)))
        .unwrap_or_default();
    let proj_url = crate::http::percent_encode(project);
    let proj_txt = esc(project);
    let theme_css = if has_theme_css {
        format!("<link rel=\"stylesheet\" href=\"/frag/{proj_url}/theme.css\">")
    } else {
        String::new()
    };
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{proj_txt} — deadlight</title>
<link rel="stylesheet" href="/static/vendor/xterm.css">
<link rel="stylesheet" href="/static/vendor/hljs-github-dark.min.css">
<link rel="stylesheet" href="/static/vendor/github-markdown.min.css">
<link rel="stylesheet" href="/static/themes/{theme}.css">
<link rel="stylesheet" href="/static/style.css">
{theme_css}
<script src="/static/vendor/htmx.min.js"></script>
<script src="/static/vendor/xterm.js"></script>
<script src="/static/vendor/xterm-addon-fit.js"></script>
<script src="/static/vendor/highlight.min.js"></script>
</head><body data-project="{proj_txt}" data-default-tab="{tab}">
<header>
  <a class="home" href="/">◆</a><span class="proj">{proj_txt}</span>
  <span id="gitinfo" hx-get="/frag/{proj_url}/status" hx-trigger="load, refresh from:body"></span>
  {warn}
  <button id="refresh" title="refresh (r)">⟳</button>
</header>
<main id="grid">
  <section class="pane" data-pane="0"><div class="tabstrip"></div><div class="content"></div></section>
  <div class="divider" data-div="left-split"></div>
  <section class="pane" data-pane="1"><div class="tabstrip"></div><div class="content"></div></section>
  <div class="divider" data-div="left-w"></div>
  <section class="pane" data-pane="2"><div class="tabstrip"></div><div class="content"></div></section>
  <div class="divider" data-div="right-w"></div>
  <section class="pane" data-pane="3"><div class="tabstrip"></div><div class="content"></div></section>
</main>
<div id="termpool" hidden></div>
<script src="/static/app.js"></script>
</body></html>"#,
        theme = esc(&s.theme),
        tab = esc(&s.default_tab)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn esc_escapes_html() {
        assert_eq!(esc("a<b>&\"c\""), "a&lt;b&gt;&amp;&quot;c&quot;");
    }

    #[test]
    fn diff_lines_are_classified() {
        let d = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old <\n+new\n ctx";
        let h = diff_html(d);
        assert!(h.contains("dl meta"));
        assert!(h.contains("dl hunk"));
        assert!(h.contains("dl del"));
        assert!(h.contains("dl add"));
        assert!(h.contains("dl ctx"));
        assert!(h.contains("-old &lt;")); // escaped
    }

    #[test]
    fn markdown_renders_wrapped() {
        let h = markdown_html("# Hi\n\n- a\n");
        assert!(h.starts_with("<article class=\"markdown-body\">"));
        assert!(h.contains("<h1>Hi</h1>"));
        assert!(h.contains("<li>a</li>"));
    }

    #[test]
    fn markdown_raw_html_is_neutralized() {
        let h = markdown_html("hello <script>alert(1)</script>\n\n<iframe src=x></iframe>\n");
        assert!(!h.contains("<script>"));
        assert!(!h.contains("<iframe"));
        assert!(h.contains("&lt;script&gt;"));
    }

    #[test]
    fn file_fragment_md_vs_code() {
        let md = file_fragment("readme.md", "# T");
        assert!(md.contains("markdown-body"));
        let code = file_fragment("main.rs", "fn x() -> Vec<u8> {}");
        assert!(code.contains("language-rs"));
        assert!(code.contains("Vec&lt;u8&gt;")); // escaped, hljs runs client-side
    }

    #[test]
    fn tree_marks_open_path_and_skips_hidden() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("src/sub")).unwrap();
        fs::create_dir(d.path().join("target")).unwrap();
        fs::create_dir(d.path().join("dist")).unwrap();
        fs::write(d.path().join("src/main.rs"), "").unwrap();
        fs::write(d.path().join("src/sub/x.rs"), "").unwrap();
        fs::write(d.path().join("README.md"), "").unwrap();
        let h = tree_fragment("proj", d.path(), "src/main.rs", &["dist".to_string()]);
        assert!(h.contains("<details open data-rel=\"src\"><summary>src</summary>"));
        assert!(h.contains("class=\"file sel\""));
        assert!(h.contains("data-rel=\"src/main.rs\""));
        assert!(h.contains("README.md"));
        assert!(!h.contains("<summary>target</summary>"));
        assert!(!h.contains("<summary>dist</summary>"));
    }

    // `sub` sits under `src`, which is on the open path, but `sub` itself is
    // not — this is the one-level contract: a directory not itself on the
    // open path renders as a closed stub (lazy hx-get, empty <ul>) and must
    // not leak its children's markup into the initial response.
    #[test]
    fn tree_renders_one_level_and_closed_dirs_omit_children() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("src/sub")).unwrap();
        fs::write(d.path().join("src/main.rs"), "").unwrap();
        fs::write(d.path().join("src/sub/x.rs"), "").unwrap();
        let h = tree_fragment("proj", d.path(), "src/main.rs", &[]);
        assert!(h.contains(
            "<details data-rel=\"src/sub\" hx-get=\"/frag/proj/tree?dir=src/sub\" \
             hx-trigger=\"toggle once\" hx-target=\"find ul\"><summary>sub</summary><ul></ul></details>"
        ));
        assert!(!h.contains("x.rs")); // sub's child must not be inlined
    }

    // Every directory along the open file's path is pre-expanded inline,
    // recursively, so the open file is visible on first load with no lazy
    // fetch required.
    #[test]
    fn tree_pre_expands_the_whole_open_path() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("a/b/c")).unwrap();
        fs::write(d.path().join("a/b/c/main.rs"), "").unwrap();
        let h = tree_fragment("proj", d.path(), "a/b/c/main.rs", &[]);
        assert!(h.contains("<details open data-rel=\"a\">"));
        assert!(h.contains("<details open data-rel=\"a/b\">"));
        assert!(h.contains("<details open data-rel=\"a/b/c\">"));
        assert!(h.contains("class=\"file sel\" data-rel=\"a/b/c/main.rs\""));
    }

    // The lazy `?dir=` fetch (routes.rs) renders through the same one-level
    // machinery, just scoped to a subdirectory and without the outer <ul>
    // wrapper, so it slots straight into the parent <details>'s own <ul>.
    #[test]
    fn tree_level_answers_a_lazy_dir_fetch() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("src/sub")).unwrap();
        fs::write(d.path().join("src/main.rs"), "").unwrap();
        fs::write(d.path().join("src/sub/x.rs"), "").unwrap();
        let mut out = String::new();
        tree_level("proj", &d.path().join("src"), "src", "", &[], &mut out);
        assert!(!out.starts_with("<ul"));
        assert!(out.contains("data-rel=\"src/main.rs\""));
        assert!(out.contains("data-rel=\"src/sub\"")); // closed stub, not expanded
        assert!(!out.contains("x.rs")); // sub's own child stays lazy
    }

    // app.js's TreeChanged handler reconciles the root level (not just open
    // subdirectories) by fetching `dir=""` — the same lazy-fetch codepath a
    // subdirectory expansion uses, just scoped to the project root — and
    // matching its <li> entries against the DOM by data-rel. That only
    // works if this rel="" call produces list items shaped exactly like
    // tree_fragment's own top level: same data-rel values, no <ul> wrapper.
    #[test]
    fn tree_level_at_empty_rel_matches_the_fragments_top_level() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("src")).unwrap();
        fs::write(d.path().join("README.md"), "").unwrap();
        let mut out = String::new();
        tree_level("proj", d.path(), "", "", &[], &mut out);
        assert!(!out.starts_with("<ul"));
        assert!(out.contains("data-rel=\"src\""));
        assert!(out.contains("data-rel=\"README.md\""));
        // must match the identity render::tree_fragment assigns those same
        // entries at the top level, since the client keys reconciliation on it
        let full = tree_fragment("proj", d.path(), "", &[]);
        assert!(full.contains("data-rel=\"src\""));
        assert!(full.contains("data-rel=\"README.md\""));
    }

    #[test]
    fn changes_and_status_fragments() {
        let st = Status {
            branch: "main".into(),
            changes: vec![crate::gitio::Change { xy: ".M".into(), path: "a.txt".into() }],
        };
        let c = changes_fragment("proj", &st);
        assert!(c.contains("full diff"));
        assert!(c.contains("class=\"xy\""));
        assert!(c.contains("hx-get=\"/frag/proj/diff?path=a.txt\""));
        let s = status_fragment(&st);
        assert!(s.contains("main"));
        assert!(s.contains("(1)"));
        let clean = changes_fragment("proj", &Status { branch: "main".into(), changes: vec![] });
        assert!(clean.contains("working tree clean"));
    }

    #[test]
    fn workspace_page_wires_everything() {
        let s = Settings { theme: "gruvbox".into(), ..Settings::default() };
        let h = workspace_page("proj", &s, true);
        assert!(h.contains("/static/themes/gruvbox.css"));
        assert!(h.contains("/frag/proj/theme.css")); // has_theme_css
        assert!(h.contains("data-project=\"proj\""));
        assert!(h.contains("data-default-tab=\"terminal\""));
        assert!(h.contains("htmx.min.js"));
        assert!(h.contains("data-pane=\"3\""));
        assert!(h.contains("id=\"termpool\""));
        let no_custom = workspace_page("proj", &s, false);
        assert!(!no_custom.contains("theme.css\">"));
    }

    #[test]
    fn index_page_renders_picker_rows_and_breadcrumb() {
        let entries = vec![
            Entry { name: "alpha".into(), rel: "alpha".into(), is_dir: true, git: true },
            Entry { name: "beta".into(), rel: "beta".into(), is_dir: true, git: false },
        ];
        let h = index_page("", &entries, false);
        assert!(h.contains("data-rel=\"alpha\""));
        assert!(h.contains("class=\"dir\""));
        // alpha is a git repo: gets a one-click shortcut straight to its
        // workspace URL, not just the plain ⎇ marker
        assert!(h.contains("<a class=\"git\" href=\"/alpha\" title=\"open this repo\">⎇</a>"));
        // beta is not a git repo: no shortcut anchor for it at all
        assert!(!h.contains("href=\"/beta\""));
        assert!(h.contains("id=\"openBtn\""));
        assert!(h.contains("crumb-current\">deadlight"));
        assert!(h.contains("/static/picker.js"));

        // browsing a subdirectory: breadcrumb links back up, files are
        // present but not marked selectable the way directories are
        let sub = vec![
            Entry { name: "sub".into(), rel: "karpie/sub".into(), is_dir: true, git: false },
            Entry { name: "main.rs".into(), rel: "karpie/main.rs".into(), is_dir: false, git: false },
        ];
        let h2 = index_page("karpie", &sub, false);
        assert!(h2.contains("<a href=\"/\">deadlight</a>"));
        assert!(h2.contains("crumb-current\">karpie"));
        assert!(h2.contains("class=\"dir\" data-rel=\"karpie/sub\""));
        assert!(h2.contains("class=\"file\""));
        assert!(!h2.contains("data-rel=\"karpie/main.rs\"")); // files carry no selection hook
    }

    // The shortcut's href is a real workspace URL, so it needs the same
    // slash-preserving percent-encoding as breadcrumb's `?at=` links (a `/`
    // between segments must stay a literal separator, not become %2F) plus
    // HTML-escaping on the visible bits, since both the segment names and
    // the entry name come straight off the filesystem.
    #[test]
    fn git_shortcut_href_is_percent_encoded_for_a_nested_path() {
        let entries = vec![Entry {
            name: "sp ace\"<>".into(),
            rel: "karpie/sp ace\"<>".into(),
            is_dir: true,
            git: true,
        }];
        let h = index_page("karpie", &entries, false);
        // "/" between segments survives; the space and quote/angle-bracket
        // characters inside the leaf segment are percent-encoded, not left
        // raw (which would break the URL) and not HTML-entity-encoded
        // (which would break the URL differently) — this is the URL
        // encoder, distinct from `esc`'s HTML entities used elsewhere in
        // the same row for the visible name.
        assert!(h.contains("href=\"/karpie/sp%20ace%22%3C%3E\""));
        // the visible name is still HTML-escaped, same as any other text
        assert!(h.contains("sp ace&quot;&lt;&gt;"));
        assert!(!h.contains("sp ace\"<>")); // raw, unescaped name must not appear
    }

    #[test]
    fn index_page_breadcrumb_links_every_intermediate_segment() {
        let h = index_page("a/b/c", &[], false);
        assert!(h.contains("<a href=\"/?at=a\">a</a>"));
        assert!(h.contains("<a href=\"/?at=a/b\">b</a>"));
        assert!(h.contains("crumb-current\">c")); // the current directory itself is not a link
    }

    #[test]
    fn project_name_is_escaped_everywhere() {
        let s = Settings::default();
        let h = workspace_page("a\"><script>", &s, false);
        assert!(!h.contains("a\"><script>"));
        let c = changes_fragment("a\"><script>", &Status { branch: String::new(), changes: vec![crate::gitio::Change { xy: "??".into(), path: "x".into() }] });
        assert!(!c.contains("\"><script>"));
    }

    // Genuinely empty directory (successfully resolved, nothing in it) reads
    // as "broken" with zero rows and no text — the hint fills the rows area
    // so it reads as "empty" instead. `refused` is false here: this is the
    // opposite situation from a rejected `?at=`, and must not also print
    // the refused notice.
    #[test]
    fn index_page_empty_listing_shows_empty_hint() {
        let h = index_page("karpie", &[], false);
        assert!(h.contains("class=\"hint\">empty directory"));
        assert!(!h.contains("showing the top level"));
        assert!(h.contains("id=\"openBtn\"")); // Open stays present and enabled by picker.js's own logic
    }

    // A rejected `?at=` (missing, outside ROOTS, refused for any other
    // reason) still falls back to the top level, but that fallback must not
    // be silent — the caller sees a notice explaining why they landed here
    // instead of where they asked to go, and the fallback listing itself is
    // still rendered (this is not an error page, just an annotated
    // redirect). The two hints are independent: a refused `?at=` whose
    // fallback happens to have entries must show the notice but not the
    // empty-directory hint.
    #[test]
    fn index_page_refused_at_shows_notice_and_still_lists_top_level() {
        let entries = vec![Entry { name: "alpha".into(), rel: "alpha".into(), is_dir: true, git: false }];
        let h = index_page("", &entries, true);
        assert!(h.contains("class=\"hint\">no such directory"));
        assert!(h.contains("showing the top level"));
        assert!(h.contains("data-rel=\"alpha\"")); // fallback listing still renders
        assert!(!h.contains("class=\"hint\">empty directory"));
    }

    // Baseline: an ordinary, non-empty, successfully-resolved listing shows
    // neither hint — both are edge-case annotations, not part of the normal
    // render.
    #[test]
    fn index_page_normal_listing_shows_neither_hint() {
        let entries = vec![Entry { name: "alpha".into(), rel: "alpha".into(), is_dir: true, git: false }];
        let h = index_page("", &entries, false);
        assert!(!h.contains("class=\"hint\""));
    }
}
