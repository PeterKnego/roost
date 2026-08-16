//! HTTP request routing. URL surface (spec §URLs):
//!   /                    directory picker (?at=<rel> browses a subdirectory)
//!   /{project}           workspace page — {project} may be multi-segment,
//!                        e.g. /karpie/src, naming a nested directory
//!   /static/*            assets
//!   /frag/{project}/*    htmx fragments — {project} may likewise be
//!                        multi-segment; the *last* segment is always the
//!                        fragment kind (tree/file/changes/status/diff/theme.css)
//! Fragment errors render as 200 + hint (htmx ignores 4xx bodies).
use crate::{config, gitio, http, projects, render};
use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

pub fn handle(stream: TcpStream, roots: &[PathBuf]) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(read_half) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read_half);
    let mut w = stream;
    match http::parse(&mut reader) {
        Ok(req) => route(&mut w, &req, roots),
        Err(e) => http::respond(&mut w, 400, "Bad Request", "text/plain", e.as_bytes()),
    }
}

fn route(w: &mut impl Write, req: &http::Request, roots: &[PathBuf]) {
    // DNS rebinding: a hostile name resolved to 127.0.0.1 is same-origin to the
    // browser, so CORS stops protecting these reads. Behind `tailscale serve`
    // the real name arrives as X-Forwarded-Host. See spec §Security.
    if !crate::origin::host_allowed(
        req.headers.get("host").map(String::as_str),
        req.headers.get("x-forwarded-host").map(String::as_str),
        &config::allowed_origins(),
    ) {
        // Logged, not silent: behind a proxy the effective host is not obvious,
        // and a misconfigured allowlist otherwise looks like an outage.
        eprintln!(
            "deadlight: rejected host={:?} x-forwarded-host={:?} (set allowed_origins)",
            req.headers.get("host"),
            req.headers.get("x-forwarded-host")
        );
        return http::respond(w, 403, "Forbidden", "text/plain; charset=utf-8", b"host not allowed");
    }
    let segs: Vec<&str> = req.path.split('/').filter(|s| !s.is_empty()).collect();
    match segs.as_slice() {
        [] => serve_index(w, req, roots),
        ["static", rest @ ..] => serve_static(w, &rest.join("/")),
        // The fragment *kind* (tree/file/…) is always exactly the last
        // segment (routes.rs's fragment endpoints never take path segments
        // of their own — `dir=`/`path=` arrive as query params, see
        // serve_frag below), so splitting from the right rather than
        // assuming `project` is a single segment is unambiguous and leaves
        // every existing single-segment call (`/frag/proj/tree`) unchanged.
        ["frag", rest @ ..] if rest.len() >= 2 => {
            let (what, proj_segs) = rest.split_last().expect("len >= 2 guarantees a last element");
            serve_frag(w, req, roots, &proj_segs.join("/"), std::slice::from_ref(what))
        }
        // `[project, rest @ ..]` accepts one or more segments; they're
        // rejoined into a single nested rel path below rather than treating
        // `rest` as something separate from `project` — e.g. /karpie/src is
        // one workspace identifier, "karpie/src", not project "karpie" with
        // some other meaning attached to "src". This is safe to fall
        // through to unconditionally (no guard, unlike the frag arm above)
        // because the arms above it already intercept every RESERVED first
        // segment that has a real meaning here: "static" and "frag" are
        // matched literally, in source order, before this arm is ever
        // tried, and "ws" is intercepted even earlier — lib.rs's `is_ws`
        // diverts any request whose raw path starts with "/ws/" to
        // route_ws before it ever reaches HTTP parsing, let alone this
        // match. A single-segment `/frag` or `/static` (no trailing
        // segment) still falls through to here, but then lands on
        // `resolve_project`, whose own first-segment RESERVED check (see
        // projects.rs) refuses it independently — belt and suspenders, not
        // reliance on this comment being right forever.
        // Every non-empty path lands here or in one of the two arms above,
        // so this is deliberately the last arm, not followed by a
        // catch-all: `[]` (handled above) and `[project, rest @ ..]`
        // together are exhaustive over segs, and the compiler enforces
        // that (an unreachable-pattern warning caught it when this arm
        // used to sit behind a redundant `_`).
        [project, rest @ ..] => {
            let full = if rest.is_empty() { project.to_string() } else { format!("{project}/{}", rest.join("/")) };
            serve_workspace(w, roots, &full)
        }
    }
}

/// `/` — the directory picker. `?at=<rel>` browses one directory; with no
/// `at` (or one that fails to resolve — refused the same way opening it as
/// a workspace would be) it shows the merged top level of both ROOTS.
fn serve_index(w: &mut impl Write, req: &http::Request, roots: &[PathBuf]) {
    let requested = req.query.get("at").map(String::as_str).unwrap_or("");
    let (at, entries, refused) = match projects::list_dir(roots, requested) {
        Some(entries) => (requested, entries, false),
        None => ("", projects::list_dir(roots, "").expect("top level never fails to resolve"), true),
    };
    http::html(w, &render::index_page(at, &entries, refused));
}

fn serve_workspace(w: &mut impl Write, roots: &[PathBuf], project: &str) {
    let Some(dir) = projects::resolve_project(roots, project) else {
        return http::not_found(w, "no such project");
    };
    let settings = config::for_project(&dir);
    let has_theme_css = dir.join(".deadlight/theme.css").is_file();
    http::html(w, &render::workspace_page(project, &settings, has_theme_css));
}

fn serve_static(w: &mut impl Write, rel: &str) {
    let base = Path::new(STATIC_DIR);
    let (Ok(f), Ok(basec)) = (base.join(rel).canonicalize(), base.canonicalize()) else {
        return http::not_found(w, "no such asset");
    };
    if !f.starts_with(&basec) || !f.is_file() {
        return http::not_found(w, "no such asset");
    }
    let ctype = match f.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    };
    match std::fs::read(&f) {
        Ok(body) => http::respond(w, 200, "OK", ctype, &body),
        Err(_) => http::not_found(w, "unreadable"),
    }
}

fn serve_frag(
    w: &mut impl Write,
    req: &http::Request,
    roots: &[PathBuf],
    project: &str,
    what: &[&str],
) {
    let Some(dir) = projects::resolve_project(roots, project) else {
        return http::not_found(w, "no such project");
    };
    let settings = config::for_project(&dir);
    match what {
        ["tree"] => {
            let open = req.query.get("open").map(String::as_str).unwrap_or("");
            match req.query.get("dir") {
                None => http::html(w, &render::tree_fragment(project, &dir, open, &settings.hide)),
                // `dir` names a subtree the client wants to lazily expand —
                // it arrives from the network, so it must be confined
                // through `safe_resolve` before any read, exactly like
                // `file`'s `path`. A `dir` that resolves outside the
                // project (or doesn't exist, or isn't a directory) renders
                // the standard hint, never a listing.
                Some(rel) => match projects::safe_resolve(&dir, rel) {
                    Ok(sub) if sub.is_dir() => {
                        let mut out = String::new();
                        render::tree_level(project, &sub, rel, open, &settings.hide, &mut out);
                        http::html(w, &out);
                    }
                    Ok(_) => http::html(w, &render::hint("not a directory")),
                    Err(e) => http::html(w, &render::hint(&e)),
                },
            }
        }
        ["file"] => match req.query.get("path") {
            None => http::html(w, &render::hint("missing path")),
            Some(rel) => match projects::safe_resolve(&dir, rel)
                .and_then(|p| projects::read_text_file(&p))
            {
                Ok(content) => http::html(w, &render::file_fragment(rel, &content)),
                Err(e) => http::html(w, &render::hint(&e)),
            },
        },
        ["changes"] => match gitio::status(&dir) {
            Ok(st) => http::html(w, &render::changes_fragment(project, &st)),
            Err(e) => http::html(w, &render::hint(&e)),
        },
        ["status"] => {
            let st = gitio::status(&dir)
                .unwrap_or(gitio::Status { branch: String::new(), changes: vec![] });
            http::html(w, &render::status_fragment(&st));
        }
        ["diff"] => {
            let path = req.query.get("path").map(String::as_str);
            if let Some(p) = path {
                if path_is_suspicious(p) {
                    return http::html(w, &render::hint("path outside project"));
                }
            }
            match gitio::diff(&dir, path) {
                Ok(d) if d.trim().is_empty() => http::html(w, &render::hint("no diff")),
                Ok(d) => http::html(
                    w,
                    &format!(
                        "<div class=\"path\">{}</div><div class=\"diffview\">{}</div>",
                        render::esc(path.unwrap_or("all changes")),
                        render::diff_html(&d)
                    ),
                ),
                Err(e) => http::html(w, &render::hint(&e)),
            }
        }
        // Resolved through `safe_resolve`, not a bare `fs::read`, so a
        // `.deadlight/theme.css` that is a symlink pointing outside the
        // project (planted by a cloned repo) is refused rather than served
        // to the browser as text/css. Every other file read in this module
        // already goes through this confinement; this one predates it.
        ["theme.css"] => match projects::safe_resolve(&dir, ".deadlight/theme.css")
            .and_then(|p| std::fs::read(&p).map_err(|e| e.to_string()))
        {
            Ok(css) => http::respond(w, 200, "OK", "text/css; charset=utf-8", &css),
            Err(_) => http::not_found(w, "no theme.css"),
        },
        _ => http::not_found(w, "no such fragment"),
    }
}

fn path_is_suspicious(p: &str) -> bool {
    p.starts_with('/') || std::path::Path::new(p).components().any(|c| matches!(c, std::path::Component::ParentDir))
}
