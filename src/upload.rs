//! The POST endpoints: `/upload/{project}` and (Task 5) `/paste/{project}/{session}`.
//!
//! This is the only part of resh that accepts a request body, and the only
//! exception to the GET-only rule — which exists because it is why resh has no
//! CSRF surface. A `multipart/form-data` POST is a CORS *simple* request, so any
//! page the user visits can submit one cross-origin with no preflight and the
//! browser will send it; nothing in the response reaches the attacker, but the
//! write still happens. The `Origin` check below is the whole of what stands
//! between a hostile page and an arbitrary file write, so it is treated the way
//! `wsconn.rs` treats its own: a request carrying no `Origin` is refused, not
//! defaulted.
//!
//! Parts stream to disk rather than being buffered. That is the reason this is
//! HTTP at all: the caps can then be enforced *mid-body*, with the connection
//! dropped before the rest arrives, where a whole-payload transport can only
//! decide after it has already accepted everything.
use crate::fileops::UploadTemp;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

pub fn handle_post(
    w: &mut impl Write,
    reader: &mut (impl BufRead + Send),
    req: &crate::http::Request,
    roots: &[PathBuf],
) {
    let allowed = crate::config::allowed_origins();
    // The same DNS-rebinding gate the GET router applies: a hostile name
    // resolved to 127.0.0.1 is same-origin to the browser.
    if !crate::origin::host_allowed(
        req.headers.get("host").map(String::as_str),
        req.headers.get("x-forwarded-host").map(String::as_str),
        &allowed,
    ) {
        return crate::http::respond(w, 403, "Forbidden", "text/plain; charset=utf-8", b"host not allowed");
    }
    let origin = req.headers.get("origin").map(String::as_str);
    if !crate::origin::origin_allowed(origin, &allowed) {
        // Logged for the same reason the router logs a rejected host: behind a
        // proxy a misconfigured allowlist otherwise looks like an outage.
        eprintln!("resh: rejected upload origin={origin:?} (set allowed_origins)");
        return crate::http::respond(w, 403, "Forbidden", "text/plain; charset=utf-8", b"origin not allowed");
    }

    let segs: Vec<&str> = req.path.split('/').filter(|s| !s.is_empty()).collect();
    match segs.as_slice() {
        ["upload", project @ ..] if !project.is_empty() => {
            do_upload(w, reader, req, roots, &project.join("/"))
        }
        // The session is the *last* segment, because a project identifier may
        // itself be multi-segment (/paste/karpie/src/term) — the same
        // split-from-the-right rule the frag route uses.
        ["paste", rest @ ..] if rest.len() >= 2 => {
            let (session, project) = rest.split_last().expect("rest.len() >= 2");
            do_paste(w, reader, req, roots, &project.join("/"), session)
        }
        _ => crate::http::respond(w, 404, "Not Found", "text/plain; charset=utf-8", b"no such endpoint"),
    }
}

/// Why a request stopped early. Distinct from a per-file error: these abandon
/// the whole body unread, because continuing to read is exactly what the caps
/// exist to prevent.
enum Halt {
    TooManyParts,
    TooLarge,
    Malformed(String),
}

fn do_upload(
    w: &mut impl Write,
    reader: &mut (impl BufRead + Send),
    req: &crate::http::Request,
    roots: &[PathBuf],
    project: &str,
) {
    let Some(dir) = crate::projects::resolve_project(roots, project) else {
        return crate::http::respond(w, 404, "Not Found", "text/plain; charset=utf-8", b"no such project");
    };
    let sub = req.query.get("dir").cloned().unwrap_or_default();
    let ctype = req.headers.get("content-type").cloned().unwrap_or_default();
    let Ok(boundary) = multer::parse_boundary(&ctype) else {
        return crate::http::respond(
            w,
            400,
            "Bad Request",
            "text/plain; charset=utf-8",
            b"expected multipart/form-data",
        );
    };
    let len: u64 = req.headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let cap = crate::config::max_upload_bytes();

    match receive(reader, len, &boundary, &dir, &sub, cap) {
        Ok(results) => {
            let body = serde_json::json!({ "results": results }).to_string();
            crate::http::respond(w, 200, "OK", "application/json", body.as_bytes());
        }
        Err(Halt::TooManyParts) => {
            let msg =
                format!("too many files in one upload (limit {})", crate::config::MAX_UPLOAD_PARTS);
            crate::http::respond(w, 413, "Payload Too Large", "text/plain; charset=utf-8", msg.as_bytes());
        }
        Err(Halt::TooLarge) => {
            let msg = format!("upload too large (limit {cap} bytes)");
            crate::http::respond(w, 413, "Payload Too Large", "text/plain; charset=utf-8", msg.as_bytes());
        }
        Err(Halt::Malformed(e)) => {
            crate::http::respond(w, 400, "Bad Request", "text/plain; charset=utf-8", e.as_bytes());
        }
    }
}

/// Adapts the blocking body reader into the `Stream` multer wants.
///
/// Blocking inside a stream is fine here: `block_on` drives it on the
/// connection's own thread, which has nothing else to do. This is what lets a
/// runtime-agnostic parser work in a server with no async runtime — multer's
/// `tokio-io` feature stays off, and `cargo tree -i tokio` finds nothing.
fn body_stream(
    reader: &mut (impl BufRead + Send),
    len: u64,
) -> impl futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + '_ {
    // `take(len)` rather than reading to EOF: the client sends the body and then
    // waits for a response, so reading to EOF would block until it gave up.
    let mut body = reader.take(len);
    futures_util::stream::iter(std::iter::from_fn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        match body.read(&mut buf) {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some(Ok(bytes::Bytes::from(buf)))
            }
            Err(e) => Some(Err(e)),
        }
    }))
}

fn receive(
    reader: &mut (impl BufRead + Send),
    len: u64,
    boundary: &str,
    dir: &Path,
    sub: &str,
    cap: u64,
) -> Result<Vec<serde_json::Value>, Halt> {
    let mut mp = multer::Multipart::new(body_stream(reader, len), boundary);

    futures_executor::block_on(async move {
        let mut results = Vec::new();
        let mut parts = 0usize;
        let mut total: u64 = 0;

        while let Some(mut field) =
            mp.next_field().await.map_err(|e| Halt::Malformed(e.to_string()))?
        {
            parts += 1;
            if parts > crate::config::MAX_UPLOAD_PARTS {
                return Err(Halt::TooManyParts);
            }
            let name = field.file_name().unwrap_or_default().to_string();

            let mut sink = match UploadTemp::create(dir, sub, &name) {
                Ok(t) => Some(t),
                Err(e) => {
                    results.push(serde_json::json!({"name": name, "ok": false, "error": e}));
                    None
                }
            };
            let mut failed: Option<String> = None;

            // A rejected part is still drained. multer is a single pass over one
            // stream: abandoning a field mid-way leaves the parser positioned
            // inside it, and every *later* part is lost or misread — the
            // difference between "one file was rejected" and "one was rejected
            // and the rest silently vanished".
            while let Some(chunk) =
                field.chunk().await.map_err(|e| Halt::Malformed(e.to_string()))?
            {
                total += chunk.len() as u64;
                if total > cap {
                    // `sink` drops here, which removes the partial temp file.
                    return Err(Halt::TooLarge);
                }
                if let Some(t) = sink.as_mut() {
                    if let Err(e) = t.write(&chunk) {
                        failed = Some(e);
                        sink = None;
                    }
                }
            }

            match (sink, failed) {
                (Some(t), _) => match t.commit() {
                    Ok(_) => results.push(serde_json::json!({"name": name, "ok": true})),
                    Err(e) => {
                        results.push(serde_json::json!({"name": name, "ok": false, "error": e}))
                    }
                },
                (None, Some(e)) => {
                    results.push(serde_json::json!({"name": name, "ok": false, "error": e}))
                }
                (None, None) => {} // already reported by UploadTemp::create
            }
        }
        Ok(results)
    })
}

fn do_paste(
    w: &mut impl Write,
    reader: &mut (impl BufRead + Send),
    req: &crate::http::Request,
    roots: &[PathBuf],
    project: &str,
    session: &str,
) {
    if !crate::session::valid_name(session) {
        return crate::http::respond(w, 400, "Bad Request", "text/plain; charset=utf-8", b"invalid session name");
    }
    if crate::projects::resolve_project(roots, project).is_none() {
        return crate::http::respond(w, 404, "Not Found", "text/plain; charset=utf-8", b"no such project");
    }
    // Checked before any bytes are accepted. Writing markers into a dead PTY is
    // not destructive, but reporting success for a paste nobody will ever see is
    // worse than an error.
    if !crate::session::has_session(project, session) {
        let msg = format!("no such session: {session}");
        return crate::http::respond(w, 404, "Not Found", "text/plain; charset=utf-8", msg.as_bytes());
    }

    let dir = crate::paste::scratch_dir(project);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        let msg = format!("cannot create paste directory: {e}");
        return crate::http::respond(w, 500, "Internal Server Error", "text/plain; charset=utf-8", msg.as_bytes());
    }

    let ctype = req.headers.get("content-type").cloned().unwrap_or_default();
    let Ok(boundary) = multer::parse_boundary(&ctype) else {
        return crate::http::respond(
            w,
            400,
            "Bad Request",
            "text/plain; charset=utf-8",
            b"expected multipart/form-data",
        );
    };
    let len: u64 = req.headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let cap = crate::config::max_upload_bytes();

    match receive_image(reader, len, &boundary, &dir, cap) {
        Ok(path) => {
            // The bracketed-paste markers are load-bearing: the same path
            // arriving as raw characters is inserted as literal text instead of
            // being read as an image. See the spec's evidence appendix.
            let mut payload = Vec::with_capacity(path.as_os_str().len() + 12);
            payload.extend_from_slice(b"\x1b[200~");
            payload.extend_from_slice(path.to_string_lossy().as_bytes());
            payload.extend_from_slice(b"\x1b[201~");
            // On this thread, with no lock held: the hub is not involved in an
            // upload at all, which is what makes a blocking PTY write safe here.
            let key = crate::session::key_for(project, session);
            match crate::session::write_input(&key, &payload) {
                Ok(()) => crate::http::respond(w, 200, "OK", "application/json", b"{\"ok\":true}"),
                Err(e) => {
                    let msg = format!("paste failed: {e}");
                    crate::http::respond(w, 500, "Internal Server Error", "text/plain; charset=utf-8", msg.as_bytes())
                }
            }
        }
        Err(Halt::TooLarge) => {
            let msg = format!("pasted image too large (limit {cap} bytes)");
            crate::http::respond(w, 413, "Payload Too Large", "text/plain; charset=utf-8", msg.as_bytes())
        }
        Err(Halt::TooManyParts) => crate::http::respond(
            w,
            400,
            "Bad Request",
            "text/plain; charset=utf-8",
            b"a paste carries one image",
        ),
        Err(Halt::Malformed(e)) => {
            crate::http::respond(w, 400, "Bad Request", "text/plain; charset=utf-8", e.as_bytes())
        }
    }
}

/// `receive`'s sibling, differing in one thing: the format is decided from the
/// *first chunk* rather than by buffering the whole image to inspect it, which
/// keeps this streaming like every other part. The extension it picks is what
/// makes the paste readable at the other end, so an unrecognised format is
/// refused rather than guessed at.
fn receive_image(
    reader: &mut (impl BufRead + Send),
    len: u64,
    boundary: &str,
    dir: &Path,
    cap: u64,
) -> Result<PathBuf, Halt> {
    let mut mp = multer::Multipart::new(body_stream(reader, len), boundary);

    futures_executor::block_on(async move {
        let mut field = mp
            .next_field()
            .await
            .map_err(|e| Halt::Malformed(e.to_string()))?
            .ok_or_else(|| Halt::Malformed("no image in the request".into()))?;

        let first = field
            .chunk()
            .await
            .map_err(|e| Halt::Malformed(e.to_string()))?
            .ok_or_else(|| Halt::Malformed("pasted image is empty".into()))?;

        let ext = crate::paste::extension_of(&first).ok_or_else(|| {
            Halt::Malformed("clipboard image is not a PNG, JPEG, GIF or WebP".into())
        })?;

        let name = crate::paste::free_name(dir, ext).map_err(Halt::Malformed)?;
        let mut sink = UploadTemp::create(dir, "", &name).map_err(Halt::Malformed)?;

        let mut total = first.len() as u64;
        if total > cap {
            return Err(Halt::TooLarge);
        }
        sink.write(&first).map_err(Halt::Malformed)?;

        while let Some(chunk) = field.chunk().await.map_err(|e| Halt::Malformed(e.to_string()))? {
            total += chunk.len() as u64;
            if total > cap {
                return Err(Halt::TooLarge); // `sink` drops, removing the partial file
            }
            sink.write(&chunk).map_err(Halt::Malformed)?;
        }
        sink.commit().map_err(Halt::Malformed)
    })
}
