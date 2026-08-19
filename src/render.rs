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
    /// `mailto:`, `tel:`, `#anchor`, empty. Not ours to rewrite.
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
    // Only the FIRST segment can carry a scheme: `a/b:c.md` is an ordinary
    // relative path, and treating its colon as a scheme would stop it being
    // rewritten.
    let first_seg = dest.split('/').next().unwrap_or(dest);
    if let Some(i) = first_seg.find(':') {
        let scheme = &first_seg[..i];
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
    use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag, TagEnd};
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    // Set while an image whose Start was dropped is still open, so its End is
    // dropped too. Images cannot nest, so one flag suffices. Dropping only the
    // Start would leave push_html emitting a stray `" />` into the document.
    let mut dropped_image = false;
    let events = Parser::new_ext(md, opts).filter_map(|ev| match ev {
        // raw HTML from repo content must never reach the page: render it as
        // text. This arm and the link arm below match disjoint Event
        // variants (Html/InlineHtml vs. Start(Tag::Link)), so their relative
        // order does not matter for correctness. What matters: the
        // Event::Html this function itself emits below is already built from
        // escaped values via link_open, so it needs no neutralizing and
        // nothing downstream re-examines it.
        Event::Html(h) => Some(Event::Text(h)),
        Event::InlineHtml(h) => Some(Event::Text(h)),

        Event::Start(Tag::Link { ref dest_url, .. }) => {
            Some(Event::Html(CowStr::from(link_open(dest_url, rel))))
        }

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
    let mut out = String::new();
    html::push_html(&mut out, events);
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

// Whole-tree eager rendering (the old design) is what made this slow: a
// 41k-entry project produced 895 KB of HTML and still hit the 4,000-entry
// budget with ~90% of the tree missing. Instead we render one level at a
// time, IDE-style — only directories on the currently-open file's path are
// expanded inline; everything else renders closed and lazily fetches its
// own children (see the `dir` query param on the `tree` fragment endpoint
// in routes.rs) the first time the user expands it.
/// The lowercased extension a row's icon is keyed on (`data-ext`, consumed by
/// the `[data-ext=…]` rules in style.css). Empty for anything without a usable
/// one, which the stylesheet then renders with its neutral file glyph: a name
/// with no dot ("README"), or a suffix too long or too odd to be a real type.
/// Deliberately permissive about what it gives up on — an unrecognised type
/// costs only a generic icon.
fn icon_ext(name: &str) -> String {
    let ext = name.rsplit('.').next().unwrap_or("");
    if ext == name || ext.is_empty() || ext.len() > 10 || !ext.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return String::new();
    }
    ext.to_ascii_lowercase()
}

/// How deep a row sits, from its own project-relative path. The stylesheet
/// indents each row by `--d` and sizes its indent-guide band from it, rather
/// than nesting padded `<ul>`s — that is what lets a hover or selection bar
/// span the full pane width instead of starting at the indent.
fn depth(rel: &str) -> usize {
    rel.matches('/').count()
}

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
                    "<li><details open data-rel=\"{}\"><summary style=\"--d:{}\">{}</summary><ul>",
                    esc(&erel),
                    depth(&erel),
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
                    "<li><details data-rel=\"{rel}\" hx-get=\"/frag/{proj}/tree?dir={qrel}\" hx-trigger=\"toggle once\" hx-target=\"find ul\"><summary style=\"--d:{d}\">{name}</summary><ul></ul></details></li>",
                    rel = esc(&erel),
                    proj = crate::http::percent_encode(project),
                    qrel = crate::http::percent_encode(&erel),
                    d = depth(&erel),
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
                "<li><a class=\"file{sel}\" data-rel=\"{}\" data-ext=\"{}\" style=\"--d:{}\">{}</a></li>",
                esc(&erel),
                icon_ext(&name),
                depth(&erel),
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
        "<ul class=\"changes\"><li><a class=\"file\" data-kind=\"diff\" data-rel=\"\" hx-get=\"/frag/{project_url}/diff\" hx-target=\"#content\">full diff</a></li>"
    );
    for c in &st.changes {
        out.push_str(&format!(
            "<li><a class=\"file\" data-rel=\"{}\" data-ext=\"{}\" hx-get=\"/frag/{}/diff?path={}\" hx-target=\"#content\"><span class=\"xy\">{}</span>{}</a></li>",
            esc(&c.path),
            icon_ext(c.path.rsplit('/').next().unwrap_or(&c.path)),
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

/// Breadcrumb for the directory picker: "resh" always links back to the
/// top level (`at=""`), every segment but the last is a clickable link to
/// browsing that prefix, and the last segment is plain text (you're already
/// there — the picker doesn't render a `..` row, this is the way up).
fn breadcrumb(at: &str) -> String {
    if at.is_empty() {
        return "<nav class=\"crumbs\"><span class=\"crumb-current\">resh</span></nav>".to_string();
    }
    let mut out = String::from("<nav class=\"crumbs\"><a href=\"/\">resh</a>");
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
/// Same ●/○ distinction as `projects_strip`, matched against `entries` via
/// rel path (`ProjectStatus.url` is the readable slashed form, exactly what
/// `Entry.rel` is), so a directory that turns out to be a known project
/// carries the same live/idle cue in the picker as it does in the header
/// strip — without leaving the picker to open it.
fn project_marker(rel: &str, projects: &[crate::registry::ProjectStatus]) -> &'static str {
    // Wrapped in an element, not emitted as a bare glyph: unstyled it inherited
    // the row's colour and rendered as a dim blob beside an accent-coloured
    // `⎇`, so the louder mark was the less important one. Which projects are
    // RUNNING is the question this page can answer that a plain directory
    // listing cannot, so it gets the accent and the git icon steps back.
    // Titles because a bare dot is not self-explanatory the first time.
    match projects.iter().find(|p| p.url == rel) {
        Some(p) if p.live > 0 => {
            " <span class=\"mark live\" title=\"terminal sessions running\">●</span>"
        }
        Some(_) => " <span class=\"mark idle\" title=\"saved layout, nothing running\">○</span>",
        None => "",
    }
}

fn picker_rows(entries: &[Entry], projects: &[crate::registry::ProjectStatus]) -> String {
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
                let marker = project_marker(&e.rel, projects);
                format!(
                    "<li class=\"dir\" data-rel=\"{rel}\"><span class=\"name\">{name}</span>{marker}{git}</li>",
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
///
/// `projects` (`registry::known_projects`) marks any row that is itself a
/// known project with the same ●/○ the header strip uses — see
/// `project_marker`.
pub fn index_page(at: &str, entries: &[Entry], refused: bool, projects: &[crate::registry::ProjectStatus]) -> String {
    let notice = if refused { hint("no such directory — showing the top level") } else { String::new() };
    let rows_hint = if entries.is_empty() { hint("empty directory") } else { String::new() };
    let rows = if entries.is_empty() { String::new() } else { picker_rows(entries, projects) };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>resh</title>\
         <link rel=\"stylesheet\" href=\"/static/themes/darcula.css\">\
         <link rel=\"stylesheet\" href=\"/static/style.css\">\
         </head><body><header><span class=\"proj\">resh</span></header>\
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

/// The header strip of known projects. ● means live sessions, ○ means a saved
/// layout with nothing running — the distinction that answers "what did I
/// leave running?" without opening anything.
///
/// `projects` arrives pre-ordered by `registry::known_projects` (a
/// `parent: None` row immediately followed by its `parent: Some(that_key)`
/// children), so this just renders in order and lets `parent.is_some()`
/// decide indentation — it never re-sorts or groups on its own. A repo's own
/// branch and a linked worktree's branch render the same way, via `branch`;
/// a worktree git reports but that doesn't canonicalise under any ROOT
/// (`reachable == false`) renders as inert text, never as a link, since
/// opening it is exactly what confinement forbids — but it still renders, so
/// the user isn't left wondering where a worktree they know exists went.
pub fn projects_strip(current_key: &str, projects: &[crate::registry::ProjectStatus]) -> String {
    let mut out = String::from("<span class=\"projstrip\">");
    // Only what is actually running. This panel answers "which projects have
    // shells alive right now" — a question you ask to switch to one or to
    // reclaim resources. "Which projects exist, opened or not" is the front
    // page's job, and it already carries the same ●/○ markers, so listing idle
    // ones here duplicated it for no gain.
    let shown: Vec<&crate::registry::ProjectStatus> =
        projects.iter().filter(|p| p.live > 0).collect();
    // A live worktree whose parent is idle would otherwise render indented
    // under a row that is no longer here, dangling off nothing. Indent only
    // when the parent survived the filter.
    let shown_keys: std::collections::HashSet<&str> =
        shown.iter().map(|p| p.key.as_str()).collect();
    for p in shown {
        let live = p.live > 0;
        let marker = if live { "●" } else { "○" };
        let is_child = p.parent.as_deref().is_some_and(|k| shown_keys.contains(k));
        let mut cls = String::from("proj");
        if is_child {
            cls.push_str(" child");
        }
        if live {
            cls.push_str(" live");
        }
        if p.key == current_key {
            cls.push_str(" current");
        }
        let indent = if is_child { "<span class=\"indent\">└</span> " } else { "" };
        let branch = if p.branch.is_empty() {
            String::new()
        } else {
            format!(" <span class=\"branch\">⎇ {}</span>", esc(&p.branch))
        };
        if !p.reachable {
            cls.push_str(" unreachable");
            out.push_str(&format!(
                "<span class=\"{}\" title=\"worktree outside resh's roots — cannot be opened\">{}{} {}{}</span>",
                cls,
                indent,
                marker,
                esc(&p.url),
                branch
            ));
            continue;
        }
        let title = if live {
            // An unknown age is stated as unknown, never as "0s": right after a
            // restart every project's age is unknown (the socket-file floor
            // supplies a count but no ages), and "oldest 0s" there reads as
            // "everything just started" — the opposite of the truth, on the one
            // question this tooltip exists to answer.
            // "1 sessions" is the common case, not an edge one: a project
            // usually has exactly one shell, so the ungrammatical form was what
            // most rows actually showed.
            let n = plural(p.live, "session");
            match p.oldest_age_secs {
                Some(age) => format!("{n} · oldest {}", human_age(age)),
                None => format!("{n} · age unknown until reattached"),
            }
        } else {
            "saved layout, nothing running".to_string()
        };
        out.push_str(&format!(
            "<a class=\"{}\" href=\"/{}\" target=\"dl-{}\" title=\"{}\">{}{} {}{}</a>",
            cls,
            crate::http::percent_encode(&p.url),
            esc(&p.key),
            esc(&title),
            indent,
            marker,
            esc(&p.url),
            branch
        ));
    }
    out.push_str("</span>");
    out
}

/// Coarse, human-readable age. Precision beyond this is noise when the
/// question is only "is this old enough that I have forgotten it?".
/// `1 session` / `2 sessions`. English only, matching the rest of this file.
fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

pub fn human_age(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3_600 {
        format!("{}h", secs / 3_600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

// `theme_rel` is interpolated into an `href` below without `esc` or
// `percent_encode`, unlike every other value this function interpolates.
// Nothing is injectable today — `routes::theme_link_for` only ever returns
// one of two `'static` literals — but the `'static` bound is what keeps
// that safe: it rules out a future caller deriving `rel` from a filesystem
// name (which would reintroduce attribute-break XSS in the same origin as
// every terminal websocket) without the compiler catching it. Any producer
// still has only literals to reach for, so this is not a call-site change,
// just a tighter promise on the type.
pub fn workspace_page(project: &str, key: &str, s: &Settings, theme_rel: Option<&'static str>) -> String {
    let warn = s
        .warning
        .as_deref()
        .map(|w| format!("<span class=\"warn\" title=\"{}\">⚠ config</span>", esc(w)))
        .unwrap_or_default();
    let proj_url = crate::http::percent_encode(project);
    let proj_txt = esc(project);
    // `key` is the storage-key form (registry::ProjectStatus.key), which
    // only escapes '/' and '%' — it can still carry raw '"', '<', '&' from
    // a filesystem name, so it goes through percent_encode (not esc) before
    // landing in a query string: that keeps it a single, round-trippable
    // percent-escape rather than corrupting the '%XX' the storage key
    // already contains, and it happens to be HTML-attribute-safe too, since
    // percent_encode's output is restricted to plain ASCII.
    let qkey = crate::http::percent_encode(key);
    let theme_css = match theme_rel {
        Some(rel) => format!("<link rel=\"stylesheet\" href=\"/frag/{proj_url}/{rel}\">"),
        None => String::new(),
    };
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{proj_txt} — resh</title>
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
  <button id="projbtn" title="running projects">◆<span id="projcount"></span></button>
  <button id="closeproj" title="close project — ends all its terminal sessions">✕ Close</button>
  <button id="bell" title="notifications (n)">🔔<span id="bellcount"></span></button>
  <button id="refresh" title="refresh (r)">⟳</button>
</header>
<div id="projpanel" hidden><span id="projstrip" hx-get="/frag/_projects?current={qkey}" hx-trigger="load, refresh from:body"></span></div>
<div id="noticepanel" hidden></div>
<main id="grid">
  <section class="pane" data-pane="0"><div class="panehead"><div class="tabstrip"></div><div class="paneicons"></div></div><div class="content"></div></section>
  <div class="divider" data-div="left-split"></div>
  <section class="pane" data-pane="1"><div class="panehead"><div class="tabstrip"></div><div class="paneicons"></div></div><div class="content"></div></section>
  <div class="divider" data-div="left-w"></div>
  <section class="pane" data-pane="2"><div class="panehead"><div class="tabstrip"></div><div class="paneicons"></div></div><div class="content"></div></section>
  <div class="divider" data-div="right-w"></div>
  <section class="pane" data-pane="3"><div class="panehead"><div class="tabstrip"></div><div class="paneicons"></div></div><div class="content"></div></section>
</main>
<!-- Empty by default and hidden: the slots exist so a future per-pane control
     (a split, a kebab, a pane menu) has somewhere to land without reopening
     the header's layout. app.js rebuilds .tabstrip wholesale on every render,
     which is why .paneicons is its sibling rather than its child — anything
     put inside the strip would be wiped on the next state broadcast. -->
<footer id="statusbar" class="hidden"><span class="left"></span><span class="right"></span></footer>
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
        let h = markdown_html("# Hi\n\n- a\n", "proj", "a.md");
        assert!(h.starts_with("<article class=\"markdown-body\">"));
        assert!(h.contains("<h1>Hi</h1>"));
        assert!(h.contains("<li>a</li>"));
    }

    #[test]
    fn markdown_raw_html_is_neutralized() {
        let h = markdown_html(
            "hello <script>alert(1)</script>\n\n<iframe src=x></iframe>\n",
            "proj",
            "a.md",
        );
        assert!(!h.contains("<script>"));
        assert!(!h.contains("<iframe"));
        assert!(h.contains("&lt;script&gt;"));
    }

    #[test]
    fn file_fragment_md_vs_code() {
        let md = file_fragment("proj", "readme.md", "# T");
        assert!(md.contains("markdown-body"));
        let code = file_fragment("proj", "main.rs", "fn x() -> Vec<u8> {}");
        assert!(code.contains("language-rs"));
        assert!(code.contains("Vec&lt;u8&gt;")); // escaped, hljs runs client-side
    }

    #[test]
    fn a_local_link_becomes_a_tab_opening_anchor() {
        let h = markdown_html("see [the plan](plan.md)\n", "proj", "docs/a.md");
        assert!(h.contains(r#"<a class="mdlink" data-rel="docs/plan.md">"#), "{h}");
        assert!(h.contains("the plan</a>"), "the link text must survive: {h}");
        // No href at all: an href would let a click navigate the SPA away before
        // the handler ran, which is the bug this fixes.
        // Verified this assertion can fail: adding `href="{}"` back to the
        // Dest::Local arm made this panic with the anchor rendered as
        // `<a href="plan.md" class="mdlink" data-rel="docs/plan.md">` —
        // i.e. `!h.contains(r#"href="plan.md""#)` failed because the href
        // was right there.
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

    #[test]
    fn a_local_image_points_at_the_raw_route() {
        let h = markdown_html("![a cat](cat.png)\n", "proj", "docs/a.md");
        assert!(h.contains(r#"src="/frag/proj/raw?path=docs/cat.png""#), "{h}");
        assert!(h.contains(r#"alt="a cat""#), "{h}");
    }

    /// Both halves are asserted. "No <img" alone passes if the image vanished
    /// entirely; "alt text present" alone passes if the <img> is still there with
    /// its alt attribute.
    ///
    /// Verified this can fail: reverting the catch-all image arm to keep
    /// emitting `Event::Start(Tag::Image { .. })` for Remote/Passthrough/Broken
    /// made this fail on the first assertion — `!h.contains("<img")` — because
    /// the tag survived with `src="https://e.com/b.png"` intact.
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
    ///
    /// Verified this can fail: replacing `crate::http::percent_encode(&p)` with
    /// `p.replace(' ', "+")` made this fail with a left/right mismatch —
    /// decoding produced "my notes drafts.png" (the `+` read back as a space)
    /// instead of the original "my notes+drafts.png".
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
        assert!(h.contains("<details open data-rel=\"src\"><summary style=\"--d:0\">src</summary>"));
        assert!(h.contains("class=\"file sel\""));
        assert!(h.contains("data-rel=\"src/main.rs\""));
        assert!(h.contains("README.md"));
        assert!(!h.contains(">target</summary>"));
        assert!(!h.contains(">dist</summary>"));
    }

    // Claude Code checks a worktree out at `{repo}/.claude/worktrees/{name}`:
    // a second, full copy of the repository inside the project directory. The
    // tree hides dot-directories nowhere else — it filters on SKIP_DIRS and
    // `hide` and nothing more — so without `.claude` on that list a project
    // renders a duplicate of itself under its own tree.
    #[test]
    fn a_worktree_checked_out_under_dot_claude_stays_out_of_its_parents_tree() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("src")).unwrap();
        fs::write(d.path().join("src/main.rs"), "").unwrap();
        fs::create_dir_all(d.path().join(".claude/worktrees/feat/src")).unwrap();
        fs::write(d.path().join(".claude/worktrees/feat/src/main.rs"), "").unwrap();
        let h = tree_fragment("proj", d.path(), "src/main.rs", &[]);
        // the project's own tree is unaffected
        assert!(h.contains("data-rel=\"src/main.rs\""));
        assert!(!h.contains(".claude"), "the worktree's parent directory must not appear");
        assert!(!h.contains("worktrees"), "nor anything beneath it");
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
             hx-trigger=\"toggle once\" hx-target=\"find ul\"><summary style=\"--d:1\">sub</summary><ul></ul></details>"
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
    // Every row carries the depth the stylesheet indents it by and the
    // extension it keys the row's icon on. Both are pure functions of the
    // row's own path, so a lazily-fetched subtree (tree_level with a non-empty
    // `rel`) has to produce the same values the initial render would have.
    #[test]
    fn rows_carry_their_depth_and_icon_extension() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("src/sub")).unwrap();
        fs::write(d.path().join("src/sub/x.rs"), "").unwrap();
        fs::write(d.path().join("README"), "").unwrap();
        fs::write(d.path().join("Cargo.toml"), "").unwrap();
        let h = tree_fragment("proj", d.path(), "src/sub/x.rs", &[]);
        assert!(h.contains("data-rel=\"src/sub/x.rs\" data-ext=\"rs\" style=\"--d:2\""));
        assert!(h.contains("data-rel=\"Cargo.toml\" data-ext=\"toml\" style=\"--d:0\""));
        // No dot at all: the stylesheet's neutral glyph, not a bogus type.
        assert!(h.contains("data-rel=\"README\" data-ext=\"\" style=\"--d:0\""));

        // The same row, reached through the lazy ?dir= path, agrees.
        let mut lazy = String::new();
        tree_level("proj", &d.path().join("src/sub"), "src/sub", "", &[], &mut lazy);
        assert!(lazy.contains("data-rel=\"src/sub/x.rs\" data-ext=\"rs\" style=\"--d:2\""));
    }

    #[test]
    fn icon_ext_gives_up_on_anything_that_is_not_a_plain_suffix() {
        assert_eq!(icon_ext("main.rs"), "rs");
        assert_eq!(icon_ext("README.MD"), "md"); // case-folded
        assert_eq!(icon_ext(".gitignore"), "gitignore"); // dotfile, suffix is the type
        assert_eq!(icon_ext("README"), ""); // no dot
        assert_eq!(icon_ext("archive.tar.gz"), "gz"); // last suffix wins
        assert_eq!(icon_ext("v1.0-final"), ""); // not alphanumeric
        assert_eq!(icon_ext("x."), ""); // empty suffix
        assert_eq!(icon_ext("notes.supercalifragilistic"), ""); // implausibly long
    }

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
        let h = workspace_page("proj", "proj", &s, Some("theme.css"));
        assert!(h.contains("/static/themes/gruvbox.css"));
        assert!(h.contains("/frag/proj/theme.css")); // has_theme_css
        assert!(h.contains("data-project=\"proj\""));
        assert!(h.contains("data-default-tab=\"terminal\""));
        assert!(h.contains("htmx.min.js"));
        assert!(h.contains("data-pane=\"3\""));
        assert!(h.contains("id=\"termpool\""));
        assert!(h.contains("hx-get=\"/frag/_projects?current=proj\""));
        assert!(h.contains("id=\"projstrip\""));
        assert!(h.contains("id=\"closeproj\""));
        let no_custom = workspace_page("proj", "proj", &s, None);
        assert!(!no_custom.contains("theme.css\">"));
    }

    #[test]
    fn the_workspace_links_exactly_one_theme_stylesheet() {
        let s = Settings::default();
        let dir_themed = workspace_page("proj", "proj", &s, Some("theme/style.css"));
        assert!(dir_themed.contains("/frag/proj/theme/style.css"));
        assert_eq!(dir_themed.matches("theme.css\"").count(), 0, "never both links");

        let file_themed = workspace_page("proj", "proj", &s, Some("theme.css"));
        assert!(file_themed.contains("/frag/proj/theme.css"));
        assert!(!file_themed.contains("/frag/proj/theme/style.css"));

        let bare = workspace_page("proj", "proj", &s, None);
        assert!(!bare.contains("/frag/proj/theme"));
    }

    // The strip's `?current=` value is the storage key, not the URL form —
    // a nested project's key contains a raw '%2F', and that '%' must round
    // trip through the query string as a single percent-escape rather than
    // being corrupted by a second layer of encoding (see workspace_page's
    // `qkey` comment).
    #[test]
    fn workspace_page_percent_encodes_the_current_key_for_the_query_string() {
        let s = Settings::default();
        let h = workspace_page("karpie/src", "karpie%2Fsrc", &s, None);
        assert!(h.contains("hx-get=\"/frag/_projects?current=karpie%252Fsrc\""));
    }

    #[test]
    fn the_workspace_page_carries_the_notification_centre() {
        let s = crate::config::Settings::default();
        let html = workspace_page("proj", "proj", &s, None);
        assert!(html.contains(r#"id="bell""#), "no bell button");
        assert!(html.contains(r#"id="bellcount""#), "no unread badge");
        assert!(html.contains(r#"id="noticepanel""#), "no panel container");
        // The panel is filled from JS with textContent; it must ship empty,
        // or notice text would be interpolated into HTML somewhere.
        assert!(html.contains(r#"<div id="noticepanel" hidden></div>"#), "panel must ship empty");
    }

    #[test]
    fn index_page_renders_picker_rows_and_breadcrumb() {
        let entries = vec![
            Entry { name: "alpha".into(), rel: "alpha".into(), is_dir: true, git: true },
            Entry { name: "beta".into(), rel: "beta".into(), is_dir: true, git: false },
        ];
        let h = index_page("", &entries, false, &[]);
        assert!(h.contains("data-rel=\"alpha\""));
        assert!(h.contains("class=\"dir\""));
        // alpha is a git repo: gets a one-click shortcut straight to its
        // workspace URL, not just the plain ⎇ marker
        assert!(h.contains("<a class=\"git\" href=\"/alpha\" title=\"open this repo\">⎇</a>"));
        // beta is not a git repo: no shortcut anchor for it at all
        assert!(!h.contains("href=\"/beta\""));
        assert!(h.contains("id=\"openBtn\""));
        assert!(h.contains("crumb-current\">resh"));
        assert!(h.contains("/static/picker.js"));

        // browsing a subdirectory: breadcrumb links back up, files are
        // present but not marked selectable the way directories are
        let sub = vec![
            Entry { name: "sub".into(), rel: "karpie/sub".into(), is_dir: true, git: false },
            Entry { name: "main.rs".into(), rel: "karpie/main.rs".into(), is_dir: false, git: false },
        ];
        let h2 = index_page("karpie", &sub, false, &[]);
        assert!(h2.contains("<a href=\"/\">resh</a>"));
        assert!(h2.contains("crumb-current\">karpie"));
        assert!(h2.contains("class=\"dir\" data-rel=\"karpie/sub\""));
        assert!(h2.contains("class=\"file\""));
        assert!(!h2.contains("data-rel=\"karpie/main.rs\"")); // files carry no selection hook
    }

    // A picker row for a directory that is also a known project carries the
    // same ●/○ the header strip uses; an ordinary directory with no
    // matching project carries neither.
    #[test]
    fn picker_rows_mark_known_projects_live_or_idle() {
        let entries = vec![
            Entry { name: "karpie".into(), rel: "karpie".into(), is_dir: true, git: false },
            Entry { name: "glow".into(), rel: "glow".into(), is_dir: true, git: false },
            Entry { name: "plain".into(), rel: "plain".into(), is_dir: true, git: false },
        ];
        let ps = vec![
            crate::registry::ProjectStatus {
                key: "karpie".into(), url: "karpie".into(),
                live: 2, oldest_age_secs: Some(60), has_layout: true,
                branch: String::new(), parent: None, reachable: true,
            },
            crate::registry::ProjectStatus {
                key: "glow".into(), url: "glow".into(),
                live: 0, oldest_age_secs: None, has_layout: true,
                branch: String::new(), parent: None, reachable: true,
            },
        ];
        let h = index_page("", &entries, false, &ps);
        // The marker must be an *element*, not a bare glyph: as plain text it
        // inherited the row colour and was quieter than the accent-coloured git
        // icon next to it, so the least important mark was the loudest. The
        // class is what lets CSS put liveness first, so assert on it.
        assert!(
            h.contains("<span class=\"name\">karpie</span> <span class=\"mark live\""),
            "a live project's marker must carry the live class, not just the glyph"
        );
        assert!(h.contains(">●</span>"), "live project row carries ●");
        assert!(
            h.contains("<span class=\"name\">glow</span> <span class=\"mark idle\""),
            "an idle project's marker must carry the idle class"
        );
        assert!(h.contains(">○</span>"), "idle-but-known project row carries ○");
        assert!(h.contains("<span class=\"name\">plain</span></li>"), "unknown directory carries neither marker");
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
        let h = index_page("karpie", &entries, false, &[]);
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
        let h = index_page("a/b/c", &[], false, &[]);
        assert!(h.contains("<a href=\"/?at=a\">a</a>"));
        assert!(h.contains("<a href=\"/?at=a/b\">b</a>"));
        assert!(h.contains("crumb-current\">c")); // the current directory itself is not a link
    }

    #[test]
    fn project_name_is_escaped_everywhere() {
        let s = Settings::default();
        let h = workspace_page("a\"><script>", "a\"><script>", &s, None);
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
        let h = index_page("karpie", &[], false, &[]);
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
        let h = index_page("", &entries, true, &[]);
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
        let h = index_page("", &entries, false, &[]);
        assert!(!h.contains("class=\"hint\""));
    }

    /// The panel lists what is RUNNING. An idle project — a saved layout with
    /// no shells — belongs on the front page, which already carries the same
    /// ●/○ markers; listing it here duplicated that at the cost of header
    /// space. Asserts the idle one is absent, not merely that the live one is
    /// present, since the latter alone would pass with the filter deleted.
    #[test]
    fn strip_lists_only_projects_with_running_sessions() {
        let ps = vec![
            crate::registry::ProjectStatus {
                key: "karpie".into(), url: "karpie".into(),
                live: 2, oldest_age_secs: Some(8 * 3600), has_layout: true,
                branch: String::new(), parent: None, reachable: true,
            },
            crate::registry::ProjectStatus {
                key: "glow".into(), url: "glow".into(),
                live: 0, oldest_age_secs: None, has_layout: true,
                branch: String::new(), parent: None, reachable: true,
            },
        ];
        let h = projects_strip("karpie", &ps);
        assert!(h.contains("target=\"dl-karpie\""), "links reuse a named browsing context");
        assert!(h.contains("href=\"/karpie\""));
        assert!(h.contains("class=\"proj live current\"") || h.contains("current"));
        assert!(
            !h.contains("glow"),
            "an idle project must not appear: this panel answers what is running, and the \
             front page already lists everything else"
        );
        assert!(h.contains("2 sessions"), "the tooltip must carry the session count");
        // The headline behaviour: ● pinned to the live project, ○ pinned to
        // the idle one — not just "a ● and a ○ appear somewhere". Swapping
        // the two glyphs in projects_strip must fail this test.
        assert!(h.contains(">● karpie</a>"), "live project must be marked with the filled dot");
        assert!(!h.contains("○"), "with idle projects filtered out, no hollow dot can remain");
        assert!(h.contains("oldest 8h"), "a known age must be rendered, coarsely");
    }

    /// A live project whose age is unknown — the normal state right after a
    /// restart, when the socket-file floor gives a count but no ages — must say
    /// so. It used to render "oldest 0s", claiming every shell had just started
    /// at precisely the moment "what did I leave running for days?" is the
    /// question being asked. Asserting the *absence* of "0s" as well, so
    /// reverting to the old formatting fails here rather than passing quietly.
    /// "1 sessions" was what most rows actually showed, since a project usually
    /// has exactly one shell — so the singular is the common case here, not an
    /// edge one. Asserts the absence of the plural form too, or reverting the
    /// fix would leave this green.
    #[test]
    fn a_single_session_is_not_described_in_the_plural() {
        let one = vec![crate::registry::ProjectStatus {
            key: "solo".into(), url: "solo".into(),
            live: 1, oldest_age_secs: Some(3600), has_layout: true,
            branch: String::new(), parent: None, reachable: true,
        }];
        let h = projects_strip("other", &one);
        assert!(h.contains("1 session ·"), "expected the singular: {h}");
        assert!(!h.contains("1 sessions"), "the plural must not survive: {h}");

        // …and two must still be plural, so this cannot be "fixed" by dropping
        // the s everywhere.
        let two = vec![crate::registry::ProjectStatus {
            key: "duo".into(), url: "duo".into(),
            live: 2, oldest_age_secs: None, has_layout: true,
            branch: String::new(), parent: None, reachable: true,
        }];
        assert!(projects_strip("other", &two).contains("2 sessions"), "two must stay plural");
    }

    #[test]
    fn strip_says_unknown_rather_than_zero_when_no_age_is_available() {
        let ps = vec![crate::registry::ProjectStatus {
            key: "karpie".into(), url: "karpie".into(),
            live: 3, oldest_age_secs: None, has_layout: false,
            branch: String::new(), parent: None, reachable: true,
        }];
        let h = projects_strip("other", &ps);
        assert!(h.contains("3 sessions"), "the count is known even when the age is not");
        assert!(
            h.contains("unknown"),
            "an unavailable age must be stated as unknown: {h}"
        );
        assert!(
            !h.contains("oldest 0s"),
            "must never pass an unknown age off as a brand-new session: {h}"
        );
    }

    #[test]
    fn strip_escapes_project_names() {
        let ps = vec![
            crate::registry::ProjectStatus {
                key: "a%3Cb".into(), url: "a<b".into(),
                live: 1, oldest_age_secs: None, has_layout: true,
                branch: String::new(), parent: None, reachable: true,
            },
            // storage_key only escapes '/' and '%' — a raw '"' from a
            // filesystem name can reach `key` unescaped, so this must be
            // neutralized here, in the target="dl-{key}" attribute, not
            // just in a text position (a fixture with no HTML metacharacter
            // at all, like the entry above, would pass even with `esc`
            // deleted from that call site).
            crate::registry::ProjectStatus {
                key: "a\" onmouseover=x".into(), url: "b".into(),
                live: 1, oldest_age_secs: None, has_layout: true,
                branch: String::new(), parent: None, reachable: true,
            },
        ];
        let h = projects_strip("", &ps);
        assert!(h.contains("a&lt;b"), "the visible label must be HTML-escaped");
        assert!(!h.contains("<b\""), "a name must never break out of the markup");
        assert!(
            h.contains("target=\"dl-a&quot; onmouseover=x\""),
            "the target attribute must be escaped too, not just the visible text"
        );
        assert!(!h.contains("dl-a\" "), "a quote in the key must not break out of the target attribute");
    }

    // The header strip's grouping contract: `known_projects` hands
    // `projects_strip` a parent immediately followed by its children, and
    // this renders that order — a linked worktree's row is indented under
    // its repo, and both the repo's own branch and the worktree's branch
    // are shown (they differ only by branch, per the module's own reason
    // for existing).
    #[test]
    fn strip_groups_worktrees_under_their_parent_labelled_by_branch() {
        let ps = vec![
            crate::registry::ProjectStatus {
                key: "ultima_marketing".into(), url: "ultima_marketing".into(),
                live: 2, oldest_age_secs: Some(60), has_layout: true,
                branch: "main".into(), parent: None, reachable: true,
            },
            crate::registry::ProjectStatus {
                key: "ultima_marketing%2F.claude%2Fworktrees%2Fsite-launch".into(),
                url: "ultima_marketing/.claude/worktrees/site-launch".into(),
                live: 1, oldest_age_secs: None, has_layout: false,
                branch: "site-launch".into(),
                parent: Some("ultima_marketing".into()),
                reachable: true,
            },
        ];
        let h = projects_strip("", &ps);
        // both branches are shown, not just the parent's
        assert!(h.contains("⎇ main"), "the repo's own branch must be shown");
        assert!(h.contains("⎇ site-launch"), "the worktree's branch must be shown");
        // the child row is visually indented and carries a distinct class —
        // not just "somewhere in the same string as the parent"
        assert!(
            h.contains("<span class=\"indent\">└</span>"),
            "a worktree row must render indented under its parent"
        );
        assert!(
            h.contains("class=\"proj child live\""),
            "a worktree row must carry a class distinguishing it from a top-level project"
        );
        // it's a real, reachable worktree: still a genuine link to its own
        // workspace URL, not merely decorative text
        assert!(h.contains("href=\"/ultima_marketing/.claude/worktrees/site-launch\""));
    }

    // Confinement (not the dot-segment allowlist) is what forbids opening a
    // worktree outside ROOTS — but git still reports it exists, so it must
    // render, dimmed and unclickable, rather than vanish and leave the user
    // wondering where it went.
    #[test]
    fn strip_renders_an_unreachable_worktree_without_a_link() {
        let ps = vec![
            crate::registry::ProjectStatus {
                key: "repo".into(), url: "repo".into(),
                live: 1, oldest_age_secs: None, has_layout: true,
                branch: "main".into(), parent: None, reachable: true,
            },
            crate::registry::ProjectStatus {
                // Live *and* unreachable is not a contradiction: ROOTS can
                // change after a session was started, leaving a running shell
                // on a path this instance may no longer open. That is exactly
                // when the user needs to see it, and the only case this panel
                // renders an unreachable row at all now that it lists running
                // projects only.
                key: "%2Felsewhere%2Fstray".into(), url: "/elsewhere/stray".into(),
                live: 1, oldest_age_secs: None, has_layout: false,
                branch: "wip".into(), parent: Some("repo".into()), reachable: false,
            },
        ];
        let h = projects_strip("", &ps);
        // still shown — never silently omitted
        assert!(h.contains("/elsewhere/stray"));
        assert!(h.contains("⎇ wip"));
        assert!(h.contains("unreachable"), "an unreachable worktree must carry a distinct, dimmable class");
        // but never as a clickable link to a path opening it would refuse anyway
        assert!(
            !h.contains("href=\"/elsewhere/stray\""),
            "an unreachable worktree must never render as a link"
        );
        // the reachable parent right beside it must be entirely unaffected
        assert!(h.contains("href=\"/repo\""));
    }

    #[test]
    fn human_age_picks_the_coarsest_unit_that_fits() {
        assert_eq!(human_age(0), "0s");
        assert_eq!(human_age(59), "59s");
        assert_eq!(human_age(60), "1m");
        assert_eq!(human_age(3599), "59m");
        assert_eq!(human_age(3600), "1h");
        assert_eq!(human_age(86399), "23h");
        assert_eq!(human_age(86400), "1d");
    }

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

    /// `find(':')` alone would classify `a/b:c.md` — a colon in a LATER segment,
    /// which is a perfectly ordinary relative path — as a scheme and stop
    /// rewriting it. A colon in the FIRST segment is genuinely ambiguous, and
    /// resolves as a scheme here for the same reason a browser resolves it that
    /// way: a Remote or Passthrough destination is handed to the browser verbatim,
    /// so our classification has to agree with the browser's or the two disagree
    /// about the same string. `./` is the standard escape hatch.
    ///
    /// Revert-the-fix check: replacing the scheme guard with a bare
    /// `dest.contains(':')` made this fail with
    /// `left: Remote, right: Local("notes/v:1.md")` on the first assertion —
    /// the colon in the second path segment was misread as a scheme.
    #[test]
    fn a_colon_is_a_scheme_only_where_a_browser_would_read_one() {
        assert_eq!(resolve_dest("notes/v:1.md", "a.md"), Dest::Local("notes/v:1.md".into()));
        assert_eq!(resolve_dest("./notes:1.md", "a.md"), Dest::Local("notes:1.md".into()));
        assert_eq!(resolve_dest("notes:1.md", "a.md"), Dest::Remote);
    }
}
