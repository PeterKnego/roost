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
//!                        into the project's `.roost/theme/` directory
//! Fragment errors render as 200 + hint (htmx ignores 4xx bodies).
use crate::{config, gitio, http, launch, projects, registry, render};
use std::io::{BufReader, Read, Write};
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
            "roost: rejected host={:?} x-forwarded-host={:?} (set allowed_origins)",
            req.headers.get("host"),
            req.headers.get("x-forwarded-host")
        );
        return http::respond(w, 403, "Forbidden", "text/plain; charset=utf-8", b"host not allowed");
    }
    let segs: Vec<&str> = req.path.split('/').filter(|s| !s.is_empty()).collect();
    match segs.as_slice() {
        [] => serve_index(w, req, roots),
        ["static", rest @ ..] => serve_static(w, &rest.join("/"), req.query.get("v").map(String::as_str)),
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
            // Every directory under the roots, not only the opened ones —
            // this is the front page, and the picker it replaced reached
            // all of them. `open` names the projects the user has expanded;
            // their worktrees are the only git work this endpoint does, so
            // a page nobody has expanded costs no subprocess at all.
            let mut open: Vec<&str> = req
                .query
                .get("open")
                .map(|v| v.split(',').filter(|k| !k.is_empty()).collect())
                .unwrap_or_default();
            // A selected project is open by definition: that is what makes a
            // shared `?sel=` URL come back as the view it described, without
            // the client having to fetch the pane a second time to expand it.
            if !sel.is_empty() && !open.contains(&sel) {
                open.push(sel);
            }
            http::html(w, &render::overview_projects(sel, &build_overview_projects(roots, &open), roots.is_empty()));
        }
        // Same shape as _overview_projects above, same reason it sits before
        // the general frag arm.
        // One project's worktrees, for the client to splice under the row
        // it opened — so selecting a project never re-fetches the list the
        // selection was made from.
        ["frag", "_overview_worktrees"] => {
            let key = req.query.get("project").map(String::as_str).unwrap_or("");
            let sel = req.query.get("sel").map(String::as_str).unwrap_or("");
            let rows = if key.is_empty() { Vec::new() } else { registry::worktree_rows(roots, key) };
            http::html(w, &render::overview_worktree_rows(sel, &rows));
        }
        ["frag", "_overview_sessions"] => {
            let sel = req.query.get("sel").map(String::as_str).unwrap_or("");
            http::html(w, &render::overview_sessions(sel, &build_overview_sessions(roots, sel)));
        }
        // #116. A JSON rendering of data the HTML fragments already serve, for
        // a native client that must not parse htmx output.
        //
        // Not a new exposure: the same rows already leave the machine through
        // `/frag/_overview_*`, behind the same DNS-rebinding host check
        // `route()` applies above and the same tunnel and Access in front of
        // it. And these are reads, so they are GETs — nothing here touches
        // CLAUDE.md's "keep the surface at two" POST cap.
        //
        // Unversioned on purpose. There is exactly one client and it ships
        // with roost; a `/v1/` that never sees a `/v2/` is a promise nobody
        // asked for. Said here rather than left to be inferred from its
        // absence.
        ["api", "projects"] => {
            // `project_rows`, not `known_projects`: the latter lists only
            // projects that have been *opened*, which is right for the header
            // strip and wrong here — a phone listing projects should see the
            // same set the front page does, which is every project under the
            // roots. Caught by the test, which created a project and then
            // could not find it.
            //
            // Worktrees are not expanded. `build_overview_projects` takes an
            // `open` list because the front page pays the git cost only for
            // rows the user expanded; a client that wants a project's
            // worktrees can ask for them, and one that does not should not pay
            // a `git worktree list` per project to get a list of names.
            let rows = registry::project_rows(roots);
            let json: Vec<ApiProject> = rows.iter().map(ApiProject::from).collect();
            http::json(w, &json)
        }
        ["api", "sessions"] => {
            let sel = req.query.get("project").map(String::as_str).unwrap_or("");
            let rows = build_overview_sessions(roots, sel);
            let json: Vec<ApiSession> = rows.iter().map(ApiSession::from).collect();
            http::json(w, &json)
        }
        // Root scope, not /static/sw.js: a service worker may only control
        // URLs under its own path, and this one has to focus and navigate
        // workspace tabs at /{project}.
        ["sw.js"] => serve_static(w, "sw.js", None),
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
        // rule apply, to reach into `.roost/theme/{rel}`.
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
/// The overview's left pane: the cheap top-level list, with each expanded
/// project's worktrees spliced in right after it.
///
/// A worktree that also sits directly under a root (a sibling checkout
/// rather than one under `.claude/worktrees/`) would otherwise appear
/// twice — once as a directory of its own, once as its project's child —
/// so the top-level copy is dropped once git has told us whose child it is.
fn build_overview_projects(roots: &[PathBuf], open: &[&str]) -> Vec<registry::ProjectStatus> {
    let top = registry::project_rows(roots);
    let mut kids: std::collections::HashMap<String, Vec<registry::ProjectStatus>> =
        Default::default();
    for key in open {
        // Only a project actually on the list may be expanded: `open` comes
        // off the query string.
        if top.iter().any(|p| p.key == *key) {
            kids.insert((*key).to_string(), registry::worktree_rows(roots, key));
        }
    }
    let child_keys: std::collections::HashSet<String> =
        kids.values().flatten().map(|c| c.key.clone()).collect();
    let mut out = Vec::new();
    for p in top {
        if child_keys.contains(&p.key) {
            continue;
        }
        let children = kids.remove(&p.key);
        out.push(p);
        if let Some(cs) = children {
            out.extend(cs);
        }
    }
    out
}

fn build_overview_sessions(roots: &[PathBuf], sel: &str) -> Vec<render::OvSession> {
    let all = registry::known_projects(roots);
    let in_scope: Vec<&registry::ProjectStatus> = if sel.is_empty() {
        all.iter().collect()
    } else {
        all.iter().filter(|p| p.key == sel || p.parent.as_deref() == Some(sel)).collect()
    };
    let ages = crate::session::ages_snapshot();
    // Who holds each socket: the dtach master is the oldest holder and is
    // the session's real age; `child_pid` is only this roost's client.
    let holders = registry::holders_snapshot().unwrap_or_default();
    let scan = crate::claudes::claude_terminals(std::path::Path::new("/proc"));
    let mut rows = Vec::new();
    for p in in_scope {
        let launched: std::collections::HashSet<String> =
            crate::session::launched_names(&p.url).into_iter().map(|(n, _)| n).collect();
        let evidence = crate::claudes::claude_evidence_with_scan(&p.url, &scan);
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
/// `/` — the overview, and now the only front page. The directory picker
/// that used to live behind `?at=` is gone: the overview lists every project
/// directory under the roots (`registry::project_rows`), so browsing to find
/// one had nothing left to add, and the button that reached it was pointing
/// at a page that answered a question the front page already answers.
fn serve_index(w: &mut impl Write, req: &http::Request, roots: &[PathBuf]) {
    let root_labels: Vec<String> = roots.iter().map(|r| r.display().to_string()).collect();
    let sel = req.query.get("sel").map(String::as_str).unwrap_or("");
    http::html(w, &render::overview_page(sel, &root_labels));
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

/// Serialises the tests that set the process-global `ROOST_STATIC`/`HOME`.
/// cargo runs a binary's tests in parallel threads, so without this two of
/// them interleave and one sees the other's environment mid-body — a
/// flakiness this project has shipped once before (see SESSION_ENV_LOCK).
#[cfg(test)]
pub static ASSET_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// Must classify the same way `assets::class_of` does — see `ext_of`'s doc
// comment for why: the two functions disagreeing about one string is the
// defect, not any individual choice either makes.

/// The wire shape of a project (#116).
///
/// A struct of its own rather than `Serialize` on `registry::ProjectStatus`:
/// that type is roost's internal answer and carries `wt`, a whole
/// `WorktreeStatus` with git evidence in it. Deriving on it would make every
/// future field of an internal struct part of a client contract by default,
/// which is how an API grows things nobody meant to promise.
#[derive(serde::Serialize)]
struct ApiProject {
    /// Storage key, percent-encoded (`karpie%2Fsrc`).
    key: String,
    /// URL form, readable slashes (`karpie/src`).
    url: String,
    live: usize,
    /// `null`, never `0`, when the age is genuinely unknown — the normal case
    /// right after a restart, when this process's session map is empty. The
    /// field's own comment in `registry.rs` records why `0` was wrong: it
    /// claimed every project's oldest shell had just started, at exactly the
    /// moment "what did I leave running for days?" is the question being
    /// asked.
    oldest_age_secs: Option<u64>,
    has_layout: bool,
    branch: String,
    parent: Option<String>,
    /// False for a worktree git vouches for that does not canonicalise under
    /// any root. **Kept, not omitted** — the web UI renders it dimmed and
    /// unclickable rather than dropping it, and a client that silently listed
    /// fewer projects than the web UI would be the harder bug to find.
    reachable: bool,
    is_worktree: bool,
}

impl From<&registry::ProjectStatus> for ApiProject {
    fn from(p: &registry::ProjectStatus) -> Self {
        ApiProject {
            key: p.key.clone(),
            url: p.url.clone(),
            live: p.live,
            oldest_age_secs: p.oldest_age_secs,
            has_layout: p.has_layout,
            branch: p.branch.clone(),
            parent: p.parent.clone(),
            reachable: p.reachable,
            is_worktree: p.wt.is_some(),
        }
    }
}

/// The wire shape of a live session (#116).
#[derive(serde::Serialize)]
struct ApiSession {
    project_url: String,
    name: String,
    /// True only on *positive* evidence that a Claude is in this terminal —
    /// `claudes.rs` has a third answer, `Unknown`, and it is reported here as
    /// `false` for the same reason the web UI renders it as a plain shell: no
    /// mark is the honest rendering of "nobody looked successfully".
    is_claude: bool,
    /// `null` for unknown, for the same reason as `oldest_age_secs` above.
    age_secs: Option<u64>,
    attached: usize,
}

impl From<&render::OvSession> for ApiSession {
    fn from(s: &render::OvSession) -> Self {
        ApiSession {
            project_url: s.project_url.clone(),
            name: s.name.clone(),
            is_claude: s.is_claude,
            age_secs: s.age_secs,
            attached: s.attached,
        }
    }
}

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
        // The one extension that gets a real type without being theme-
        // replaceable (#112). Everything above is in `assets::THEME_EXT`, and
        // the invariant the test below documents — a real type implies
        // `Class::Theme` — holds for all of them. A manifest must not: it
        // names the installed app, its icons, its scope and its start_url, so
        // a project theme that could replace it could rename roost on a home
        // screen and re-point where launching it lands. `Class::Code`, and
        // replaceable only by the operator through `$ROOST_STATIC`.
        "webmanifest" => "application/manifest+json",
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

/// Extensions served as a document for the browser's own viewer (#121) — not
/// on `IMAGE_EXT`, because that list also decides what a markdown `<img>` may
/// embed, and a PDF is not a picture. Deny-by-default like the list above.
pub const PDF_EXT: &[&str] = &["pdf"];

pub fn is_pdf(rel: &str) -> bool {
    PDF_EXT.contains(&crate::assets::ext_of(rel).as_str())
}

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
    if is_pdf(rel) {
        return serve_pdf(w, dir, rel);
    }
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

/// `~/.config/roost/static`, the optional user overlay. Absent on a fresh
/// install, which is not an error — the layer is simply skipped.
fn user_static_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".config/roost/static"))
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
///   1. $ROOST_STATIC        any class   (operator runtime switch)
///   2. ~/.config/roost/static  theme class only
///   3. embedded            any class   (always present)
///
/// The class restriction on layer 2 is the enforcement mechanism, not a
/// check that could be forgotten: a code-class path never consults it.
///
/// `v` is the request's `?v=`. Only the embedded layer may answer with a
/// forever-cache, and only when `v` is the hash of the bytes being sent: the
/// other two layers change without the build changing, so their `?v=` (a hash
/// of the *embedded* copy) vouches for nothing; and a stale page asking with
/// an old `v` must not have today's bytes stored under yesterday's URL. Every
/// other answer falls to `respond_with`'s `no-cache`.
fn serve_static(w: &mut impl Write, rel: &str, v: Option<&str>) {
    // Before any layer, so a traversal attempt cannot reveal which layers exist.
    let Some(rel) = crate::assets::normalize(rel) else {
        return http::not_found(w, "no such asset");
    };
    let ctype = content_type(rel);

    if let Some(dir) = std::env::var_os("ROOST_STATIC") {
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
        Some(body) if v.is_some_and(|v| v == crate::render::av(rel)) => {
            http::respond_with(w, 200, "OK", ctype, &[NOSNIFF, IMMUTABLE], body)
        }
        Some(body) => http::respond_with(w, 200, "OK", ctype, &[NOSNIFF], body),
        None => http::not_found(w, "no such asset"),
    }
}

/// A year is the conventional "forever"; `immutable` stops a reload from even
/// revalidating. Safe only because the URL changes whenever the bytes do.
const IMMUTABLE: (&str, &str) = ("Cache-Control", "public, max-age=31536000, immutable");

/// Every fragment kind `serve_frag` matches on — the closed set `route()`
/// consults to tell an ordinary fragment request from a `.roost/theme/{rel}`
/// request when the project path itself contains a "theme" segment.
///
/// This list must be kept in sync with `serve_frag`'s match arms by hand:
/// adding a new fragment kind there and forgetting to add it here makes
/// `route()` treat it as a theme-asset path instead (or vice versa if one
/// is removed from serve_frag but left here). There is no compiler check
/// for that — it's the one hazard this dispatch-by-kind approach carries
/// that a fully generic parse wouldn't.
const FRAGMENT_KINDS: &[&str] =
    &["tree", "file", "raw", "changes", "status", "diff", "proposal", "theme.css", "backup", "download"];


/// A PDF for the browser's own viewer (#121), in a tab's `<iframe>` or opened
/// on its own.
///
/// **Still `SANDBOX`.** #121 expected the built-in viewers to refuse a
/// sandboxed document and planned a narrower policy for this route; the probe
/// in its spec found both Chromium's and Firefox's viewer render under plain
/// `sandbox`, so the route keeps exactly what every other project byte gets.
/// The type comes from the extension and `nosniff` holds it there: an HTML
/// file named `x.pdf` is offered to the PDF viewer, never rendered as a page.
///
/// Streamed, with no size cap, for the reason on `serve_download`: the cap
/// bounds a whole-file read this path never makes. `MAX_FILE_BYTES` is not
/// raised — images on the route above keep it.
fn serve_pdf(w: &mut impl Write, dir: &Path, rel: &str) {
    let (f, _) = match open_regular(dir, rel) {
        Ok(opened) => opened,
        Err(msg) => return http::not_found(w, msg),
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\n{}: {}\r\n{}: {}\r\n\
         Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        NOSNIFF.0, NOSNIFF.1, SANDBOX.0, SANDBOX.1
    );
    stream_chunked(w, f, &head);
}

/// The file at `rel`, confined and open, if it is a regular file, with the
/// path it resolved to. The
/// pipeline both streaming routes share, in the order `serve_raw` uses:
/// normalise, then canonicalise-and-confine. `safe_resolve` resolves
/// symlinks, so what is checked here is the target, which is what gets read.
fn open_regular(dir: &Path, rel: &str) -> Result<(std::fs::File, PathBuf), &'static str> {
    let rel = crate::assets::normalize(rel).ok_or("no such file")?;
    let path = projects::safe_resolve(dir, rel).map_err(|_| "no such file")?;
    let meta = std::fs::metadata(&path).map_err(|_| "no such file")?;
    if meta.is_dir() {
        // Upload's own wording, because it is the same answer to the same
        // question asked from the other direction.
        return Err("folders are not downloaded — use git or scp for a directory");
    }
    // Matched on the type, not inferred from "not a directory". A FIFO or a
    // device node in a cloned repository would block this connection thread on
    // `read` forever — no error, no timeout, one thread gone per attempt.
    if !meta.file_type().is_file() {
        return Err("not a regular file");
    }
    let f = std::fs::File::open(&path).map_err(|_| "no such file")?;
    Ok((f, path))
}

/// Writes `head`, then `f` as a chunked body, `DOWNLOAD_CHUNK` at a time.
fn stream_chunked(w: &mut impl Write, mut f: std::fs::File, head: &str) {
    if w.write_all(head.as_bytes()).is_err() {
        return;
    }
    let mut buf = vec![0u8; DOWNLOAD_CHUNK];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if write!(w, "{n:x}\r\n").is_err()
                    || w.write_all(&buf[..n]).is_err()
                    || w.write_all(b"\r\n").is_err()
                {
                    return; // the peer went away; nothing useful left to say
                }
            }
            // A read error after the first chunk cannot change the status any
            // more, so the connection is dropped *without* the terminating
            // chunk. That is what makes the browser report a failed transfer
            // instead of keeping what arrived as if it were the whole file.
            Err(_) => return,
        }
    }
    let _ = w.write_all(b"0\r\n\r\n");
    let _ = w.flush();
}

/// One file from the tree, as a download (#120).
///
/// The second streaming download in this file, and it follows `serve_backup`
/// rather than reinventing: **chunked**, because once the first byte is out the
/// status is spent and a browser must report a failed transfer rather than save
/// a truncated file that looks complete; **no `Origin` check**, for the reason
/// spelled out on `serve_backup` — a download is a top-level navigation and
/// browsers send none, the response is not readable cross-origin, and the
/// DNS-rebinding host gate in `route()` applies before either is reached.
///
/// It carries `SANDBOX` as well as `NOSNIFF`, which `serve_backup` does not
/// need: this serves arbitrary repository content, so a `.html` or `.svg` must
/// be unable to render in roost's origin by two independent rules.
///
/// **No size cap, and that is not an oversight.** `projects::MAX_FILE_BYTES`
/// bounds the other read paths because they `fs::read` the whole file into
/// memory; this one reads `DOWNLOAD_CHUNK` at a time and writes each chunk as
/// it goes, so peak memory is one chunk whatever the file size. The cap would
/// be guarding a cost this path does not pay, and build output and logs — the
/// reason to download anything — are routinely past 2 MB. What it *does* cost
/// is one connection thread for the length of the transfer.
fn serve_download(w: &mut impl Write, dir: &Path, rel: &str) {
    let (f, path) = match open_regular(dir, rel) {
        Ok(opened) => opened,
        Err(msg) => return http::not_found(w, msg),
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (fallback, encoded) = disposition_name(&name);
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
         Content-Disposition: attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}\r\n\
         {}: {}\r\n{}: {}\r\n\
         Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        NOSNIFF.0, NOSNIFF.1, SANDBOX.0, SANDBOX.1
    );
    stream_chunked(w, f, &head);
}

/// 64 KB. Large enough that a big file is not a syscall storm, small enough
/// that peak memory per transfer is uninteresting.
const DOWNLOAD_CHUNK: usize = 64 * 1024;

/// The two forms of a filename in `Content-Disposition`, per RFC 6266.
///
/// The name comes out of the repository, so it is **attacker-controlled header
/// content**: a file called `a"b.txt`, or one with a newline in its name, would
/// otherwise end the quoted string or inject a header outright. So the quoted
/// fallback keeps only printable ASCII minus the two characters that can end
/// it, and the RFC 5987 form carries the real name percent-encoded for anything
/// that understands it.
///
/// An empty result would emit `filename=""`, which some browsers save as a
/// literal empty name, so it falls back to a word.
fn disposition_name(name: &str) -> (String, String) {
    let fallback: String = name
        .chars()
        .map(|c| match c {
            '"' | '\\' => '-',
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => '-',
            c if !c.is_ascii() => '-',
            c => c,
        })
        .collect();
    let fallback = if fallback.trim().is_empty() { "download".to_string() } else { fallback };
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    (fallback, encoded)
}

/// The archive, as a download.
///
/// Chunked rather than `Content-Length`, for three reasons that point the same
/// way. The length is not known until the walk has finished; the alternative is
/// staging the whole archive somewhere to measure it, which for a file holding
/// every credential the machine has seen means either `/tmp` or a second copy
/// in memory; and an interrupted chunked response is an error in the browser
/// rather than a file that looks complete. The per-entry declared lengths in
/// `backup::parse` are the second line of defence at restore time.
///
/// The walk's own `Err` — an unreadable conversation directory — arrives
/// *before* any body is written, which is what makes a 500 possible here at
/// all. Once the first chunk is out the status is spent, and the only honest
/// signal left is to drop the connection without the terminating chunk. That
/// is why `collect` builds its entries before writing a byte.
fn serve_backup(w: &mut impl Write, project: &str, dir: &Path, conversations: bool) {
    let mut body = Vec::new();
    if let Err(e) = crate::backup::collect(project, dir, conversations, &mut body) {
        // The refusal reaches the user as text, because it is the one thing
        // they can act on: it names the directory and says why nothing was
        // written rather than handing back an archive with a hole in it.
        return http::respond(w, 500, "Internal Server Error", "text/plain; charset=utf-8", e.as_bytes());
    }
    // The storage key, not the project name: it is already percent-encoded, so
    // no quote, newline or `/` from a project name can reach this header raw.
    // `storage_key` leaves `%` and every printable ASCII alone, hence the
    // second pass — a filename is not a storage key, and the only characters
    // that matter here are the ones that would end the quoted string.
    let stem: String = crate::projects::storage_key(project)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let day = crate::backup::today_utc();
    let name = format!("{stem}-{day}.roostbak");
    // `nosniff` because this body is attacker-influenced in the only sense
    // that matters here: it contains a project's own files. Without it a
    // browser is free to sniff an archive whose first transcript happens to
    // begin with markup and render it as HTML in roost's own origin.
    //
    // No `Origin` check, deliberately, and the reasoning belongs here because
    // every other write path in this file has one. A GET cannot require it: a
    // download is a top-level navigation and browsers send no `Origin` on one,
    // so requiring it would refuse the only request this endpoint exists to
    // serve. What stands in its place is that a GET response is not *readable*
    // cross-origin — roost sends no CORS headers, so a hostile page can cause
    // this download and never see a byte of it — plus the DNS-rebinding gate
    // `route()` applies to every request before this is reached. The residual
    // is a page that can make a file land in someone's downloads folder, which
    // is true of every URL on the internet.
    let _ = write!(
        w,
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
         Content-Disposition: attachment; filename=\"{name}\"\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    );
    // One chunk per 64 KB, so a large archive does not sit in a single write.
    for part in body.chunks(64 * 1024) {
        let _ = write!(w, "{:x}\r\n", part.len());
        let _ = w.write_all(part);
        let _ = w.write_all(b"\r\n");
    }
    let _ = w.write_all(b"0\r\n\r\n");
    let _ = w.flush();
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
    // The header toggle beats the config file; a project with no hub has no
    // toggle, and asking must not create one (see `Hub::show_hidden`).
    let filter = settings.tree_filter_with(crate::hub::Hub::show_hidden(project));
    match what {
        // The ✻ menu's rows. A fragment rather than a websocket event because
        // it is asked for on a click and answered once — the same shape as the
        // project strip, and it keeps the list out of every snapshot, which
        // goes out on each debounced keystroke.
        ["claudehist"] => {
            http::html(
                w,
                &render::claude_history(&crate::claudehist::recent(&dir, crate::claudehist::MAX_ROWS)),
            );
        }
        // #18 step 3. A GET because a backup is a read: it changes nothing,
        // and there was never a reason for it to be anything else. The
        // *restore* half is the existing `POST /upload` plus a
        // `RestoreWorkspace` intent, so the two-POST surface CLAUDE.md caps
        // stays at two.
        //
        // `conversations=1` is the opt-in #18 asks for and the only thing that
        // turns transcripts on. Anything else in the query — including
        // `conversations=0`, `conversations=true`, or a repeat — leaves them
        // off: this is the switch that decides whether every prompt, file and
        // command output on the machine leaves it, so it matches one exact
        // string rather than being parsed leniently.
        ["backup"] => {
            let conversations =
                crate::backup::wants_conversations(req.query.get("conversations").map(String::as_str));
            serve_backup(w, project, &dir, conversations)
        }
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
            //
            // A PDF takes the same arm: it has no text either, and its tab is
            // nothing but an <iframe> on the same raw route (#121).
            Some(rel) if is_image(rel) || is_pdf(rel) => match crate::assets::normalize(rel) {
                None => http::html(w, &render::file_error_fragment(rel, "path outside project")),
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
                        if is_pdf(rel) {
                            http::html(w, &render::pdf_fragment(project, rel, mtime_secs))
                        } else {
                            http::html(w, &render::image_fragment(project, rel, mtime_secs))
                        }
                    },
                    Err(e) => http::html(w, &render::file_error_fragment(rel, &e)),
                },
            },
            Some(rel) => match projects::safe_resolve(&dir, rel)
                .and_then(|p| projects::read_text_file(&p))
            {
                Ok(content) => http::html(w, &render::file_fragment(project, rel, &content)),
                Err(e) => http::html(w, &render::file_error_fragment(rel, &e)),
            },
        },
        ["raw"] => match req.query.get("path") {
            None => http::not_found(w, "missing path"),
            Some(rel) => serve_raw(w, &dir, rel),
        },
        // #120. The other half of upload: roost could put a file on the server
        // and had no way to hand one back.
        ["download"] => match req.query.get("path") {
            None => http::not_found(w, "missing path"),
            Some(rel) => serve_download(w, &dir, rel),
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
        // roost's own opaque key (`ide::new_pending_id`), not a path — there
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
        // `.roost/theme.css` that is a symlink pointing outside the
        // project (planted by a cloned repo) is refused rather than served
        // to the browser as text/css. Every other file read in this module
        // already goes through this confinement; this one predates it.
        //
        // Same untrusted-content policy as the theme directory route
        // (NOSNIFF + SANDBOX): this is project-controlled CSS too, and an
        // attacker doesn't care which of the two theme routes they're
        // abusing.
        ["theme.css"] => match projects::safe_resolve(&dir, ".roost/theme.css")
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

/// A project's own theme directory, `{project}/.roost/theme/`.
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
    let Ok(path) = projects::safe_resolve(dir, &format!(".roost/theme/{rel}")) else {
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
    if dir.join(".roost/theme/style.css").is_file() {
        Some("theme/style.css")
    } else if dir.join(".roost/theme.css").is_file() {
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
        serve_v(rel, None)
    }

    fn serve_v(rel: &str, v: Option<&str>) -> String {
        let mut buf: Vec<u8> = Vec::new();
        serve_static(&mut buf, rel, v);
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[test]
    fn an_absent_overlay_serves_the_embedded_copy() {
        let _g = ASSET_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ROOST_STATIC");
        let home = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(home.path());
        let out = serve("style.css");
        assert!(out.starts_with("HTTP/1.1 200 OK"));
        assert!(out.contains("X-Content-Type-Options: nosniff"));
        assert!(!out.contains("Content-Security-Policy"), "embedded assets are not untrusted");
    }

    /// The forever-cache needs both halves: the embedded layer, and a `v` that
    /// is the hash of what is being sent. The old `v` case is the deploy this
    /// exists for — a page from before it asks with yesterday's hash, and
    /// must not get today's bytes cached for a year under that URL.
    #[test]
    fn only_the_current_version_of_an_embedded_asset_is_cached_for_good() {
        let _g = ASSET_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ROOST_STATIC");
        let home = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(home.path());
        let cur = crate::render::av("app.js");
        let out = serve_v("app.js", Some(&cur));
        assert!(out.contains("Cache-Control: public, max-age=31536000, immutable\r\n"), "current v: {}", &out[..200]);
        assert_eq!(out.matches("Cache-Control:").count(), 1);
        for v in [Some("00000000"), None] {
            let out = serve_v("app.js", v);
            assert!(out.contains("Cache-Control: no-cache\r\n") && !out.contains("immutable"), "v = {v:?}: {}", &out[..200]);
        }
    }

    /// Both overlays change without the build changing, and their `?v=` is
    /// the embedded copy's hash — so even the *current* `v` vouches for
    /// nothing there. Fixtures carry a marker, so this cannot pass by the
    /// overlay being skipped and the embedded file served instead.
    #[test]
    fn an_overlay_is_never_cached_for_good() {
        let _g = ASSET_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cur = crate::render::av("style.css");
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("style.css"), "/*DEV*/").unwrap();
        std::env::set_var("ROOST_STATIC", d.path());
        let home = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(home.path());
        let out = serve_v("style.css", Some(&cur));
        std::env::remove_var("ROOST_STATIC");
        assert!(out.contains("/*DEV*/") && out.contains("Cache-Control: no-cache\r\n") && !out.contains("immutable"), "ROOST_STATIC: {out}");
        let user = home.path().join(".config/roost/static");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("style.css"), "/*USER*/").unwrap();
        let out = serve_v("style.css", Some(&cur));
        assert!(out.contains("/*USER*/") && out.contains("Cache-Control: no-cache\r\n") && !out.contains("immutable"), "user dir: {out}");
    }

    #[test]
    fn roost_static_overrides_one_file_and_the_rest_fall_through() {
        let _g = ASSET_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("style.css"), "/*OVERRIDDEN*/").unwrap();
        std::env::set_var("ROOST_STATIC", d.path());
        let home = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(home.path());

        assert!(serve("style.css").contains("/*OVERRIDDEN*/"));
        // Not present in the overlay dir, so it must still resolve.
        assert!(serve("app.js").starts_with("HTTP/1.1 200 OK"));

        std::env::remove_var("ROOST_STATIC");
    }

    /// The rule the whole class split exists for. A .js in the user dir must
    /// not merely be "blocked" — the layer is never consulted for it, so the
    /// embedded copy is what comes back.
    #[test]
    fn the_user_directory_may_not_replace_code() {
        let _g = ASSET_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ROOST_STATIC");
        let home = tempfile::tempdir().unwrap();
        let userdir = home.path().join(".config/roost/static");
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
        let userdir = home.path().join(".config/roost/static");
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

        std::env::remove_var("ROOST_STATIC");
        let without: Vec<String> = probes.iter().map(|p| serve(p)).collect();

        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("themes")).unwrap();
        std::fs::write(d.path().join("style.css"), "/*LAYER1*/").unwrap();
        std::env::set_var("ROOST_STATIC", d.path());
        let with: Vec<String> = probes.iter().map(|p| serve(p)).collect();
        std::env::remove_var("ROOST_STATIC");

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
        let t = d.path().join(".roost/theme");
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
        let t = d.path().join(".roost/theme");
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
        let t = d.path().join(".roost/theme");
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
        std::fs::create_dir_all(d.path().join(".roost/theme")).unwrap();
        assert_eq!(theme_link_for(d.path()), None, "neither present");

        std::fs::write(d.path().join(".roost/theme.css"), "a{}").unwrap();
        assert_eq!(theme_link_for(d.path()), Some("theme.css"), "the legacy file still works");

        std::fs::write(d.path().join(".roost/theme/style.css"), "b{}").unwrap();
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
        String::from_utf8_lossy(&frag_route_bytes(roots, path)).into_owned()
    }

    /// The same call without the lossy UTF-8 conversion.
    ///
    /// Every other fragment is text, so `frag_route` returning a `String` has
    /// always been right. A download is not: `from_utf8_lossy` replaces every
    /// invalid sequence with U+FFFD, which silently rewrites a binary body —
    /// the 160 KB test first failed at exactly 65536 bytes and looked like a
    /// chunking bug in `serve_download`, when it was the harness mangling the
    /// bytes on the way out.
    fn frag_route_bytes(roots: &[PathBuf], path: &str) -> Vec<u8> {
        let raw = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        let req = http::parse(&mut std::io::Cursor::new(raw.as_bytes())).unwrap();
        let mut buf: Vec<u8> = Vec::new();
        route(&mut buf, &req, roots);
        buf
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
        std::fs::create_dir_all(a.join(".roost/theme")).unwrap();
        std::fs::write(a.join(".roost/theme/style.css"), "css-a-style").unwrap();

        // Project "a/theme": a nested project whose own name contains
        // "theme" — the case the regression broke.
        let a_theme = root.path().join("a/theme");
        std::fs::create_dir_all(a_theme.join(".roost/theme")).unwrap();
        std::fs::write(a_theme.join("inner.rs"), "fn main() {}").unwrap();
        std::fs::write(a_theme.join(".roost/theme.css"), "css-a-theme-legacy").unwrap();
        std::fs::write(a_theme.join(".roost/theme/style.css"), "css-a-theme-style").unwrap();

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
        // theme-asset rule, reading project "a"'s .roost/theme/style.css —
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
    /// panic showed the literal 404 response body, no `ovscope`/`ovsessions`
    /// anywhere. Restored.
    ///
    /// Asserts on `ovscope` (the out-of-band scope label) and the list
    /// class, not on the old inline "SESSIONS" heading: the pane title is
    /// now static chrome in `overview_page`, so the fragment no longer
    /// carries that word at all.
    /// Worktrees are loaded lazily: the top-level list must not contain
    /// them (and must not pay `git worktree list` to find out), and opening
    /// a project must bring its worktrees in as children of that row.
    ///
    /// Revert-checked: with `open` ignored (the `kids` map left empty) the
    /// second assertion failed — no `data-parent`, no `feat` row — while the
    /// One front page. `?at=` used to serve a directory picker; it is gone,
    /// and the query is simply ignored rather than reserved.
    ///
    /// Revert-checked: with `serve_index` restored to its `at`-branching
    /// form, the second assertion fails — `/?at=` renders `id="picker"` and
    /// no overview.
    #[test]
    fn the_front_page_is_the_overview_whatever_the_query() {
        let d = tempfile::tempdir().unwrap();
        let roots = vec![d.path().to_path_buf()];
        for path in ["/", "/?at=", "/?at=karpie"] {
            let out = frag_route(&roots, path);
            assert!(out.contains("id=\"overview\""), "{path} is the overview: {out}");
            assert!(!out.contains("id=\"picker\""), "{path} is not the picker: {out}");
        }
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
        // The one deliberate exception (#112), asserted rather than left for
        // someone to discover and "fix" by adding `webmanifest` to
        // `THEME_EXT` — which would let a project theme rename roost on a home
        // screen and re-point its start_url.
        assert_eq!(content_type("manifest.webmanifest"), "application/manifest+json");
        assert_eq!(
            crate::assets::class_of("manifest.webmanifest"),
            crate::assets::Class::Code,
            "a theme must never be able to replace the manifest"
        );
    }

    /// #18 step 3. Goes through `route()` like every other fragment test —
    /// the download has to be reachable by the *router*, not merely by a
    /// direct call to `serve_backup`, which is the gap the harness comment
    /// above describes.
    #[test]
    fn the_backup_download_is_an_archive_and_conversations_are_off_unless_asked() {
        crate::wsstate::set_state_dir_for_test();
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("bkproj")).unwrap();
        let roots = vec![d.path().to_path_buf()];

        let out = frag_route(&roots, "/frag/bkproj/backup");
        assert!(out.starts_with("HTTP/1.1 200 OK"), "{}", &out[..out.len().min(80)]);
        assert!(out.contains("Content-Disposition: attachment;"), "{out}");
        assert!(out.contains(".roostbak\""), "the name must say what it is: {out}");
        assert!(out.contains("Transfer-Encoding: chunked"), "{out}");
        // An archive whose first transcript begins with markup must not be
        // sniffable as HTML in roost's own origin.
        assert!(out.contains("X-Content-Type-Options: nosniff"), "{out}");
        // The body is chunked, so the magic is not at a fixed offset — but it
        // must be in there, and this is the assertion that would catch a
        // handler that sent headers and no body at all.
        let body = out.split_once("\r\n\r\n").expect("headers end").1;
        assert!(body.contains(crate::backup::MAGIC), "no archive in the body: {body:?}");
        assert!(body.ends_with("0\r\n\r\n"), "the terminating chunk is missing: {body:?}");
    }

    /// The opt-in reaches the handler at all.
    ///
    /// Deliberately narrow: with no transcript directory the response is
    /// byte-identical whichever way the flag goes, so this asserts only that
    /// both forms route and neither 500s. The exactness of the match is where
    /// it can actually be tested — `backup::wants_conversations`, a pure
    /// function with its own exhaustive test. An assertion here that the
    /// archive "holds no conversation" would pass against a handler that
    /// ignored the query string completely, which is the trap this project
    /// names as its dominant failure mode.
    #[test]
    fn both_forms_of_the_backup_request_route() {
        crate::wsstate::set_state_dir_for_test();
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("bkoptin")).unwrap();
        let roots = vec![d.path().to_path_buf()];
        for q in ["", "?conversations=1", "?conversations=0", "?conversations=nonsense"] {
            let out = frag_route(&roots, &format!("/frag/bkoptin/backup{q}"));
            assert!(out.starts_with("HTTP/1.1 200 OK"), "{q}: {}", &out[..out.len().min(60)]);
        }
    }

    /// A project name that would end the quoted filename, or start a second
    /// header line. Neither reaches the response.
    #[test]
    fn a_project_name_cannot_write_its_own_download_header() {
        crate::wsstate::set_state_dir_for_test();
        let d = tempfile::tempdir().unwrap();
        // resolve_project accepts a nested rel, so a `/` in the name is the
        // realistic case (a worktree); the rest are what a hostile directory
        // name on the roots would be called.
        std::fs::create_dir_all(d.path().join("bk/sub")).unwrap();
        let roots = vec![d.path().to_path_buf()];
        let out = frag_route(&roots, "/frag/bk/sub/backup");
        assert!(out.starts_with("HTTP/1.1 200 OK"), "{}", &out[..out.len().min(80)]);
        let disp = out
            .lines()
            .find(|l| l.starts_with("Content-Disposition"))
            .expect("a disposition header");
        assert_eq!(disp.matches('"').count(), 2, "the filename must stay one quoted token: {disp}");
        assert!(!disp.contains('/'), "no separator survives into the name: {disp}");
        assert!(disp.contains("bk-2Fsub") || disp.contains("bk-sub"), "{disp}");
    }

    /// #116. The JSON the native client lists from — asserted through
    /// `route()`, like every other endpoint test here, so the *router* has to
    /// reach it and not merely the handler.
    #[test]
    fn the_api_lists_projects_as_json() {
        crate::wsstate::set_state_dir_for_test();
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("apiproj")).unwrap();
        let roots = vec![d.path().to_path_buf()];
        let out = frag_route(&roots, "/api/projects");
        assert!(out.starts_with("HTTP/1.1 200 OK"), "{}", &out[..out.len().min(80)]);
        assert!(out.contains("Content-Type: application/json"), "{out}");
        // The body carries project names and branch names, which come off the
        // filesystem; a browser free to sniff it as HTML would render them in
        // roost's own origin.
        assert!(out.contains("X-Content-Type-Options: nosniff"), "{out}");
        let body = out.split_once("\r\n\r\n").expect("headers end").1;
        let v: serde_json::Value = serde_json::from_str(body).expect("a JSON array");
        assert!(v.is_array(), "got {v}");
        let row = v.as_array().unwrap().iter().find(|r| r["url"] == "apiproj").expect("the project");
        // The distinction this endpoint has to preserve, and the one most
        // easily lost in a hand-written serializer: unknown is `null`, never
        // `0`. `registry.rs` records why — `0` claimed every project's oldest
        // shell had just started, at the moment "what did I leave running for
        // days?" is the question.
        assert!(row["oldest_age_secs"].is_null() || row["oldest_age_secs"].is_u64());
        assert!(row["reachable"].is_boolean(), "reachable must be carried, not omitted: {row}");
        // The internal `wt` is a whole WorktreeStatus with git evidence in it.
        // It is not part of the contract; only whether this *is* a worktree.
        assert!(row.get("wt").is_none(), "internal state leaked into the wire shape: {row}");
        assert!(row["is_worktree"].is_boolean(), "{row}");
    }

    /// The sessions list, and that `?project=` narrows it rather than being
    /// ignored — an endpoint that returned everything regardless would pass a
    /// shape check and show a phone every session on the machine.
    #[test]
    fn the_api_lists_sessions_and_the_project_filter_is_honoured() {
        crate::wsstate::set_state_dir_for_test();
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("apisess")).unwrap();
        let roots = vec![d.path().to_path_buf()];
        let all = frag_route(&roots, "/api/sessions");
        assert!(all.starts_with("HTTP/1.1 200 OK"), "{}", &all[..all.len().min(80)]);
        let body = all.split_once("\r\n\r\n").expect("headers end").1;
        assert!(serde_json::from_str::<serde_json::Value>(body).unwrap().is_array());
        // A key that matches nothing must produce an empty list, not the
        // unfiltered one.
        let none = frag_route(&roots, "/api/sessions?project=nosuchproject");
        let nbody = none.split_once("\r\n\r\n").expect("headers end").1;
        let v: serde_json::Value = serde_json::from_str(nbody).unwrap();
        assert_eq!(v.as_array().map(|a| a.len()), Some(0), "the filter was ignored: {v}");
    }

    // ---- downloading a file (#120) ----

    fn dl(roots: &[PathBuf], q: &str) -> String {
        frag_route(roots, &format!("/frag/p/download?path={q}"))
    }

    #[test]
    fn a_download_serves_the_exact_bytes_with_the_headers_that_stop_it_rendering() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("p");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("build.log"), b"line one\nline two\n").unwrap();
        let roots = vec![d.path().to_path_buf()];

        let out = dl(&roots, "build.log");
        assert!(out.starts_with("HTTP/1.1 200 OK"), "{}", &out[..out.len().min(80)]);
        assert!(out.contains("Content-Disposition: attachment;"), "{out}");
        assert!(out.contains(r#"filename="build.log""#), "{out}");
        // Two independent rules, because this serves arbitrary repository
        // content: a downloaded .html or .svg must not render in roost's origin.
        assert!(out.contains("X-Content-Type-Options: nosniff"), "{out}");
        assert!(out.contains("Content-Security-Policy: sandbox"), "{out}");
        assert!(out.contains("Transfer-Encoding: chunked"), "{out}");
        let body = out.split_once("\r\n\r\n").expect("headers end").1;
        assert_eq!(dechunk(body), b"line one\nline two\n", "wrong bytes: {body:?}");
    }

    /// More than one chunk. A single-chunk fixture passes against a writer that
    /// ignores its loop and sends only the first read.
    #[test]
    fn a_file_larger_than_one_chunk_arrives_whole() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("p");
        std::fs::create_dir_all(&p).unwrap();
        // 160 KB: three chunks at 64 KB, with the last one short.
        let want: Vec<u8> = (0..160_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(p.join("big.bin"), &want).unwrap();
        let roots = vec![d.path().to_path_buf()];

        let raw = frag_route_bytes(&roots, "/frag/p/download?path=big.bin");
        let sep = raw.windows(4).position(|w| w == b"\r\n\r\n").expect("headers end") + 4;
        let got = dechunk_bytes(&raw[sep..]);
        assert_eq!(got.len(), want.len(), "truncated at {} bytes", got.len());
        assert_eq!(got, want, "the bytes changed somewhere in the chunking");
    }

    #[test]
    fn a_directory_is_refused_with_the_words_upload_uses() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("p");
        std::fs::create_dir_all(p.join("src")).unwrap();
        let roots = vec![d.path().to_path_buf()];
        let out = dl(&roots, "src");
        assert!(out.starts_with("HTTP/1.1 404"), "{out}");
        assert!(out.contains("folders are not downloaded"), "{out}");
    }

    /// A FIFO would block the connection thread on `read` forever — no error,
    /// no timeout, one thread gone per attempt. **Bounded by its own timeout**,
    /// because a regression here hangs the run rather than failing it, and
    /// CLAUDE.md is explicit that a hang proves nothing about a green suite.
    #[test]
    fn a_fifo_is_refused_promptly_rather_than_blocking_the_thread() {
        #[cfg(unix)]
        {
            let d = tempfile::tempdir().unwrap();
            let p = d.path().join("p");
            std::fs::create_dir_all(&p).unwrap();
            let fifo = p.join("pipe");
            let made = std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !made {
                return; // no mkfifo on this host; skipped rather than faked
            }
            let roots = vec![d.path().to_path_buf()];
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(dl(&roots, "pipe"));
            });
            let out = rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("serve_download blocked on a FIFO instead of refusing it");
            assert!(out.starts_with("HTTP/1.1 404"), "{out}");
            assert!(out.contains("not a regular file"), "{out}");
        }
    }

    /// The confinement, written so it *reaches* the confinement.
    ///
    /// The symlink's target must exist, or the call fails with `ENOENT` before
    /// `safe_resolve` compares anything — which is exactly how a symlink escape
    /// survived review here once already (CLAUDE.md, *Testing*).
    #[test]
    fn a_path_leaving_the_project_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("p");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(d.path().join("secret.txt"), b"not yours\n").unwrap();
        let roots = vec![d.path().to_path_buf()];

        for q in ["../secret.txt", "..%2Fsecret.txt", "/etc/passwd"] {
            let out = dl(&roots, q);
            assert!(out.starts_with("HTTP/1.1 404"), "{q} was served: {out}");
            assert!(!out.contains("not yours"), "{q} leaked the file: {out}");
        }
        #[cfg(unix)]
        {
            // The escape is genuinely reachable: the target exists and the
            // symlink resolves. Only `safe_resolve`'s prefix check refuses it.
            std::os::unix::fs::symlink(d.path().join("secret.txt"), p.join("link.txt")).unwrap();
            assert!(p.join("link.txt").canonicalize().is_ok(), "setup: the link must resolve");
            let out = dl(&roots, "link.txt");
            assert!(out.starts_with("HTTP/1.1 404"), "a symlink out of the project was served");
            assert!(!out.contains("not yours"), "the symlink leaked its target: {out}");
        }
    }

    /// The filename is attacker-controlled header content: it comes out of the
    /// repository. The fixture really contains a quote, a newline and non-ASCII
    /// — an escaping test whose fixture has nothing to escape is one of the
    /// vacuous ones CLAUDE.md lists by name.
    #[test]
    fn a_hostile_filename_cannot_end_the_header_or_start_another() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("p");
        std::fs::create_dir_all(&p).unwrap();
        let nasty = "a\"b\nSet-Cookie: x=1\u{2028}č.txt";
        if std::fs::write(p.join(nasty), b"ok\n").is_err() {
            return; // a filesystem that refuses the name; nothing to assert
        }
        let roots = vec![d.path().to_path_buf()];
        let out = dl(&roots, &crate::http::percent_encode(nasty));
        assert!(out.starts_with("HTTP/1.1 200 OK"), "{}", &out[..out.len().min(120)]);
        let head = out.split_once("\r\n\r\n").expect("headers end").0;
        let disp = head
            .lines()
            .find(|l| l.starts_with("Content-Disposition"))
            .expect("a disposition header");
        // Exactly two quotes: the ones roost wrote. A third means the name
        // closed the string early.
        assert_eq!(disp.matches('"').count(), 2, "the filename escaped its quotes: {disp}");
        // Injection means a *newline*, not the literal word. The fallback
        // neutered the newline to `-`, so "Set-Cookie" survives as ordinary
        // text inside a quoted value — harmless, and asserting on the word
        // instead of the CR/LF was this test failing for the wrong reason.
        assert!(
            !head.contains("\nSet-Cookie") && !head.contains("\rSet-Cookie"),
            "a header was injected: {head:?}"
        );
        assert!(!disp.contains('\n') && !disp.contains('\r'), "{disp:?}");
        // And the real name survives, percent-encoded, in the RFC 5987 form.
        assert!(disp.contains("filename*=UTF-8''"), "{disp}");
        assert!(disp.contains("%C4%8D"), "the real name was lost, not encoded: {disp}");
    }

    // ---- PDF (#121) ---------------------------------------------------------

    fn pdf_fixture() -> (tempfile::TempDir, Vec<PathBuf>) {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("p/docs")).unwrap();
        let roots = vec![d.path().to_path_buf()];
        (d, roots)
    }

    /// Past the 2 MB cap, and more than one chunk: this is the test that fails
    /// if the PDF goes through the image path's `MAX_FILE_BYTES` check (it did,
    /// as "asset too large", before #121), and a single-chunk fixture would pass
    /// a writer that sent only its first read. That images keep the cap is
    /// `raw_refuses_an_oversize_image_and_a_symlink_out`, unchanged.
    ///
    /// Revert-checked: without the `is_pdf` branch in `serve_raw` this fails
    /// with a 404 "not an image".
    #[test]
    fn a_pdf_past_the_cap_is_served_whole_with_the_headers_that_stop_it_rendering() {
        let (d, roots) = pdf_fixture();
        let mut want = b"%PDF-1.4\n".to_vec();
        want.extend((0..(crate::projects::MAX_FILE_BYTES as u32 + 300_000)).map(|i| (i % 251) as u8));
        std::fs::write(d.path().join("p/docs/paper.pdf"), &want).unwrap();

        let raw = frag_route_bytes(&roots, "/frag/p/raw?path=docs/paper.pdf");
        let sep = raw.windows(4).position(|w| w == b"\r\n\r\n").expect("headers end") + 4;
        let head = String::from_utf8_lossy(&raw[..sep]);
        assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
        assert!(head.contains("Content-Type: application/pdf"), "{head}");
        assert!(head.contains("X-Content-Type-Options: nosniff"), "{head}");
        // The probe in the spec found both browsers' viewers render under plain
        // `sandbox`, so the PDF gets no exception from it.
        assert!(head.contains("Content-Security-Policy: sandbox\r\n"), "{head}");
        assert!(!head.contains("Content-Disposition"), "a viewer, not a download: {head}");
        let got = dechunk_bytes(&raw[sep..]);
        assert_eq!(got.len(), want.len(), "truncated at {} bytes", got.len());
        assert_eq!(got, want, "the bytes changed somewhere in the chunking");
    }

    /// The type comes from the extension and is pinned by `nosniff`. The
    /// fixture really is HTML with a script in it — one with nothing to
    /// mis-sniff would pass against a route that sniffed.
    #[test]
    fn html_named_pdf_is_still_a_pdf() {
        let (d, roots) = pdf_fixture();
        std::fs::write(d.path().join("p/x.pdf"), "<!doctype html><script>alert(1)</script>").unwrap();
        let out = frag_route(&roots, "/frag/p/raw?path=x.pdf");
        assert!(out.starts_with("HTTP/1.1 200 OK"), "{out}");
        assert!(out.contains("Content-Type: application/pdf"), "{out}");
        assert!(!out.contains("text/html"), "{out}");
        assert!(out.contains("X-Content-Type-Options: nosniff"), "{out}");
        assert!(out.contains("Content-Security-Policy: sandbox"), "{out}");
    }

    /// Bounded by its own timeout, like the download's FIFO test: a regression
    /// hangs rather than fails. Revert-checked: without the regular-file check
    /// in `open_regular` this fails at the 5 s `recv_timeout`.
    #[test]
    fn a_fifo_named_pdf_is_refused_promptly() {
        #[cfg(unix)]
        {
            let (d, roots) = pdf_fixture();
            let made = std::process::Command::new("mkfifo")
                .arg(d.path().join("p/pipe.pdf"))
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !made {
                return; // no mkfifo on this host; skipped rather than faked
            }
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(frag_route(&roots, "/frag/p/raw?path=pipe.pdf"));
            });
            let out = rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("serve_pdf blocked on a FIFO instead of refusing it");
            assert!(out.starts_with("HTTP/1.1 404"), "{out}");
            assert!(out.contains("not a regular file"), "{out}");
        }
    }

    /// The symlink's target exists, so only `safe_resolve`'s prefix check can
    /// refuse it (CLAUDE.md, *Testing*).
    #[test]
    fn a_pdf_outside_the_project_is_refused() {
        let (d, roots) = pdf_fixture();
        std::fs::write(d.path().join("secret.pdf"), b"%PDF not yours").unwrap();
        for q in ["../secret.pdf", "..%2Fsecret.pdf", "/etc/x.pdf"] {
            let out = frag_route(&roots, &format!("/frag/p/raw?path={q}"));
            assert!(out.starts_with("HTTP/1.1 404"), "{q} was served: {out}");
            assert!(!out.contains("not yours"), "{q} leaked the file: {out}");
        }
        #[cfg(unix)]
        {
            let link = d.path().join("p/link.pdf");
            std::os::unix::fs::symlink(d.path().join("secret.pdf"), &link).unwrap();
            assert!(link.canonicalize().is_ok(), "setup: the link must resolve");
            let out = frag_route(&roots, "/frag/p/raw?path=link.pdf");
            assert!(out.starts_with("HTTP/1.1 404"), "a symlink out of the project was served: {out}");
            assert!(!out.contains("not yours"), "{out}");
        }
    }

    /// The `file` fragment for a PDF is the viewer, not the text path's
    /// "binary file" error — which is what it was before #121. The name has
    /// an `&` and a quote in it, so the escaping has something to escape.
    /// Revert-checked: without `|| is_pdf(rel)` on the arm, the first
    /// assertion fails and the fragment is the text path's error.
    #[test]
    fn the_file_fragment_for_a_pdf_is_a_frame_on_the_raw_route() {
        let (d, roots) = pdf_fixture();
        std::fs::write(d.path().join("p/docs/a&b\"c.pdf"), b"%PDF-1.4\n\0\0binary").unwrap();
        let q = crate::http::percent_encode("docs/a&b\"c.pdf");
        let out = frag_route(&roots, &format!("/frag/p/file?path={q}"));
        assert!(out.contains("<iframe class=\"pdfview\""), "{out}");
        assert!(!out.contains("binary"), "fell through to the text path: {out}");
        // `&` in the query separator is escaped as an attribute value must be,
        // and the name's own `&` and `"` are percent-encoded into the URL.
        assert!(out.contains(&format!("src=\"/frag/p/raw?path={}&amp;v=", q.replace('&', "&amp;"))), "{out}");
        assert!(out.contains("a&amp;b&quot;c.pdf"), "the path stripe must be escaped: {out}");
        assert!(!out.contains("a&b\"c"), "an unescaped name reached the HTML: {out}");
        assert!(out.contains("class=\"pdfopen\""), "the phone's way in: {out}");
    }

    /// Reassembles a chunked body. Asserting on the raw chunked text would pass
    /// against a writer that emitted the right bytes with the wrong framing.
    fn dechunk(body: &str) -> Vec<u8> {
        dechunk_bytes(body.as_bytes())
    }

    fn dechunk_bytes(b: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            let Some(nl) = b[i..].windows(2).position(|w| w == b"\r\n") else { break };
            let size = std::str::from_utf8(&b[i..i + nl])
                .ok()
                .and_then(|s| usize::from_str_radix(s.trim(), 16).ok())
                .unwrap_or(0);
            i += nl + 2;
            if size == 0 {
                break;
            }
            out.extend_from_slice(&b[i..(i + size).min(b.len())]);
            i += size + 2;
        }
        out
    }
}
