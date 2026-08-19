//! HTTP request routing. URL surface (spec §URLs):
//!   /                    directory picker (?at=<rel> browses a subdirectory)
//!   /{project}           workspace page — {project} may be multi-segment,
//!                        e.g. /karpie/src, naming a nested directory
//!   /static/*            assets
//!   /frag/{project}/*    htmx fragments — {project} may likewise be
//!                        multi-segment; the *last* segment is always the
//!                        fragment kind (tree/file/changes/status/diff/theme.css)
//! Fragment errors render as 200 + hint (htmx ignores 4xx bodies).
use crate::{config, gitio, http, projects, registry, render};
use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
            "resh: rejected host={:?} x-forwarded-host={:?} (set allowed_origins)",
            req.headers.get("host"),
            req.headers.get("x-forwarded-host")
        );
        return http::respond(w, 403, "Forbidden", "text/plain; charset=utf-8", b"host not allowed");
    }
    let segs: Vec<&str> = req.path.split('/').filter(|s| !s.is_empty()).collect();
    match segs.as_slice() {
        [] => serve_index(w, req, roots),
        ["static", rest @ ..] => serve_static(w, &rest.join("/")),
        // Cross-project data (the header strip) has no single project to
        // hang off — `serve_frag` below always resolves a project first, so
        // this cannot be folded into it. Must come before the general frag
        // arm and the catch-all `[project, rest @ ..]` below: with only two
        // segments `_projects` doesn't satisfy that arm's `rest.len() >= 2`
        // guard, so without this arm sitting first, `/frag/_projects` would
        // fall all the way through to the catch-all and be treated as a
        // request to open a workspace project literally named
        // "frag/_projects" instead of serving the fragment.
        ["frag", "_projects"] => {
            let current = req.query.get("current").map(String::as_str).unwrap_or("");
            let ps = registry::known_projects(roots);
            http::html(w, &render::projects_strip(current, &ps));
        }
        // Root scope, not /static/sw.js: a service worker may only control
        // URLs under its own path, and this one has to focus and navigate
        // workspace tabs at /{project}.
        ["sw.js"] => serve_static(w, "sw.js"),
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
        // segment that has a real meaning here: "static", "sw.js", and
        // "frag" are matched literally, in source order, before this arm is
        // ever tried, and "ws" is intercepted even earlier — lib.rs's `is_ws`
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
    let ps = registry::known_projects(roots);
    http::html(w, &render::index_page(at, &entries, refused, &ps));
}

fn serve_workspace(w: &mut impl Write, roots: &[PathBuf], project: &str) {
    let Some(dir) = projects::resolve_project(roots, project) else {
        return http::not_found(w, "no such project");
    };
    let settings = config::for_project(&dir);
    let has_theme_css = dir.join(".resh/theme.css").is_file();
    let key = projects::storage_key(project);
    http::html(w, &render::workspace_page(project, &key, &settings, has_theme_css));
}

/// Serialises the tests that set the process-global `RESH_STATIC`/`HOME`.
/// cargo runs a binary's tests in parallel threads, so without this two of
/// them interleave and one sees the other's environment mid-body — a
/// flakiness this project has shipped once before (see SESSION_ENV_LOCK).
#[cfg(test)]
pub static ASSET_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn content_type(rel: &str) -> &'static str {
    match Path::new(rel).extension().and_then(|e| e.to_str()).unwrap_or("") {
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        _ => "application/octet-stream",
    }
}

const NOSNIFF: (&str, &str) = ("X-Content-Type-Options", "nosniff");
const SANDBOX: (&str, &str) = ("Content-Security-Policy", "sandbox");

/// `~/.config/resh/static`, the optional user overlay. Absent on a fresh
/// install, which is not an error — the layer is simply skipped.
fn user_static_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".config/resh/static"))
}

/// Reads `rel` under `base`, confined. Returns `None` for "not there" and
/// for "cannot look" alike: this is a read path, so falling through to the
/// next layer is the safe response to both — the codebase-wide rule that
/// absence of evidence is not evidence of absence applies to a missing
/// overlay file too, and the safe action here is identical either way
/// (try the next layer), unlike a destructive path where the two must
/// never be conflated.
fn read_confined(base: &Path, rel: &str) -> Option<Vec<u8>> {
    let basec = base.canonicalize().ok()?;
    let f = basec.join(rel).canonicalize().ok()?;
    if !f.starts_with(&basec) || !f.is_file() {
        return None;
    }
    std::fs::read(&f).ok()
}

/// Layered lookup — see docs/superpowers/specs/2026-08-19-embedded-assets-design.md.
///
///   1. $RESH_STATIC        any class   (operator runtime switch)
///   2. ~/.config/resh/static  theme class only
///   3. embedded            any class   (always present)
///
/// The class restriction on layer 2 is the enforcement mechanism, not a
/// check that could be forgotten: a code-class path never consults it.
fn serve_static(w: &mut impl Write, rel: &str) {
    // Before any layer, so a traversal attempt cannot reveal which layers exist.
    let Some(rel) = crate::assets::normalize(rel) else {
        return http::not_found(w, "no such asset");
    };
    let ctype = content_type(rel);

    if let Some(dir) = std::env::var_os("RESH_STATIC") {
        if let Some(body) = read_confined(Path::new(&dir), rel) {
            return http::respond_with(w, 200, "OK", ctype, &[NOSNIFF], &body);
        }
    }

    if crate::assets::class_of(rel) == crate::assets::Class::Theme {
        if let Some(body) = user_static_dir().and_then(|d| read_confined(&d, rel)) {
            return http::respond_with(w, 200, "OK", ctype, &[NOSNIFF, SANDBOX], &body);
        }
    }

    match crate::assets::get(rel) {
        Some(body) => http::respond_with(w, 200, "OK", ctype, &[NOSNIFF], body),
        None => http::not_found(w, "no such asset"),
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
        // `.resh/theme.css` that is a symlink pointing outside the
        // project (planted by a cloned repo) is refused rather than served
        // to the browser as text/css. Every other file read in this module
        // already goes through this confinement; this one predates it.
        ["theme.css"] => match projects::safe_resolve(&dir, ".resh/theme.css")
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `HOME` is process-global, and cargo runs a binary's tests in parallel
    /// threads. `ASSET_ENV_LOCK` keeps these tests from interleaving with
    /// each other, but each still has to leave `HOME` exactly as it found
    /// it — otherwise a later test in the same binary run inherits a `HOME`
    /// pointed at a `tempfile::TempDir` that has already been deleted.
    /// Restoring on `Drop` (rather than a manual statement at the end of
    /// each test body) also covers a panicking assertion, which a plain
    /// "restore at the bottom" would miss.
    struct HomeGuard(Option<std::ffi::OsString>);
    impl HomeGuard {
        fn set(path: &Path) -> Self {
            let prev = std::env::var_os("HOME");
            std::env::set_var("HOME", path);
            HomeGuard(prev)
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    fn serve(rel: &str) -> String {
        let mut buf: Vec<u8> = Vec::new();
        serve_static(&mut buf, rel);
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[test]
    fn an_absent_overlay_serves_the_embedded_copy() {
        let _g = ASSET_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("RESH_STATIC");
        let home = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(home.path());
        let out = serve("style.css");
        assert!(out.starts_with("HTTP/1.1 200 OK"));
        assert!(out.contains("X-Content-Type-Options: nosniff"));
        assert!(!out.contains("Content-Security-Policy"), "embedded assets are not untrusted");
    }

    #[test]
    fn resh_static_overrides_one_file_and_the_rest_fall_through() {
        let _g = ASSET_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("style.css"), "/*OVERRIDDEN*/").unwrap();
        std::env::set_var("RESH_STATIC", d.path());
        let home = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(home.path());

        assert!(serve("style.css").contains("/*OVERRIDDEN*/"));
        // Not present in the overlay dir, so it must still resolve.
        assert!(serve("app.js").starts_with("HTTP/1.1 200 OK"));

        std::env::remove_var("RESH_STATIC");
    }

    /// The rule the whole class split exists for. A .js in the user dir must
    /// not merely be "blocked" — the layer is never consulted for it, so the
    /// embedded copy is what comes back.
    #[test]
    fn the_user_directory_may_not_replace_code() {
        let _g = ASSET_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("RESH_STATIC");
        let home = tempfile::tempdir().unwrap();
        let userdir = home.path().join(".config/resh/static");
        std::fs::create_dir_all(&userdir).unwrap();
        std::fs::write(userdir.join("app.js"), "alert('pwned')").unwrap();
        std::fs::write(userdir.join("style.css"), "/*MINE*/").unwrap();
        let _home = HomeGuard::set(home.path());

        let js = serve("app.js");
        assert!(!js.contains("pwned"), "a user-dir .js must never be served");
        assert!(js.starts_with("HTTP/1.1 200 OK"), "it falls through to embedded, not 404");

        let css = serve("style.css");
        assert!(css.contains("/*MINE*/"), "but theme-class assets DO come from there");
        assert!(css.contains("Content-Security-Policy: sandbox"), "and are sandboxed");
    }

    /// Identical 404 either way: a difference here would let a caller probe
    /// which layers are configured.
    #[test]
    fn traversal_is_refused_the_same_with_and_without_an_overlay() {
        let _g = ASSET_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(home.path());
        let probes = ["../Cargo.toml", "/etc/passwd", "themes/../../Cargo.toml", "a\\..\\b"];

        std::env::remove_var("RESH_STATIC");
        let without: Vec<String> = probes.iter().map(|p| serve(p)).collect();

        let d = tempfile::tempdir().unwrap();
        std::env::set_var("RESH_STATIC", d.path());
        let with: Vec<String> = probes.iter().map(|p| serve(p)).collect();
        std::env::remove_var("RESH_STATIC");

        for (i, p) in probes.iter().enumerate() {
            assert!(without[i].starts_with("HTTP/1.1 404"), "{p} must 404");
            assert_eq!(without[i], with[i], "{p}: responses must be byte-identical");
        }
    }
}
