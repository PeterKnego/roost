//! Minimal HTTP/1.1 layer: GET-only request parsing, response writing,
//! percent encoding/decoding. Hand-rolled deliberately — see spec §Stack.
use std::collections::HashMap;
use std::io::{BufRead, Write};

pub struct Request {
    pub path: String,
    pub query: HashMap<String, String>,
}

pub fn parse<R: BufRead>(r: &mut R) -> Result<Request, String> {
    let mut line = String::new();
    r.read_line(&mut line).map_err(|e| e.to_string())?;
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or("empty request")?;
    let target = parts.next().ok_or("no path")?.to_string();
    if method != "GET" {
        return Err(format!("method {method} not allowed"));
    }
    loop {
        // drain headers; we need none of them for plain GETs
        let mut h = String::new();
        let n = r.read_line(&mut h).map_err(|e| e.to_string())?;
        if n == 0 || h == "\r\n" || h == "\n" {
            break;
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
    Ok(Request { path: percent_decode(&path), query })
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
    let _ = write!(
        w,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
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

    #[test]
    fn rejects_non_get() {
        assert!(parse_str("POST / HTTP/1.1\r\n\r\n").is_err());
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
}
