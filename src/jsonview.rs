//! The JSON view: a `.json` file shown indented and foldable, without
//! touching the file.
//!
//! A JSON file opens in Edit as it always has. The 👁 toggle shows this view
//! instead (`render::json_fragment`), and ✎ goes back. Nothing here writes,
//! so the file keeps its own format, which is the point: a minified
//! `package-lock.json` or an API dump can be read without being reformatted
//! into a diff.
//!
//! **Why its own parser rather than `serde_json`.** The view must show what
//! the file says, and a round trip through `serde_json::Value` does not:
//! - `12345678901234567890123` becomes `1.2345678901234568e22`;
//! - `1.0` becomes `1.0` but `1e2` becomes `100.0`;
//! - a duplicate key keeps only its last value;
//! - an escape such as `\u00e9` comes back as `é`.
//!
//! Turning on `arbitrary_precision` would fix numbers alone, and would change
//! number handling for the whole crate (the wire protocol's untagged
//! enums included). This parser keeps every scalar as the exact slice of the
//! file it came from and only finds the structure.

/// Deep enough for any real document. The bound is the recursion's: each
/// level is a stack frame on a connection thread.
pub const MAX_DEPTH: usize = 512;

#[derive(Debug, PartialEq)]
pub enum Node<'a> {
    /// Keys as written, quotes and escapes included, in file order,
    /// duplicates kept.
    Obj(Vec<(&'a str, Node<'a>)>),
    Arr(Vec<Node<'a>>),
    /// The literal as written, quotes included.
    Str(&'a str),
    Num(&'a str),
    /// `true` or `false`.
    Bool(&'a str),
    Null,
}

impl Node<'_> {
    /// Nodes in this subtree, itself included.
    pub fn count(&self) -> usize {
        1 + match self {
            Node::Obj(kv) => kv.iter().map(|(_, v)| v.count()).sum(),
            Node::Arr(v) => v.iter().map(Node::count).sum(),
            _ => 0,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct ParseError {
    pub msg: &'static str,
    /// 1-based, as an editor counts.
    pub line: usize,
    /// 1-based, in characters.
    pub col: usize,
    /// Byte offset of the error, for showing its line.
    pub at: usize,
}

pub fn is_json(rel: &str) -> bool {
    crate::assets::ext_of(rel) == "json"
}

pub fn parse(src: &str) -> Result<Node<'_>, ParseError> {
    let mut p = P { s: src, b: src.as_bytes(), i: 0 };
    // A BOM is not JSON, but editors write one and the file is otherwise fine.
    if src.starts_with('\u{feff}') {
        p.i = 3;
    }
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i < p.b.len() {
        return Err(p.err("unexpected text after the end"));
    }
    Ok(v)
}

struct P<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn err(&self, msg: &'static str) -> ParseError {
        let at = self.i.min(self.b.len());
        let before = &self.s[..floor_char(self.s, at)];
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        ParseError { msg, line, col, at }
    }

    fn ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.b.get(self.i) {
            self.i += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Node<'a>, ParseError> {
        if depth > MAX_DEPTH {
            return Err(self.err("nested too deep to show"));
        }
        match self.b.get(self.i) {
            None => Err(self.err("unexpected end of file")),
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Node::Str),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b't') => self.word("true").map(Node::Bool),
            Some(b'f') => self.word("false").map(Node::Bool),
            Some(b'n') => self.word("null").map(|_| Node::Null),
            Some(_) => Err(self.err("expected a value")),
        }
    }

    fn word(&mut self, w: &'static str) -> Result<&'a str, ParseError> {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            let start = self.i;
            self.i += w.len();
            Ok(&self.s[start..self.i])
        } else {
            Err(self.err("expected a value"))
        }
    }

    fn object(&mut self, depth: usize) -> Result<Node<'a>, ParseError> {
        self.i += 1;
        let mut out = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Node::Obj(out));
        }
        loop {
            self.ws();
            if self.b.get(self.i) != Some(&b'"') {
                return Err(self.err("expected a key in double quotes"));
            }
            let k = self.string()?;
            self.ws();
            if self.b.get(self.i) != Some(&b':') {
                return Err(self.err("expected ':' after the key"));
            }
            self.i += 1;
            self.ws();
            let v = self.value(depth + 1)?;
            out.push((k, v));
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Node::Obj(out));
                }
                _ => return Err(self.err("expected ',' or '}'")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Node<'a>, ParseError> {
        self.i += 1;
        let mut out = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Node::Arr(out));
        }
        loop {
            self.ws();
            out.push(self.value(depth + 1)?);
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Node::Arr(out));
                }
                _ => return Err(self.err("expected ',' or ']'")),
            }
        }
    }

    fn string(&mut self) -> Result<&'a str, ParseError> {
        let start = self.i;
        self.i += 1;
        loop {
            match self.b.get(self.i) {
                None => return Err(self.err("string never closed")),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(&self.s[start..self.i]);
                }
                Some(b'\\') => {
                    self.i += 1;
                    match self.b.get(self.i) {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => self.i += 1,
                        Some(b'u') => {
                            let hex = self.b.get(self.i + 1..self.i + 5);
                            if !hex.is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)) {
                                return Err(self.err("bad \\u escape"));
                            }
                            self.i += 5;
                        }
                        _ => return Err(self.err("bad escape")),
                    }
                }
                Some(c) if *c < 0x20 => return Err(self.err("control character in a string")),
                Some(_) => self.i += 1,
            }
        }
    }

    fn number(&mut self) -> Result<Node<'a>, ParseError> {
        let start = self.i;
        let digits = |p: &mut Self| {
            let s = p.i;
            while p.b.get(p.i).is_some_and(u8::is_ascii_digit) {
                p.i += 1;
            }
            p.i > s
        };
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        match self.b.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                digits(self);
            }
            _ => return Err(self.err("bad number")),
        }
        if self.b.get(self.i) == Some(&b'.') {
            self.i += 1;
            if !digits(self) {
                return Err(self.err("bad number"));
            }
        }
        if let Some(b'e' | b'E') = self.b.get(self.i) {
            self.i += 1;
            if let Some(b'+' | b'-') = self.b.get(self.i) {
                self.i += 1;
            }
            if !digits(self) {
                return Err(self.err("bad number"));
            }
        }
        Ok(Node::Num(&self.s[start..self.i]))
    }
}

/// The largest char boundary at or before `i`.
fn floor_char(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The line holding byte `at`, for showing under an error.
pub fn line_at(src: &str, at: usize) -> &str {
    let at = floor_char(src, at.min(src.len()));
    let start = src[..at].rfind('\n').map_or(0, |n| n + 1);
    let end = src[at..].find('\n').map_or(src.len(), |n| at + n);
    src[start..end].trim_end_matches('\r')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each of these is something `serde_json::Value`, the obvious shortcut,
    /// changes (see the module doc); the probe below the test shows it.
    #[test]
    fn scalars_and_keys_are_kept_exactly_as_written() {
        let src = r#"{"n": 12345678901234567890123, "e": 1e2, "a": 1, "a": 2, "s": "caf\u00e9"}"#;
        let Node::Obj(kv) = parse(src).unwrap() else { panic!() };
        assert_eq!(kv[0], (r#""n""#, Node::Num("12345678901234567890123")));
        assert_eq!(kv[1].1, Node::Num("1e2"));
        assert_eq!((kv[2].0, kv[3].0), (r#""a""#, r#""a""#), "a duplicate key is kept, both times");
        assert_eq!(kv[4].1, Node::Str(r#""caf\u00e9""#));
        // The shortcut this parser exists to avoid: serde rounds the number,
        // drops the first "a" and decodes the escape.
        let v: serde_json::Value = serde_json::from_str(src).unwrap();
        assert_ne!(v["n"].to_string(), "12345678901234567890123");
        assert_eq!(v.as_object().unwrap().len(), 4);
        assert_eq!(v["s"], "café");
    }

    #[test]
    fn structure_and_counts() {
        let n = parse(" [1, {\"x\": [true, false, null]}, \"s\", -0.5E-3, {}, []] ").unwrap();
        let Node::Arr(v) = &n else { panic!() };
        assert_eq!(v.len(), 6);
        assert_eq!(v[3], Node::Num("-0.5E-3"));
        assert_eq!(n.count(), 11);
        assert!(parse("\u{feff}{}").is_ok(), "a BOM is tolerated");
    }

    #[test]
    fn errors_say_where() {
        let e = parse("{\n  \"a\": 1,\n  \"b\" 2\n}").unwrap_err();
        assert_eq!((e.msg, e.line, e.col), ("expected ':' after the key", 3, 7));
        assert_eq!(line_at("{\n  \"a\": 1,\n  \"b\" 2\n}", e.at), "  \"b\" 2");
        for bad in ["", "{", "[1,]", "{\"a\":1,}", "01", "1.", "-", "\"x", "\"\\q\"", "\"\\u12\"", "tru", "{} x", "{'a':1}", "\"a\tb\""] {
            assert!(parse(bad).is_err(), "{bad:?} parsed");
        }
        // Column counts characters, not bytes.
        assert_eq!(parse("[\"é\" 1]").unwrap_err().col, 6);
    }

    /// Revert-checked: without the depth bound, the 100 000-deep array
    /// overflows the test thread's stack and aborts the whole run.
    #[test]
    fn a_pathologically_deep_document_is_refused_not_a_stack_overflow() {
        let deep = "[".repeat(100_000) + &"]".repeat(100_000);
        assert_eq!(parse(&deep).unwrap_err().msg, "nested too deep to show");
        let ok = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert!(parse(&ok).is_ok());
    }
}
