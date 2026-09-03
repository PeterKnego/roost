# Raw HTML in Markdown Preview Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A README written for GitHub, with `<div align>`, `<img width>`,
`<details>` and `<sub>`, renders in roost's markdown preview the way it does
on GitHub, while every tag and attribute roost does not explicitly allow
still renders as escaped text.

**Architecture:** One new sanitizer in `src/render.rs`, beside the resolver
it calls. A pure tag parser turns `<name attr="v">` into a struct; a
`HtmlSanitizer` walks a raw string, re-emits allowlisted tags from escaped
values, keeps a stack so the document ends balanced, and routes every `src`
and `href` through the existing `resolve_dest` and `link_open`. A pass over
`markdown_html`'s collected event vector joins each HTML block's lines,
sanitizes them once, and replaces the two `Event::Text` arms that neutralize
raw HTML today.

**Tech Stack:** Rust, pulldown-cmark 0.13 (already a dependency), no new
crates. Browser test in Deno driving Chromium through
`tests/browser/harness.mjs`.

**Spec:** `docs/superpowers/specs/2026-09-03-preview-raw-html-design.md`

## Global Constraints

- **Output is only what we construct.** No byte of raw HTML reaches the
  output unescaped. Every attribute value passes through `esc`; every tag is
  rebuilt from its parsed parts.
- **Allowlist, verbatim from the spec.** `div`, `p`: `align`. `img`: `src`,
  `alt`, `width`, `height`, `align`. `a`: `href`, `title`. `details`:
  `open`. `summary`, `b`, `strong`, `i`, `em`, `sub`, `sup`, `kbd`, `code`,
  `br`: no attributes. Nothing else.
- **Refused tags print, they do not vanish.** `<script>` renders as
  `&lt;script&gt;`, the behaviour `markdown_raw_html_is_neutralized` already
  pins.
- **Every `src` and `href` is decided by `resolve_dest` and `link_open`**,
  never copied.
- **No new dependency.** The tokenizer is a loop over bytes.
- **Run the Rust suite as `cargo test -- --test-threads=1`.** A bare
  `cargo test` hangs on this project.
- **Revert-check every new test**: apply the broken version named in the
  step, watch the test fail, restore, and record the failure in the test's
  doc comment. A test that has not been watched failing is not done.
- **Build from this checkout only.** The shared cargo target dir bakes
  absolute asset paths; if `cargo build` fails with `include_bytes!` naming
  another directory, run `cargo clean -p roost` first.
- **`percent_encode` keeps `/` literal** (`src/http.rs:93`), so a raw-route
  URL for `docs/img/x.png` is `/frag/proj/raw?path=docs/img/x.png`. Tests
  below spell URLs that way.

---

## File Structure

- `src/render.rs` — everything server-side. New items, all placed
  immediately after `link_open` (which ends at the closing brace following
  `_ => INERT.to_string(),`, around line 241) and before `slug`:
  - `struct ParsedTag` and `fn parse_tag(s: &str) -> Option<ParsedTag>`:
    the tokenizer for one tag.
  - `fn allowed_attrs`, `fn static_tag`, `fn is_void`: the allowlist.
  - `struct HtmlSanitizer<'a>` with `sanitize(&mut self, raw: &str) ->
    String` and `finish(&mut self) -> String`.
  - `fn sanitize_raw_html(events: &mut Vec<Event>, project: &str, rel:
    &str)`: the pass over the event vector.
  - `markdown_html` loses its two `Event::Text` arms and gains one call.
- `tests/browser/mdhtml.mjs` — new browser test.
- `tests/browser/README.md` — one line in the run list.

---

### Task 1: The tag parser

**Files:**
- Modify: `src/render.rs` (insert after `link_open`)
- Test: `src/render.rs` `mod tests`

**Interfaces:**
- Consumes: nothing.
- Produces:
  ```rust
  struct ParsedTag {
      /// Lowercased.
      name: String,
      close: bool,
      /// (lowercased name, raw value). Value is unescaped input; the
      /// sanitizer escapes it. A bare attribute has an empty value.
      attrs: Vec<(String, String)>,
      /// Bytes consumed from the input, including the closing `>`.
      len: usize,
  }
  fn parse_tag(s: &str) -> Option<ParsedTag>
  ```
  `s` starts at a `<`. Returns `None` for anything that is not a complete,
  well-formed open or close tag: comments, `<!DOCTYPE`, `<?`, a `<` followed
  by a non-letter, a tag with no `>` before the input ends.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/render.rs`, next to the markdown tests:

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test parse_tag -- --test-threads=1`
Expected: compile error, `cannot find function parse_tag`.

- [ ] **Step 3: Write the parser**

Insert after `link_open`:

```rust
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
            !c.is_ascii_whitespace() && !matches!(c, b'"' | b'\'' | b'>' | b'/' | b'=')
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
                    let end = s[vs..].find(q as char)? + vs;
                    value = s[vs..end].to_string();
                    i = end + 1;
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test parse_tag -- --test-threads=1`
Expected: 3 passed.

- [ ] **Step 5: Revert-check**

Change `if !b.get(i).map_or(false, u8::is_ascii_alphabetic)` to
`if false` and run again. Expected: `parse_tag_refuses_what_is_not_a_tag`
fails on "comment" or "space before name". Restore, run, green. Record the
failing assertion in the test's doc comment as the existing tests do
("Verified this can fail: …").

- [ ] **Step 6: Commit**

```bash
git add src/render.rs
git commit -m "render: parse_tag, the tokenizer under the raw-HTML sanitizer"
```

---

### Task 2: The sanitizer

**Files:**
- Modify: `src/render.rs` (insert after `parse_tag`)
- Test: `src/render.rs` `mod tests`

**Interfaces:**
- Consumes: `parse_tag`, `esc`, `resolve_dest(dest, from_rel) -> Dest`,
  `link_open(dest, title, from_rel) -> String`,
  `crate::http::percent_encode`.
- Produces:
  ```rust
  fn allowed_attrs(tag: &str) -> Option<&'static [&'static str]>
  struct HtmlSanitizer<'a> { project: &'a str, rel: &'a str, open: Vec<&'static str> }
  impl<'a> HtmlSanitizer<'a> {
      fn new(project: &'a str, rel: &'a str) -> Self
      fn sanitize(&mut self, raw: &str) -> String
      fn finish(&mut self) -> String
  }
  ```
  The `open` stack holds `&'static str` names taken from the allowlist
  table, which is what makes closing cheap. One instance lives for a whole
  document; Task 3 constructs it.

- [ ] **Step 1: Write the failing tests**

```rust
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
    #[test]
    fn sanitizer_is_case_insensitive() {
        assert_eq!(san("<DIV ALIGN=\"Center\">x</Div>"), "<div align=\"center\">x</div>");
        assert_eq!(san("<ScRiPt>x</ScRiPt>"), "&lt;ScRiPt&gt;x&lt;/ScRiPt&gt;");
    }

    /// Text and attribute values are escaped, never copied.
    #[test]
    fn sanitizer_escapes_text_and_values() {
        assert_eq!(san("a < b & c > d"), "a &lt; b &amp; c &gt; d");
        assert_eq!(
            san(r#"<img src="x.png" alt="x&quot;y<z">"#),
            r#"<img src="/frag/proj/raw?path=x.png" alt="x&amp;quot;y&lt;z">"#
        );
        assert_eq!(san("<!-- hidden -->"), "&lt;!-- hidden --&gt;");
    }

    /// Value rules: `align` is one of three words, `width`/`height` are
    /// short digit strings, `open` is bare, `br` takes nothing.
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -- --test-threads=1 sanitizer_ an_html_`
Expected: compile error, `cannot find struct HtmlSanitizer`.

- [ ] **Step 3: Write the sanitizer**

Insert after `parse_tag`:

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -- --test-threads=1 sanitizer_ an_html_`
Expected: 8 passed.

- [ ] **Step 5: Revert-check**

Five reverts, one at a time, each restored before the next:

1. In `emit`, replace `out.push_str(&esc(raw)); return;` in the not-allowed
   branch with `out.push_str(raw); return;`. Expected:
   `sanitizer_keeps_allowed_tags_and_prints_the_rest` fails on the
   `<script>` assertion with the tag intact.
2. Remove `.to_ascii_lowercase()` from `name` in `parse_tag`. Expected:
   `sanitizer_is_case_insensitive` fails, `<DIV` printed as text.
3. Replace `self.open.last() != Some(&name)` with `false`. Expected:
   `sanitizer_balances_across_calls` fails on the lone `</div>`.
4. In `emit_img`, replace the `match resolve_dest(...)` with
   `let url = src.to_string();`. Expected:
   `an_html_image_roost_will_not_fetch_becomes_its_alt` fails with
   `https://e.com/b.png` intact in a `src`.
5. In `emit_a`, replace the `link_open(...)` call with
   `format!("<a href=\"{}\">", esc(href))`. Expected:
   `an_html_anchor_is_built_by_link_open` fails on the `javascript:` case
   with an `href=` present.

Record each failure in the corresponding test's doc comment.

- [ ] **Step 6: Commit**

```bash
git add src/render.rs
git commit -m "render: HtmlSanitizer — allowlisted tags, escaped values, resolver-decided src and href, a balanced stack"
```

---

### Task 3: Wire the sanitizer into `markdown_html`

**Files:**
- Modify: `src/render.rs:326-392` (`markdown_html`) and insert
  `sanitize_raw_html` after `HtmlSanitizer`'s `impl`
- Test: `src/render.rs` `mod tests`

**Interfaces:**
- Consumes: `HtmlSanitizer::{new, sanitize, finish}`.
- Produces:
  ```rust
  fn sanitize_raw_html(events: &mut Vec<pulldown_cmark::Event<'_>>, project: &str, rel: &str)
  ```
  Replaces each `Start(HtmlBlock)`…`End(HtmlBlock)` run with one
  `Event::Html`, each `InlineHtml` with one `Event::Html`, and appends a
  final `Event::Html` holding `finish()` if it is non-empty.

- [ ] **Step 1: Write the failing tests**

```rust
    /// The README shape: a centred header whose `<div>` closes after
    /// twenty lines of markdown, with an `<img>` whose attributes continue
    /// on the next line. Both halves are asserted: the tag survives with
    /// both attributes (block lines were joined), and the close tag is a
    /// tag, not text (the stack crossed the markdown in between).
    #[test]
    fn a_block_is_joined_and_balanced_across_the_document() {
        let md = "<div align=\"center\">\n\n<img\n  src=\"docs/img/hero.png\"\n  width=\"900\">\n\n# Title\n\nBody *text*.\n\n</div>\n";
        let h = markdown_html(md, "proj", "README.md");
        assert!(h.contains(r#"<div align="center">"#), "{h}");
        assert!(h.contains(r#"<img src="/frag/proj/raw?path=docs/img/hero.png" width="900">"#), "{h}");
        assert!(h.contains("<h1"), "markdown between the tags still renders: {h}");
        assert!(h.contains("</div>"), "{h}");
        assert!(!h.contains("&lt;/div&gt;"), "{h}");
    }

    /// Inline HTML in a paragraph goes through the same sanitizer.
    #[test]
    fn inline_html_is_sanitized_not_neutralized() {
        let h = markdown_html("see <b>this</b> and <span onclick=\"x\">that</span>\n", "proj", "a.md");
        assert!(h.contains("<b>this</b>"), "{h}");
        assert!(h.contains("&lt;span onclick=\"x\"&gt;that&lt;/span&gt;"), "{h}");
    }

    /// A document that never closes its `<details>` still ends balanced.
    #[test]
    fn an_unclosed_tag_is_closed_at_the_end_of_the_document() {
        let h = markdown_html("<details>\n<summary>more</summary>\n\nhidden text\n", "proj", "a.md");
        assert!(h.trim_end().ends_with("</details></article>"), "{h}");
    }

    /// A block that ends inside a tag prints the fragment rather than
    /// guessing at it.
    #[test]
    fn an_unterminated_tag_at_the_end_of_a_block_is_text() {
        let h = markdown_html("<img src=\"x.png\"\n\ntext\n", "proj", "a.md");
        assert!(h.contains("&lt;img src=\"x.png\""), "{h}");
        assert!(!h.contains("<img"), "{h}");
    }
```

Note on `inline_html_is_sanitized_not_neutralized`: pulldown-cmark emits
`<span onclick="x">` and `</span>` as two `InlineHtml` events with `that`
as `Text` between them; each is sanitized on its own and the assertion is
on the concatenation `push_html` produces. Check the exact output in
Step 4 and, if push_html escapes the `"` in the text differently from
`esc` (`&quot;`), adjust the expected string to what appears; the point of
the assertion is that no `<span` element survives and the text does.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -- --test-threads=1 a_block_is_joined inline_html_is an_unclosed_tag an_unterminated_tag`
Expected: all four fail; the first on `<div align="center">` being absent
(today it is `&lt;div`).

- [ ] **Step 3: Write the pass and wire it in**

Insert after `HtmlSanitizer`'s `impl`:

```rust
/// Replaces every raw-HTML event with its sanitized form. Runs on the
/// collected vector rather than in the streaming `filter_map` because an
/// HTML block arrives one `Html` event per line, and a tag whose attributes
/// continue on the next line (the `<img\n  src=…\n  width=…>` of a centred
/// README header) is only whole once the block's lines are joined.
fn sanitize_raw_html(events: &mut Vec<pulldown_cmark::Event<'_>>, project: &str, rel: &str) {
    use pulldown_cmark::{CowStr, Event, Tag, TagEnd};
    let mut san = HtmlSanitizer::new(project, rel);
    let mut out = Vec::with_capacity(events.len());
    let mut block: Option<String> = None;
    for ev in events.drain(..) {
        match ev {
            Event::Start(Tag::HtmlBlock) => block = Some(String::new()),
            Event::Html(h) if block.is_some() => block.as_mut().unwrap().push_str(&h),
            Event::End(TagEnd::HtmlBlock) => {
                let joined = block.take().unwrap_or_default();
                out.push(Event::Html(CowStr::from(san.sanitize(&joined))));
            }
            Event::InlineHtml(h) => out.push(Event::Html(CowStr::from(san.sanitize(&h)))),
            other => out.push(other),
        }
    }
    let tail = san.finish();
    if !tail.is_empty() {
        out.push(Event::Html(CowStr::from(tail)));
    }
    *events = out;
}
```

In `markdown_html`, delete these two arms and the comment block above them
(the comment starting "raw HTML from repo content must never reach the
page" through the `InlineHtml` arm):

```rust
        Event::Html(h) => Some(Event::Text(h)),
        Event::InlineHtml(h) => Some(Event::Text(h)),
```

Replace the deleted comment with:

```rust
        // Raw HTML is not handled here. It is sanitized by
        // `sanitize_raw_html` on the collected vector below, because an
        // HTML block's lines have to be joined before a tag split across
        // them can be read. The Event::Html this function emits for links
        // is built from escaped values by link_open and is untouched by
        // that pass, which only rewrites HtmlBlock runs and InlineHtml.
```

Then, after `let mut events: Vec<Event> = events.collect();`, add:

```rust
    sanitize_raw_html(&mut events, project, rel);
```

- [ ] **Step 4: Run the tests to verify they pass, and the old ones still do**

Run: `cargo test -- --test-threads=1 markdown`
Expected: the four new tests pass; `markdown_raw_html_is_neutralized`,
`a_remote_image_is_dropped_to_its_alt_text`,
`a_hand_built_anchor_escapes_what_it_interpolates` and every other
`markdown` test still pass.

- [ ] **Step 5: Revert-check**

Change `Event::Html(h) if block.is_some() => block.as_mut().unwrap().push_str(&h)`
to push the line's sanitized form immediately instead
(`out.push(Event::Html(CowStr::from(san.sanitize(&h))))`). Expected:
`a_block_is_joined_and_balanced_across_the_document` fails, the `<img`
printed as text because its `>` was on another line. Restore. Record.

- [ ] **Step 6: Run the whole suite**

Run: `cargo test -- --test-threads=1`
Expected: all green, counts one higher than before per test added.

- [ ] **Step 7: Commit**

```bash
git add src/render.rs
git commit -m "render: raw HTML in markdown preview is sanitized, not neutralized"
```

---

### Task 4: Browser test against this repository's README shape

**Files:**
- Create: `tests/browser/mdhtml.mjs`
- Modify: `tests/browser/README.md` (run list, after the `mdlinks.mjs` line)

**Interfaces:**
- Consumes: `harness.mjs` exports `fixture, freePort, openPage, profileDir,
  startBrowser, startRoost, until`; the `send({t:"OpenTab", …})` intent
  and `.markdown-body` selector that `mdlinks.mjs` uses.
- Produces: nothing downstream.

- [ ] **Step 1: Write the test**

```js
//! Does a GitHub-style README render its HTML in a real browser?
//!
//! The Rust tests prove the fragment's text; only a browser can prove that
//! the <img> elements the sanitizer emits actually fetch their bytes through
//! the raw route, that a <details> is collapsible, and that nothing the
//! README wrote reached the page as a live element it should not be.
//!
//! The four traps in README.md apply. In particular: naturalWidth, not
//! presence — an <img> exists whether or not its request succeeded.
//!
//! Revert-the-fix, watched fail and restored:
//!   1. Restored `Event::Html(h) => Some(Event::Text(h))` in markdown_html.
//!      Assertion "five images fetched their bytes" failed with 0.
//!   2. Allowed every attribute through in HtmlSanitizer::emit.
//!      Assertion "no attribute starting with on" failed.
import { fixture, freePort, openPage, profileDir, startBrowser, startRoost, until }
  from "./harness.mjs";

const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
let fail = 0;
const ok = (c, m) => { console.log(`${c ? "  ok  " : "  FAIL"}  ${m}`); if (!c) fail++; };

const fx = await fixture();
const PNG = Uint8Array.from(atob(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
), (c) => c.charCodeAt(0));
const proj = `${fx.roots}/${fx.project}`;
await Deno.mkdir(`${proj}/docs/img`, { recursive: true });
for (const n of ["hero", "proposal", "search", "overview", "a5"]) {
  await Deno.writeFile(`${proj}/docs/img/${n}.png`, PNG);
}
// This repository's README shape, reduced: a centred header, five images
// with widths, one with attributes on following lines, a collapsible block
// left unclosed, and the three things that must not survive.
await Deno.writeTextFile(`${proj}/README.md`, [
  '<div align="center">',
  '',
  '<img src="docs/img/hero.png" alt="hero" width="900">',
  '<img',
  '  src="docs/img/proposal.png"',
  '  width="900">',
  '',
  '# title',
  '',
  '<img src="docs/img/search.png" width="600"> <img src="docs/img/overview.png"> <img src="docs/img/a5.png">',
  '<img src="https://example.invalid/x.png" alt="remote alt">',
  '<img src="docs/img/hero.png" onerror="window.__xss = 1">',
  '<script>window.__xss = 2</script>',
  '',
  '</div>',
  '',
  '<details>',
  '<summary>More</summary>',
  '',
  'hidden paragraph',
].join("\n") + "\n");

const roost = await startRoost({ repoRoot, stateDir: fx.stateDir, roots: fx.roots, port: await freePort() });
const browser = await startBrowser(profileDir(repoRoot));
let page;

try {
  page = await openPage(browser.port, `http://127.0.0.1:${roost.port}/${fx.project}`);
  const { evalIn } = page;
  await until(() => evalIn("ctrl && ctrl.readyState === 1 && !!state"), 30, "app.js");
  await evalIn(`send({ t: "OpenTab", pane: 2, tab: { k: "File", rel: "README.md", mode: "Preview" } })`);
  await until(() => evalIn(`!!document.querySelector(".markdown-body details")`), 15, "preview");

  // The onerror image points at a real file, so it is one of the six
  // <img> tags in the source; the sanitizer keeps it (minus onerror) and
  // drops only the remote one. Six elements, six with bytes.
  const loaded = await until(() => evalIn(
    `[...document.querySelectorAll(".markdown-body img")].filter((i) => i.naturalWidth === 1).length === 6`,
  ), 15, "six images");
  ok(loaded, "six images fetched their bytes through the raw route");
  ok(await evalIn(`document.querySelectorAll(".markdown-body img").length`) === 6,
    "and no seventh image exists (the remote one was dropped)");
  ok(await evalIn(`document.querySelector('.markdown-body img[width="900"]') !== null`),
    "width survived on an image");
  ok(await evalIn(`document.querySelector('.markdown-body div[align="center"]') !== null`),
    "the centred div is a real div");
  ok(await evalIn(`document.querySelector(".markdown-body").textContent.includes("remote alt")`),
    "the remote image left its alt text");
  ok(await evalIn(`!document.body.innerHTML.includes("example.invalid")`),
    "and its URL is nowhere in the page");
  ok(await evalIn(`[...document.querySelectorAll(".markdown-body *")]
      .every((e) => [...e.attributes].every((a) => !a.name.startsWith("on")))`),
    "no element carries an attribute starting with on");
  ok(await evalIn(`document.querySelector(".markdown-body script") === null`),
    "no script element exists");
  ok(await evalIn(`document.querySelector(".markdown-body").textContent.includes("<script>")`),
    "the script tag printed as text instead");
  const ran = await until(() => evalIn(`typeof window.__xss !== "undefined"`), 2);
  ok(!ran, "nothing the README wrote executed");

  const d = `document.querySelector(".markdown-body details")`;
  ok(await evalIn(`!${d}.open`), "the details block starts collapsed");
  await evalIn(`${d}.querySelector("summary").click()`);
  ok(await until(() => evalIn(`${d}.open`), 5, "details open"), "and opens on click");
  ok(await evalIn(`${d}.textContent.includes("hidden paragraph")`),
    "the unclosed details still contains its paragraph, closed by the sanitizer");
} finally {
  try { await page?.close?.(); } catch {}
  try { browser.close(); } catch {}
  try { await roost.close(); } catch {}
  await fx.cleanup();
}

console.log(fail ? `\n${fail} FAILED` : "\nall passed");
Deno.exit(fail ? 1 : 0);
```

- [ ] **Step 2: Run it**

Before the run, confirm the debug binary is built from this checkout:
`cargo build` and check the output has no `include_bytes!` error naming
another directory. If it does, `cargo clean -p roost` and rebuild.

Run: `deno run -A tests/browser/mdhtml.mjs`
Expected: `all passed`. If the machine has no Chromium the harness skips
with a message; this host has one.

- [ ] **Step 3: Revert-check**

Apply revert 1 from the file's header (restore the two `Event::Text` arms
and comment out the `sanitize_raw_html` call), rebuild, run. Expected:
"six images fetched their bytes" fails with 0. Restore. Apply revert 2 (in
`HtmlSanitizer::emit`, temporarily push every attribute through with
`for (k, v) in &tag.attrs { attrs.push_str(&format!(" {k}=\"{}\"", esc(v))); }`
in place of the allowlist loop), rebuild, run. Expected: "no element
carries an attribute starting with on" fails. Restore, rebuild, run,
`all passed`.

- [ ] **Step 4: Add the run line to the browser README**

In `tests/browser/README.md`, after the `mdlinks.mjs` line:

```
deno run -A tests/browser/mdhtml.mjs    # a GitHub-style README's raw HTML: images fetch, details fold, nothing executes
```

- [ ] **Step 5: Commit**

```bash
git add tests/browser/mdhtml.mjs tests/browser/README.md
git commit -m "browser test: a GitHub-style README's HTML renders, and nothing it wrote executes"
```

---

### Task 5: Verify against the real README and deploy

**Files:** none changed.

- [ ] **Step 1: Render this repository's README through the new code**

```bash
cargo build
BIN="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/roost"
ST=/tmp/claude-1001/-home-claude-projects-roost/d1ee67db-6e70-422b-9c14-628fdad359b3/scratchpad/rh-state
mkdir -p "$ST"
ROOST_ROOTS=$HOME/projects ROOST_STATE_DIR=$ST ROOST_STATIC=$PWD/static "$BIN" 8556 &
sleep 1
curl -s "http://127.0.0.1:8556/frag/roost/file?path=README.md" | grep -o '<img src="/frag/roost/raw?path=docs[^"]*"' | wc -l
curl -s "http://127.0.0.1:8556/frag/roost/file?path=README.md" | grep -c '&lt;img'
kill %1
```

Expected: `5` then `0`. This is the reproduction from the spec's first
section, inverted. The state dir is throwaway; nothing attaches a browser
to this instance, so no live terminal is resized.

- [ ] **Step 2: Run the full Rust suite and the two markdown browser tests**

```bash
cargo test -- --test-threads=1
deno run -A tests/browser/mdlinks.mjs
deno run -A tests/browser/mdhtml.mjs
```

Expected: all green. `mdlinks.mjs` is the regression guard for the
markdown arms this change sits beside.

- [ ] **Step 3: Deploy per `docs/deploy.md` and confirm the running binary changed**

Follow the deploy steps in `docs/deploy.md`; then open README.md in a
Preview tab in a real browser on the live instance and confirm the hero
image renders. Per CLAUDE.md, `cargo build` alone updates neither path the
service uses, so compare the installed binary's hash against the release
build before believing the preview.
