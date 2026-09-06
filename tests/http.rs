//! Plain GET routes: pages, fragments, static assets, tree/diff traversal
//! guards, and the routing edge cases around them.

mod common;
use common::*;

#[test]
fn the_service_worker_is_served_from_the_root_scope() {
    let (_d, port) = fixture();
    let (status, ctype, body) = get_full(port, "/sw.js");
    assert_eq!(status, 200, "sw.js must be at the root, or its scope cannot cover /{{project}}");
    assert!(ctype.contains("javascript"), "wrong content-type: {ctype}");
    assert!(body.contains("notificationclick"), "not the service worker: {body:.120}");
}

#[test]
fn index_lists_projects() {
    let (_d, port) = fixture();
    let body = ureq::get(&format!("http://127.0.0.1:{port}/"))
        .call().unwrap().into_string().unwrap();
    assert!(body.contains("proj"));
}

#[test]
fn workspace_page_applies_project_settings() {
    let (_d, port) = fixture();
    let body = ureq::get(&format!("http://127.0.0.1:{port}/proj"))
        .call().unwrap().into_string().unwrap();
    assert!(body.contains("/static/themes/light.css")); // .roost config read per request
    assert!(body.contains("data-project=\"proj\""));
}

#[test]
fn fragments_render_and_errors_become_hints() {
    let (_d, port) = fixture();
    let base = format!("http://127.0.0.1:{port}");
    let tree = ureq::get(&format!("{base}/frag/proj/tree")).call().unwrap().into_string().unwrap();
    assert!(tree.contains("hello.md"));
    let file = ureq::get(&format!("{base}/frag/proj/file?path=hello.md"))
        .call().unwrap().into_string().unwrap();
    // The id is part of a heading's normal output now (render::fill_heading_ids),
    // so this asserts the whole tag rather than being loosened to a
    // `contains("Hello")` that could not tell a heading from a paragraph.
    assert!(file.contains("<h1 id=\"hello\">Hello</h1>"), "{file}");
    // escape attempt: 200 + hint, and definitely no file content
    let esc = ureq::get(&format!("{base}/frag/proj/file?path=../../../etc/passwd"))
        .call().unwrap().into_string().unwrap();
    assert!(esc.contains("class=\"hint\""));
    assert!(!esc.contains("root:"));
}

#[test]
fn multi_segment_workspace_url_resolves_the_nested_directory() {
    let (_d, port) = nested_fixture();
    let body = ureq::get(&format!("http://127.0.0.1:{port}/karpie/sub"))
        .call().unwrap().into_string().unwrap();
    assert!(body.contains("data-project=\"karpie/sub\""));
}

#[test]
fn frag_route_resolves_a_nested_projects_fragment_kind() {
    let (_d, port) = nested_fixture();
    let tree = ureq::get(&format!("http://127.0.0.1:{port}/frag/karpie/sub/tree"))
        .call().unwrap().into_string().unwrap();
    assert!(tree.contains("inner.rs"));
}

// Guards the route-ordering fix in routes.rs: `_projects` has too few
// segments to match the general `["frag", rest @ ..] if rest.len() >= 2`
// arm, so without its own arm sitting ahead of the catch-all, this request
// falls through to `serve_workspace` and tries to open a project literally
// named "frag/_projects" instead of serving the cross-project fragment.
// Deleting the `["frag", "_projects"]` arm, or moving it after the
// catch-all, is exactly what this test would catch.
#[test]
fn frag_projects_route_serves_the_cross_project_strip() {
    let (_d, port) = fixture();
    let resp = ureq::get(&format!("http://127.0.0.1:{port}/frag/_projects?current=x"))
        .call()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.into_string().unwrap();
    assert!(body.contains("class=\"projstrip\""));
}

#[test]
fn nested_project_websockets_connect() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    std::env::set_var("ROOST_CMD", "cat");
    let (_d, port) = nested_fixture();

    // routes::route's `[project, rest @ ..]` change is only half the fix —
    // lib.rs's route_ws and term.rs's handle_ws each had their own
    // single-segment-project assumption to update, or a nested workspace
    // page would render while its sockets silently failed to connect.
    let mut ws = ws_connect_path(port, "/ws/karpie/sub/_workspace").unwrap();
    let state = read_until(&mut ws, r#""t":"State""#);
    assert!(state.contains(r#""t":"State""#));
    let _ = ws.close(None);

    let mut term = ws_connect_term(port, "/ws/karpie/sub/term/shell").unwrap();
    term.send(tungstenite::Message::Binary(b"hi\r".to_vec())).unwrap();
    let mut seen = String::new();
    for _ in 0..60 {
        match term.read() {
            Ok(tungstenite::Message::Binary(b)) => seen.push_str(&String::from_utf8_lossy(&b)),
            Ok(_) => {}
            Err(_) => break,
        }
        if seen.contains("hi") {
            break;
        }
    }
    assert!(seen.contains("hi"), "nested project's terminal must echo through the PTY");
    let _ = term.close(None);

    std::env::remove_var("ROOST_STATE_DIR");
    std::env::remove_var("ROOST_CMD");
}

#[test]
fn unknown_pages_are_404() {
    let (_d, port) = fixture();
    assert!(ureq::get(&format!("http://127.0.0.1:{port}/no-such-project")).call().is_err());
    assert!(ureq::get(&format!("http://127.0.0.1:{port}/frag/proj/nope")).call().is_err());
}

#[test]
fn static_assets_served_with_type() {
    let (_d, port) = fixture();
    let resp = ureq::get(&format!("http://127.0.0.1:{port}/static/vendor/highlight.min.js"))
        .call().unwrap();
    assert!(resp.content_type().starts_with("text/javascript"));
}

#[cfg(unix)]
#[test]
fn theme_css_symlink_escaping_the_project_is_refused() {
    // A cloned repo controls .roost/theme.css. If the fragment handler
    // did a bare fs::read of that path, a symlink planted there pointing at
    // e.g. ~/.ssh/id_rsa would be served straight to the browser as
    // text/css. serve_frag must resolve it through safe_resolve like every
    // other file read, so the escape is refused the same way path
    // traversal already is.
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("themeleak")).unwrap();
    std::fs::create_dir(d.path().join("themeleak/.roost")).unwrap();
    let secret = d.path().join("secret.txt");
    std::fs::write(&secret, "top secret\n").unwrap();
    std::os::unix::fs::symlink(&secret, d.path().join("themeleak/.roost/theme.css")).unwrap();
    let port = start(vec![d.path().to_path_buf()]);

    match ureq::get(&format!("http://127.0.0.1:{port}/frag/themeleak/theme.css")).call() {
        Err(ureq::Error::Status(code, r)) => {
            assert_eq!(code, 404);
            assert!(!r.into_string().unwrap().contains("top secret"));
        }
        Ok(r) => panic!("symlink escape must not be served; got {:?}", r.into_string()),
        Err(e) => panic!("unexpected error: {e:?}"),
    }
}

/// The `serve_theme` unit-test helper in routes.rs calls `serve_project_theme`
/// directly, which never proves the URL actually reaches it — the fragment
/// router splits on the *last* path segment for every other fragment kind,
/// and a first cut of this route's dispatch arm required two-or-more
/// segments after "theme" and so could never match, 404ing every request as
/// "no such project" while every direct-call unit test stayed green. This
/// test goes over real HTTP through the router, the only way to catch that.
#[test]
fn frag_theme_directory_serves_presentation_and_refuses_code_over_http() {
    let (d, port) = fixture_named("themedir");
    let t = d.path().join("themedir/.roost/theme");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("style.css"), "body{color:red}").unwrap();
    std::fs::write(t.join("app.js"), "alert('pwned')").unwrap();

    let css = ureq::get(&format!("http://127.0.0.1:{port}/frag/themedir/theme/style.css"))
        .call()
        .unwrap();
    assert_eq!(css.status(), 200);
    assert_eq!(css.header("Content-Security-Policy"), Some("sandbox"));
    let body = css.into_string().unwrap();
    assert!(body.contains("body{color:red}"));

    match ureq::get(&format!("http://127.0.0.1:{port}/frag/themedir/theme/app.js")).call() {
        Err(ureq::Error::Status(code, r)) => {
            assert_eq!(code, 404);
            assert!(!r.into_string().unwrap().contains("pwned"));
        }
        Ok(r) => panic!("a project may never serve code; got {:?}", r.into_string()),
        Err(e) => panic!("unexpected error: {e:?}"),
    }
}

/// A first version of the theme-directory router dispatch keyed off "does
/// any path segment say theme" rather than "is the last segment a real
/// fragment kind". A project is legitimately multi-segment
/// (`resolve_project` accepts nested rels, see
/// `multi_segment_workspace_url_resolves_the_nested_directory` /
/// `frag_route_resolves_a_nested_projects_fragment_kind`), so a project
/// literally named ".../theme" made that version hijack every one of its
/// ordinary fragments into a theme-asset lookup under its *parent*
/// project instead — the workspace page would render, and every pane
/// would 404. This has to go over real HTTP: a test that reaches
/// `serve_frag`/`serve_project_theme` directly, bypassing `route()`'s
/// dispatch, is exactly what let the dead-route bug and this one both
/// ship green.
#[test]
fn nested_project_named_theme_still_serves_ordinary_fragments_over_http() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("a/theme")).unwrap();
    std::fs::write(d.path().join("a/theme/inner.rs"), "fn main() {}").unwrap();
    let port = start(vec![d.path().to_path_buf()]);

    let body = ureq::get(&format!("http://127.0.0.1:{port}/frag/a/theme/tree"))
        .call()
        .unwrap()
        .into_string()
        .unwrap();
    assert!(body.contains("inner.rs"), "project a/theme's own tree, not a 404: {body}");
}

#[test]
fn diff_traversal_path_is_rejected_with_hint() {
    let (_d, port) = fixture();
    let body = ureq::get(&format!("http://127.0.0.1:{port}/frag/proj/diff?path=../../../etc/passwd"))
        .call().unwrap().into_string().unwrap();
    assert!(body.contains("class=\"hint\""));
    assert!(!body.contains("root:"));
}

#[test]
fn tree_dir_traversal_is_rejected_with_hint_and_leaks_no_listing() {
    // `dir` is network-supplied and must be confined through
    // `safe_resolve` the same way `file`'s `path` is (see routes.rs) —
    // a `dir` that escapes the project must never render a listing.
    let (d, port) = fixture();
    std::fs::write(d.path().join("secret.txt"), "top secret\n").unwrap();
    let base = format!("http://127.0.0.1:{port}");
    // ".." from the project dir resolves to the tempdir root, which holds
    // `secret.txt` alongside `proj` — a real, canonicalizable escape.
    let body = ureq::get(&format!("{base}/frag/proj/tree?dir=.."))
        .call().unwrap().into_string().unwrap();
    assert!(body.contains("class=\"hint\""));
    assert!(!body.contains("secret.txt"));
    assert!(!body.contains("<li"));
}

#[test]
fn tree_dir_lazily_returns_a_subdirectorys_children() {
    let (d, port) = fixture();
    std::fs::create_dir(d.path().join("proj/sub")).unwrap();
    std::fs::write(d.path().join("proj/sub/inner.txt"), "").unwrap();
    let base = format!("http://127.0.0.1:{port}");
    // the root render shows `sub` closed, without its child inlined
    let root = ureq::get(&format!("{base}/frag/proj/tree")).call().unwrap().into_string().unwrap();
    assert!(root.contains("data-rel=\"sub\""));
    assert!(!root.contains("inner.txt"));
    // the lazy fetch for that same directory returns exactly its children
    let sub = ureq::get(&format!("{base}/frag/proj/tree?dir=sub")).call().unwrap().into_string().unwrap();
    assert!(sub.contains("inner.txt"));
}

#[test]
fn tree_dir_with_empty_rel_returns_the_root_listing() {
    // app.js reconciles the root level of the tree in place on every
    // TreeChanged (see refreshTree/reconcileList) instead of re-fetching
    // the whole `tree_fragment`, so a brand-new root-level file doesn't
    // wait for a reload while an existing open subdirectory doesn't
    // collapse. It does that by hitting `dir=` (empty rel) — the same
    // lazy-fetch endpoint a subdirectory expansion uses — which must
    // resolve to the project root itself, not 404 or error.
    let (d, port) = fixture();
    std::fs::create_dir(d.path().join("proj/sub")).unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let root_dir = ureq::get(&format!("{base}/frag/proj/tree?dir=")).call().unwrap().into_string().unwrap();
    assert!(!root_dir.contains("class=\"hint\""));
    assert!(root_dir.contains("data-rel=\"hello.md\""));
    assert!(root_dir.contains("data-rel=\"sub\""));
}

#[test]
fn http_rejects_rebinding_host() {
    use std::io::{Read, Write};
    let (_d, port) = fixture();
    // DNS rebinding: the browser resolves a hostile name to 127.0.0.1, so the
    // page becomes same-origin and CORS no longer protects these reads.
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(b"GET / HTTP/1.1\r\nHost: evil.example.com\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 403"), "got: {}", &resp[..resp.len().min(60)]);
}

/// The `/frag/{project}/proposal` route (added in review, replacing a
/// client-side port of textdiff.rs::unified in static/app.js): a real HTTP
/// fetch against a real open proposal must show the changed hunk, and an id
/// that is not (or no longer) open must render the "no longer open"
/// fragment rather than a 500 or an empty body — a browser fetch racing an
/// answer is the ordinary case, not an edge case (see `Hub::do_answer_proposal`,
/// which closes the tab and answers in that order).
#[test]
fn proposal_fragment_route_shows_the_hunk_and_handles_a_missing_id() {
    let _g = WS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sd = tempfile::tempdir().unwrap();
    std::env::set_var("ROOST_STATE_DIR", sd.path());
    let (_d, port) = fixture_named("proposal-frag");
    // The first connection is what builds the hub this proposal opens on —
    // same reasoning as the other proposal tests above.
    let mut a = ws_connect_path(port, "/ws/proposal-frag/_workspace").unwrap();
    let _ = read_until(&mut a, r#""t":"State""#);

    roost::hub::open_proposal(
        "proposal-frag",
        "frag-1",
        "hello.md",
        "line one
line two
line three
",
        "line one
CHANGED
line three
",
    );

    let body = ureq::get(&format!("http://127.0.0.1:{port}/frag/proposal-frag/proposal?id=frag-1"))
        .call()
        .unwrap()
        .into_string()
        .unwrap();
    assert!(body.contains("hello.md"), "must name the file: {body}");
    assert!(body.contains("-line two") && body.contains("+CHANGED"), "must show the changed hunk: {body}");

    // A withdrawn/answered/unknown id: the fragment must say so, not 500 or
    // silently render nothing.
    let gone = ureq::get(&format!("http://127.0.0.1:{port}/frag/proposal-frag/proposal?id=no-such-id"))
        .call()
        .unwrap()
        .into_string()
        .unwrap();
    assert!(gone.contains("no longer open"), "got {gone}");

    roost::hub::close_proposal("proposal-frag", "frag-1");
    let _ = a.close(None);
    std::env::remove_var("ROOST_STATE_DIR");
}
