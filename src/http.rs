//! Minimal HTTP/1.1 layer: GET-only request parsing, response writing,
//! percent encoding/decoding. Hand-rolled deliberately — see spec §Stack.
use std::collections::HashMap;
use std::io::{BufRead, Write};

#[derive(Debug)]
pub struct Request {
    /// Uppercase, as sent. GET everywhere except the two upload endpoints —
    /// see CLAUDE.md's amended GET-only constraint. Carried rather than
    /// discarded so `routes::handle` can dispatch a body-bearing request away
    /// from `route()`, which must never see one.
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    /// Header names lowercased. Only Host / X-Forwarded-Host are consulted.
    pub headers: HashMap<String, String>,
}

pub fn parse<R: BufRead>(r: &mut R) -> Result<Request, String> {
    let mut line = String::new();
    r.read_line(&mut line).map_err(|e| e.to_string())?;
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or("empty request")?.to_string();
    let target = parts.next().ok_or("no path")?.to_string();
    // POST is admitted only so `routes::handle` can hand it to the upload
    // endpoints; nothing below this layer treats it as reachable, and the
    // parser deliberately stops at the blank line without touching the body.
    if method != "GET" && method != "POST" {
        return Err(format!("method {method} not allowed"));
    }
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        let n = r.read_line(&mut h).map_err(|e| e.to_string())?;
        if n == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        // Host and X-Forwarded-Host gate DNS-rebinding; the rest are ignored.
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let (path, query_str) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let mut query = HashMap::new();
    for pair in query_str.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(percent_decode(k), percent_decode(v));
    }
    Ok(Request { method, path: percent_decode(&path), query, headers })
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match std::str::from_utf8(&b[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn respond(w: &mut impl Write, status: u16, reason: &str, ctype: &str, body: &[u8]) {
    respond_with(w, status, reason, ctype, &[], body);
}

/// `respond` plus caller-supplied headers, for the security headers static
/// assets carry. Kept as a separate entry point so the dozens of existing
/// `respond` call sites need no change.
pub fn respond_with(
    w: &mut impl Write,
    status: u16,
    reason: &str,
    ctype: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) {
    let _ = write!(
        w,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in extra {
        let _ = write!(w, "{k}: {v}\r\n");
    }
    let _ = write!(w, "\r\n");
    let _ = w.write_all(body);
    let _ = w.flush();
}

pub fn html(w: &mut impl Write, body: &str) {
    respond(w, 200, "OK", "text/html; charset=utf-8", body.as_bytes());
}

pub fn not_found(w: &mut impl Write, msg: &str) {
    respond(w, 404, "Not Found", "text/plain; charset=utf-8", msg.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse_str(raw: &str) -> Result<Request, String> {
        parse(&mut Cursor::new(raw.as_bytes()))
    }

    #[test]
    fn parse_keeps_the_method_and_accepts_post() {
        let r = parse_str("POST /upload/proj?dir=src HTTP/1.1\r\nHost: h\r\nContent-Length: 3\r\n\r\nabc")
            .unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/upload/proj");
        assert_eq!(r.query.get("dir").map(String::as_str), Some("src"));
    }

    /// The body must still be readable after parsing: `routes::handle` wraps the
    /// socket in a BufReader, so the first bytes of a body are frequently
    /// sitting in that buffer already. A body reader that goes back to the raw
    /// TcpStream silently loses them — the upload arrives with a hole at the
    /// front and multer reports a malformed boundary, which reads as a client
    /// bug rather than as ours.
    #[test]
    fn the_body_survives_header_parsing() {
        let raw = "POST /upload/proj HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\n\r\nhello";
        let mut r = std::io::BufReader::new(Cursor::new(raw.as_bytes()));
        let req = parse(&mut r).unwrap();
        assert_eq!(req.headers.get("content-length").map(String::as_str), Some("5"));
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut r, &mut rest).unwrap();
        assert_eq!(rest, "hello", "the body was consumed or lost by header parsing");
    }

    #[test]
    fn other_methods_are_still_refused() {
        let e = parse_str("DELETE /x HTTP/1.1\r\n\r\n").unwrap_err();
        assert!(e.contains("not allowed"), "unexpected message: {e}");
    }

    #[test]
    fn parses_path_and_query() {
        let r = parse_str("GET /frag/proj/file?path=src%2Fmain.rs&x=a+b HTTP/1.1\r\nHost: h\r\n\r\n").unwrap();
        assert_eq!(r.path, "/frag/proj/file");
        assert_eq!(r.query["path"], "src/main.rs");
        assert_eq!(r.query["x"], "a b");
    }

    #[test]
    fn parses_bare_path() {
        let r = parse_str("GET /alpha HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(r.path, "/alpha");
        assert!(r.query.is_empty());
    }

    /// Was `rejects_non_get`, which asserted POST was refused here. POST is now
    /// admitted at this layer for the two upload endpoints, so the property
    /// this test protected — that a body-bearing request cannot reach the
    /// fragment routes — moved to `routes::handle`'s dispatch, and is pinned by
    /// `post_to_an_ordinary_path_does_not_reach_the_router` in the integration
    /// suite. What stays here is that everything *else* is still refused.
    #[test]
    fn rejects_methods_other_than_get_and_post() {
        for raw in ["PUT / HTTP/1.1\r\n\r\n", "DELETE / HTTP/1.1\r\n\r\n", "PATCH / HTTP/1.1\r\n\r\n"] {
            assert!(parse_str(raw).is_err(), "should have been refused: {raw}");
        }
        assert!(parse_str("").is_err());
    }

    #[test]
    fn percent_roundtrip() {
        assert_eq!(percent_decode("a%20b%2Fc+d"), "a b/c d");
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
        assert_eq!(percent_encode("src/main file.rs"), "src/main%20file.rs");
        assert_eq!(percent_encode("a&b?c"), "a%26b%3Fc");
    }

    #[test]
    fn respond_writes_status_and_headers() {
        let mut out = Vec::new();
        respond(&mut out, 404, "Not Found", "text/plain", b"nope");
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(s.contains("Content-Length: 4\r\n"));
        assert!(s.ends_with("\r\n\r\nnope"));
    }

    #[test]
    fn extra_headers_are_emitted_once_before_the_body() {
        let mut buf = Cursor::new(Vec::new());
        respond_with(
            &mut buf,
            200,
            "OK",
            "text/css; charset=utf-8",
            &[("X-Content-Type-Options", "nosniff"), ("Content-Security-Policy", "sandbox")],
            b"body{}",
        );
        let out = String::from_utf8(buf.into_inner()).unwrap();
        let (head, body) = out.split_once("\r\n\r\n").expect("headers end exactly once");
        assert!(head.contains("X-Content-Type-Options: nosniff"));
        assert!(head.contains("Content-Security-Policy: sandbox"));
        assert!(head.contains("Content-Length: 6"));
        assert_eq!(body, "body{}", "the body must follow the blank line, not precede it");
        assert_eq!(head.matches("Content-Type:").count(), 1, "no duplicated headers");
    }

    #[test]
    fn respond_still_emits_no_extra_headers() {
        let mut buf = Cursor::new(Vec::new());
        respond(&mut buf, 200, "OK", "text/plain", b"hi");
        let out = String::from_utf8(buf.into_inner()).unwrap();
        assert!(!out.contains("Content-Security-Policy"));
        assert!(out.ends_with("\r\n\r\nhi"));
    }
}
