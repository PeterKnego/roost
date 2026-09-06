//! Adding a project root from the front page.
//!
//! A root is a directory roost scans for projects; today they come only from
//! `ROOST_ROOTS` or the global config's `roots`. This is the one place a
//! browser may extend that list, behind the same Origin check every
//! shell-spawning socket has. Validation reads metadata and canonicalises;
//! it never creates, lists or follows into anything — the scan that follows
//! is the existing one.
use std::net::TcpStream;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tungstenite::handshake::server::{Request as WsRequest, Response as WsResponse};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::Message;

use crate::proto::SettingValue;

/// Validate `path` and append it to `global`'s `roots`. Returns the new list.
///
/// `current` is the root list in effect (so a duplicate is caught against
/// what the server actually serves), `env_roots` is `ROOST_ROOTS` if set.
pub fn add_root(path: &str, current: &[PathBuf], env_roots: Option<&str>, global: &Path) -> Result<Vec<PathBuf>, String> {
    let raw = path.trim();
    if raw.is_empty() {
        return Err("enter a directory path".into());
    }
    let expanded = crate::config::expand_home(raw);
    if !expanded.is_absolute() {
        return Err(format!("`{raw}` is not an absolute path"));
    }
    // Three outcomes, not two: absent, present, and *could not look*. The
    // last is never folded into the first — see CLAUDE.md.
    match std::fs::symlink_metadata(&expanded) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(format!("{}: no such directory", expanded.display())),
        Err(e) => return Err(format!("cannot read {}: {e}", expanded.display())),
        Ok(_) => {}
    }
    // A symlink to a directory is fine as a root; `metadata` follows it for
    // this one question.
    match std::fs::metadata(&expanded) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(format!("{}: not a directory", expanded.display())),
        Err(e) => return Err(format!("cannot read {}: {e}", expanded.display())),
    }
    let canon = expanded.canonicalize().map_err(|e| format!("cannot resolve {}: {e}", expanded.display()))?;
    let already = current.iter().any(|r| r.canonicalize().map(|c| c == canon).unwrap_or(false));
    if already {
        return Err(format!("{} is already a root", canon.display()));
    }
    // The environment wins over the file for every read; writing the file
    // would change nothing visible and read as a silent failure.
    if env_roots.map(|v| !v.trim().is_empty()).unwrap_or(false) {
        return Err("this roost's roots come from ROOST_ROOTS; add it there (the unit file, for a service)".into());
    }
    // The list is stored as TOML strings, so a path that is not UTF-8 cannot
    // be written at all. Caught here, before the write, rather than losing
    // the bytes silently through a lossy `display()`.
    let Some(canon_str) = canon.to_str().map(str::to_string) else {
        return Err(format!("{}: not valid UTF-8; roost cannot store it", canon.display()));
    };
    // Read-modify-write, all under one lock acquisition: reading the list
    // outside the lock would let two concurrent adds each read the
    // pre-write list, and the second writer's write would then drop the
    // first writer's entry.
    crate::config::with_global_write_lock(|| {
        let mut list = current_roots_in(global)?;
        list.push(canon_str);
        crate::config::write_setting(global, "roots", Some(&SettingValue::List(list)))
    })?;
    let mut out = current.to_vec();
    out.push(canon);
    Ok(out)
}

/// The `roots` list as the file actually spells it.
///
/// Read here rather than through `config::raw_setting`, which answers a
/// different question: it is lenient by design (a non-list is `None`, a list
/// with a non-string entry silently loses that entry), and appending to
/// *that* answer would rewrite a hand-edited value out of the user's file.
/// Absent is an empty list; anything that is not an array of strings is a
/// refusal, and the file is left alone — same rule as every other config
/// write, and the same rule as CLAUDE.md's: "I could not read it" is not
/// "it is empty".
fn current_roots_in(global: &Path) -> Result<Vec<String>, String> {
    let text = match std::fs::read_to_string(global) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", global.display())),
    };
    let doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{}: {e}", global.display()))?;
    let Some(item) = doc.get("roots") else { return Ok(Vec::new()) };
    let found = match item {
        toml_edit::Item::None => return Ok(Vec::new()),
        toml_edit::Item::Table(_) => "a table",
        toml_edit::Item::ArrayOfTables(_) => "an array of tables",
        toml_edit::Item::Value(toml_edit::Value::Array(a)) => {
            let mut out = Vec::with_capacity(a.len());
            for entry in a.iter() {
                let Some(s) = entry.as_str() else {
                    return Err(not_a_list(global, "an array with a non-string entry"));
                };
                out.push(s.to_string());
            }
            return Ok(out);
        }
        toml_edit::Item::Value(toml_edit::Value::String(_)) => "a string",
        toml_edit::Item::Value(toml_edit::Value::Integer(_) | toml_edit::Value::Float(_)) => "a number",
        toml_edit::Item::Value(toml_edit::Value::Boolean(_)) => "a boolean",
        toml_edit::Item::Value(toml_edit::Value::Datetime(_)) => "a date",
        toml_edit::Item::Value(toml_edit::Value::InlineTable(_)) => "a table",
    };
    Err(not_a_list(global, found))
}

fn not_a_list(global: &Path, found: &str) -> String {
    format!("roots in {} is not a list of strings (found {found}); fix it by hand", global.display())
}

#[derive(Debug, Deserialize)]
#[serde(tag = "t")]
enum RootsIntent {
    AddRoot { path: String },
}

/// `/ws/_roots`: the front page's one write. One exchange per connection.
pub fn handle_ws(stream: TcpStream) {
    // One exchange, then closed: a client that completes the handshake and
    // then sends nothing must not pin this thread for the process's life.
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(15)));
    // WebSocket handshakes bypass the same-origin policy: without this check
    // any page the user visits could extend the directories roost serves.
    // Same check as wsconn.rs and term.rs.
    let allowed = crate::config::allowed_origins();
    let config = WebSocketConfig { max_message_size: Some(64 * 1024), ..Default::default() };
    let accepted = tungstenite::accept_hdr_with_config(
        stream,
        |req: &WsRequest, resp: WsResponse| {
            let origin = req.headers().get("origin").and_then(|v| v.to_str().ok());
            if !crate::origin::origin_allowed(origin, &allowed) {
                eprintln!("roost: rejected roots ws origin={origin:?} (set allowed_origins)");
                return Err(tungstenite::http::Response::builder().status(403).body(Some("origin not allowed".to_string())).expect("static 403 response"));
            }
            Ok(resp)
        },
        Some(config),
    );
    let Ok(mut ws) = accepted else { return };
    let reply = match ws.read() {
        Ok(Message::Text(t)) => match serde_json::from_str::<RootsIntent>(&t) {
            Ok(RootsIntent::AddRoot { path }) => {
                let current = crate::projects::roots();
                let env = std::env::var("ROOST_ROOTS").ok();
                match add_root(&path, &current, env.as_deref(), &crate::config::global_config_path()) {
                    Ok(list) => serde_json::json!({ "t": "Roots", "roots": list.iter().map(|p| p.display().to_string()).collect::<Vec<_>>() }),
                    Err(msg) => serde_json::json!({ "t": "Error", "msg": msg }),
                }
            }
            Err(e) => serde_json::json!({ "t": "Error", "msg": format!("bad intent: {e}") }),
        },
        _ => return,
    };
    let _ = ws.send(Message::Text(reply.to_string().into()));
    let _ = ws.close(None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn global(d: &tempfile::TempDir) -> std::path::PathBuf { d.path().join("config.toml") }

    #[test]
    fn a_relative_path_is_refused_by_name() {
        let d = tempfile::tempdir().unwrap();
        let e = add_root("projects", &[], None, &global(&d)).unwrap_err();
        assert!(e.contains("projects") && e.contains("absolute"), "{e}");
        assert!(!global(&d).exists(), "nothing written");
        let e = add_root("  ", &[], None, &global(&d)).unwrap_err();
        assert!(e.contains("enter a directory path"), "{e}");
    }

    #[test]
    fn missing_and_unreadable_and_file_are_three_different_refusals() {
        let d = tempfile::tempdir().unwrap();
        let missing = d.path().join("nope");
        let e = add_root(missing.to_str().unwrap(), &[], None, &global(&d)).unwrap_err();
        assert!(e.contains("no such directory"), "{e}");
        let file = d.path().join("f.txt");
        fs::write(&file, "x").unwrap();
        let e = add_root(file.to_str().unwrap(), &[], None, &global(&d)).unwrap_err();
        assert!(e.contains("not a directory"), "{e}");
        // Unreadable: a directory we may not search. Skipped as root, who can.
        #[cfg(unix)]
        if !nix_is_root() {
            use std::os::unix::fs::PermissionsExt;
            let locked = d.path().join("locked");
            fs::create_dir(&locked).unwrap();
            let inner = locked.join("inner");
            fs::create_dir(&inner).unwrap();
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
            let e = add_root(inner.to_str().unwrap(), &[], None, &global(&d)).unwrap_err();
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            // Revert-check: mapping every metadata error to "no such
            // directory" fails here — "cannot look" must stay distinct.
            assert!(e.contains("cannot read"), "{e}");
        }
        assert!(!global(&d).exists(), "nothing written by a refusal");
    }

    #[test]
    fn a_duplicate_is_detected_canonically_and_tilde_expands() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("projects");
        fs::create_dir(&dir).unwrap();
        let canon = dir.canonicalize().unwrap();
        // First add writes the canonical path.
        let list = add_root(dir.to_str().unwrap(), &[], None, &global(&d)).unwrap();
        assert_eq!(list, vec![canon.clone()]);
        let text = fs::read_to_string(global(&d)).unwrap();
        assert!(text.contains(&format!("roots = [\"{}\"]", canon.display())), "{text}");
        // Same directory spelled with a dot segment: refused as already present.
        let spelled = format!("{}/./projects", d.path().display());
        let e = add_root(&spelled, &[canon.clone()], None, &global(&d)).unwrap_err();
        assert!(e.contains("already a root"), "{e}");
        // `~/` expands against HOME. `set_var` is process-wide and every
        // test in this binary shares that process, so this both takes the
        // lock the other env-setting tests take and puts HOME back on the
        // way out — an escaped tempdir HOME sends any later test that reads
        // it (or expands a `~`) at a directory that no longer exists.
        let _envg = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home_guard = HomeGuard(std::env::var_os("HOME"));
        let home = d.path().join("home");
        fs::create_dir_all(home.join("work")).unwrap();
        std::env::set_var("HOME", &home);
        let list = add_root("~/work", &[canon.clone()], None, &global(&d)).unwrap();
        assert_eq!(list.last().unwrap(), &home.join("work").canonicalize().unwrap());
    }

    /// Restores `HOME` however the test leaves — including on a panicking
    /// assertion, which is exactly when an early `set_var` back would be
    /// skipped.
    struct HomeGuard(Option<std::ffi::OsString>);
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
    // A `./` dot segment is not a strong enough fixture to prove
    // `canonicalize()` is doing anything: Rust's own `Path` equality already
    // discards a mid-path "." component, so this test's duplicate check would
    // pass even comparing un-canonicalized paths. The real case — a symlink
    // alias, which only `canonicalize()` resolves — is covered separately by
    // `a_symlinked_alias_of_a_root_is_the_same_root` below.

    #[cfg(unix)]
    #[test]
    fn a_symlinked_alias_of_a_root_is_the_same_root() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real");
        fs::create_dir(&real).unwrap();
        let canon = real.canonicalize().unwrap();
        let alias = d.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        // The real path first, then its alias is refused as the same root.
        let list = add_root(real.to_str().unwrap(), &[], None, &global(&d)).unwrap();
        assert_eq!(list, vec![canon.clone()]);
        let e = add_root(alias.to_str().unwrap(), &[canon.clone()], None, &global(&d)).unwrap_err();
        assert!(e.contains("already a root"), "{e}");

        // The alias added first (fresh global file): the entry written is the
        // resolved real path, never the alias's own spelling.
        let global2 = d.path().join("config2.toml");
        let list = add_root(alias.to_str().unwrap(), &[], None, &global2).unwrap();
        assert_eq!(list, vec![canon.clone()]);
        let text = fs::read_to_string(&global2).unwrap();
        assert!(text.contains(&real.canonicalize().unwrap().display().to_string()), "{text}");
        assert!(!text.contains("alias"), "the alias's own path must not appear: {text}");
    }
    // Revert-check: replacing `canon` with `expanded` in both the duplicate
    // check and the written entry (`add_root`'s body) made this test fail —
    // the alias-second case returned `Ok` instead of `Err` (an
    // un-canonicalized alias path never equals the canonical real path, so
    // "already a root" never fires), so `.unwrap_err()` panicked on `Ok(..)`;
    // separately the alias-first case wrote the alias's own path, and
    // `assert!(!text.contains("alias"))` failed. Restored.

    #[test]
    fn appending_keeps_the_existing_list_comments_and_other_keys() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a"); let b = d.path().join("b");
        fs::create_dir(&a).unwrap(); fs::create_dir(&b).unwrap();
        let before = format!("# mine\ntheme = \"dark\"\nroots = [\"{}\"] # first\n", a.display());
        fs::write(global(&d), &before).unwrap();
        let list = add_root(b.to_str().unwrap(), &[a.clone()], None, &global(&d)).unwrap();
        assert_eq!(list, vec![a.clone(), b.canonicalize().unwrap()]);
        let after = fs::read_to_string(global(&d)).unwrap();
        assert!(after.starts_with("# mine\ntheme = \"dark\"\n"), "{after}");
        assert!(after.contains(&a.display().to_string()) && after.contains(&b.canonicalize().unwrap().display().to_string()), "{after}");
        assert!(after.contains("# first"), "the inline comment survives: {after}");
    }

    #[test]
    fn a_roots_value_that_is_not_a_list_of_strings_is_refused_untouched() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("p");
        fs::create_dir(&dir).unwrap();
        // A hand-edited file where `roots` is a bare string. Appending to it
        // means first deciding what "it" is, and the only honest answer is
        // "not a list" — the file is the user's, and a value roost cannot
        // read is not a value roost may replace.
        for (value, found) in [("\"/x\"", "a string"), ("[\"/a\", 5]", "an array with a non-string entry")] {
            let before = format!("# mine\ntheme = \"dark\"\nroots = {value}\n");
            fs::write(global(&d), &before).unwrap();
            let e = add_root(dir.to_str().unwrap(), &[], None, &global(&d)).unwrap_err();
            assert!(e.contains("not a list of strings"), "{e}");
            assert!(e.contains(found), "names what it found: {e}");
            assert_eq!(fs::read_to_string(global(&d)).unwrap(), before, "the file is untouched");
        }
    }
    // Revert-checked: putting `raw_setting`'s lenient read back (the
    // `_ => Vec::new()` arm) makes both cases fail — `add_root` returns
    // `Ok(["/tmp/.../p"])` where `unwrap_err()` panics, because the string
    // and the mixed array are both read as "no roots yet" and the
    // hand-edited value is overwritten. Restored.

    #[test]
    fn roost_roots_in_the_environment_refuses_the_write() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("p"); fs::create_dir(&dir).unwrap();
        let e = add_root(dir.to_str().unwrap(), &[], Some("/somewhere"), &global(&d)).unwrap_err();
        assert!(e.contains("ROOST_ROOTS"), "{e}");
        assert!(!global(&d).exists());
        // Set but empty is "unset": `roots_from` ignores it, so the file rules.
        assert!(add_root(dir.to_str().unwrap(), &[], Some(""), &global(&d)).is_ok());
    }

    #[cfg(unix)]
    fn nix_is_root() -> bool {
        std::fs::metadata("/proc/self").map(|m| { use std::os::unix::fs::MetadataExt; m.uid() == 0 }).unwrap_or(false)
    }
}
