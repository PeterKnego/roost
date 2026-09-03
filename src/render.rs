//! All HTML generation. Plain string building, no template engine.
//! Fragments target htmx swap sites; pages are full documents.
use crate::config::Settings;
use crate::gitio::Status;
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
        // The fragment is not part of the path on disk — `resolve_dest` split
        // it off precisely so it could resolve one — but it *is* part of where
        // the link points: `deploy.md#running` means open that file and go to
        // that heading. It rides as its own attribute rather than staying glued
        // to the path, because `data-rel` is how `wireFileLinks` looks a tab
        // up: a rel with a `#` on the end would match no open tab and no file.
        let hash = match dest.split_once('#') {
            Some((_, h)) if !h.is_empty() => format!(" data-hash=\"{}\"", esc(h)),
            _ => String::new(),
        };
        return format!(
            "<a class=\"mdlink\" data-rel=\"{}\"{}{}>",
            esc(p),
            hash,
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

/// One raw HTML tag as the sanitizer reads it. Names are lowercased here so
/// the allowlist can be case-sensitive and still catch `<IMG>` and
/// `<ScRiPt>`; values are left raw because the sanitizer escapes them.
struct ParsedTag {
    name: String,
    close: bool,
    attrs: Vec<(String, String)>,
    len: usize,
}

/// Reads one open or close tag from the start of `s`, which must begin with
/// `<`. Anything that is not a complete, well-formed tag is `None`, and the
/// caller treats the `<` as text. That covers comments, doctypes and
/// processing instructions on purpose: the safe reading of markup this
/// function does not understand is "print it".
///
/// Every scan inside here — an attribute name, a quoted value — stops at the
/// next `<` even though HTML would let one appear unescaped in a quoted
/// value. This is not HTML strictness; it bounds `sanitize`'s work. Without
/// it, an unterminated `<` makes every scan run to the end of the input, and
/// the caller retries one byte later, so an input with no `>` at all costs
/// O(n^2): quadratic on a render thread, against a 2 MB file.
///
/// The name scan also stops at the first non-alphanumeric byte, so
/// `<img-caption src="x.png">` is read as an `img` tag with a stray
/// `-caption` attribute, and `</div-x>` as a close for `div`. This is a
/// rendering divergence from a browser, deliberately accepted: it can only
/// ever yield an allowlisted element with allowlisted attributes.
fn parse_tag(s: &str) -> Option<ParsedTag> {
    let b = s.as_bytes();
    if b.first() != Some(&b'<') {
        return None;
    }
    let mut i = 1;
    let close = b.get(i) == Some(&b'/');
    if close {
        i += 1;
    }
    let name_start = i;
    if !b.get(i).map_or(false, u8::is_ascii_alphabetic) {
        return None;
    }
    while b.get(i).map_or(false, |c| c.is_ascii_alphanumeric()) {
        i += 1;
    }
    let name = s[name_start..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    loop {
        while b.get(i).map_or(false, u8::is_ascii_whitespace) {
            i += 1;
        }
        match b.get(i) {
            None => return None,
            Some(b'>') => return Some(ParsedTag { name, close, attrs, len: i + 1 }),
            Some(b'/') => {
                i += 1;
                continue;
            }
            Some(_) => {}
        }
        let an = i;
        while b.get(i).map_or(false, |c| {
            !c.is_ascii_whitespace() && !matches!(c, b'"' | b'\'' | b'>' | b'/' | b'=' | b'<')
        }) {
            i += 1;
        }
        if i == an {
            return None;
        }
        let aname = s[an..i].to_ascii_lowercase();
        while b.get(i).map_or(false, u8::is_ascii_whitespace) {
            i += 1;
        }
        let mut value = String::new();
        if b.get(i) == Some(&b'=') {
            i += 1;
            while b.get(i).map_or(false, u8::is_ascii_whitespace) {
                i += 1;
            }
            match b.get(i) {
                Some(&q) if q == b'"' || q == b'\'' => {
                    let vs = i + 1;
                    // Bounded by the next `<`, not by the end of the input: a
                    // `<` before the closing quote makes the tag print as
                    // text rather than paying for a scan to end-of-file.
                    let rest = &s[vs..];
                    match (rest.find(q as char), rest.find('<')) {
                        (Some(end), Some(lt)) if lt < end => return None,
                        (Some(end), _) => {
                            value = s[vs..vs + end].to_string();
                            i = vs + end + 1;
                        }
                        (None, _) => return None,
                    }
                }
                Some(_) => {
                    let vs = i;
                    while b.get(i).map_or(false, |c| {
                        !c.is_ascii_whitespace()
                            && !matches!(c, b'"' | b'\'' | b'=' | b'<' | b'>' | b'`')
                    }) {
                        i += 1;
                    }
                    if i == vs {
                        return None;
                    }
                    value = s[vs..i].to_string();
                }
                None => return None,
            }
        }
        attrs.push((aname, value));
    }
}

/// The tags a markdown file's raw HTML may keep, and for each the
/// attributes it may keep. Chosen from what GitHub-style READMEs use,
/// starting with this repository's own; anything else renders as text. A
/// tag may be added with a test. `style`, `class` and `id` are refused
/// rather than filtered: a CSS allowlist would be a second sanitizer.
fn allowed_attrs(tag: &str) -> Option<&'static [&'static str]> {
    Some(match tag {
        "div" | "p" => &["align"],
        "img" => &["src", "alt", "width", "height", "align"],
        "a" => &["href", "title"],
        "details" => &["open"],
        "summary" | "b" | "strong" | "i" | "em" | "sub" | "sup" | "kbd" | "code" | "br" => &[],
        _ => return None,
    })
}

/// The allowlist's `&'static` spelling of a tag name, so the open-tag stack
/// holds no owned strings. Must list exactly the names `allowed_attrs`
/// accepts.
fn static_tag(tag: &str) -> Option<&'static str> {
    const TAGS: &[&str] = &[
        "div", "p", "img", "a", "details", "summary", "b", "strong", "i", "em", "sub", "sup",
        "kbd", "code", "br",
    ];
    TAGS.iter().copied().find(|t| *t == tag)
}

fn is_void(tag: &str) -> bool {
    matches!(tag, "img" | "br")
}

/// Rebuilds raw HTML from a markdown file out of allowlisted tags only.
/// Output is only what this struct constructs: every text run and every
/// attribute value goes through `esc`, and every `src`/`href` through the
/// same resolver the markdown arms use. The stack is why one instance
/// lives for the whole document — a `<div>` that opens a centred header
/// closes twenty lines of markdown later, in a different HTML block.
struct HtmlSanitizer<'a> {
    project: &'a str,
    rel: &'a str,
    // Bounded by the 2 MB preview file cap (`MAX_FILE_BYTES`): every entry
    // costs at least three input bytes (`<b>`), so the worst case is under a
    // million entries and no separate cap is needed.
    open: Vec<&'static str>,
}

impl<'a> HtmlSanitizer<'a> {
    fn new(project: &'a str, rel: &'a str) -> Self {
        Self { project, rel, open: Vec::new() }
    }

    fn sanitize(&mut self, raw: &str) -> String {
        let mut out = String::with_capacity(raw.len());
        let mut rest = raw;
        while let Some(lt) = rest.find('<') {
            out.push_str(&esc(&rest[..lt]));
            rest = &rest[lt..];
            match parse_tag(rest) {
                Some(tag) => {
                    let consumed = &rest[..tag.len];
                    rest = &rest[tag.len..];
                    self.emit(&tag, consumed, &mut out);
                }
                None => {
                    out.push_str("&lt;");
                    rest = &rest[1..];
                }
            }
        }
        out.push_str(&esc(rest));
        out
    }

    /// Closes whatever the document left open, innermost first.
    fn finish(&mut self) -> String {
        let mut out = String::new();
        while let Some(t) = self.open.pop() {
            out.push_str("</");
            out.push_str(t);
            out.push('>');
        }
        out
    }

    fn emit(&mut self, tag: &ParsedTag, raw: &str, out: &mut String) {
        let (Some(allowed), Some(name)) = (allowed_attrs(&tag.name), static_tag(&tag.name)) else {
            out.push_str(&esc(raw));
            return;
        };
        if tag.close {
            if is_void(name) || self.open.last() != Some(&name) {
                out.push_str(&esc(raw));
                return;
            }
            self.open.pop();
            out.push_str("</");
            out.push_str(name);
            out.push('>');
            return;
        }
        let attr = |n: &str| tag.attrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
        // `emit_a` builds every attribute of an anchor itself, `title`
        // included, via `link_open` — skip the loop below for `a` so its
        // result (which `emit_a` never reads) can't be mistaken for where
        // `title` comes from.
        let attrs = if name == "a" {
            String::new()
        } else {
            let mut attrs = String::new();
            for &a in allowed {
                let Some(v) = attr(a) else { continue };
                match a {
                    "align" => {
                        let v = v.to_ascii_lowercase();
                        if matches!(v.as_str(), "left" | "center" | "right") {
                            attrs.push_str(&format!(" align=\"{v}\""));
                        }
                    }
                    "width" | "height" => {
                        if !v.is_empty() && v.len() <= 4 && v.bytes().all(|c| c.is_ascii_digit()) {
                            attrs.push_str(&format!(" {a}=\"{v}\""));
                        }
                    }
                    "open" => attrs.push_str(" open"),
                    "alt" | "title" => attrs.push_str(&format!(" {a}=\"{}\"", esc(v))),
                    // `src` and `href` are decided by the resolver in
                    // `emit_img` and `emit_a`; they never take this path.
                    _ => {}
                }
            }
            attrs
        };
        match name {
            "img" => self.emit_img(tag, &attrs, out),
            "a" => self.emit_a(tag, out),
            _ => {
                out.push('<');
                out.push_str(name);
                out.push_str(&attrs);
                out.push('>');
                if !is_void(name) {
                    self.open.push(name);
                }
            }
        }
    }

    /// `src` is decided by the resolver, exactly as the markdown image arm
    /// decides it: a local path becomes the raw route, a `data:` URI is
    /// kept, and anything roost will not fetch drops the tag and leaves the
    /// alt text, escaped, in its place. `attrs` already excludes `src`.
    fn emit_img(&mut self, tag: &ParsedTag, attrs: &str, out: &mut String) {
        let attr = |n: &str| tag.attrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
        let src = attr("src").unwrap_or("");
        let url = match resolve_dest(src, self.rel) {
            Dest::Local(p) => format!(
                "/frag/{}/raw?path={}",
                crate::http::percent_encode(self.project),
                crate::http::percent_encode(&p)
            ),
            Dest::Data => src.to_string(),
            _ => {
                out.push_str(&esc(attr("alt").unwrap_or("")));
                return;
            }
        };
        out.push_str("<img src=\"");
        out.push_str(&esc(&url));
        out.push('"');
        out.push_str(attrs);
        out.push('>');
    }

    /// The open tag is `link_open`'s, so an HTML link and a markdown link
    /// to the same place are the same anchor: tab-opening for local,
    /// `_blank`/`noopener` for `http(s)`, inert for every other scheme. A
    /// missing or empty href is inert too, decided here rather than by
    /// asking the resolver what an empty string means.
    fn emit_a(&mut self, tag: &ParsedTag, out: &mut String) {
        let attr = |n: &str| tag.attrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
        match attr("href").filter(|h| !h.is_empty()) {
            Some(href) => out.push_str(&link_open(href, attr("title").unwrap_or(""), self.rel)),
            None => out.push_str("<a class=\"mdbroken\">"),
        }
        self.open.push("a");
    }
}

/// Replaces every raw-HTML event with its sanitized form. Runs on the
/// collected vector rather than in the streaming `filter_map` because an
/// HTML block arrives one `Html` event per line, and a tag whose attributes
/// continue on the next line (the `<img\n  src=…\n  width=…>` of a centred
/// README header) is only whole once the block's lines are joined.
///
/// A bare `Event::Html` that reaches this loop with `block == None` falls to
/// the `other` arm and is passed through raw, unsanitized. That is safe only
/// because pulldown-cmark 0.13 never emits `Event::Html` outside a
/// `Start(HtmlBlock)`/`End(HtmlBlock)` pair, so the only such event this
/// function ever sees with `block` empty is the one `markdown_html` itself
/// creates for a markdown link: built by `link_open` from already-escaped
/// values, so sanitizing it again would double-escape it and strip the
/// attributes (`data-rel`, `data-hash`) `link_open` set that are not on this
/// module's allowlist. If a future pulldown-cmark version — or any other
/// producer feeding this function — ever emits `Event::Html` for content
/// that did not come from `link_open`, that event would need to be treated
/// as untrusted raw input here, the same as a block's or inline tag's.
fn sanitize_raw_html(events: &mut Vec<pulldown_cmark::Event<'_>>, project: &str, rel: &str) {
    use pulldown_cmark::{CowStr, Event, Tag, TagEnd};
    let mut san = HtmlSanitizer::new(project, rel);
    let mut out = Vec::with_capacity(events.len());
    let mut block: Option<String> = None;
    for ev in events.drain(..) {
        match ev {
            Event::Start(Tag::HtmlBlock) => block = Some(String::new()),
            Event::Html(h) if block.is_some() => {
                if let Some(b) = &mut block {
                    b.push_str(&h);
                }
            }
            Event::End(TagEnd::HtmlBlock) => {
                let joined = block.take().unwrap_or_default();
                out.push(Event::Html(CowStr::from(san.sanitize(&joined))));
            }
            Event::InlineHtml(h) => out.push(Event::Html(CowStr::from(san.sanitize(&h)))),
            other => out.push(other),
        }
    }
    // A `Start(HtmlBlock)` never followed by its `End` — unreachable with
    // pulldown-cmark today, since every block it opens it closes — would
    // otherwise drop the accumulated text silently. Sanitize and emit
    // whatever the block held rather than assume "never happens" means
    // "safe to discard"; see the CLAUDE.md rule this codebase learned that
    // rule from.
    if let Some(residue) = block.take() {
        out.push(Event::Html(CowStr::from(san.sanitize(&residue))));
    }
    let tail = san.finish();
    if !tail.is_empty() {
        out.push(Event::Html(CowStr::from(tail)));
    }
    *events = out;
}

/// GitHub's heading-anchor slug, so a `#section` link written against GitHub
/// resolves in the preview too.
///
/// Deliberately mirrors `github-slugger` rather than being tidier than it:
/// lowercase, drop every character that is not alphanumeric, `-`, `_` or a
/// space, then turn each surviving space into `-`. It does not collapse runs,
/// so "Files & folders" slugs to `files--folders` — two hyphens — on GitHub and
/// therefore here. A neater algorithm would disagree with every link anyone
/// copied off a GitHub page, which is the only reason this function exists.
///
/// The ids are bare rather than namespaced, which is what lets a link written
/// for GitHub work unchanged. That cost is accepted, not overlooked: a preview
/// fragment is injected into the live workspace document, which already owns
/// ids like `#settings`, `#content` and `#refresh`, so a heading with one of
/// those names emits a duplicate — and a same-document jump then lands on
/// whichever comes first in document order, which is the chrome, because the
/// header is rendered before the panes.
fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == ' ' {
            out.push('-');
        } else if ch.is_alphanumeric() || ch == '-' || ch == '_' {
            // `to_lowercase` yields a sequence rather than a char — 'İ' becomes
            // two code points — so taking only the first would corrupt it.
            out.extend(ch.to_lowercase());
        }
    }
    out
}

/// Fills each heading's `id` with its slug, in place.
///
/// A second pass over materialised events rather than a step in the streaming
/// `filter_map` in `markdown_html`, because a heading's text arrives in the
/// events *after* its `Start` — the same constraint that makes the image arm
/// there rewrite a tag instead of emitting raw HTML. `push_html` writes and
/// escapes the attribute itself, so nothing here builds markup.
fn fill_heading_ids(events: &mut [pulldown_cmark::Event<'_>]) {
    use pulldown_cmark::{CowStr, Event, Tag, TagEnd};
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut i = 0;
    while i < events.len() {
        if !matches!(events[i], Event::Start(Tag::Heading { .. })) {
            i += 1;
            continue;
        }
        // Headings cannot nest, so the next End(Heading) always closes this one
        // and no depth counter is needed.
        let mut text = String::new();
        let mut j = i + 1;
        while j < events.len() && !matches!(events[j], Event::End(TagEnd::Heading(_))) {
            // Code spans contribute their text: GitHub slugs the heading as
            // rendered, so `## The `hub` module` is `the-hub-module`.
            if let Event::Text(t) | Event::Code(t) = &events[j] {
                text.push_str(t);
            }
            j += 1;
        }
        let base = slug(&text);
        if !base.is_empty() {
            // First occurrence takes the bare slug and each repeat takes the
            // next number, matching GitHub. Without this a document with two
            // "Notes" headings makes the second unreachable and the first
            // swallows both links.
            let n = seen.entry(base.clone()).or_insert(0);
            let id = if *n == 0 { base } else { format!("{base}-{n}") };
            *n += 1;
            if let Event::Start(Tag::Heading { id: slot, .. }) = &mut events[i] {
                // Only when the author gave none. Heading attributes are not
                // enabled today so this is always None, but silently
                // overwriting an explicit `{#id}` if they ever are would break
                // exactly the links that went to the trouble of naming it.
                if slot.is_none() {
                    *slot = Some(CowStr::from(id));
                }
            }
        }
        i = j;
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
        // Raw HTML is not handled here. It is sanitized by
        // `sanitize_raw_html` on the collected vector below, because an
        // HTML block's lines have to be joined before a tag split across
        // them can be read. The Event::Html this function emits for links
        // is built from escaped values by link_open and is untouched by
        // that pass, which only rewrites HtmlBlock runs and InlineHtml.
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
    // Collected rather than streamed, so `fill_heading_ids` can look ahead
    // from a heading's Start to the text that names it. Bounded by the same
    // 2 MB file cap every other read is.
    let mut events: Vec<Event> = events.collect();
    sanitize_raw_html(&mut events, project, rel);
    fill_heading_ids(&mut events);
    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    format!("<article class=\"markdown-body\">{out}</article>")
}

/// A File tab whose content could not be produced: deleted from under the tab,
/// past the 2 MB cap, or not text at all.
///
/// Carries the same `.path` breadcrumb the readable fragments do, and that is
/// load-bearing twice over. A bare "not found: No such file or directory" does
/// not say *which* file, which is no help with three panes open; and app.js
/// hangs the Edit/Preview switch on that breadcrumb, so a tab the server
/// demoted into Preview because it could not read the file would otherwise
/// have no control at all to get back once the file returns.
pub fn file_error_fragment(rel: &str, msg: &str) -> String {
    format!("<div class=\"path\">{}</div>{}", esc(rel), hint(msg))
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

/// Breadcrumb for the directory picker: "roost" always links back to the
/// top level (`at=""`), every segment but the last is a clickable link to
/// browsing that prefix, and the last segment is plain text (you're already
/// there — the picker doesn't render a `..` row, this is the way up).
/// The front page: a two-pane overview. Both panes are htmx fragments that
/// load on open and poll (see `overview.js` / the fragment routes); this
/// shell only lays them out. The picker still lives on `/`, reached by the
/// `?at=` query and the "Open a directory" button here — no new reserved
/// path, which would collide with a project of that name the way `static`
/// and `frag` already can.
pub fn overview_page(sel: &str, roots_label: &str) -> String {
    // `sel` is already a percent-encoded storage key (e.g. `karpie%2Fsrc`);
    // encoding it again here means the server's single `percent_decode` of
    // the query value lands back on that exact key — same pattern as the
    // header switcher's `?current={qkey}`.
    let qsel = crate::http::percent_encode(sel);
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>roost</title>\
         <link rel=\"icon\" type=\"image/svg+xml\" href=\"/static/logo.svg\">\
         <link rel=\"stylesheet\" href=\"/static/themes/darcula.css\">\
         <link rel=\"stylesheet\" href=\"/static/style.css\">\
         <script src=\"/static/vendor/htmx.min.js\"></script>\
         </head><body class=\"overview-body\">\
         <header>\
           <span class=\"home\">{SVG_DIAMOND}</span><span class=\"proj\">roost</span>\
           <span class=\"vsep\"></span>\
           <span class=\"roots\" title=\"{roots}\">{roots}</span>\
         </header>\
         <main id=\"overview\">\
           <section class=\"pane ovpane tool\">\
             <div class=\"panehead\"><span class=\"panetitle\">Projects</span></div>\
             <div id=\"ovprojects\" class=\"ovbody\" hx-get=\"/frag/_overview_projects?sel={qsel}\" hx-trigger=\"load\"></div>\
           </section>\
           <section class=\"pane ovpane\">\
             <div class=\"panehead\"><span class=\"panetitle\">Sessions</span><span id=\"ovscope\" class=\"panemeta\"></span></div>\
             <div id=\"ovsessions\" class=\"ovbody\" hx-get=\"/frag/_overview_sessions?sel={qsel}\" hx-trigger=\"load\"></div>\
           </section>\
         </main>\
         <script src=\"/static/overview.js\"></script>\
         </body></html>",
        roots = esc(roots_label),
        SVG_DIAMOND = SVG_DIAMOND,
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
                "<span class=\"{}\" title=\"worktree outside roost's roots — cannot be opened\">{}{} {}{}</span>",
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

/// The per-worktree state chips (Claude / dirty / ahead) and, when
/// `show_remove` is set and every axis is positively clean, the remove
/// control. Shared by the header switcher and the overview's left pane so
/// the two never drift on the chips themselves — but only the switcher
/// carries the JS handler for `.wtremove`, so only it passes `show_remove:
/// true`; the overview's left pane loads no such handler and would leave
/// the button dead, so it passes `false`. `None` on any axis renders `?`,
/// never "clean" — see the switcher's own comment.
fn worktree_chips(w: &crate::registry::WorktreeStatus, key: &str, live: usize, show_remove: bool) -> String {
    let claude = match &w.claude {
        crate::claudes::ClaudeEvidence::Present(_) => "<span class=\"wtf on\" title=\"a Claude is running here\">✻</span>".to_string(),
        crate::claudes::ClaudeEvidence::Absent => "<span class=\"wtf\" title=\"no Claude here\">—</span>".to_string(),
        crate::claudes::ClaudeEvidence::Unknown => "<span class=\"wtf\" title=\"IDE integration is off, so roost cannot tell\">?</span>".to_string(),
    };
    let dirty = match w.dirty {
        Some(true) => "<span class=\"wtf on\">dirty</span>".to_string(),
        Some(false) => "<span class=\"wtf\">clean</span>".to_string(),
        None => "<span class=\"wtf\" title=\"git did not answer (status)\">?</span>".to_string(),
    };
    let against = if w.base_recorded {
        format!("measured against {}, recorded when roost created this worktree", esc(&w.base))
    } else {
        format!("measured against {}, the main worktree's branch — roost did not create this worktree", esc(&w.base))
    };
    let ahead = match w.ahead {
        Some(n) => format!("<span class=\"wtf{}\" title=\"{against}. A squash-merged branch stays ahead forever; remove it by hand.\">{n} ahead</span>", if n > 0 { " on" } else { "" }),
        None => "<span class=\"wtf\" title=\"git did not answer (rev-list), or no base is known\">?</span>".to_string(),
    };
    let remove = if show_remove && crate::registry::removable(w, live) {
        format!(" <button class=\"wtremove\" data-key=\"{}\" title=\"remove this worktree and its branch\">✕</button>", esc(key))
    } else {
        String::new()
    };
    format!("{claude} {dirty} {ahead}{remove}")
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
                "<span class=\"{cls}\" title=\"worktree outside roost's roots — cannot be opened\">{marker} {}{branch}</span>",
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
            Some(w) => format!(" · {}", worktree_chips(w, &p.key, p.live, true)),
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

/// The overview's left pane: known projects, each expandable to its worktree
/// family. Rows are pre-ordered by `known_projects_with_state` (parent then
/// its children), so this renders in order and lets `parent` decide nesting.
/// A parent with children gets a caret; a worktree row carries `worktree_chips`.
/// Selection (`sel`, a storage key) marks the current row; expansion is the
/// client's job (`overview.js`). A reachable row is a link to `/<url>` (open
/// the project); an unreachable worktree is inert text.
pub fn overview_projects(sel: &str, projects: &[crate::registry::ProjectStatus]) -> String {
    let mut out = String::from("<ul class=\"ovtree\">");
    for p in projects {
        // Children are present exactly when their project is open, which is
        // also how the arrow knows which way to point.
        let expanded = projects.iter().any(|c| c.parent.as_deref() == Some(p.key.as_str()));
        out.push_str(&ov_row(p, sel, expanded));
    }
    out.push_str("</ul>");
    out
}

/// One project's worktrees as bare `<li>`s, for the client to splice in
/// under the row it opened. Selecting a project must not re-fetch the list
/// the selection was made from — the only thing the server can add is that
/// project's own children, so that is all this returns.
pub fn overview_worktree_rows(sel: &str, rows: &[crate::registry::ProjectStatus]) -> String {
    rows.iter().map(|p| ov_row(p, sel, false)).collect()
}

fn ov_row(p: &crate::registry::ProjectStatus, sel: &str, expanded: bool) -> String {
    let is_child = p.parent.is_some();
    let mut cls = String::from("ovrow");
    if is_child {
        cls.push_str(" child");
    }
    if p.live > 0 {
        cls.push_str(" live");
    }
    if p.key == sel {
        cls.push_str(" current");
    }
    let marker = if p.live > 0 { "\u{25cf}" } else { "\u{25cb}" };
    // A repository may have worktrees, so it gets an expander; whether it
    // actually has any is only known once the user opens it and the server
    // pays `git worktree list` for that one project.
    let caret = if is_child || p.branch.is_empty() {
        "<span class=\"ovcaret placeholder\" aria-hidden=\"true\"></span>".to_string()
    } else if expanded {
        "<span class=\"ovcaret\" aria-hidden=\"true\">\u{25be}</span>".to_string()
    } else {
        "<span class=\"ovcaret\" aria-hidden=\"true\">\u{25b8}</span>".to_string()
    };
    let name = if is_child { p.url.rsplit('/').next().unwrap_or(&p.url) } else { p.url.as_str() };
    // A worktree roost made is checked out on a branch of the same name, so
    // its row said `claude-1 ⎇ claude-1` — the same word twice, and wide
    // enough to push the name into an ellipsis in a narrow pane. Say it once.
    let branch = if p.branch.is_empty() || (is_child && p.branch == name) {
        String::new()
    } else {
        format!(" <span class=\"branch\">\u{2387} {}</span>", esc(&p.branch))
    };
    let chips = match &p.wt {
        Some(w) => format!(" <span class=\"ovchips\">{}</span>", worktree_chips(w, &p.key, p.live, false)),
        None => String::new(),
    };
    let parent_attr = p
        .parent
        .as_deref()
        .map(|pk| format!(" data-parent=\"{}\"", esc(pk)))
        .unwrap_or_default();
    if !p.reachable {
        return format!(
            "<li class=\"{cls} unreachable\" data-key=\"{}\"{parent_attr} title=\"worktree outside roost's roots — cannot be opened\">{caret}{marker} {}{branch}{chips}</li>",
            esc(&p.key),
            esc(name)
        );
    }
    // Opening is a property of the project, not of something running inside
    // it: a project with no sessions had no way to be opened at all, because
    // the only link on the page that reached a workspace was a session row.
    // A plain click still selects (that is what fills the right pane), so
    // the way in is its own control — shown on the row under the pointer and
    // on the selected row, so the selected project always offers it.
    let open = format!(
        " <a class=\"ovgo\" href=\"/{}\" title=\"open {}\">open</a>",
        crate::http::percent_encode(&p.url),
        esc(&p.url)
    );
    format!(
        "<li class=\"{cls}\" data-key=\"{}\"{parent_attr}>{caret}<a class=\"ovname\" href=\"/{}\">{marker} {}{branch}</a>{chips}{open}</li>",
        esc(&p.key),
        crate::http::percent_encode(&p.url),
        esc(name)
    )
}

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
    let scope = if sel.is_empty() {
        "all active".to_string()
    } else {
        format!("in {}", esc(&crate::registry::decode_key(sel)))
    };
    // The scope belongs in the pane's head, which htmx must not swap away —
    // so it travels out of band into `#ovscope`, the same trick
    // `worktrees_strip` uses for `#wtlabel`. The `All` way out only appears
    // when there is something to get back from.
    let all = if sel.is_empty() {
        String::new()
    } else {
        " <a class=\"ovall\" href=\"/\">All</a>".to_string()
    };
    let mut out = format!(
        "<span id=\"ovscope\" class=\"panemeta\" hx-swap-oob=\"true\">· {scope}{all}</span>\
         <ul class=\"ovsessions\">"
    );
    if rows.is_empty() {
        // Nothing is running here, which is exactly when the way in matters
        // most: without this the pane was a dead end for every project that
        // had never been opened.
        let go = if sel.is_empty() {
            String::new()
        } else {
            let url = crate::registry::decode_key(sel);
            format!(
                " <a class=\"ovgo\" href=\"/{}\">open {}</a>",
                crate::http::percent_encode(&url),
                esc(&url)
            )
        };
        out.push_str(&format!("<li class=\"ovempty\">no sessions running{go}</li></ul>"));
        return out;
    }
    for r in rows {
        let mark = if r.is_claude {
            "<span class=\"ovkind on\" title=\"Claude\">✻ claude</span>"
        } else {
            "<span class=\"ovkind\" title=\"shell\">○ shell</span>"
        };
        let age = match r.age_secs {
            Some(s) => human_age(s),
            None => "—".to_string(),
        };
        let attached = if r.attached > 0 {
            format!(" <span class=\"ovatt\">·{}</span>", r.attached)
        } else {
            String::new()
        };
        let href = format!(
            "/{}?focus={}",
            crate::http::percent_encode(&r.project_url),
            crate::http::percent_encode(&r.name)
        );
        out.push_str(&format!(
            "<li class=\"ovsession\"><a href=\"{href}\">{mark} <span class=\"ovlabel\">{} · {}</span> <span class=\"ovage\">{age}</span>{attached}</a></li>",
            esc(&r.project_url),
            esc(&r.name)
        ));
    }
    out.push_str("</ul>");
    out
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
// The owl from docs/img/roost-logo.svg, inlined so it takes the header's
// accent colour like the diamond it replaced; `static/logo.svg` is the same
// drawing in the brand blue for the favicon, where there is no CSS to
// inherit from. 14:12 drawn at 16px tall is 19px wide, not 16.
const SVG_HOME: &str = r#"<svg width="19" height="16" viewBox="0 0 14 12" fill="currentColor" shape-rendering="crispEdges" aria-hidden="true"><rect x="1" y="0" width="1" height="1"/><rect x="3" y="0" width="1" height="1"/><rect x="9" y="0" width="1" height="1"/><rect x="11" y="0" width="1" height="1"/><rect x="1" y="1" width="4" height="1"/><rect x="9" y="1" width="3" height="1"/><rect x="2" y="2" width="3" height="1"/><rect x="9" y="2" width="4" height="1"/><rect x="3" y="3" width="3" height="1"/><rect x="9" y="3" width="5" height="1"/><rect x="4" y="4" width="3" height="1"/><rect x="9" y="4" width="2" height="1"/><rect x="12" y="4" width="1" height="1"/><rect x="5" y="5" width="6" height="1"/><rect x="4" y="6" width="7" height="1"/><rect x="4" y="7" width="7" height="1"/><rect x="5" y="8" width="5" height="1"/><rect x="6" y="9" width="1" height="1"/><rect x="9" y="9" width="1" height="1"/><rect x="6" y="10" width="1" height="1"/><rect x="9" y="10" width="1" height="1"/><rect x="5" y="11" width="6" height="1"/></svg>"#;
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
/// `~/.config/roost/config.toml`.
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
<title>{proj_txt}</title>
<link rel="icon" type="image/svg+xml" href="/static/logo.svg">
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
  <button id="wtbtn" title="branch and worktrees">{SVG_BRANCH}<span id="gitinfo" hx-get="/frag/{proj_url}/status" hx-trigger="load, refresh from:body, git from:body"></span><span id="wtlabel"></span></button>
  {warn}
  {sharing_indicator}
  <label id="searchbox" for="searchinput" title="search this project (ctrl-shift-F or ⌘⇧F)">{SVG_SEARCH}<input id="searchinput" type="search" autocomplete="off" spellcheck="false" placeholder="Search files, contents, sessions" aria-label="Search files, contents, sessions"><kbd>⇧⌃F</kbd></label>
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
<div id="searchoverlay" hidden>
  <div class="searchpanel">
    <div id="searchresults"></div>
    <div id="searchnote"></div>
  </div>
</div>
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
        // The id is part of the heading's normal output now, so asserting the
        // bare `<h1>Hi</h1>` would fail for the right reason but read like a
        // regression. It is spelled out here rather than loosened to a
        // `contains("Hi")` that could not tell a heading from a paragraph.
        assert!(h.contains("<h1 id=\"hi\">Hi</h1>"));
        assert!(h.contains("<li>a</li>"));
    }

    /// A `#section` link written for GitHub has to land here too, which needs
    /// the ids GitHub generates and `pulldown-cmark` does not.
    ///
    /// Verified this can fail: with the id-filling pass reverted every
    /// assertion below fails against a bare `<h2>`.
    #[test]
    fn heading_ids_are_github_style_slugs() {
        let h = markdown_html(
            "## Running\n\n## The `hub` module\n\n## Files & folders\n\n## snake_case kept\n",
            "proj",
            "a.md",
        );
        assert!(h.contains(r#"<h2 id="running">"#), "{h}");
        // A code span contributes its text: GitHub slugs the rendered heading,
        // not the markdown source, so `hub` is part of the slug and the
        // backticks are not.
        assert!(h.contains(r#"<h2 id="the-hub-module">"#), "{h}");
        // GitHub strips the punctuation and *then* turns each surviving space
        // into a hyphen, without collapsing runs — so "Files & folders" is
        // `files--folders`, with two. Asserting the doubled hyphen is what
        // makes this test GitHub-compatible rather than merely reasonable; a
        // single-hyphen expectation would pass with a tidier algorithm that
        // silently disagreed with every link copied from GitHub.
        assert!(h.contains(r#"<h2 id="files--folders">"#), "{h}");
        // Underscores survive; they are word characters to GitHub's slugger.
        assert!(h.contains(r#"<h2 id="snake_case-kept">"#), "{h}");
    }

    /// Two headings with the same text must not produce the same id, or the
    /// second one is unreachable and the first swallows both links.
    #[test]
    fn repeated_headings_get_numbered_ids() {
        let h = markdown_html("## Notes\n\n## Notes\n\n## Notes\n", "proj", "a.md");
        assert!(h.contains(r#"<h2 id="notes">"#), "{h}");
        assert!(h.contains(r#"<h2 id="notes-1">"#), "{h}");
        assert!(h.contains(r#"<h2 id="notes-2">"#), "{h}");
    }

    /// `resolve_dest` splits the fragment off to find the path on disk, and
    /// before this it was simply dropped — so every `deploy.md#section` link in
    /// the README rendered as the byte-identical `data-rel="docs/deploy.md"`
    /// and opened the file at the top.
    #[test]
    fn a_local_link_carries_its_fragment_beside_its_path() {
        let h = markdown_html("[run](../docs/deploy.md#running)\n", "proj", "a/b.md");
        assert!(
            h.contains(r#"data-rel="docs/deploy.md" data-hash="running""#),
            "the fragment must survive as its own attribute: {h}"
        );
        // A link with no fragment must not grow an empty attribute, or every
        // plain link starts carrying a meaningless data-hash="".
        let plain = markdown_html("[p](../docs/deploy.md)\n", "proj", "a/b.md");
        assert!(!plain.contains("data-hash"), "{plain}");
    }

    /// The fragment is author-controlled text going into an attribute, exactly
    /// like `data-rel` beside it.
    ///
    /// Verified this can fail: dropping the `esc()` around the fragment ends
    /// the attribute early and emits `data-hash="a" onerror="y"`.
    #[test]
    fn a_link_fragment_is_escaped() {
        let h = markdown_html("[x](<b.md#a\" onerror=\"y>)\n", "proj", "a.md");
        assert!(h.contains(r#"data-hash="a&quot; onerror=&quot;y""#), "{h}");
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

    /// The README shape: a centred header whose `<div>` closes after
    /// twenty lines of markdown, with an `<img>` whose attributes continue
    /// on the next line. Both halves are asserted: the tag survives with
    /// both attributes (block lines were joined), and the close tag is a
    /// tag, not text (the stack crossed the markdown in between).
    ///
    /// Verified this can fail: sanitizing each `Event::Html` line as it
    /// arrives instead of joining the block first turns `<img\n  src=…`
    /// into text, since its `>` is on a line the sanitizer never sees
    /// joined to it — asserted `<img src="/frag/proj/raw?path=docs/img/hero.png" width="900">`,
    /// got `&lt;img\n  src=&quot;docs/img/hero.png&quot;\n  width=&quot;900&quot;&gt;`.
    #[test]
    fn a_block_is_joined_and_balanced_across_the_document() {
        // No blank line between the div and the img: that keeps the img's
        // lines inside the div's HTML block, where they arrive one event
        // per line. After a blank line, `<img` alone is not a complete tag,
        // so CommonMark would make it a paragraph holding one inline tag —
        // and that path never exercises the joining this test is for.
        let md = "<div align=\"center\">\n<img\n  src=\"docs/img/hero.png\"\n  width=\"900\">\n\n# Title\n\nBody *text*.\n\n</div>\n";
        let h = markdown_html(md, "proj", "README.md");
        assert!(h.contains(r#"<div align="center">"#), "{h}");
        assert!(h.contains(r#"<img src="/frag/proj/raw?path=docs/img/hero.png" width="900">"#), "{h}");
        assert!(h.contains("<h1"), "markdown between the tags still renders: {h}");
        assert!(h.contains("</div>"), "{h}");
        assert!(!h.contains("&lt;/div&gt;"), "{h}");
    }

    /// Inline HTML in a paragraph goes through the same sanitizer.
    ///
    /// Verified this can fail: routing the `InlineHtml` arm to
    /// `out.push(Event::Text(h))` instead of the sanitizer turns the whole
    /// document into neutralized text — output contained
    /// `&lt;b&gt;this&lt;/b&gt;` and `&lt;span onclick="x"&gt;that&lt;/span&gt;`
    /// (quotes left unescaped, since `Event::Text` is escaped by
    /// `push_html` with no awareness this was ever a tag), instead of
    /// `<b>this</b>` and the allowlist-refused, fully escaped span.
    #[test]
    fn inline_html_is_sanitized_not_neutralized() {
        let h = markdown_html("see <b>this</b> and <span onclick=\"x\">that</span>\n", "proj", "a.md");
        assert!(h.contains("<b>this</b>"), "{h}");
        assert!(h.contains("&lt;span onclick=&quot;x&quot;&gt;that&lt;/span&gt;"), "{h}");
        assert!(!h.contains("<span"), "{h}");
    }

    /// A document that never closes its `<details>` still ends balanced.
    ///
    /// Verified this can fail: dropping the `if !tail.is_empty() { … }`
    /// block that appends `san.finish()`'s output (keeping the call to
    /// `finish()` itself, so the stack still pops but the closing tags are
    /// discarded) makes the output end `</p>\n</article>` instead of
    /// `</details></article>` — `</details>` and `</summary>` never reach
    /// the page.
    #[test]
    fn an_unclosed_tag_is_closed_at_the_end_of_the_document() {
        let h = markdown_html("<details>\n<summary>more</summary>\n\nhidden text\n", "proj", "a.md");
        assert!(h.trim_end().ends_with("</details></article>"), "{h}");
    }

    /// A block that ends inside a tag prints the fragment rather than
    /// guessing at it. The fragment sits inside a `<div>` block so that it
    /// reaches the sanitizer at all: on its own, `<img src="x.png"` with
    /// no `>` is not a complete tag, so CommonMark would never make it an
    /// HTML block and push_html would escape it without our help.
    ///
    /// Verified this can fail: changing `sanitize`'s `None` arm from
    /// `out.push_str("&lt;");` to `out.push('<');` lets a live, unescaped
    /// `<img src=&quot;x.png&quot;` reach the page instead of the fully
    /// escaped `&lt;img src=&quot;x.png&quot;`.
    #[test]
    fn an_unterminated_tag_at_the_end_of_a_block_is_text() {
        let h = markdown_html("<div>\n<img src=\"x.png\"\n\ntext\n", "proj", "a.md");
        assert!(h.contains("&lt;img src=&quot;x.png&quot;"), "{h}");
        assert!(!h.contains("<img"), "{h}");
        assert!(h.trim_end().ends_with("</div></article>"), "{h}");
    }

    /// The one `Event::Html` this renderer emits itself — the anchor
    /// `link_open` builds for a markdown link — must pass the sanitizing
    /// pass untouched. Sanitizing it would double-escape a value that was
    /// escaped once already, and the anchor would lose its `data-rel`.
    ///
    /// Verified this can fail: routing a bare `Event::Html` through
    /// `san.sanitize` before the `other` arm (as if it were untrusted, the
    /// way an `InlineHtml` or `HtmlBlock` event is treated) turns the anchor
    /// into `<a class="mdbroken">t</a>`, because `href` is not among the
    /// attributes `link_open` used (it used `data-rel`/`data-hash` instead),
    /// so re-parsing the tag through the allowlist sees no `href` and treats
    /// the link as broken.
    #[test]
    fn a_markdown_link_anchor_passes_the_html_pass_byte_for_byte() {
        let h = markdown_html("[t](docs/deploy.md#running \"my title\")\n", "proj", "README.md");
        assert!(h.contains(r#"<a class="mdlink" data-rel="docs/deploy.md" data-hash="running" title="my title">t</a>"#), "{h}");
        assert!(!h.contains("&lt;a"), "{h}");
    }

    /// A `Start(HtmlBlock)` with no matching `End` — unreachable through
    /// `pulldown_cmark` today, since it always closes a block it opens, but
    /// the wrong default in this codebase is to drop the accumulated text
    /// silently rather than sanitize and emit it. Built by hand rather than
    /// from a markdown string, since there is no markdown input that leaves
    /// `sanitize_raw_html` an unterminated block to react to.
    ///
    /// Verified this can fail: removing the residue flush (calling only
    /// `san.finish()` after the loop, as the pre-fix code did) drops the
    /// `<b>x` text entirely, and since `sanitize` — the only thing that
    /// pushes onto the sanitizer's open-tag stack — is then never called,
    /// `finish()` has nothing to close either: the result was `left: []`,
    /// empty, against the expected two-element vector.
    #[test]
    fn an_unterminated_html_block_still_emits_its_text() {
        use pulldown_cmark::{Event, Tag};
        let mut events = vec![Event::Start(Tag::HtmlBlock), Event::Html("<b>x".into())];
        sanitize_raw_html(&mut events, "proj", "a.md");
        assert_eq!(events, vec![Event::Html("<b>x".into()), Event::Html("</b>".into())]);
    }

    /// Both halves matter and each has its own caller: the message is what the
    /// user reads, and the breadcrumb is what app.js appends the Edit/Preview
    /// switch to (`mountTab` looks for `.path`). Dropping either one leaves a
    /// pane that either does not say which file, or cannot be got out of.
    #[test]
    fn an_unreadable_file_still_names_itself() {
        let h = file_error_fragment("src/gone.rs", "not found: No such file or directory");
        assert!(h.contains(r#"<div class="path">src/gone.rs</div>"#), "{h}");
        assert!(h.contains("not found: No such file or directory"), "{h}");
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

    /// The parser is the one place the sanitizer trusts its own reading of
    /// the input, so its edges are pinned individually.
    #[test]
    fn parse_tag_reads_open_close_and_attributes() {
        let s = r#"<IMG SRC="a.png" width=9 alt='x y' open>rest"#;
        let t = parse_tag(s).unwrap();
        assert_eq!(t.name, "img");
        assert!(!t.close);
        assert_eq!(
            t.attrs,
            vec![
                ("src".to_string(), "a.png".to_string()),
                ("width".to_string(), "9".to_string()),
                ("alt".to_string(), "x y".to_string()),
                ("open".to_string(), String::new()),
            ]
        );
        assert_eq!(&s[t.len..], "rest");

        let c = parse_tag("</Div >tail").unwrap();
        assert_eq!(c.name, "div");
        assert!(c.close);
        assert_eq!(&"</Div >tail"[c.len..], "tail");

        let sc = parse_tag("<br/>").unwrap();
        assert_eq!(sc.name, "br");
        assert_eq!(sc.len, 5);
    }

    /// Everything the parser must refuse. Each refusal makes the `<` plain
    /// text downstream, which is the safe default the spec asks for.
    ///
    /// Verified this can fail: changing the alphabetic check to `if false` made
    /// this fail on the "comment" assertion.
    #[test]
    fn parse_tag_refuses_what_is_not_a_tag() {
        assert!(parse_tag("<!-- c -->").is_none(), "comment");
        assert!(parse_tag("<!DOCTYPE html>").is_none(), "doctype");
        assert!(parse_tag("<?php ?>").is_none(), "processing instruction");
        assert!(parse_tag("< b>").is_none(), "space before name");
        assert!(parse_tag("<3 things").is_none(), "digit");
        assert!(parse_tag("<img src=\"x.png\"").is_none(), "unterminated");
        assert!(parse_tag("<img src=\"x.png>").is_none(), "unterminated quote");
        assert!(parse_tag("a <b>").is_none(), "does not start at <");
    }

    /// An attribute value may hold a `>`; the parser must not stop there.
    #[test]
    fn parse_tag_keeps_a_gt_inside_a_quoted_value() {
        let s = r#"<a title="x > y">z"#;
        let t = parse_tag(s).unwrap();
        assert_eq!(t.attrs[0].1, "x > y");
        assert_eq!(&s[t.len..], "z");
    }

    /// `<` ends every scan inside a tag, including a quoted value, so a
    /// `<` that never closes costs the sanitizer one bounded look, not a
    /// scan to the end of the file for every `<` after it.
    ///
    /// Verified this can fail: removing `b'<'` from the attribute-name
    /// exclusion list made the `<a x<b>` assertion panic with
    /// `panicked at ...: lt inside an attribute name` — the assert message
    /// itself, meaning `is_none()` returned `false`: without the exclusion
    /// the name scan swallows `<b` as part of the attribute name and the
    /// tag parses instead of being refused.
    #[test]
    fn parse_tag_stops_every_scan_at_the_next_lt() {
        assert!(parse_tag("<a title=\"x < y\">").is_none(), "lt inside a quoted value");
        assert!(parse_tag("<a x<b>").is_none(), "lt inside an attribute name");
        // The unquoted-value scan already stopped at `<` before this fix (it
        // was never the bug an unbounded scan would be). But that `<` is
        // exactly what the next loop iteration dispatches on next, and a
        // bare `<` there can never lead to a closing `>` — the same reason
        // the line above is `None` — so this is `None` too, not a
        // successful parse with a truncated `x=y` attribute.
        assert!(parse_tag("<a x=y<b>").is_none(), "lt right after an unquoted value");
    }

    /// Runs one instance over one string, `rel` = README.md at the project
    /// root, and closes what it left open.
    fn san(raw: &str) -> String {
        san_in("README.md", raw)
    }
    fn san_in(rel: &str, raw: &str) -> String {
        let mut s = HtmlSanitizer::new("proj", rel);
        let mut out = s.sanitize(raw);
        out.push_str(&s.finish());
        out
    }

    /// The allowlist in one test: an allowed tag keeps only its allowed
    /// attributes, and a refused tag prints.
    ///
    /// Verified this can fail: printing `raw` unescaped in `emit`'s
    /// not-allowed branch made this fail with `left: "<script>alert(1)</script>"`.
    #[test]
    fn sanitizer_keeps_allowed_tags_and_prints_the_rest() {
        assert_eq!(
            san(r#"<div align="center" class="x" style="color:red" id="i" onclick="f()">t</div>"#),
            r#"<div align="center">t</div>"#
        );
        assert_eq!(san("<script>alert(1)</script>"), "&lt;script&gt;alert(1)&lt;/script&gt;");
        assert_eq!(san("<iframe src=x></iframe>"), "&lt;iframe src=x&gt;&lt;/iframe&gt;");
        assert_eq!(san("<b>x</b> <sub>y</sub> <kbd>k</kbd>"), "<b>x</b> <sub>y</sub> <kbd>k</kbd>");
    }

    /// Case must not be a bypass in either direction.
    ///
    /// Verified this can fail: dropping `.to_ascii_lowercase()` from `name`
    /// in `parse_tag` made this fail with
    /// `left: "&lt;DIV ALIGN=&quot;Center&quot;&gt;x&lt;/Div&gt;"`.
    #[test]
    fn sanitizer_is_case_insensitive() {
        assert_eq!(san("<DIV ALIGN=\"Center\">x</Div>"), "<div align=\"center\">x</div>");
        assert_eq!(san("<ScRiPt>x</ScRiPt>"), "&lt;ScRiPt&gt;x&lt;/ScRiPt&gt;");
    }

    /// Text and attribute values are escaped, never copied.
    ///
    /// Verified this can fail: in `emit`, changing the `"alt" | "title"`
    /// arm to push `v` unescaped (`format!(" {a}=\"{v}\"")` instead of
    /// `esc(v)`) made this fail on the accepted-tag assertion with
    /// `left: "<img src=\"/frag/proj/raw?path=x.png\" alt=\"x&quot;y>z\">"`
    /// — the raw `&` and `>` came through instead of `&amp;` and `&gt;`.
    #[test]
    fn sanitizer_escapes_text_and_values() {
        assert_eq!(san("a < b & c > d"), "a &lt; b &amp; c &gt; d");
        // The accepted-tag case: this tag parses (a `>` inside a quoted
        // value is fine; only `<` isn't), so its `alt` value must reach
        // the output through `esc(v)` inside `emit`, not through
        // `sanitize`'s fallback text-escaping path for a refused tag.
        assert_eq!(
            san(r#"<img src="x.png" alt="x&quot;y>z">"#),
            r#"<img src="/frag/proj/raw?path=x.png" alt="x&amp;quot;y&gt;z">"#
        );
        // The refusal case: a `<` before the closing quote makes
        // `parse_tag` refuse the whole tag (Finding A's fix), so this
        // otherwise-identical fixture prints as text instead of parsing.
        // Verified against `esc()` of the whole raw string: the two agree
        // byte for byte.
        assert_eq!(
            san(r#"<img src="x.png" alt="x&quot;y<z">"#),
            r#"&lt;img src=&quot;x.png&quot; alt=&quot;x&amp;quot;y&lt;z&quot;&gt;"#
        );
        assert_eq!(san("<!-- hidden -->"), "&lt;!-- hidden --&gt;");
    }

    /// Value rules: `align` is one of three words, `width`/`height` are
    /// short digit strings, `open` is bare, `br` takes nothing.
    ///
    /// Verified this can fail: in `emit`, dropping the
    /// `matches!(v.as_str(), "left" | "center" | "right")` check (always
    /// pushing `align`) made this fail on `<p align="middle">x</p>` with
    /// `left: "<p align=\"middle\">x</p>"`.
    #[test]
    fn sanitizer_applies_value_rules() {
        assert_eq!(san("<p align=\"middle\">x</p>"), "<p>x</p>");
        assert_eq!(
            san("<img src=\"x.png\" width=\"10px\" height=\"99999\">"),
            "<img src=\"/frag/proj/raw?path=x.png\">"
        );
        assert_eq!(
            san("<img src=\"x.png\" width=\"900\" height=\"1\">"),
            "<img src=\"/frag/proj/raw?path=x.png\" width=\"900\" height=\"1\">"
        );
        assert_eq!(
            san("<details open=\"open\"><summary>m</summary>x</details>"),
            "<details open><summary>m</summary>x</details>"
        );
        assert_eq!(san("<br clear=\"right\"><br/>"), "<br><br>");
    }

    /// Balance: unmatched close tags print, unclosed open tags are closed
    /// by `finish`, and the stack survives across `sanitize` calls because
    /// a `<div>` and its `</div>` arrive in different HTML blocks.
    ///
    /// Verified this can fail: replacing `self.open.last() != Some(&name)`
    /// with `false` in `emit` made this fail on the lone `</div>` with
    /// `left: "</div>"`, `right: "&lt;/div&gt;"`.
    #[test]
    fn sanitizer_balances_across_calls() {
        let mut s = HtmlSanitizer::new("proj", "README.md");
        assert_eq!(s.sanitize("<div align=\"center\">"), "<div align=\"center\">");
        assert_eq!(s.sanitize("</div>"), "</div>");
        assert_eq!(s.finish(), "");

        let mut s = HtmlSanitizer::new("proj", "README.md");
        assert_eq!(s.sanitize("</div>"), "&lt;/div&gt;");
        assert_eq!(s.sanitize("<details><summary>x</summary>"), "<details><summary>x</summary>");
        assert_eq!(s.finish(), "</details>");

        let mut s = HtmlSanitizer::new("proj", "README.md");
        assert_eq!(s.sanitize("<b><i>x</b>"), "<b><i>x&lt;/b&gt;");
        assert_eq!(s.finish(), "</i></b>");
    }

    /// The third emitter obeys the same table as the two markdown arms:
    /// a local path becomes the raw route, resolved against the file's own
    /// directory; a `data:` URI is kept.
    ///
    /// Verified this can fail: in `emit_img`, replacing
    /// `resolve_dest(src, self.rel)` with `resolve_dest(src, "README.md")`
    /// made this fail on the `docs/a.md` case with
    /// `left: "<img src=\"/frag/proj/raw?path=img/x.png\" ...">` — resolved
    /// against the project root instead of `docs/`.
    #[test]
    fn an_html_image_resolves_like_a_markdown_image() {
        assert_eq!(
            san_in("docs/a.md", "<img src=\"img/x.png\" alt=\"cat\" width=\"12\">"),
            "<img src=\"/frag/proj/raw?path=docs/img/x.png\" alt=\"cat\" width=\"12\">"
        );
        assert_eq!(
            san_in("docs/a.md", "<img src=\"../top.png\">"),
            "<img src=\"/frag/proj/raw?path=top.png\">"
        );
        assert_eq!(
            san("<img src=\"data:image/gif;base64,R0lGOD\">"),
            "<img src=\"data:image/gif;base64,R0lGOD\">"
        );
    }

    /// Remote, escaping and empty sources drop the tag to its alt text,
    /// the fallback the markdown arm gives a remote image.
    ///
    /// Verified this can fail: replacing the `match resolve_dest(...)` in
    /// `emit_img` with `let url = src.to_string();` made this fail with
    /// `left: "<img src=\"https://e.com/b.png\" alt=\"a cat\">"`.
    #[test]
    fn an_html_image_roost_will_not_fetch_becomes_its_alt() {
        for src in ["https://e.com/b.png", "//e.com/b.png", "../../etc/x.png", ""] {
            let h = san(&format!("<img src=\"{src}\" alt=\"a cat\">"));
            assert_eq!(h, "a cat", "{src}");
        }
        assert_eq!(san("<img src=\"x.png\" onerror=\"alert(1)\">"), "<img src=\"/frag/proj/raw?path=x.png\">");
    }

    /// Anchors are `link_open`'s: local opens a tab, remote opens a new
    /// window with no opener, an unlisted or missing href is inert.
    ///
    /// Verified this can fail: replacing the `link_open(...)` call in
    /// `emit_a` with `format!("<a href=\"{}\">", esc(href))` made this fail
    /// on the local-link assertion first, with
    /// `left: "<a href=\"docs/deploy.md#running\">d</a>"` in place of the
    /// `mdlink`/`data-rel` form — before it even reaches the `javascript:`
    /// case, since every branch here goes through the same replaced call.
    #[test]
    fn an_html_anchor_is_built_by_link_open() {
        assert_eq!(
            san("<a href=\"docs/deploy.md#running\" title=\"t\">d</a>"),
            "<a class=\"mdlink\" data-rel=\"docs/deploy.md\" data-hash=\"running\" title=\"t\">d</a>"
        );
        assert_eq!(
            san("<a href=\"https://e.com/\">e</a>"),
            "<a href=\"https://e.com/\" target=\"_blank\" rel=\"noopener noreferrer\">e</a>"
        );
        for href in ["javascript:alert(1)", "data:text/html,x", "vbscript:x"] {
            assert_eq!(san(&format!("<a href=\"{href}\">x</a>")), "<a class=\"mdbroken\">x</a>", "{href}");
        }
        assert_eq!(san("<a>x</a>"), "<a class=\"mdbroken\">x</a>");
    }

    /// A `<` that never closes must cost a bounded look, not a rescan of
    /// everything after it. 50 000 unterminated tags is 350 kB, well under
    /// the 2 MB file cap; the quadratic version of `sanitize` took tens of
    /// seconds on it, the linear one takes milliseconds, so a five-second
    /// bound separates them with a wide margin on any machine.
    ///
    /// Verified this can fail: restoring *both* pre-fix behaviors at once —
    /// the attribute-name scan not excluding `<`, and the quoted-value
    /// search running `s[vs..].find(q as char)?` to end of input instead of
    /// stopping at the next `<` — made this fail with `took 176.79s` on this
    /// machine, far over the five-second bound.
    ///
    /// Restoring only the quoted-value half, with the name-scan exclusion
    /// left in place, does *not* reproduce the failure on this exact input:
    /// after the value `"` fails to close, the very next byte in each block
    /// is `<`, so the name-scan exclusion alone already forces `parse_tag`
    /// to bail in O(1) before the quoted-value scan's own bound would ever
    /// matter. The two fixes are independently sufficient for this
    /// particular repeating pattern, so this timing test guards the
    /// combined regression (either fix reverted alone is still caught by
    /// `parse_tag_stops_every_scan_at_the_next_lt`'s correctness
    /// assertions) rather than isolating the quoted-value bound by itself.
    #[test]
    fn sanitizer_is_linear_on_unterminated_tags() {
        let raw = "<a x=\"".repeat(50_000);
        let start = std::time::Instant::now();
        let out = san(&raw);
        assert!(start.elapsed() < std::time::Duration::from_secs(5), "took {:?}", start.elapsed());
        assert!(!out.contains('<'), "everything printed as text");
        assert_eq!(out.matches("&lt;a x=&quot;").count(), 50_000);
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
    // developer's real `~/.config/roost/config.toml`.
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

    #[test]
    fn the_header_advertises_what_search_actually_does() {
        let s = Settings { theme: "gruvbox".into(), ..Settings::default() };
        let h = workspace_page("proj", "proj", &s, Some("theme.css"), false, &[]);
        assert!(h.contains("id=\"searchoverlay\""), "the overlay shell must be in the page");
        assert!(h.contains("id=\"searchinput\""), "{h}");
        assert!(h.contains("id=\"searchresults\""), "{h}");
        assert!(h.contains("id=\"searchnote\""), "{h}");
        // Symbols are out of scope. A slot that keeps promising a category
        // nobody is building is how a placeholder becomes a lie.
        assert!(!h.contains("Search files, symbols"), "the hint must not promise symbols");
        assert!(h.contains("Search files, contents, sessions"), "{h}");
        assert!(
            !h.contains("project-wide search — not implemented yet"),
            "the tooltip must stop saying search is unimplemented"
        );
    }

    /// The header used to carry a <button> dressed as a text field while the
    /// real field lived in the overlay — two things that look like one
    /// control, and only the second accepts typing. There is now exactly one,
    /// and it is the one you can see when the overlay is closed.
    #[test]
    fn the_search_field_exists_once_and_lives_in_the_header() {
        let s = Settings { theme: "gruvbox".into(), ..Settings::default() };
        let h = workspace_page("proj", "proj", &s, Some("theme.css"), false, &[]);
        assert_eq!(
            h.matches("id=\"searchinput\"").count(),
            1,
            "exactly one search field in the page: {h}"
        );
        let head = h.find("<header>").expect("a header");
        let tail = h.find("</header>").expect("a closed header");
        assert!(
            h[head..tail].contains("id=\"searchinput\""),
            "the field must be in the header: {}",
            &h[head..tail]
        );
        let ov = h.find("id=\"searchoverlay\"").expect("the overlay");
        assert!(
            !h[ov..].contains("<input"),
            "the overlay must not carry a second field: {}",
            &h[ov..]
        );
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

    /// The owl is the home mark and the favicon on both pages, and the
    /// workspace tab is titled by the project alone: the favicon now says
    /// which app a tab belongs to, so the "— roost" suffix only cost width
    /// in a crowded tab strip.
    ///
    /// Verified this can fail: run before the change, it panicked on the
    /// first assertion, the title still reading `<title>proj — roost</title>`.
    #[test]
    fn both_pages_carry_the_owl_and_the_workspace_title_is_the_project_alone() {
        let s = crate::config::Settings::default();
        let ws = workspace_page("proj", "proj", &s, None, false, &[]);
        assert!(ws.contains("<title>proj</title>"), "{}", &ws[..400]);
        assert!(!ws.contains("— roost</title>"), "suffix still present");
        let icon = r#"<link rel="icon" type="image/svg+xml" href="/static/logo.svg">"#;
        assert!(ws.contains(icon), "workspace page has no favicon link");
        let home = ws.split(r#"<a class="home" href="/" title="all projects">"#).nth(1).expect("home anchor");
        assert!(home.starts_with(r#"<svg"#) && home[..home.find("</svg>").unwrap()].contains(r#"viewBox="0 0 14 12""#),
            "home anchor does not hold the owl: {}", &home[..120.min(home.len())]);
        let ov = overview_page("", "/home/x");
        assert!(ov.contains(icon), "overview page has no favicon link");
        assert!(ov.contains("<title>roost</title>"), "overview title changed");
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
    fn overview_page_wires_both_fragment_panes() {
        let h = overview_page("", "/home/claude/projects");
        assert!(h.contains("id=\"overview\""));
        // sel="" still emits `?sel=` (empty, unfiltered) — the URL always
        // carries the param so htmx has a consistent shape to trigger from.
        assert!(h.contains("hx-get=\"/frag/_overview_projects?sel=\""), "{h}");
        assert!(h.contains("hx-get=\"/frag/_overview_sessions?sel=\""), "{h}");
        assert!(h.contains("/static/overview.js"), "{h}");
        // The directory picker is gone, and so is the button that reached
        // it: the overview lists every project directory under the roots, so
        // browsing to find one had nothing left to offer.
        assert!(!h.contains("?at="), "no picker entry point: {h}");
    }

    // The overview shell is stateless HTML re-rendered fresh on every `/`
    // load; it's the fragment hx-get URLs that must carry `sel` forward so
    // htmx fetches the *filtered* panes and the left pane can mark the
    // current row. `sel` is a storage key, already percent-encoded once
    // (e.g. `karpie%2Fsrc`); it goes through `percent_encode` a second time
    // so the server's single `percent_decode` on the query value lands back
    // on the exact storage key — same pattern as the header switcher's
    // `hx-get="/frag/_worktrees?current={qkey}"`.
    //
    // Revert-checked: with the hardcoded (no-`?sel=`) URLs this fails —
    // `h.contains("_overview_projects?sel=karpie%252Fsrc")` is false because
    // the emitted URL is bare `/frag/_overview_projects` with no query at
    // all.
    #[test]
    fn overview_page_threads_sel_into_both_fragment_urls() {
        let sel = "karpie%2Fsrc";
        // Confirm the round-trip property this test relies on: encoding sel
        // once and decoding it once yields sel back, unchanged.
        let encoded = crate::http::percent_encode(sel);
        assert_eq!(encoded, "karpie%252Fsrc", "{encoded}");
        assert_eq!(crate::http::percent_decode(&encoded), sel);

        let h = overview_page(sel, "/roots");
        assert!(h.contains("/frag/_overview_projects?sel=karpie%252Fsrc"), "{h}");
        assert!(h.contains("/frag/_overview_sessions?sel=karpie%252Fsrc"), "{h}");
    }

    // A picker row for a directory that is also a known project carries the
    // same ●/○ the header strip uses; an ordinary directory with no
    // matching project carries neither.
    // The shortcut's href is a real workspace URL, so it needs the same
    // slash-preserving percent-encoding as breadcrumb's `?at=` links (a `/`
    // between segments must stay a literal separator, not become %2F) plus
    // HTML-escaping on the visible bits, since both the segment names and
    // the entry name come straight off the filesystem.
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
    // A rejected `?at=` (missing, outside ROOTS, refused for any other
    // reason) still falls back to the top level, but that fallback must not
    // be silent — the caller sees a notice explaining why they landed here
    // instead of where they asked to go, and the fallback listing itself is
    // still rendered (this is not an error page, just an annotated
    // redirect). The two hints are independent: a refused `?at=` whose
    // fallback happens to have entries must show the notice but not the
    // empty-directory hint.
    // Baseline: an ordinary, non-empty, successfully-resolved listing shows
    // neither hint — both are edge-case annotations, not part of the normal
    // render.
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
        assert!(h.contains("worktree outside roost's roots"), "{h}");
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
        // Revert-checked: rendering the chip span as empty (chips not reused
        // from worktree_chips) fails the "3 ahead"/"dirty"/"✻" assertion below.
        // Parent row carries an expansion caret and is current; child row is present with its chips.
        assert!(out.contains("ovcaret") && out.contains("current"), "{out}");
        assert!(out.contains("data-key=\"ultima\""), "{out}");
        assert!(out.contains("data-parent=\"ultima\"") && out.contains("claude-1"), "child under parent: {out}");
        assert!(out.contains("✻") && out.contains("dirty") && out.contains("3 ahead"), "chips reused: {out}");
    }

    #[test]
    fn overview_projects_never_renders_the_remove_button_even_when_removable() {
        // The overview's left pane loads only overview.js + htmx — no remove
        // handler — so the ✕ affordance from worktree_chips must stay
        // suppressed there even for a worktree that `registry::removable`
        // would allow (live == 0, everything clean).
        //
        // Revert-checked: passing `true` (or dropping the gate entirely) for
        // overview_projects's call into worktree_chips makes this fail —
        // observed `assertion failed: !out.contains("wtremove")` because the
        // row then renders `<button class="wtremove" ...>✕</button>`.
        use crate::claudes::ClaudeEvidence;
        let wt = crate::registry::WorktreeStatus { claude: ClaudeEvidence::Absent, dirty: Some(false), ahead: Some(0), base: "main".into(), base_recorded: true };
        let ps = vec![
            ps_row("ultima", "ultima", 1, "main", None, None),
            ps_row("ultima%2F.claude%2Fworktrees%2Fclaude-1", "ultima/.claude/worktrees/claude-1", 0, "claude-1", Some("ultima"), Some(wt)),
        ];
        assert!(crate::registry::removable(ps[1].wt.as_ref().unwrap(), ps[1].live), "fixture must actually be removable");
        let out = overview_projects("ultima", &ps);
        assert!(!out.contains("wtremove"), "{out}");
    }

    /// `claude-1 ⎇ claude-1` is the same word twice: a worktree roost made is
    /// on a branch named after it. Revert-checked: without the `is_child &&
    /// branch == name` guard the first assertion fails, the row carrying
    /// `⎇ claude-1` after the name.
    #[test]
    fn a_worktree_row_does_not_repeat_its_branch_as_its_name() {
        use crate::registry::{ProjectStatus, WorktreeStatus};
        let row = |name: &str, branch: &str| ProjectStatus {
            key: format!("repo%2F.claude%2Fworktrees%2F{name}"),
            url: format!("repo/.claude/worktrees/{name}"),
            live: 0, oldest_age_secs: None, has_layout: false,
            branch: branch.into(), parent: Some("repo".into()), reachable: true,
            wt: Some(WorktreeStatus { claude: crate::claudes::ClaudeEvidence::Absent,
                dirty: Some(false), ahead: Some(0), base: "main".into(), base_recorded: true }),
        };
        let same = overview_projects("", &[row("claude-1", "claude-1")]);
        assert!(!same.contains("\u{2387} claude-1"), "the branch is not repeated: {same}");
        assert!(same.contains("claude-1</a>"), "the name is still there: {same}");
        // A worktree on a differently-named branch still says which.
        let diff = overview_projects("", &[row("wt", "feature/x")]);
        assert!(diff.contains("\u{2387} feature/x"), "{diff}");
    }

    #[test]
    fn overview_projects_renders_an_unreachable_worktree_as_inert_text() {
        // Revert-checked: disabling the `!p.reachable` branch (so this row
        // renders as an ordinary `<a>` link) fails the `contains("unreachable")` assertion below.
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

    #[test]
    fn overview_sessions_marks_claude_only_on_evidence_and_shows_label_age_attached() {
        // Revert-checked: forcing `mark` to always render the claude span
        // (dropping `r.is_claude`) fails the "shell row marked ○" assertion
        // — panic showed the shell row rendered "✻ claude" instead of "○
        // shell". Also revert-checked: rendering `None` age as the literal
        // "0" (rather than "—") fails the "unknown age must not render as 0"
        // assertion. Both restored.
        let rows = vec![
            OvSession { project_url: "ultima".into(), name: "term".into(), is_claude: true, age_secs: Some(14400), attached: 1 },
            OvSession { project_url: "ultima/.claude/worktrees/claude-1".into(), name: "term".into(), is_claude: true, age_secs: Some(1200), attached: 0 },
            OvSession { project_url: "roost".into(), name: "shell".into(), is_claude: false, age_secs: None, attached: 0 },
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
        assert!(!out.contains(">0<"), "unknown age must not render as 0: {out}");
    }

    #[test]
    fn overview_sessions_empty_scope_says_so() {
        // Revert-checked: skipping the empty-rows branch (never emitting the
        // "no sessions running" `<li>`) fails this assertion — panic showed
        // just the empty `<ul class="ovsessions"></ul>` with no message. Restored.
        let out = overview_sessions("", &[]);
        assert!(out.contains("no sessions") || out.contains("nothing running"), "{out}");
    }
}
