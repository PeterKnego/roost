//! HTTP request routing. URL surface (spec §URLs):
//!   /                    index page
//!   /{project}           workspace page
//!   /static/*            assets
//!   /frag/{project}/*    htmx fragments
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
    let segs: Vec<&str> = req.path.split('/').filter(|s| !s.is_empty()).collect();
    match segs.as_slice() {
        [] => http::html(w, &render::index_page(&projects::list_projects(roots))),
        ["static", rest @ ..] => serve_static(w, &rest.join("/")),
        ["frag", project, what @ ..] => serve_frag(w, req, roots, project, what),
        [project] => serve_workspace(w, roots, project),
        _ => http::not_found(w, "no such page"),
    }
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
            http::html(w, &render::tree_fragment(project, &dir, open, &settings.hide));
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
        ["theme.css"] => match std::fs::read(dir.join(".deadlight/theme.css")) {
            Ok(css) => http::respond(w, 200, "OK", "text/css; charset=utf-8", &css),
            Err(_) => http::not_found(w, "no theme.css"),
        },
        _ => http::not_found(w, "no such fragment"),
    }
}

fn path_is_suspicious(p: &str) -> bool {
    p.starts_with('/') || std::path::Path::new(p).components().any(|c| matches!(c, std::path::Component::ParentDir))
}
