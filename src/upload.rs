//! The two POST endpoints: `/upload/{project}` and `/paste/{project}/{session}`.
//!
//! This is the only part of resh that accepts a request body, and the only
//! exception to the GET-only rule — which exists because it is why resh has no
//! CSRF surface. A `multipart/form-data` POST is a CORS *simple* request, so any
//! page the user visits can submit one cross-origin with no preflight and the
//! browser will send it; nothing in the response reaches the attacker, but the
//! write still happens. The `Origin` check is the whole of what stands between
//! a hostile page and an arbitrary file write, so treat it the way `wsconn.rs`
//! treats its own: a request carrying no `Origin` is refused, not defaulted.
use std::io::{BufRead, Write};
use std::path::PathBuf;

pub fn handle_post(
    w: &mut impl Write,
    _reader: &mut impl BufRead,
    req: &crate::http::Request,
    _roots: &[PathBuf],
) {
    let _ = req;
    crate::http::respond(w, 404, "Not Found", "text/plain; charset=utf-8", b"no such endpoint");
}
