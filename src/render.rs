//! All HTML generation. Plain string building, no template engine.
//! Fragments target htmx swap sites; pages are full documents.
use crate::config::Settings;
use crate::gitio::Status;
use crate::projects::Project;
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

pub fn index_page(projects: &[Project]) -> String {
    let rows: String = projects
        .iter()
        .map(|p| {
            format!(
                "<li><a href=\"/{0}\">{0}</a><span class=\"path\">{1}{2}</span></li>",
                esc(&p.name),
                esc(&p.path.to_string_lossy()),
                if p.git { " ⎇" } else { "" }
            )
        })
        .collect();
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>deadlight</title>\
         <link rel=\"stylesheet\" href=\"/static/themes/dark.css\">\
         <link rel=\"stylesheet\" href=\"/static/style.css\">\
         </head><body><header><span class=\"proj\">deadlight</span></header>\
         <main><ul class=\"projects\">{rows}</ul></main></body></html>"
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
    fn index_page_lists_projects() {
        let ps = vec![Project { name: "alpha".into(), path: "/tmp/alpha".into(), git: true }];
        let h = index_page(&ps);
        assert!(h.contains("href=\"/alpha\""));
        assert!(h.contains("/tmp/alpha"));
    }

    #[test]
    fn project_name_is_escaped_everywhere() {
        let s = Settings::default();
        let h = workspace_page("a\"><script>", &s, false);
        assert!(!h.contains("a\"><script>"));
        let c = changes_fragment("a\"><script>", &Status { branch: String::new(), changes: vec![crate::gitio::Change { xy: "??".into(), path: "x".into() }] });
        assert!(!c.contains("\"><script>"));
    }
}
