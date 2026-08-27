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

/// The hunk view for an `openDiff` proposal Claude is still waiting on an
/// answer for. Same shape as the `diff` fragment's own output (a `.path`
/// breadcrumb over a `.diffview` of `diff_html`-classified lines) — reusing
/// `textdiff::unified` rather than a second diff implementation, the same
/// way the save-conflict banner does: a proposal compares disk against
/// content that has never been written, exactly the case `textdiff.rs`
/// exists for.
///
/// Was a client-side port of `textdiff::unified` in `static/app.js` (a
/// second implementation of the trim/LCS/context-cap algorithm in a second
/// language); review caught it had already drifted — an empty `old_text`
/// produced a phantom `-` line there that Rust's `str::lines()` never
/// would. This is the one implementation both the conflict banner and a
/// proposal now render through.
///
/// The `.proposalview` wrapper is the one departure from the `diff`
/// fragment's shape, and it is a layout element, not decoration: a proposal
/// is the only fragment a pane must *fill* rather than merely scroll,
/// because its Accept/Reject bar has to stay reachable without scrolling
/// past the diff to find it. It carries the `height: 100%` flex column that
/// makes that true (style.css, next to `.editwrap`, which fills its pane the
/// same way), and `app.js`'s `renderProposal` inserts the edit box and the
/// action bar *into* it — appending them beside it would put them outside
/// the column and undo both properties.
pub fn proposal_fragment(rel: &str, old_text: &str, new_text: &str) -> String {
    format!(
        "<div class=\"proposalview\"><div class=\"path\">{}</div>\
         <div class=\"diffview\">{}</div></div>",
        esc(rel),
        diff_html(&crate::textdiff::unified(old_text, new_text)),
    )
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

/// The destination's URL scheme, lowercased, or `None` if it carries none.
///
/// A URL scheme is `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":"`. Testing
/// for a bare ':' would misread `notes:1.md` — a legal filename — as a scheme
/// and stop rewriting it. Only the FIRST segment can carry one: `a/b:c.md` is
/// an ordinary relative path.
///
/// `resolve_dest` and `link_open` both need this answer, and they must not
/// disagree: `resolve_dest` decides whether a destination is ours to rewrite,
/// while `link_open` decides whether it may carry a live `href` at all.
fn scheme_of(dest: &str) -> Option<String> {
    let first_seg = dest.split('/').next().unwrap_or(dest);
    let i = first_seg.find(':')?;
    let scheme = &first_seg[..i];
    let is_scheme = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    is_scheme.then(|| scheme.to_ascii_lowercase())
}

pub fn resolve_dest(dest: &str, from_rel: &str) -> Dest {
    if dest.is_empty() || dest.starts_with('#') {
        return Dest::Passthrough;
    }
    if dest.starts_with("//") {
        return Dest::Remote;
    }
    if let Some(scheme) = scheme_of(dest) {
        return match scheme.as_str() {
            "data" => Dest::Data,
            "mailto" | "tel" => Dest::Passthrough,
            _ => Dest::Remote,
        };
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
/// The only schemes a markdown link may carry a live `href` for.
///
/// Deny-by-default, the posture `assets::class_of` and `IMAGE_EXT` already
/// take. Blacklisting `javascript:` would be the wrong shape: `vbscript:` and
/// `data:text/html` hand over control the same way, and so will whatever a
/// browser adds next. Clicking a link in a cloned repo's README runs in the
/// workspace origin — the origin that drives every terminal websocket — so
/// anything not on this list renders inert instead.
///
/// A `data:` IMAGE is deliberately not held to this: it is self-contained and
/// renders with no user action, while a link is a click that hands control to
/// whatever the scheme names.
const HREF_SCHEMES: &[&str] = &["http", "https", "mailto", "tel"];

/// A markdown link's title (`[t](b.md "my title")`) as an attribute, or
/// nothing. Every link form that survives keeps it: it is the tooltip the
/// author wrote, and dropping it silently was a regression from before
/// `link_open` started building the tag itself.
fn title_attr(title: &str) -> String {
    if title.is_empty() {
        String::new()
    } else {
        format!(" title=\"{}\"", esc(title))
    }
}

fn link_open(dest: &str, title: &str, from_rel: &str) -> String {
    // Inert: no href, no data-rel, nothing derived from the destination, so
    // neither the browser nor the client will follow it.
    const INERT: &str = "<a class=\"mdbroken\">";
    let resolved = resolve_dest(dest, from_rel);
    // Deliberately no href — `wireFileLinks` opens it as a tab, and an href
    // would race that handler by navigating the workspace away.
    if let Dest::Local(p) = &resolved {
        return format!(
            "<a class=\"mdlink\" data-rel=\"{}\"{}>",
            esc(p),
            title_attr(title)
        );
    }
    if let Some(scheme) = scheme_of(dest) {
        if !HREF_SCHEMES.contains(&scheme.as_str()) {
            return INERT.to_string();
        }
    }
    match resolved {
        // Kept, because a link is a deliberate click that shows its target,
        // unlike an image's automatic fetch. `_blank` stops it replacing the
        // workspace; `noopener` denies it `window.opener`; `noreferrer` keeps
        // the workspace URL out of the request.
        Dest::Remote => format!(
            "<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\"{}>",
            esc(dest),
            title_attr(title)
        ),
        // `mailto:`, `tel:` and in-page `#anchor`s — the only href-bearing
        // forms left, since every other scheme was rejected above.
        Dest::Passthrough => format!("<a href=\"{}\"{}>", esc(dest), title_attr(title)),
        // `Dest::Data` (already refused by the allowlist, since "data" is not
        // on it) and `Dest::Broken`. `Dest::Local` returned above.
        _ => INERT.to_string(),
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

        Event::Start(Tag::Link { ref dest_url, ref title, .. }) => {
            Some(Event::Html(CowStr::from(link_open(dest_url, title, rel))))
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

/// An image opened as a tab. Not `file_fragment`'s business, because that
/// function's whole contract is that it has already been handed the file's
/// text — which for an image does not exist.
pub fn image_fragment(project: &str, rel: &str, mtime_secs: u64) -> String {
    format!(
        // `v` is a cache key, not a parameter the route reads: an <img> whose
        // src is unchanged is served from the browser's cache, so a re-mount
        // after the file changed would repaint the old picture.
        "<div class=\"path\">{}</div><img class=\"imgview\" src=\"/frag/{}/raw?path={}&v={}\" alt=\"{}\">",
        esc(rel),
        crate::http::percent_encode(project),
        crate::http::percent_encode(rel),
        mtime_secs,
        esc(rel)
    )
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

pub fn tree_fragment(
    project: &str,
    dir: &Path,
    open: &str,
    filter: &crate::projects::TreeFilter,
) -> String {
    let mut out = String::from("<ul class=\"tree\">");
    tree_level(project, dir, "", open, filter, &mut out);
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
pub fn tree_level(
    project: &str,
    dir: &Path,
    rel: &str,
    open: &str,
    filter: &crate::projects::TreeFilter,
    out: &mut String,
) {
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
        if filter.skips(&name) {
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
                tree_level(project, &e.path(), &erel, open, filter, out);
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

// `#gitinfo` (whose content this fills) is only ever rendered inside the
// header chip now, next to the SVG_BRANCH icon — the icon *is* the branch
// marker there, so a second textual "⎇ " here would double it. The other
// three renderers of a branch name (projects_strip, worktrees_strip, the
// picker) have no icon beside their rows and keep the "⎇ " prefix.
/// Categorise a porcelain-v2 XY code. X (index) and Y (worktree) are each a
/// real status letter or `.` for unchanged; an untracked entry is the
/// synthetic "??" the parser emits. A file can be both staged and modified
/// (e.g. "MM"), and is counted in both — matching what `git diff --cached`
/// and `git diff` report separately, which is the convention the reference
/// statusline follows.
fn categorise(xy: &str) -> (bool, bool, bool) {
    if xy == "??" {
        return (false, false, true); // untracked
    }
    let mut cs = xy.chars();
    let staged = cs.next().is_some_and(|x| x != '.'); // index side
    let modified = cs.next().is_some_and(|y| y != '.'); // worktree side
    (staged, modified, false)
}

/// The header chip's git status, in the shape a developer's shell prompt uses:
/// `branch ● +staged ~modified ↑ahead ↓behind`. The bullet is the at-a-glance
/// signal — red when the tree is dirty, green when clean — and each count is
/// omitted when zero, so a clean, up-to-date branch is just its name and a
/// green dot. A plain-language `title` on the wrapper spells the same state
/// out for hover, because a row of coloured glyphs explains nothing to
/// someone who has not memorised the convention.
pub fn status_fragment(st: &Status) -> String {
    if st.branch.is_empty() {
        // Not a git repo (or git could not answer): render nothing rather
        // than an empty dirty/clean claim.
        return String::new();
    }
    let (mut staged, mut modified, mut untracked) = (0u32, 0u32, 0u32);
    for c in &st.changes {
        let (s, m, u) = categorise(&c.xy);
        staged += s as u32;
        modified += m as u32;
        untracked += u as u32;
    }
    let dirty = staged + modified + untracked > 0;

    let mut parts = format!("<span id=\"branch\">{}</span>", esc(&st.branch));
    parts.push_str(&format!(
        " <span class=\"gbullet {}\">●</span>",
        if dirty { "dirty" } else { "clean" }
    ));
    if staged > 0 {
        parts.push_str(&format!(" <span class=\"gstaged\">+{staged}</span>"));
    }
    if modified > 0 {
        parts.push_str(&format!(" <span class=\"gmod\">~{modified}</span>"));
    }
    // Untracked has no count glyph of its own (it would crowd the chip); it
    // reddens the bullet and is named in the tooltip. The reference statusline
    // makes the same call.
    if st.ahead > 0 {
        parts.push_str(&format!(" <span class=\"gahead\">↑{}</span>", st.ahead));
    }
    if st.behind > 0 {
        parts.push_str(&format!(" <span class=\"gbehind\">↓{}</span>", st.behind));
    }

    // The tooltip: the same state as a sentence. Built from the same numbers,
    // so it can never disagree with the glyphs.
    let mut bits: Vec<String> = Vec::new();
    if staged > 0 {
        bits.push(format!("{staged} staged"));
    }
    if modified > 0 {
        bits.push(format!("{modified} modified"));
    }
    if untracked > 0 {
        bits.push(format!("{untracked} untracked"));
    }
    let changed = if bits.is_empty() { "working tree clean".to_string() } else { bits.join(", ") };
    let sync = if st.upstream.is_empty() {
        "no upstream set".to_string()
    } else if st.ahead == 0 && st.behind == 0 {
        format!("up to date with {}", st.upstream)
    } else {
        let mut ab = Vec::new();
        if st.ahead > 0 {
            ab.push(format!("{} ahead", st.ahead));
        }
        if st.behind > 0 {
            ab.push(format!("{} behind", st.behind));
        }
        format!("{} {}", ab.join(", "), st.upstream)
    };
    let title = esc(&format!("On {} · {changed} · {sync}", st.branch));

    format!("<span id=\"gitstatus\" title=\"{title}\">{parts}</span>")
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
        // Per-worktree state (Claude, dirty, ahead) and, when every axis is
        // positively clean, a remove control. Only present when the caller
        // asked for it (`known_projects_with_state`) — `p.wt` is `None`
        // otherwise, and `None` renders no state at all, never "clean".
        let state_html = match &p.wt {
            None => String::new(),
            Some(w) => {
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
                let remove = if crate::registry::removable(w, p.live) {
                    format!(" <button class=\"wtremove\" data-key=\"{}\" title=\"remove this worktree and its branch\">✕</button>", esc(&p.key))
                } else {
                    String::new()
                };
                format!(" · {claude} {dirty} {ahead}{remove}")
            }
        };
        // A `<button>` cannot nest inside an `<a>` (invalid HTML), so the row
        // is a flex span wrapping the link and the state/control separately.
        out.push_str(&format!(
            "<span class=\"wtrow\"><a class=\"{cls}\" href=\"/{}\" title=\"{}\">{marker} {}{branch}</a>{state_html}</span>",
            crate::http::percent_encode(&p.url),
            esc(&p.url),
            esc(name),
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

/// `sharing_on` is passed in rather than read here: it is a *global-only*
/// setting (`config::share_selection`), and this function stays pure so its
/// tests can drive both states without touching the developer's real
/// `~/.config/resh/config.toml`.
pub fn workspace_page(
    project: &str,
    key: &str,
    s: &Settings,
    theme_rel: Option<&'static str>,
    sharing_on: bool,
    launches: &[&str],
) -> String {
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
    // The config file's value, embedded once per page load rather than
    // resolved into every State snapshot — those go out on each debounced
    // keystroke, and this changes only when someone edits a config file,
    // which already needs a reload to be seen. app.js resolves the workspace
    // override against it: `ws.show_hidden ?? SHOW_HIDDEN_DEFAULT`.
    let sh = if s.show_hidden { "1" } else { "0" };
    let autosave = if s.autosave { "1" } else { "0" };
    let share_selection = if sharing_on { "1" } else { "0" };
    // The launch buttons the tab strip may show, space-separated so a second
    // program is one more word, not one more attribute. Names are the wire
    // names of `proto::Launch`, which the client sends straight back.
    let launches = esc(&launches.join(" "));
    // Rendered only when the key is on — never a `hidden` element the client
    // toggles, and never present-but-empty. This is the whole visibility
    // half of the "off by default, visible when on" contract: a highlighted
    // line of `.env` leaving the host with no indicator on the page at all
    // would be exactly the silent exfiltration this feature exists to avoid.
    // The same reason the header shows which projects have shells running.
    let sharing_indicator = if sharing_on {
        r#"<span id="sharing" title="the editor's current selection is sent to Claude as context on every change">⧉ sharing selection</span>"#
    } else {
        ""
    };
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
<link rel="stylesheet" href="/static/vendor/code-input.min.css">
<link rel="stylesheet" href="/static/themes/{theme}.css">
<link rel="stylesheet" href="/static/style.css">
{theme_css}
<script src="/static/vendor/htmx.min.js"></script>
<script src="/static/vendor/xterm.js"></script>
<script src="/static/vendor/xterm-addon-fit.js"></script>
<script src="/static/vendor/highlight.min.js"></script>
<script src="/static/vendor/code-input.min.js"></script>
</head><body data-project="{proj_txt}" data-key="{qkey}" data-default-tab="{tab}" data-show-hidden="{sh}" data-autosave="{autosave}" data-share-selection="{share_selection}" data-launches="{launches}">
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
        tab = esc(&s.default_tab),
        SVG_HOME = SVG_HOME,
        SVG_BRANCH = SVG_BRANCH,
        SVG_SEARCH = SVG_SEARCH,
        SVG_DIAMOND = SVG_DIAMOND,
        SVG_BELL = SVG_BELL,
        SVG_GEAR = SVG_GEAR,
        SVG_REFRESH = SVG_REFRESH,
        SVG_X = SVG_X,
    )
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::TreeFilter;
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

    /// Both halves must sit *inside* one `.proposalview` element, because
    /// that element is what carries `height: 100%` and the flex column —
    /// the pane-filling layout, the scrolling `.diffview`, and the pinned
    /// action bar all hang off it, and `app.js`'s `renderProposal` inserts
    /// the edit box and the Accept/Reject bar into it by query.
    ///
    /// Asserting on containment rather than `contains("proposalview")`: a
    /// stray empty wrapper emitted *beside* the two divs would satisfy a
    /// substring check while leaving every one of those properties broken,
    /// and `querySelector` in app.js would then append into an element that
    /// is not an ancestor of the diff.
    #[test]
    fn proposal_fragment_wraps_both_halves_in_one_layout_element() {
        let h = proposal_fragment("a.rs", "old\n", "new\n");
        assert!(h.starts_with("<div class=\"proposalview\">"), "wrapper must open the fragment: {h}");
        assert!(h.ends_with("</div>"), "wrapper must close the fragment: {h}");
        let open = h.find("<div class=\"proposalview\">").unwrap();
        let path = h.find("class=\"path\"").expect("path div");
        let diff = h.find("class=\"diffview\"").expect("diffview div");
        assert!(open < path && path < diff, "both halves must fall inside the wrapper: {h}");
        assert_eq!(h.matches("proposalview").count(), 1, "exactly one wrapper: {h}");
    }

    /// The property the JS port had no test for at all before it was
    /// deleted: two distant single-line changes must render as two
    /// separate `@@` hunks, with the untouched middle omitted — proving
    /// `unified`'s own multi-hunk behavior actually reaches the browser
    /// through this fragment, not just through textdiff.rs's own suite.
    #[test]
    fn proposal_fragment_reports_distant_changes_as_separate_hunks() {
        let numbered = |n: usize| (1..=n).map(|i| format!("line {i}\n")).collect::<String>();
        let old = numbered(60);
        let new = old.replace("line 10\n", "line 10 edited\n").replace("line 50\n", "line 50 edited\n");
        let h = proposal_fragment("a.rs", &old, &new);
        let hunks = h.matches("dl hunk").count();
        assert_eq!(hunks, 2, "one header per hunk, two hunks: {h}");
        assert!(!h.contains("line 30<"), "the untouched middle must not be included: {h}");
    }

    /// Two changes close enough that their context windows overlap must
    /// merge into one hunk, not render as two with a redundant context
    /// line printed twice between them.
    #[test]
    fn proposal_fragment_merges_nearby_changes_into_one_hunk() {
        let numbered = |n: usize| (1..=n).map(|i| format!("line {i}\n")).collect::<String>();
        let old = numbered(20);
        // Two edits four lines apart — within CONTEXT (3) of each other's window.
        let new = old.replace("line 10\n", "line 10 edited\n").replace("line 14\n", "line 14 edited\n");
        let h = proposal_fragment("a.rs", &old, &new);
        let hunks = h.matches("dl hunk").count();
        assert_eq!(hunks, 1, "close changes must merge into one hunk, got: {h}");
    }

    /// textdiff.rs's MAX_DIVERGENT_LINES fallback (a wholly divergent pair
    /// is quadratic to align and useless to read) has to survive the trip
    /// through this fragment too, or a huge proposal would park the socket
    /// thread the way the save-conflict banner used to.
    #[test]
    fn proposal_fragment_falls_back_to_a_summary_for_a_wholly_divergent_pair() {
        let old: String = (0..1100).map(|i| format!("disk {i}\n")).collect();
        let new: String = (0..1100).map(|i| format!("proposed {i}\n")).collect();
        let h = proposal_fragment("a.rs", &old, &new);
        assert!(h.contains("too different to show"), "must say why, not dump either file: {h}");
        assert!(h.matches("<div").count() < 20, "and must not dump either file: {h}");
    }

    /// The breadcrumb names the file, and — since a proposal's `rel` and
    /// content both arrive off a socket — everything interpolated has to be
    /// escaped, per CLAUDE.md.
    #[test]
    fn proposal_fragment_escapes_the_path_and_the_content() {
        let h = proposal_fragment("<script>.rs", "a", "a<b");
        assert!(!h.contains("<script>"), "the path must be escaped: {h}");
        assert!(h.contains("&lt;script&gt;"), "{h}");
        assert!(h.contains("a&lt;b"), "the proposed content must be escaped too: {h}");
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

    /// A `javascript:` href in a preview is script execution in the workspace
    /// origin — the origin that drives every terminal websocket — one click
    /// after opening a cloned repo's README. The page sends no `script-src`,
    /// so nothing else would stop it.
    ///
    /// Mixed case is asserted because a scheme is case-insensitive to the
    /// browser: a check that compared the raw string would let `JaVaScRiPt:`
    /// straight through.
    ///
    /// Verified this can fail: with `HREF_SCHEMES` widened to also contain
    /// "javascript", the first assertion panicked with
    /// `<a href="javascript:alert(document.domain)" target="_blank"
    /// rel="noopener noreferrer">click</a>` — the live href right there in the
    /// output. The mixed-case case failed identically.
    #[test]
    fn a_javascript_link_is_inert_in_any_casing() {
        for md in [
            "[click](javascript:alert(document.domain))\n",
            "[click](JaVaScRiPt:alert(document.domain))\n",
        ] {
            let h = markdown_html(md, "proj", "a.md");
            assert!(!h.contains("href="), "no href may survive: {h}");
            // Not just "no href": the scheme must not reach the page in any
            // attribute or as text, or a later change that reintroduced it
            // somewhere else would still pass.
            assert!(!h.to_ascii_lowercase().contains("javascript"), "{h}");
            assert!(h.contains(r#"<a class="mdbroken">"#), "{h}");
            assert!(h.contains("click</a>"), "the link text must survive: {h}");
        }
    }

    /// The allowlist is what makes this work: `data:text/html` was never
    /// spelled out anywhere as dangerous, and a blacklist of `javascript:`
    /// would have shipped it as a live href.
    ///
    /// The `data:` IMAGE assertion belongs in the same test as the `data:`
    /// LINK one. The asymmetry is deliberate — an image renders with no user
    /// action and is self-contained, a link is a click that hands control to
    /// the scheme — and without the image half, a change that made the image
    /// arm inert too would leave this test green.
    ///
    /// Verified this can fail: restoring the pre-fix arm
    /// `Dest::Data | Dest::Passthrough => format!("<a href=\"{}\">", ...)`
    /// made the first assertion panic with
    /// `<a href="data:text/html,&lt;script&gt;alert(1)&lt;/script&gt;">x</a>`.
    #[test]
    fn a_data_link_is_inert_but_a_data_image_still_renders() {
        let h = markdown_html("[x](data:text/html,<script>alert(1)</script>)\n", "proj", "a.md");
        assert!(!h.contains("href="), "{h}");
        assert!(h.contains(r#"<a class="mdbroken">"#), "{h}");

        let img = markdown_html("![d](data:image/gif;base64,R0lGOD)\n", "proj", "a.md");
        assert!(img.contains(r#"src="data:image/gif;base64,R0lGOD""#), "{img}");
    }

    /// The other side of the allowlist: the schemes that must keep working.
    #[test]
    fn allowed_schemes_keep_their_href() {
        let h = markdown_html("[m](mailto:p@example.com) [t](tel:+15551234)\n", "proj", "a.md");
        assert!(h.contains(r#"href="mailto:p@example.com""#), "{h}");
        assert!(h.contains(r#"href="tel:+15551234""#), "{h}");

        let s = markdown_html("[s](https://example.com/x)\n", "proj", "a.md");
        assert!(s.contains(r#"href="https://example.com/x""#), "{s}");
        assert!(s.contains(r#"target="_blank""#), "{s}");
        assert!(s.contains(r#"rel="noopener noreferrer""#), "{s}");
    }

    /// `link_open` builds its opening tag by hand and hands it to push_html
    /// as `Event::Html`, which push_html copies out verbatim — so this anchor
    /// is the one place in the document whose attributes nothing escapes for
    /// us. A destination or a title carrying a quote would otherwise close the
    /// attribute and open one the repo author chose.
    ///
    /// This replaces `rewriting_links_did_not_reopen_the_raw_html_hole`, whose
    /// stated reason (reordering the match arms reopens the raw-HTML hole) was
    /// false — the arms match disjoint Event variants, as markdown_html's own
    /// comment says — and whose assertions were a strict subset of
    /// `markdown_raw_html_is_neutralized`'s.
    ///
    /// Verified this can fail: dropping the `esc()` around the `data-rel`
    /// value made the first assertion panic on
    /// `<a class="mdlink" data-rel="a" onerror="x.md">` — the quote out of the
    /// filename closing the attribute, and an author-named one opening.
    /// Dropping it around the title panicked on the title assertion the same
    /// way.
    #[test]
    fn a_hand_built_anchor_escapes_what_it_interpolates() {
        let h = markdown_html("[x](<a\" onerror=\"x.md>)\n", "proj", "a.md");
        assert!(h.contains(r#"data-rel="a&quot; onerror=&quot;x.md""#), "{h}");

        let t = markdown_html("[x](b.md \"a\\\" onerror=\\\"y\")\n", "proj", "a.md");
        assert!(t.contains(r#"title="a&quot; onerror=&quot;y""#), "{t}");
    }

    /// The titles themselves: `[t](b.md "my title")` rendered `title="my
    /// title"` before `link_open` started building the tag by hand, and every
    /// form that survives has to keep doing so.
    #[test]
    fn a_link_keeps_its_title() {
        let l = markdown_html("[t](b.md \"my title\")\n", "proj", "docs/a.md");
        assert!(l.contains(r#"data-rel="docs/b.md" title="my title""#), "{l}");

        let r = markdown_html("[t](https://e.com/x \"my title\")\n", "proj", "a.md");
        assert!(r.contains(r#"title="my title""#), "{r}");
        assert!(r.contains(r#"target="_blank""#), "the title must not displace the rest: {r}");

        let m = markdown_html("[t](mailto:p@example.com \"my title\")\n", "proj", "a.md");
        assert!(m.contains(r#"title="my title""#), "{m}");

        // A link with no title must not grow an empty attribute.
        let n = markdown_html("[t](b.md)\n", "proj", "a.md");
        assert!(!n.contains("title="), "{n}");
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
        let hide = vec!["dist".to_string()];
        fs::create_dir_all(d.path().join("src/sub")).unwrap();
        fs::create_dir(d.path().join("target")).unwrap();
        fs::create_dir(d.path().join("dist")).unwrap();
        fs::write(d.path().join("src/main.rs"), "").unwrap();
        fs::write(d.path().join("src/sub/x.rs"), "").unwrap();
        fs::write(d.path().join("README.md"), "").unwrap();
        let h = tree_fragment("proj", d.path(), "src/main.rs", &TreeFilter { hide: &hide, ..Default::default() });
        assert!(h.contains("<details open data-rel=\"src\"><summary style=\"--d:0\">src</summary>"));
        assert!(h.contains("class=\"file sel\""));
        assert!(h.contains("data-rel=\"src/main.rs\""));
        assert!(h.contains("README.md"));
        assert!(!h.contains(">target</summary>"));
        assert!(!h.contains(">dist</summary>"));
    }

    // Claude Code checks a worktree out at `{repo}/.claude/worktrees/{name}`:
    // a second, full copy of the repository inside the project directory.
    // `.claude` is a dot entry, so the default filter keeps that duplicate out
    // of the parent's tree — a user who turns `show_hidden` on has asked to
    // see it and gets it (see the pair of tests below).
    #[test]
    fn a_worktree_checked_out_under_dot_claude_stays_out_of_its_parents_tree() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("src")).unwrap();
        fs::write(d.path().join("src/main.rs"), "").unwrap();
        fs::create_dir_all(d.path().join(".claude/worktrees/feat/src")).unwrap();
        fs::write(d.path().join(".claude/worktrees/feat/src/main.rs"), "").unwrap();
        let h = tree_fragment("proj", d.path(), "src/main.rs", &TreeFilter::default());
        // the project's own tree is unaffected
        assert!(h.contains("data-rel=\"src/main.rs\""));
        assert!(!h.contains(".claude"), "the worktree's parent directory must not appear");
        assert!(!h.contains("worktrees"), "nor anything beneath it");
    }

    // The default and the opt-in asserted against one fixture, so neither can
    // pass by rendering nothing: every case names a row that must be present
    // alongside the rows that must not be.
    #[test]
    fn dot_entries_are_hidden_by_default() {
        let d = dot_fixture();
        let h = tree_fragment("proj", d.path(), "", &TreeFilter::default());
        assert!(h.contains(">README.md<"), "the ordinary file must still render");
        assert!(h.contains(">src</summary>"), "and so must the ordinary directory");
        assert!(!h.contains(".gitignore"), "a dotfile is hidden");
        assert!(!h.contains(".claude"), "and so is a dot-directory");
        assert!(!h.contains(".git<"), "and so is .git");
        assert!(!h.contains(">target</summary>"), "build output stays hidden");
    }

    #[test]
    fn show_hidden_renders_every_dot_entry_but_no_build_output() {
        let d = dot_fixture();
        let filter = TreeFilter { show_hidden: true, ..Default::default() };
        let h = tree_fragment("proj", d.path(), "", &filter);
        assert!(h.contains(">README.md<"), "ordinary rows are unaffected");
        assert!(h.contains(r#"data-rel=".gitignore""#), "the dotfile is a real row");
        assert!(h.contains(">.claude</summary>"), "the dot-directory is expandable");
        assert!(h.contains(">.git</summary>"), "including .git");
        assert!(!h.contains(">target</summary>"), "but target/ is not a hidden file");
    }

    // `hide` is the user's own list, so it survives the opt-in that reveals
    // everything else — otherwise `show_hidden` would silently undo it.
    #[test]
    fn the_hide_list_still_applies_when_show_hidden_is_on() {
        let d = dot_fixture();
        let hide = vec![".gitignore".to_string()];
        let h = tree_fragment("proj", d.path(), "", &TreeFilter { hide: &hide, show_hidden: true });
        assert!(h.contains(">.claude</summary>"), "the unlisted dot entry is revealed");
        assert!(!h.contains(".gitignore"), "the listed one is not");
    }

    // A lazily-fetched subtree renders through the same filter as the initial
    // load: a `.claude` visible at the root that fetched an unfiltered listing
    // when expanded would leak rows the root render had refused.
    #[test]
    fn a_lazy_fetch_applies_the_same_filter_as_the_first_render() {
        let d = dot_fixture();
        fs::write(d.path().join("src/.secret"), "").unwrap();
        let mut off = String::new();
        tree_level("proj", &d.path().join("src"), "src", "", &TreeFilter::default(), &mut off);
        assert!(off.contains("src/main.rs"), "the ordinary child renders");
        assert!(!off.contains(".secret"));
        let mut on = String::new();
        let filter = TreeFilter { show_hidden: true, ..Default::default() };
        tree_level("proj", &d.path().join("src"), "src", "", &filter, &mut on);
        assert!(on.contains(r#"data-rel="src/.secret""#));
    }

    fn dot_fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("src")).unwrap();
        fs::create_dir(d.path().join("target")).unwrap();
        fs::create_dir(d.path().join(".git")).unwrap();
        fs::create_dir_all(d.path().join(".claude/worktrees/feat")).unwrap();
        fs::write(d.path().join("src/main.rs"), "").unwrap();
        fs::write(d.path().join("README.md"), "").unwrap();
        fs::write(d.path().join(".gitignore"), "").unwrap();
        d
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
        let h = tree_fragment("proj", d.path(), "src/main.rs", &TreeFilter::default());
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
        let h = tree_fragment("proj", d.path(), "a/b/c/main.rs", &TreeFilter::default());
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
        let h = tree_fragment("proj", d.path(), "src/sub/x.rs", &TreeFilter::default());
        assert!(h.contains("data-rel=\"src/sub/x.rs\" data-ext=\"rs\" style=\"--d:2\""));
        assert!(h.contains("data-rel=\"Cargo.toml\" data-ext=\"toml\" style=\"--d:0\""));
        // No dot at all: the stylesheet's neutral glyph, not a bogus type.
        assert!(h.contains("data-rel=\"README\" data-ext=\"\" style=\"--d:0\""));

        // The same row, reached through the lazy ?dir= path, agrees.
        let mut lazy = String::new();
        tree_level("proj", &d.path().join("src/sub"), "src/sub", "", &TreeFilter::default(), &mut lazy);
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
        tree_level("proj", &d.path().join("src"), "src", "", &TreeFilter::default(), &mut out);
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
        tree_level("proj", d.path(), "", "", &TreeFilter::default(), &mut out);
        assert!(!out.starts_with("<ul"));
        assert!(out.contains("data-rel=\"src\""));
        assert!(out.contains("data-rel=\"README.md\""));
        // must match the identity render::tree_fragment assigns those same
        // entries at the top level, since the client keys reconciliation on it
        let full = tree_fragment("proj", d.path(), "", &TreeFilter::default());
        assert!(full.contains("data-rel=\"src\""));
        assert!(full.contains("data-rel=\"README.md\""));
    }

    #[test]
    fn changes_and_status_fragments() {
        let st = Status {
            branch: "main".into(),
            changes: vec![crate::gitio::Change { xy: ".M".into(), path: "a.txt".into() }],
            ..Default::default()
        };
        let c = changes_fragment("proj", &st);
        assert!(c.contains("full diff"));
        assert!(c.contains("class=\"xy\""));
        assert!(c.contains("hx-get=\"/frag/proj/diff?path=a.txt\""));
        let s = status_fragment(&st);
        assert!(s.contains(r#"id="branch">main"#), "{s}");
        // One unstaged modification (".M") → the ~1 glyph and a dirty bullet,
        // no staged glyph.
        assert!(s.contains("gbullet dirty") || s.contains(r#"gbullet dirty"#), "{s}");
        assert!(s.contains("~1"), "{s}");
        assert!(!s.contains("+1"), "nothing is staged: {s}");
        // The chip draws the branch icon as SVG, so a text ⎇ here would double
        // the marker.
        assert!(!s.contains("⎇"), "{s}");
        let clean = changes_fragment("proj", &Status { branch: "main".into(), changes: vec![], ..Default::default() });
        assert!(clean.contains("working tree clean"));
    }

    #[test]
    fn status_fragment_reports_the_git_state_like_a_shell_prompt() {
        use crate::gitio::{Change, Status};
        let ch = |xy: &str| Change { xy: xy.into(), path: "p".into() };
        // Staged add ("A."), staged+unstaged modify ("MM" = one staged AND one
        // modified), a plain unstaged modify (".M"), and an untracked file.
        let st = Status {
            branch: "feat/x".into(),
            changes: vec![ch("A."), ch("MM"), ch(".M"), ch("??")],
            ahead: 3,
            behind: 1,
            upstream: "origin/main".into(),
        };
        let s = status_fragment(&st);
        // A.=staged, MM=staged+modified, .M=modified → +2 staged, ~2 modified.
        assert!(s.contains("+2"), "two staged: {s}");
        assert!(s.contains("~2"), "two modified: {s}");
        assert!(s.contains("↑3") && s.contains("↓1"), "ahead/behind: {s}");
        assert!(s.contains("gbullet dirty"), "{s}");
        // The tooltip spells the same state, untracked included (it has no glyph).
        assert!(s.contains("2 staged, 2 modified, 1 untracked"), "tooltip counts: {s}");
        assert!(s.contains("3 ahead, 1 behind origin/main"), "tooltip sync: {s}");

        // Clean and up to date: green bullet, no count glyphs, calm tooltip.
        let clean = Status {
            branch: "main".into(),
            changes: vec![],
            ahead: 0,
            behind: 0,
            upstream: "origin/main".into(),
        };
        let c = status_fragment(&clean);
        assert!(c.contains("gbullet clean"), "{c}");
        assert!(!c.contains('+') && !c.contains('~') && !c.contains('↑'), "no glyphs when clean: {c}");
        assert!(c.contains("working tree clean · up to date with origin/main"), "{c}");

        // A branch with no upstream says so rather than claiming sync.
        let solo = Status { branch: "wip".into(), upstream: String::new(), ..Default::default() };
        assert!(status_fragment(&solo).contains("no upstream set"), "{}", status_fragment(&solo));

        // Not a repo → nothing, never a false "clean".
        assert_eq!(status_fragment(&Status::default()), "");

        // A hostile branch name is escaped in both the body and the title.
        let evil = Status { branch: "a\"><script>".into(), ..Default::default() };
        let e = status_fragment(&evil);
        assert!(!e.contains("<script>"), "{e}");
        assert!(e.contains("&lt;script&gt;"), "{e}");
    }

    // app.js resolves `ws.show_hidden ?? SHOW_HIDDEN_DEFAULT`, and this
    // attribute is the whole of that default — without it every page would
    // start by claiming dotfiles are hidden, so a project with
    // `show_hidden = true` would render its dot rows under a toggle drawn in
    // the off position.
    #[test]
    fn the_page_carries_the_configured_default_for_the_toggle() {
        let off = workspace_page("proj", "proj", &Settings::default(), None, false, &[]);
        assert!(off.contains(r#"data-show-hidden="0""#), "the default is off");
        let on = Settings { show_hidden: true, ..Settings::default() };
        let h = workspace_page("proj", "proj", &on, None, false, &[]);
        assert!(h.contains(r#"data-show-hidden="1""#), "and a configured true reaches the page");
    }

    // The client reads this once per page load to decide whether to run its
    // autosave timer at all. Both directions asserted: a constant "1" would
    // otherwise pass, and would silently autosave in a project that turned
    // it off.
    #[test]
    fn the_page_carries_the_autosave_setting() {
        let on = workspace_page("proj", "proj", &Settings::default(), None, false, &[]);
        assert!(on.contains(r#"data-autosave="1""#), "autosave is on by default");
        let off = Settings { autosave: false, ..Settings::default() };
        let h = workspace_page("proj", "proj", &off, None, false, &[]);
        assert!(h.contains(r#"data-autosave="0""#), "and a configured false reaches the page");
    }

    // "Off unless a project asks for it, and visible whenever it is on" is
    // two separate properties, and this checks both directions of both: the
    // default page carries neither the "1" attribute nor the indicator text,
    // and a project that opted in carries both. A test that only checked the
    // on-path would pass with the indicator rendered unconditionally, which
    // is exactly the silent-exfiltration failure mode this feature exists to
    // avoid — a project could turn sharing off and the page would still
    // claim, or simply never say, whether it was happening.
    //
    // Revert-checked: hardcoding `sharing_indicator` to always render (moving
    // it out of the `if sharing_on` branch) failed this test's second
    // assertion — "no visible indicator when sharing is off" — since the
    // default page then contained "sharing selection" too. Then restored.
    //
    // The flag is a parameter, not read in here, because `share_selection`
    // is global-only: reading it inside would make this test depend on the
    // developer's real `~/.config/resh/config.toml`.
    #[test]
    fn share_selection_is_off_by_default_and_the_indicator_appears_only_when_it_is_on() {
        let off = workspace_page("proj", "proj", &Settings::default(), None, false, &[]);
        assert!(off.contains(r#"data-share-selection="0""#), "the default is off");
        assert!(!off.contains("sharing selection"), "no visible indicator when sharing is off");
        let on = workspace_page("proj", "proj", &Settings::default(), None, true, &[]);
        assert!(on.contains(r#"data-share-selection="1""#), "a configured true reaches the page");
        assert!(on.contains("sharing selection"), "the indicator must be visible whenever sharing is on");
        // The page attribute the client gates on and the human-visible
        // indicator come from the same parameter, so they cannot disagree —
        // sending with no indicator shown is the failure this prevents.
    }

    // Which launch buttons the page offers is a parameter, not read in here:
    // it comes from a startup probe of the developer's real login shell, and
    // a test must not depend on what that shell has installed.
    #[test]
    fn the_page_lists_the_launches_the_startup_check_allowed() {
        let with = workspace_page("proj", "proj", &Settings::default(), None, false, &["claude"]);
        assert!(with.contains(r#"data-launches="claude""#), "{with}");
        let without = workspace_page("proj", "proj", &Settings::default(), None, false, &[]);
        assert!(without.contains(r#"data-launches="""#), "an empty list is an explicit empty attribute");
        assert!(!without.contains(r#"data-launches="claude""#));
    }

    #[test]
    fn workspace_page_wires_everything() {
        let s = Settings { theme: "gruvbox".into(), ..Settings::default() };
        let h = workspace_page("proj", "proj", &s, Some("theme.css"), false, &[]);
        assert!(h.contains("/static/themes/gruvbox.css"));
        assert!(h.contains("/frag/proj/theme.css")); // has_theme_css
        assert!(h.contains("data-project=\"proj\""));
        assert!(h.contains("data-default-tab=\"terminal\""));
        assert!(h.contains("htmx.min.js"));
        assert!(h.contains("data-pane=\"3\""));
        assert!(h.contains("id=\"termpool\""));
        assert!(h.contains("hx-get=\"/frag/_projects?current=proj\""));
        assert!(h.contains("id=\"projstrip\""));
        // The name app.js dispatches on `ProjectsChanged`; if the two drift
        // apart the strip silently goes back to refetching only on reload.
        assert!(
            h.contains("hx-trigger=\"load, refresh from:body, projects from:body\""),
            "the projects strip must refetch on the `projects` body event"
        );
        assert!(h.contains("id=\"closeproj\""));
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
        let no_custom = workspace_page("proj", "proj", &s, None, false, &[]);
        assert!(!no_custom.contains("theme.css\">"));
    }

    #[test]
    fn the_workspace_links_exactly_one_theme_stylesheet() {
        let s = Settings::default();
        let dir_themed = workspace_page("proj", "proj", &s, Some("theme/style.css"), false, &[]);
        assert!(dir_themed.contains("/frag/proj/theme/style.css"));
        assert_eq!(dir_themed.matches("theme.css\"").count(), 0, "never both links");

        let file_themed = workspace_page("proj", "proj", &s, Some("theme.css"), false, &[]);
        assert!(file_themed.contains("/frag/proj/theme.css"));
        assert!(!file_themed.contains("/frag/proj/theme/style.css"));

        let bare = workspace_page("proj", "proj", &s, None, false, &[]);
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
        let h = workspace_page("karpie/src", "karpie%2Fsrc", &s, None, false, &[]);
        assert!(h.contains("hx-get=\"/frag/_projects?current=karpie%252Fsrc\""));
    }

    #[test]
    fn the_workspace_page_carries_the_notification_centre() {
        let s = crate::config::Settings::default();
        let html = workspace_page("proj", "proj", &s, None, false, &[]);
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
                wt: None,
            },
            crate::registry::ProjectStatus {
                key: "glow".into(), url: "glow".into(),
                live: 0, oldest_age_secs: None, has_layout: true,
                branch: String::new(), parent: None, reachable: true,
                wt: None,
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
        let h = workspace_page("a\"><script>", "a\"><script>", &s, None, false, &[]);
        assert!(!h.contains("a\"><script>"));
        let c = changes_fragment("a\"><script>", &Status { branch: String::new(), changes: vec![crate::gitio::Change { xy: "??".into(), path: "x".into() }], ..Default::default() });
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
                wt: None,
            },
            crate::registry::ProjectStatus {
                key: "glow".into(), url: "glow".into(),
                live: 0, oldest_age_secs: None, has_layout: true,
                branch: String::new(), parent: None, reachable: true,
                wt: None,
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
            wt: None,
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
            wt: None,
        }];
        assert!(projects_strip("other", &two).contains("2 sessions"), "two must stay plural");
    }

    #[test]
    fn strip_says_unknown_rather_than_zero_when_no_age_is_available() {
        let ps = vec![crate::registry::ProjectStatus {
            key: "karpie".into(), url: "karpie".into(),
            live: 3, oldest_age_secs: None, has_layout: false,
            branch: String::new(), parent: None, reachable: true,
            wt: None,
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
                wt: None,
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
                wt: None,
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
                wt: None,
            },
            crate::registry::ProjectStatus {
                key: "ultima_marketing%2F.claude%2Fworktrees%2Fsite-launch".into(),
                url: "ultima_marketing/.claude/worktrees/site-launch".into(),
                live: 1, oldest_age_secs: None, has_layout: false,
                branch: "site-launch".into(),
                parent: Some("ultima_marketing".into()),
                reachable: true,
                wt: None,
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
                wt: None,
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
                wt: None,
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

    /// Without a cache key the img src is byte-identical before and after the
    /// file changes, so the browser is free to reuse what it already has and
    /// the re-mount shows the old picture. Deleting the `&v=` fails this.
    #[test]
    fn an_image_fragment_carries_a_cache_key_that_tracks_the_file() {
        let a = image_fragment("proj", "shot.png", 1_000);
        let b = image_fragment("proj", "shot.png", 2_000);
        assert!(a.contains("v=1000"), "{a}");
        assert!(a != b, "the same file at a different mtime must not reuse one URL");
        // The path is still the path: the key is additional, not a rewrite.
        assert!(a.contains("path=shot.png"), "{a}");
    }

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
            wt: None,
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
        // `from_child.contains("karpie")` alone would pass even if from-child
        // resolution wrongly listed only the child (its own url/title also
        // contain "karpie"), so pin the ROOT'S OWN row specifically.
        assert!(from_child.contains(r#"href="/karpie" title="karpie""#), "{from_child}");
        assert!(from_child.contains("feat") && from_child.contains("karpie"), "{from_child}");
        assert!(from_root.contains("feat"), "{from_root}");
        // Both renderings must list exactly the 2-member family (root + child),
        // not just the querying end — count `<a class="wt` (row links only;
        // `wt-empty` starts with "wt" too but is a <span>, not an <a>).
        assert_eq!(from_root.matches(r#"<a class="wt"#).count(), 2, "{from_root}");
        assert_eq!(from_child.matches(r#"<a class="wt"#).count(), 2, "{from_child}");
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

    #[test]
    fn a_worktree_row_shows_its_state_and_offers_removal_only_when_clean() {
        // Revert-checked: rendering the control whenever `ahead == Some(0)`
        // alone fails the dirty case; `?` for None fails if None renders as `—`.
        use crate::registry::{ProjectStatus, WorktreeStatus};
        use crate::claudes::ClaudeEvidence;
        let mk = |wt: WorktreeStatus, live: usize| vec![
            ProjectStatus { key: "r".into(), url: "r".into(), live: 1, oldest_age_secs: None, has_layout: true, branch: "main".into(), parent: None, reachable: true, wt: None },
            ProjectStatus { key: "r%2F.claude%2Fworktrees%2Fclaude-1".into(), url: "r/.claude/worktrees/claude-1".into(), live, oldest_age_secs: None, has_layout: false, branch: "claude-1".into(), parent: Some("r".into()), reachable: true, wt: Some(wt) },
        ];
        let clean = WorktreeStatus { claude: ClaudeEvidence::Absent, dirty: Some(false), ahead: Some(0), base: "main".into(), base_recorded: true };
        let out = worktrees_strip("r", &mk(clean.clone(), 0));
        assert!(out.contains("0 ahead") && out.contains("class=\"wtremove\"") && out.contains("data-key=\"r%2F.claude%2Fworktrees%2Fclaude-1\""), "{out}");
        let dirty = WorktreeStatus { dirty: Some(true), ..clean.clone() };
        let out = worktrees_strip("r", &mk(dirty, 0));
        assert!(out.contains("dirty") && !out.contains("wtremove"), "{out}");
        let unknown = WorktreeStatus { dirty: None, ..clean.clone() };
        let out = worktrees_strip("r", &mk(unknown, 0));
        assert!(out.contains("title=\"git did not answer") && !out.contains("wtremove"), "{out}");
        let present = WorktreeStatus { claude: ClaudeEvidence::Present(vec!["term".into()]), ..clean.clone() };
        let out = worktrees_strip("r", &mk(present, 0));
        assert!(out.contains("✻") && !out.contains("wtremove"), "{out}");
        let out = worktrees_strip("r", &mk(clean.clone(), 1));
        assert!(!out.contains("wtremove"), "a live terminal blocks removal: {out}");
        let unrecorded = WorktreeStatus { base_recorded: false, ..clean };
        let out = worktrees_strip("r", &mk(unrecorded, 0));
        assert!(out.contains("measured against main, the main worktree's branch"), "{out}");
    }
}
