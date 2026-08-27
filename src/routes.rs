//! HTTP request routing. URL surface (spec §URLs):
//!   /                    overview shell (?at=<rel> reaches the directory
//!                        picker instead, to browse a subdirectory)
//!   /{project}           workspace page — {project} may be multi-segment,
//!                        e.g. /karpie/src, naming a nested directory
//!   /static/*            assets
//!   /frag/{project}/*    htmx fragments — {project} may likewise be
//!                        multi-segment; the *last* segment is normally the
//!                        fragment kind (tree/file/changes/status/diff/theme.css),
//!                        except /frag/{project}/theme/{rel}, whose {rel}
//!                        after the last "theme" segment is itself a path
//!                        into the project's `.resh/theme/` directory
//! Fragment errors render as 200 + hint (htmx ignores 4xx bodies).
use crate::{config, gitio, http, launch, projects, registry, render};
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
        // POST is the upload surface and nothing else. It deliberately does not
        // reach `route`, so no existing route can be invoked with a body — the
        // property the old `rejects_non_get` parser test used to guarantee.
        Ok(req) if req.method == "POST" => {
            // The 10s read timeout above is an inactivity timer sized for a
            // request that arrives in one packet. A 100 MB body over a tailnet
            // hiccup exceeds it while making perfectly good progress, and the
            // upload would die mid-stream with no error the user can act on.
            let _ = w.set_read_timeout(Some(Duration::from_secs(60)));
            crate::upload::handle_post(&mut w, &mut reader, &req, roots);
        }
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
        // Same shape as _projects above, same reason it sits before the
        // general frag arm. Unlike _projects this does not filter to live
        // projects — see worktrees_strip's doc comment.
        ["frag", "_worktrees"] => {
            let current = req.query.get("current").map(String::as_str).unwrap_or("");
            let ps = if req.query.get("state").map(String::as_str) == Some("1") {
                registry::known_projects_with_state(roots)
            } else {
                registry::known_projects(roots)
            };
            http::html(w, &render::worktrees_strip(current, &ps));
        }
        // Same shape as _projects/_worktrees above, same reason it sits
        // before the general frag arm.
        ["frag", "_overview_projects"] => {
            let sel = req.query.get("sel").map(String::as_str).unwrap_or("");
            let ps = registry::known_projects_with_state(roots);
            http::html(w, &render::overview_projects(sel, &ps));
        }
        // Same shape as _overview_projects above, same reason it sits before
        // the general frag arm.
        ["frag", "_overview_sessions"] => {
            let sel = req.query.get("sel").map(String::as_str).unwrap_or("");
            http::html(w, &render::overview_sessions(sel, &build_overview_sessions(roots, sel)));
        }
        // Root scope, not /static/sw.js: a service worker may only control
        // URLs under its own path, and this one has to focus and navigate
        // workspace tabs at /{project}.
        ["sw.js"] => serve_static(w, "sw.js"),
        // The fragment *kind* (tree/file/…) is normally exactly the last
        // segment (every other fragment endpoint takes no path segments of
        // its own — `dir=`/`path=` arrive as query params, see serve_frag
        // below), so splitting from the right rather than assuming
        // `project` is a single segment is unambiguous and leaves every
        // existing single-segment call (`/frag/proj/tree`) unchanged.
        //
        // The theme route is the one exception: `/frag/{project}/theme/{rel}`
        // carries a path of its own after "theme", so it cannot always be
        // found by taking the last segment — that would misparse
        // "style.css" as the kind and "proj/theme" as the project.
        //
        // A first version of this dispatched on the *last* "theme" segment
        // whenever the split-last kind wasn't literally "theme" — but a
        // project is legitimately multi-segment (`resolve_project` accepts
        // nested rels), so a project named e.g. "a/theme" made every one of
        // its ordinary fragments ("tree", "theme.css", …) match the theme
        // rule instead and 404 as "no such asset". The rule has to be keyed
        // on whether the *last* segment is a real fragment kind, not on
        // whether some earlier segment happens to say "theme": if it is,
        // this is an ordinary fragment (even one whose project path
        // contains "theme"); only otherwise does the "last theme segment"
        // rule apply, to reach into `.resh/theme/{rel}`.
        ["frag", rest @ ..] if rest.len() >= 2 => {
            let (what, proj_segs) =
                rest.split_last().expect("len >= 2 guarantees a last element");
            if FRAGMENT_KINDS.contains(what) {
                serve_frag(w, req, roots, &proj_segs.join("/"), std::slice::from_ref(what))
            } else {
                match rest.iter().rposition(|s| *s == "theme") {
                    Some(i) if i >= 1 && i + 1 < rest.len() => {
                        let Some(dir) = projects::resolve_project(roots, &rest[..i].join("/"))
                        else {
                            return http::not_found(w, "no such project");
                        };
                        serve_project_theme(w, &dir, &rest[i + 1..].join("/"))
                    }
                    _ => serve_frag(w, req, roots, &proj_segs.join("/"), std::slice::from_ref(what)),
                }
            }
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

/// The scope resolution + data gathering for the session pane. `sel` empty →
/// every known project; a project key → it and its worktree children; a
/// worktree key → just it. One ages snapshot for the whole set, since
/// `session::ages_snapshot` is already a single `ps` fork for the host — see
/// its doc comment on why per-session `ps` is what the overview must avoid.
fn build_overview_sessions(roots: &[PathBuf], sel: &str) -> Vec<render::OvSession> {
    let all = registry::known_projects(roots);
    let in_scope: Vec<&registry::ProjectStatus> = if sel.is_empty() {
        all.iter().collect()
    } else {
        all.iter().filter(|p| p.key == sel || p.parent.as_deref() == Some(sel)).collect()
    };
    let ages = crate::session::ages_snapshot();
    // Who holds each socket: the dtach master is the oldest holder and is
    // the session's real age; `child_pid` is only this resh's client.
    let holders = registry::holders_snapshot().unwrap_or_default();
    let mut rows = Vec::new();
    for p in in_scope {
        let launched: std::collections::HashSet<String> =
            crate::session::launched_names(&p.url).into_iter().map(|(n, _)| n).collect();
        let evidence = crate::claudes::claude_evidence(&p.url);
        for (name, pid, attached) in crate::session::session_rows(&p.url) {
            let is_claude = launched.contains(&name)
                || matches!(&evidence, crate::claudes::ClaudeEvidence::Present(ts) if ts.iter().any(|t| t == &name));
            let age_secs = {
                let sock = crate::session::socket_path(&p.url, &name);
                let mut pids = registry::pids_holding_path(&holders, &sock);
                if pid != 0 {
                    pids.push(pid);
                }
                crate::session::oldest_age_of(&pids, &ages)
            };
            rows.push(render::OvSession { project_url: p.url.clone(), name, is_claude, age_secs, attached });
        }
    }
    rows
}

/// `/` — with no `at` query, the projects/sessions overview shell (panes
/// fill in over htmx). `?at=<rel>` browses one directory in the picker
/// instead; an `at` that fails to resolve is refused the same way opening it
/// as a workspace would be, showing the merged top level of both ROOTS.
fn serve_index(w: &mut impl Write, req: &http::Request, roots: &[PathBuf]) {
    // `at` absent → the overview; `at` present (empty included) → the picker.
    if req.query.get("at").is_none() {
        let label = roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(":");
        let sel = req.query.get("sel").map(String::as_str).unwrap_or("");
        return http::html(w, &render::overview_page(sel, &label));
    }
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
    let theme_rel = theme_link_for(&dir);
    let key = projects::storage_key(project);
    http::html(
        w,
        &render::workspace_page(
            project,
            &key,
            &settings,
            theme_rel,
            config::share_selection(),
            &launch::offered_names(),
        ),
    );
}

/// Serialises the tests that set the process-global `RESH_STATIC`/`HOME`.
/// cargo runs a binary's tests in parallel threads, so without this two of
/// them interleave and one sees the other's environment mid-body — a
/// flakiness this project has shipped once before (see SESSION_ENV_LOCK).
#[cfg(test)]
pub static ASSET_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// Must classify the same way `assets::class_of` does — see `ext_of`'s doc
// comment for why: the two functions disagreeing about one string is the
// defect, not any individual choice either makes.
pub fn content_type(rel: &str) -> &'static str {
    match crate::assets::ext_of(rel).as_str() {
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

/// Extensions the raw route serves and the `file` fragment renders as a
/// picture. The question it answers: **can these bytes be handed to the
/// browser as a picture?**
///
/// Deny-by-default, for the reason `assets::class_of` gives: an unrecognised
/// extension must fall outside this list, so widening it is an edit here and
/// never a side effect of an unfamiliar file appearing in a cloned repo.
pub const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg", "ico"];

/// Extensions the text editor must refuse. A different question from
/// `IMAGE_EXT`: **would a `<textarea>` over this file destroy it?**
///
/// The two lists differ by exactly one entry, and the difference is the
/// point. SVG renders as a picture (so it is on `IMAGE_EXT`) *and* is text
/// (so it is not here): `projects::read_text_file` sniffs for NUL bytes,
/// finds none in an SVG, and has always served it — editing one worked before
/// image tabs existed. Gating Edit on `is_image` silently removed that.
///
/// Keep them separate. Merging them back either strands SVG in a read-only
/// tab again, or lets a PNG into a textarea whose first save truncates it.
///
/// `static/app.js` keeps a copy of THIS list, for the ✎ toggle. Nothing
/// checks the two agree — the design doc records why neither direction of
/// mismatch loses data, because `workspace.rs` is the actual guard.
pub const NO_TEXT_EDIT_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "ico", "pdf"];

pub fn is_image(rel: &str) -> bool {
    IMAGE_EXT.contains(&crate::assets::ext_of(rel).as_str())
}

pub fn refuses_text_edit(rel: &str) -> bool {
    NO_TEXT_EDIT_EXT.contains(&crate::assets::ext_of(rel).as_str())
}

/// Image bytes out of a project, for `<img>` in a markdown preview and for an
/// image tab.
///
/// `metadata` before `read` is not a style preference: a bare `fs::read`
/// allocates the whole file — a size a cloned repo controls — on this
/// connection's thread before any cap could reject it.
fn serve_raw(w: &mut impl Write, dir: &Path, rel: &str) {
    let Some(rel) = crate::assets::normalize(rel) else {
        return http::not_found(w, "no such asset");
    };
    if !is_image(rel) {
        return http::not_found(w, "not an image");
    }
    let Ok(path) = projects::safe_resolve(dir, rel) else {
        return http::not_found(w, "no such asset");
    };
    match std::fs::metadata(&path) {
        Ok(meta) if meta.len() > projects::MAX_FILE_BYTES => http::not_found(w, "asset too large"),
        Ok(_) => match std::fs::read(&path) {
            Ok(body) => {
                http::respond_with(w, 200, "OK", content_type(rel), &[NOSNIFF, SANDBOX], &body)
            }
            Err(_) => http::not_found(w, "no such asset"),
        },
        Err(_) => http::not_found(w, "no such asset"),
    }
}

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

/// Every fragment kind `serve_frag` matches on — the closed set `route()`
/// consults to tell an ordinary fragment request from a `.resh/theme/{rel}`
/// request when the project path itself contains a "theme" segment.
///
/// This list must be kept in sync with `serve_frag`'s match arms by hand:
/// adding a new fragment kind there and forgetting to add it here makes
/// `route()` treat it as a theme-asset path instead (or vice versa if one
/// is removed from serve_frag but left here). There is no compiler check
/// for that — it's the one hazard this dispatch-by-kind approach carries
/// that a fully generic parse wouldn't.
const FRAGMENT_KINDS: &[&str] =
    &["tree", "file", "raw", "changes", "status", "diff", "proposal", "theme.css"];

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
    // The header toggle beats the config file; a project with no hub has no
    // toggle, and asking must not create one (see `Hub::show_hidden`).
    let filter = settings.tree_filter_with(crate::hub::Hub::show_hidden(project));
    match what {
        ["tree"] => {
            let open = req.query.get("open").map(String::as_str).unwrap_or("");
            match req.query.get("dir") {
                None => {
                    http::html(w, &render::tree_fragment(project, &dir, open, &filter))
                }
                // `dir` names a subtree the client wants to lazily expand —
                // it arrives from the network, so it must be confined
                // through `safe_resolve` before any read, exactly like
                // `file`'s `path`. A `dir` that resolves outside the
                // project (or doesn't exist, or isn't a directory) renders
                // the standard hint, never a listing.
                Some(rel) => match projects::safe_resolve(&dir, rel) {
                    Ok(sub) if sub.is_dir() => {
                        let mut out = String::new();
                        render::tree_level(
                            project,
                            &sub,
                            rel,
                            open,
                            &filter,
                            &mut out,
                        );
                        http::html(w, &out);
                    }
                    Ok(_) => http::html(w, &render::hint("not a directory")),
                    Err(e) => http::html(w, &render::hint(&e)),
                },
            }
        }
        ["file"] => match req.query.get("path") {
            None => http::html(w, &render::hint("missing path")),
            // Branch BEFORE read_text_file: it sniffs for NUL bytes and returns
            // "binary file" for every image, which is what made a .png in the tree
            // unopenable. safe_resolve still runs, so a path leaving the project
            // gets the standard hint rather than an <img> that would 404.
            // `normalize` first, exactly as the raw route does. The fragment
            // this returns is nothing but an <img> pointed at that route, and
            // it rejects `..` outright — so without this, `docs/../shot.png`
            // renders a fragment whose picture then 404s, which reads to the
            // user as a corrupt file rather than a bad path.
            Some(rel) if is_image(rel) => match crate::assets::normalize(rel) {
                None => http::html(w, &render::hint("path outside project")),
                Some(rel) => match projects::safe_resolve(&dir, rel) {
                    Ok(path) => {
                        let mtime_secs = std::fs::metadata(&path)
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs())
                            // Not a failure worth refusing the page for: an unreadable mtime only
                            // costs the cache key, and 0 is a legitimate "I could not tell".
                            .unwrap_or(0);
                        http::html(w, &render::image_fragment(project, rel, mtime_secs))
                    },
                    Err(e) => http::html(w, &render::hint(&e)),
                },
            },
            Some(rel) => match projects::safe_resolve(&dir, rel)
                .and_then(|p| projects::read_text_file(&p))
            {
                Ok(content) => http::html(w, &render::file_fragment(project, rel, &content)),
                Err(e) => http::html(w, &render::hint(&e)),
            },
        },
        ["raw"] => match req.query.get("path") {
            None => http::not_found(w, "missing path"),
            Some(rel) => serve_raw(w, &dir, rel),
        },
        ["changes"] => match gitio::status(&dir) {
            Ok(st) => http::html(w, &render::changes_fragment(project, &st)),
            Err(e) => http::html(w, &render::hint(&e)),
        },
        ["status"] => {
            let st = gitio::status(&dir)
                .unwrap_or(gitio::Status { branch: String::new(), changes: vec![], ..Default::default() });
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
        // An `openDiff` proposal Claude is still blocked on. `id` is
        // resh's own opaque key (`ide::new_pending_id`), not a path — there
        // is nothing here to confine, only a hub lookup that can miss (the
        // proposal was already answered or withdrawn by the time this
        // browser's fetch lands, or the id was never valid to begin with).
        // Both misses render the same fragment; there is no reason to tell
        // a browser which one happened.
        ["proposal"] => match req.query.get("id") {
            None => http::html(w, &render::hint("missing id")),
            Some(id) => match crate::hub::proposal_by_id(project, id) {
                Some(p) => http::html(w, &render::proposal_fragment(&p.rel, &p.old_text, &p.new_text)),
                None => http::html(w, &render::hint("this proposal is no longer open")),
            },
        },
        // Resolved through `safe_resolve`, not a bare `fs::read`, so a
        // `.resh/theme.css` that is a symlink pointing outside the
        // project (planted by a cloned repo) is refused rather than served
        // to the browser as text/css. Every other file read in this module
        // already goes through this confinement; this one predates it.
        //
        // Same untrusted-content policy as the theme directory route
        // (NOSNIFF + SANDBOX): this is project-controlled CSS too, and an
        // attacker doesn't care which of the two theme routes they're
        // abusing.
        ["theme.css"] => match projects::safe_resolve(&dir, ".resh/theme.css")
            .and_then(|p| std::fs::read(&p).map_err(|e| e.to_string()))
        {
            Ok(css) => http::respond_with(
                w,
                200,
                "OK",
                "text/css; charset=utf-8",
                &[NOSNIFF, SANDBOX],
                &css,
            ),
            Err(_) => http::not_found(w, "no theme.css"),
        },
        _ => http::not_found(w, "no such fragment"),
    }
}

/// A project's own theme directory, `{project}/.resh/theme/`.
///
/// Not part of the `/static` overlay: `/static` carries no project context,
/// and threading one through it would be the larger change. This route
/// already resolves a project and already refuses symlinks escaping it.
///
/// A code-class path 404s rather than falling through to the embedded copy —
/// serving embedded bytes under a project URL would imply the project
/// supplied bytes it did not.
fn serve_project_theme(w: &mut impl Write, dir: &Path, rel: &str) {
    let Some(rel) = crate::assets::normalize(rel) else {
        return http::not_found(w, "no such asset");
    };
    if crate::assets::class_of(rel) != crate::assets::Class::Theme {
        return http::not_found(w, "no such asset");
    }
    let Ok(path) = projects::safe_resolve(dir, &format!(".resh/theme/{rel}")) else {
        return http::not_found(w, "no such asset");
    };
    // Stat before reading: a project directory is untrusted, and a bare
    // `fs::read` would allocate the whole file — attacker-controlled, up to
    // however big a cloned repo can make it — on this connection's thread
    // before the cap could reject it. CLAUDE.md's 2 MB cap applies to every
    // project-controlled read, not just `read_text_file`'s.
    match std::fs::metadata(&path) {
        Ok(meta) if meta.len() > projects::MAX_FILE_BYTES => http::not_found(w, "asset too large"),
        Ok(_) => match std::fs::read(&path) {
            Ok(body) => {
                http::respond_with(w, 200, "OK", content_type(rel), &[NOSNIFF, SANDBOX], &body)
            }
            Err(_) => http::not_found(w, "no such asset"),
        },
        Err(_) => http::not_found(w, "no such asset"),
    }
}

/// Which single theme stylesheet this project's page should link, as a
/// fragment-relative path.
///
/// The directory wins over the legacy single file, and only ever one is
/// returned: emitting both would style the project twice, with the winner
/// decided by link order rather than by intent.
///
/// `is_file()` folds "absent" and "cannot look" together, which this project
/// treats as a defect where the answer gates destruction. Here it gates only
/// whether a stylesheet is linked, so the worst case is an unstyled page —
/// recoverable, and not worth a `symlink_metadata` dance.
fn theme_link_for(dir: &Path) -> Option<&'static str> {
    if dir.join(".resh/theme/style.css").is_file() {
        Some("theme/style.css")
    } else if dir.join(".resh/theme.css").is_file() {
        Some("theme.css")
    } else {
        None
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
    ///
    /// The first four probes are Code class, so a bug that deleted the
    /// `normalize` call outright would still pass them — `assets::get`
    /// normalizes internally, and layer 2 is skipped for Code regardless.
    /// `./style.css` and `themes/../style.css` close that hole: both are
    /// Theme class, and both overlay directories hold a `style.css` with a
    /// distinct marker, so if `normalize` ran *after* a layer instead of
    /// before it, that layer's `canonicalize()` would lexically collapse the
    /// `.`/`..` and hand back the marker instead of 404 — turning the
    /// byte-equality assertion below into one that can actually fail.
    #[test]
    fn traversal_is_refused_the_same_with_and_without_an_overlay() {
        let _g = ASSET_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let userdir = home.path().join(".config/resh/static");
        // The empty `themes` dir is load-bearing, not decoration:
        // `canonicalize()` is realpath, which must walk into `themes` to
        // resolve back out of it — on a directory that doesn't exist on
        // disk, `themes/../style.css` fails to canonicalize regardless of
        // ordering, which would silently defeat this probe.
        std::fs::create_dir_all(userdir.join("themes")).unwrap();
        std::fs::write(userdir.join("style.css"), "/*LAYER2*/").unwrap();
        let _home = HomeGuard::set(home.path());
        let probes = [
            "../Cargo.toml",
            "/etc/passwd",
            "themes/../../Cargo.toml",
            "a\\..\\b",
            "./style.css",
            "themes/../style.css",
        ];

        std::env::remove_var("RESH_STATIC");
        let without: Vec<String> = probes.iter().map(|p| serve(p)).collect();

        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("themes")).unwrap();
        std::fs::write(d.path().join("style.css"), "/*LAYER1*/").unwrap();
        std::env::set_var("RESH_STATIC", d.path());
        let with: Vec<String> = probes.iter().map(|p| serve(p)).collect();
        std::env::remove_var("RESH_STATIC");

        for (i, p) in probes.iter().enumerate() {
            assert!(without[i].starts_with("HTTP/1.1 404"), "{p} must 404");
            assert_eq!(without[i], with[i], "{p}: responses must be byte-identical");
        }
    }

    fn serve_theme(dir: &Path, rel: &str) -> String {
        let mut buf: Vec<u8> = Vec::new();
        serve_project_theme(&mut buf, dir, rel);
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[test]
    fn a_project_theme_serves_presentation_and_refuses_code() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join(".resh/theme");
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(t.join("style.css"), "body{color:red}").unwrap();
        std::fs::write(t.join("logo.png"), [0x89, 0x50, 0x4e, 0x47]).unwrap();
        std::fs::write(t.join("app.js"), "alert('pwned')").unwrap();

        let css = serve_theme(d.path(), "style.css");
        assert!(css.contains("body{color:red}"));
        assert!(css.contains("Content-Security-Policy: sandbox"), "project assets are untrusted");
        assert!(css.contains("Content-Type: text/css"));

        assert!(serve_theme(d.path(), "logo.png").contains("Content-Type: image/png"));

        let js = serve_theme(d.path(), "app.js");
        assert!(js.starts_with("HTTP/1.1 404"), "a project may never serve code");
        assert!(!js.contains("pwned"));

        assert!(serve_theme(d.path(), "../../Cargo.toml").starts_with("HTTP/1.1 404"));
    }

    /// `../../Cargo.toml` above is caught by `assets::normalize` before
    /// `safe_resolve` is ever reached, so on its own it cannot tell a real
    /// confinement check from a missing one (swap `safe_resolve` for a bare
    /// `fs::read` and every assertion up there still passes). A symlink
    /// *inside* the theme directory pointing outside the project is the
    /// probe that only a real canonicalize-and-`starts_with` check catches —
    /// `normalize` has no opinion on it, since the path it sees never
    /// contains `..` at all.
    #[cfg(unix)]
    #[test]
    fn a_theme_symlink_escaping_the_project_is_refused() {
        let secret_dir = tempfile::tempdir().unwrap();
        std::fs::write(secret_dir.path().join("secret.css"), "body{SECRET}").unwrap();

        let d = tempfile::tempdir().unwrap();
        let t = d.path().join(".resh/theme");
        std::fs::create_dir_all(&t).unwrap();
        std::os::unix::fs::symlink(secret_dir.path().join("secret.css"), t.join("leak.css"))
            .unwrap();

        let out = serve_theme(d.path(), "leak.css");
        assert!(out.starts_with("HTTP/1.1 404"), "a symlink escaping the project must be refused");
        assert!(!out.contains("SECRET"), "the outside file's content must never reach the response");
    }

    /// A project directory is untrusted, and CLAUDE.md's 2 MB cap applies to
    /// every project-controlled read — not just `projects::read_text_file`'s.
    /// Without the cap this route would allocate an attacker-sized file on a
    /// per-connection thread before ever inspecting it.
    #[test]
    fn an_oversize_theme_asset_is_refused_before_being_read_fully() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join(".resh/theme");
        std::fs::create_dir_all(&t).unwrap();
        let oversize = vec![b'a'; (projects::MAX_FILE_BYTES + 1) as usize];
        std::fs::write(t.join("big.css"), &oversize).unwrap();

        let out = serve_theme(d.path(), "big.css");
        assert!(out.starts_with("HTTP/1.1 404"), "an oversize project asset must be refused");
    }

    /// The selection itself, which decides which single link the page emits.
    /// Without this the precedence lives only in the route and nothing would
    /// catch it regressing to "both" or to the wrong one.
    #[test]
    fn a_theme_directory_wins_over_the_single_stylesheet() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".resh/theme")).unwrap();
        assert_eq!(theme_link_for(d.path()), None, "neither present");

        std::fs::write(d.path().join(".resh/theme.css"), "a{}").unwrap();
        assert_eq!(theme_link_for(d.path()), Some("theme.css"), "the legacy file still works");

        std::fs::write(d.path().join(".resh/theme/style.css"), "b{}").unwrap();
        assert_eq!(
            theme_link_for(d.path()),
            Some("theme/style.css"),
            "the directory wins where both exist — and only one link is ever emitted"
        );
    }

    /// Sends a raw request through `http::parse` and the real `route()`
    /// dispatch — not a hand-built `Request` and not a direct call to
    /// `serve_frag`/`serve_project_theme` — so this exercises the exact
    /// code path `handle()` uses. The first round of this feature had
    /// tests that called `serve_project_theme` directly and stayed green
    /// while the router itself could never reach it; this is the fix for
    /// that class of gap, short of opening a real socket.
    fn frag_route(roots: &[PathBuf], path: &str) -> String {
        let raw = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        let req = http::parse(&mut std::io::Cursor::new(raw.as_bytes())).unwrap();
        let mut buf: Vec<u8> = Vec::new();
        route(&mut buf, &req, roots);
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// The tree lists every file, so a .png can be clicked. Before this, the file
    /// fragment read through read_text_file and answered "binary file" — the tree
    /// offered a file it then refused to open.
    #[test]
    fn clicking_an_image_shows_a_picture_not_a_binary_error() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("p");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("shot.png"), b"\x89PNG\x00\x01\x02").unwrap();
        let roots = vec![d.path().to_path_buf()];

        let out = frag_route(&roots, "/frag/p/file?path=shot.png");
        assert!(out.contains(r#"src="/frag/p/raw?path=shot.png&v="#), "{out}");
        assert!(!out.contains("binary file"), "{out}");

        // The fragment is nothing but an <img> pointed at the raw route, and
        // that route runs `assets::normalize`, which rejects `..` rather than
        // collapsing it. A rel this branch accepts but the raw route will not
        // must therefore be refused HERE, or the user gets a fragment whose
        // picture 404s — indistinguishable from a corrupt file.
        //
        // The target really exists (`p/shot.png`, written above) and
        // `safe_resolve` resolves `docs/../shot.png` inside the project
        // happily, so this cannot pass on ENOENT or on confinement.
        std::fs::create_dir_all(proj.join("docs")).unwrap();
        let dotted = frag_route(&roots, "/frag/p/file?path=docs/../shot.png");
        assert!(!dotted.contains("<img"), "an un-normalized rel must not yield an <img>: {dotted}");
        assert!(dotted.contains("path outside project"), "{dotted}");
    }

    /// The escape target must EXIST. A test pointing `path` at a file that is not
    /// there passes on ENOENT without ever reaching the confinement check — the
    /// exact hole that let a symlink escape survive review once already.
    #[test]
    fn raw_serves_an_image_and_refuses_everything_else() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("p");
        std::fs::create_dir_all(proj.join("docs")).unwrap();
        // A one-pixel PNG: real bytes, so a content-type assertion means something.
        let png: &[u8] = &[
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R',
        ];
        std::fs::write(proj.join("docs/cat.png"), png).unwrap();
        std::fs::write(proj.join("secret.rs"), "fn main() {}").unwrap();
        let roots = vec![d.path().to_path_buf()];

        let ok = frag_route(&roots, "/frag/p/raw?path=docs/cat.png");
        assert!(ok.starts_with("HTTP/1.1 200 OK"), "{ok}");
        assert!(ok.contains("Content-Type: image/png"), "{ok}");
        assert!(ok.contains("X-Content-Type-Options: nosniff"), "{ok}");
        assert!(ok.contains("Content-Security-Policy: sandbox"), "{ok}");

        // Deny-by-default: the file exists and is readable, and is still refused.
        // Confirmed by reverting the `is_image` guard: the assertion below
        // failed with the response "HTTP/1.1 200 OK ... fn main() {}" — the
        // secret file's own text was served back verbatim.
        let code = frag_route(&roots, "/frag/p/raw?path=secret.rs");
        assert!(code.starts_with("HTTP/1.1 404"), "{code}");
        assert!(code.contains("not an image"), "must refuse on class, not absence: {code}");

        let up = frag_route(&roots, "/frag/p/raw?path=../p/docs/cat.png");
        assert!(up.starts_with("HTTP/1.1 404"), "{up}");
    }

    #[test]
    fn raw_refuses_an_oversize_image_and_a_symlink_out() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("p");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("big.png"), vec![0u8; (crate::projects::MAX_FILE_BYTES + 1) as usize])
            .unwrap();
        // Outside the project, and REAL — see the comment on the test above.
        std::fs::write(d.path().join("outside.png"), b"not yours").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(d.path().join("outside.png"), proj.join("escape.png")).unwrap();
        let roots = vec![d.path().to_path_buf()];

        let big = frag_route(&roots, "/frag/p/raw?path=big.png");
        assert!(big.starts_with("HTTP/1.1 404"), "{big}");
        assert!(big.contains("asset too large"), "must refuse on size, not absence: {big}");

        // Confirmed by reverting `safe_resolve` to a bare `dir.join(rel)`:
        // the assertion below failed with the response "HTTP/1.1 200 OK ...
        // not yours" — the symlink target's content, from outside the
        // project, served straight through.
        #[cfg(unix)]
        {
            let esc = frag_route(&roots, "/frag/p/raw?path=escape.png");
            assert!(esc.starts_with("HTTP/1.1 404"), "a symlink leaving the project: {esc}");
            assert!(!esc.contains("not yours"), "and its bytes must not appear: {esc}");
        }
    }

    /// A project is legitimately multi-segment (`resolve_project` accepts
    /// nested rels), so a project whose own path contains a "theme" segment
    /// must not have every one of its ordinary fragments hijacked by the
    /// theme-asset rule. This is the regression an earlier version of the
    /// dispatch introduced: it keyed off "does any segment say theme"
    /// instead of "is the last segment a real fragment kind", so
    /// `/frag/a/theme/tree` resolved project "a" (wrong — should be
    /// "a/theme") and 404'd every pane of that project's workspace.
    #[test]
    fn frag_route_dispatches_theme_paths_by_last_segment_not_by_containing_theme() {
        let root = tempfile::tempdir().unwrap();

        // Project "a": an ordinary project with its own theme directory.
        let a = root.path().join("a");
        std::fs::create_dir_all(a.join(".resh/theme")).unwrap();
        std::fs::write(a.join(".resh/theme/style.css"), "css-a-style").unwrap();

        // Project "a/theme": a nested project whose own name contains
        // "theme" — the case the regression broke.
        let a_theme = root.path().join("a/theme");
        std::fs::create_dir_all(a_theme.join(".resh/theme")).unwrap();
        std::fs::write(a_theme.join("inner.rs"), "fn main() {}").unwrap();
        std::fs::write(a_theme.join(".resh/theme.css"), "css-a-theme-legacy").unwrap();
        std::fs::write(a_theme.join(".resh/theme/style.css"), "css-a-theme-style").unwrap();

        // Project "theme": a top-level project literally named "theme".
        let theme_proj = root.path().join("theme");
        std::fs::create_dir_all(&theme_proj).unwrap();
        std::fs::write(theme_proj.join("hello.txt"), "hello").unwrap();

        // Project "proj": no theme assets and no "theme"-named segment at
        // all, so `/frag/proj/theme` has nothing to fall back to.
        let proj = root.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("hello.txt"), "hello").unwrap();

        // karpie/sub: the plain nested-project regression case (also
        // covered over real HTTP by
        // frag_route_resolves_a_nested_projects_fragment_kind in
        // tests/integration.rs) — asserted here too since it exercises
        // the very same match arm this test is about.
        let sub = root.path().join("karpie/sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("inner.rs"), "fn main() {}").unwrap();

        let roots = vec![root.path().to_path_buf()];

        // Last segment "tree" IS a fragment kind: project is "a/theme"
        // wholesale, never truncated at its "theme" segment.
        let out = frag_route(&roots, "/frag/a/theme/tree");
        assert!(out.contains("inner.rs"), "must list project a/theme's own tree, not 404: {out}");

        // Last segment "theme.css" IS a fragment kind: project "a/theme",
        // its own legacy stylesheet fragment.
        let out = frag_route(&roots, "/frag/a/theme/theme.css");
        assert!(out.contains("css-a-theme-legacy"), "must resolve project a/theme, kind theme.css");

        // Last segment "style.css" is NOT a fragment kind: falls to the
        // theme-asset rule, reading project "a"'s .resh/theme/style.css —
        // not project "a/theme"'s.
        let out = frag_route(&roots, "/frag/a/theme/style.css");
        assert!(out.contains("css-a-style"), "must resolve project a, theme asset style.css: {out}");
        assert!(!out.contains("css-a-theme-style"), "must not read project a/theme's asset instead");

        // Last segment "style.css" is again not a kind; the *last* "theme"
        // segment is the second one, giving project "a/theme" and asset
        // "style.css" inside it.
        let out = frag_route(&roots, "/frag/a/theme/theme/style.css");
        assert!(out.contains("css-a-theme-style"), "must resolve project a/theme, theme asset style.css: {out}");

        // Last segment "tree" IS a kind, so the whole first segment is the
        // project name — a project literally named "theme" still works.
        let out = frag_route(&roots, "/frag/theme/tree");
        assert!(out.contains("hello.txt"), "must resolve project theme, kind tree: {out}");

        // Last segment "theme" is not a fragment kind, and there is no
        // earlier "theme" segment for the theme-asset rule to use (the
        // rule requires at least one project segment before it) — falls
        // back to serve_frag's own catch-all, a 404, not a theme-asset
        // lookup with an empty project name.
        let out = frag_route(&roots, "/frag/proj/theme");
        assert!(out.starts_with("HTTP/1.1 404"), "no fallback project for a bare theme kind: {out}");
        assert!(out.contains("no such fragment"));

        // Plain nested-project regression: an ordinary kind under a
        // multi-segment project with no "theme" segment anywhere.
        let out = frag_route(&roots, "/frag/karpie/sub/tree");
        assert!(out.contains("inner.rs"), "must still resolve an ordinary nested project: {out}");
    }

    /// Dispatch through the real router, like every frag test here (the
    /// helper exists because direct-call tests once stayed green while the
    /// router could never reach the handler). Before the arm exists this
    /// request falls through to the catch-all and is treated as a project
    /// named "frag/_worktrees" — so the assertions below cannot pass early.
    #[test]
    fn the_worktrees_fragment_is_routed() {
        let d = tempfile::tempdir().unwrap();
        let roots = vec![d.path().to_path_buf()];
        let out = frag_route(&roots, "/frag/_worktrees?current=nosuch");
        assert!(out.contains("id=\"wtlabel\""), "{out}");
        assert!(out.contains("no worktrees"), "{out}");
    }

    /// Dispatch through the real router, like `the_worktrees_fragment_is_routed`
    /// above (whose own comment explains why: a direct call to
    /// `render::overview_projects` would stay green even if the `["frag",
    /// "_overview_projects"]` arm were deleted or the router could never
    /// reach it, since that arm must sit ahead of the general two-segment
    /// `["frag", rest @ ..]` fragment arm — see that arm's own comment on
    /// why `_projects`/`_worktrees` need the same placement). Asserting on
    /// `ovtree`, the class the fragment always emits regardless of how many
    /// (if any) real projects the host's state directory holds, keeps this
    /// independent of what's actually on disk.
    ///
    /// Revert-checked: with the `["frag", "_overview_projects"]` arm
    /// removed, this falls through the catch-all and 404s ("no such
    /// project") instead of ever reaching `render::overview_projects` —
    /// panic showed the literal 404 response body, no `ovtree` anywhere.
    /// Restored.
    #[test]
    fn the_overview_projects_fragment_is_routed() {
        let d = tempfile::tempdir().unwrap();
        let roots = vec![d.path().to_path_buf()];
        let out = frag_route(&roots, "/frag/_overview_projects");
        assert!(out.contains("ovtree"), "{out}");
    }

    /// Same reasoning as `the_overview_projects_fragment_is_routed` above,
    /// for the `["frag", "_overview_sessions"]` arm added in Task 4. Before
    /// that arm exists, this path falls through to the catch-all and is
    /// treated as a project literally named "frag/_overview_sessions" — a
    /// 404, not this fragment's markup — so the assertion below cannot pass
    /// early. `SESSIONS` (the pane heading) and `ovsessions` (the list
    /// class) are both emitted unconditionally, independent of whether the
    /// host's state directory holds any real sessions.
    ///
    /// Revert-checked: with the `["frag", "_overview_sessions"]` arm
    /// removed, this falls through the catch-all and 404s ("no such
    /// project") instead of ever reaching `render::overview_sessions` —
    /// panic showed the literal 404 response body, no `SESSIONS`/`ovsessions`
    /// anywhere. Restored.
    #[test]
    fn the_overview_sessions_fragment_is_routed() {
        let d = tempfile::tempdir().unwrap();
        let roots = vec![d.path().to_path_buf()];
        let out = frag_route(&roots, "/frag/_overview_sessions");
        assert!(out.contains("SESSIONS") && out.contains("ovsessions"), "{out}");
    }

    #[test]
    fn root_with_no_at_serves_the_overview_and_at_serves_the_picker() {
        // Revert-checked: with serve_index always calling index_page, the
        // first assertion fails (no overview markup at `/`) — the response
        // body is the picker's `<ul class="picker" id="picker" ...>`, no
        // `id="overview"` anywhere. See
        // .superpowers/sdd/2026-08-26-overview-page/task-1-report.md for the
        // captured failure output.
        let d = tempfile::tempdir().unwrap();
        let roots = vec![d.path().to_path_buf()];
        let overview = frag_route(&roots, "/");
        assert!(overview.contains("id=\"overview\""), "no-at is the overview: {overview}");
        assert!(overview.contains("_overview_projects") && overview.contains("_overview_sessions"), "{overview}");
        let picker = frag_route(&roots, "/?at=");
        assert!(picker.contains("id=\"picker\""), "?at= is the picker: {picker}");
    }

    // `sel` in the overview's own query string must reach the fragment
    // hx-get URLs the shell emits, or htmx re-fetches unfiltered on load and
    // the round-trip from static/overview.js's `/?sel=<key>` navigation is
    // lost.
    //
    // Revert-checked: with `serve_index` not reading `sel` (calling
    // `render::overview_page("", &label)` unconditionally), this fails —
    // the response carries `_overview_projects?sel=` (empty), not
    // `?sel=proj`, so `.contains("_overview_projects?sel=proj")` is false.
    #[test]
    fn overview_at_root_threads_sel_to_fragments() {
        let d = tempfile::tempdir().unwrap();
        let roots = vec![d.path().to_path_buf()];
        let out = frag_route(&roots, "/?sel=proj");
        assert!(out.contains("_overview_projects?sel=proj"), "{out}");
    }

    // Revert-checked: dropping the `state=1` branch entirely (always calling
    // plain `known_projects`) does NOT fail this assertion — with an empty
    // tempdir root, both branches yield the same empty-family output ("no
    // worktrees"), so this test alone only proves the route still answers,
    // not that `state=1` changes what is computed. The two-git-calls-only-
    // when-asked property is covered by `known_projects_with_state` costing
    // real subprocess calls (exercised directly by nothing here, but the
    // registry-level function exists as a distinct, separately reachable
    // path, and the render-level state rendering is covered by
    // `a_worktree_row_shows_its_state_and_offers_removal_only_when_clean`).
    #[test]
    fn the_worktrees_fragment_computes_state_only_when_asked() {
        let d = tempfile::tempdir().unwrap();
        let roots = vec![d.path().to_path_buf()];
        assert!(frag_route(&roots, "/frag/_worktrees?current=nosuch&state=1").contains("id=\"wtlabel\""));
    }

    /// `content_type` must classify the same way `assets::class_of` does —
    /// case-insensitively, and treating a leading-dot name as having no
    /// extension. Before the shared `ext_of` helper, `content_type` used
    /// `Path::extension()` (case-preserving, `None` on ".css"), so an
    /// uppercase or dotfile-shaped Theme path would 200 with
    /// `application/octet-stream` and then get blocked outright by the
    /// nosniff header this branch adds to every response — "my theme
    /// silently does nothing".
    #[test]
    fn content_type_agrees_with_class_of_on_the_same_string() {
        assert_eq!(content_type("logo.PNG"), "image/png", "extension match must be case-insensitive");
        assert_eq!(content_type(".css"), "text/css; charset=utf-8", "a leading dot is still an extension here");

        for rel in ["logo.PNG", ".css", "Inter.WOFF2", "theme/Solarized.CSS"] {
            let is_theme_type = content_type(rel) != "application/octet-stream";
            let is_theme_class = crate::assets::class_of(rel) == crate::assets::Class::Theme;
            assert_eq!(
                is_theme_type, is_theme_class,
                "content_type and class_of disagree about {rel:?}"
            );
        }
    }
}
