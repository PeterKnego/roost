//! Adding a project root, and making a project, from the front page.
//!
//! A root is a directory roost scans for projects; they otherwise come only
//! from `ROOST_ROOTS` or the global config's `roots`. This is the one place a
//! browser may extend that list, behind the same Origin check every
//! shell-spawning socket has.
//!
//! ## One field, two meanings, and why they cannot collide
//!
//! The front page's `+` takes one string. **Absolute means a root; relative
//! means a project.** A relative path has no reading as a root — a root is a
//! place on disk roost scans, and there is nothing for it to be relative to —
//! so the split is a fact about the input rather than a mode the user has to
//! remember.
//!
//! ## This module used to create nothing, and now it does
//!
//! Its doc ended "it never creates, lists or follows into anything", and that
//! was worth writing down: a browser can reach this socket, and the thing at
//! the end of it is now a `mkdir`. Both creating paths are therefore explicit
//! about what stands between the two.
//!
//! **A project (relative) is confined** by `projects::safe_resolve_parent`,
//! the primitive CLAUDE.md names for creation destinations "because the target
//! does not exist yet". Reused rather than reimplemented, which carries its
//! restriction along: the parent must already exist, so `a/b` works when `a`
//! does. One level at a time, by a check this codebase already trusts.
//!
//! **A root (absolute) is confined by nothing, because there is nothing to
//! confine it to.** A root is by definition an arbitrary directory, and
//! someone typing an absolute path into a field labelled *directory to scan
//! for projects* has named it deliberately. What stands in place of a
//! confinement is a confirmation: the client asks before this is called, so
//! the old refusal became a question rather than an action.
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
    add_root_maybe_creating(path, current, env_roots, global, false)
}

/// `add_root`, with the option to create the directory first.
///
/// `create` is set only by the client's second click — the confirmation that
/// replaced the old flat refusal. It is a parameter rather than a behaviour
/// because "add this root" and "make this directory and add it" are different
/// requests, and a caller that has not asked the user must not be able to make
/// the second one by accident.
pub fn add_root_maybe_creating(
    path: &str,
    current: &[PathBuf],
    env_roots: Option<&str>,
    global: &Path,
    create: bool,
) -> Result<Vec<PathBuf>, String> {
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
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Absent — the one outcome the confirmation can act on. "Could not
            // look" deliberately stays a refusal below: creating over
            // something roost cannot stat is how a directory that *is* there
            // gets written into.
            if !create {
                return Err(format!("{}: no such directory", expanded.display()));
            }
            std::fs::create_dir_all(&expanded)
                .map_err(|e| format!("cannot create {}: {e}", expanded.display()))?;
        }
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

/// Creates `rel` as a project under one of `roots`, and `git init`s it.
///
/// Returns the created directory and the project key that names it — which is
/// the path *relative to its root*, not the final segment: a project made as
/// `a/b` is reached at `/a/b` and keyed `a/b`, and `file_name()` would call it
/// `b` and send the browser to a project that does not exist.
///
/// `root` is the root the browser chose, and it
/// is **re-validated against the list roost actually serves** rather than
/// trusted: the dialog that offered it is a hint, not an authorisation — the
/// same rule `RemoveWorktree` and `claudehist::has` state about their own.
/// `None` means "there was only one to choose from", which is checked here
/// rather than assumed.
pub fn new_project(
    rel: &str,
    root: Option<&str>,
    roots: &[PathBuf],
) -> Result<(PathBuf, String), String> {
    let name = rel.trim();
    if name.is_empty() {
        return Err("enter a name for the project".into());
    }
    if roots.is_empty() {
        // The one refusal that is really a redirection: there is nothing for a
        // relative path to be relative *to*, and the fix is the other half of
        // this same field.
        return Err(
            "roost has no project roots yet, so there is nowhere to make this. \
             Enter an absolute path first and roost will add it as a root."
                .into(),
        );
    }
    let base = match root {
        Some(r) => {
            let want = crate::config::expand_home(r);
            roots
                .iter()
                .find(|k| *k == &want)
                .ok_or_else(|| format!("{} is not one of this roost's project roots", want.display()))?
                .clone()
        }
        // Absent is only an answer when there is one root. With several it is
        // a missing choice, and guessing at it is how a folder lands somewhere
        // nobody picked.
        None if roots.len() == 1 => roots[0].clone(),
        None => return Err("choose which project root to make it in".into()),
    };
    // The confinement. `rel` came from a browser and is about to become a
    // directory: `safe_resolve_parent` canonicalises the parent and validates
    // the final component, which is what CLAUDE.md prescribes for a target
    // that does not exist yet.
    let dest = crate::projects::safe_resolve_parent(&base, name)?;
    // Positive evidence before creating, the CLAUDE.md way: `symlink_metadata`
    // rather than `exists()`, because `exists()` follows symlinks and folds
    // "cannot look" into "not there" — and here "not there" means "make a
    // repository in it".
    match std::fs::symlink_metadata(&dest) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot read {}: {e}", dest.display())),
        Ok(_) => {
            // Named, and left alone. "It is already there" is a different
            // sentence from "I made it", and running `git init` over a
            // directory someone already has is a change to a repository this
            // was not asked to touch.
            return Err(format!("{} already exists", dest.display()));
        }
    }
    std::fs::create_dir(&dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
    // `git init`, always: roost's project list *is* the set of git
    // repositories under the roots, so a folder without one would not appear
    // in the list it was created from — the feature would look broken in the
    // most confusing way available.
    //
    // A failure here is reported as what it is and the directory **stays**.
    // Undoing a create by removing a directory is the move CLAUDE.md's table
    // is eleven rows of, and a `git` that would not run says nothing about
    // what is now in that directory.
    crate::gitio::init(&dest).map_err(|e| {
        format!("created {}, but `git init` failed: {e}", dest.display())
    })?;
    // Derived from the canonical parent `safe_resolve_parent` returned, not
    // from the string the browser sent: `./a`, `a/` and `a` are one directory
    // and must be one key.
    let key = dest
        .strip_prefix(base.canonicalize().as_deref().unwrap_or(&base))
        .map(|k| k.to_string_lossy().to_string())
        .unwrap_or_else(|_| name.to_string());
    Ok((dest, key))
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
    AddRoot {
        path: String,
        /// Set by the client's second click: the confirmation that replaced
        /// the old flat "no such directory". Defaulted so a client from
        /// before this existed still parses, and still cannot create.
        #[serde(default)]
        create: bool,
    },
    /// Make a project under a root. `root` is which one; `None` is only an
    /// answer when there is exactly one, and is checked, not assumed.
    NewProject {
        rel: String,
        #[serde(default)]
        root: Option<String>,
    },
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
            Ok(RootsIntent::AddRoot { path, create }) => {
                let current = crate::projects::roots();
                let env = std::env::var("ROOST_ROOTS").ok();
                match add_root_maybe_creating(&path, &current, env.as_deref(), &crate::config::global_config_path(), create) {
                    Ok(list) => serde_json::json!({ "t": "Roots", "roots": list.iter().map(|p| p.display().to_string()).collect::<Vec<_>>() }),
                    // `missing` tells the client whether this refusal is the
                    // one its confirmation can answer. A flag rather than the
                    // client matching on the message: a refusal string is for
                    // a person to read, and a client that branches on its
                    // wording breaks the day the wording improves.
                    //
                    // One extra `symlink_metadata` on a path that has already
                    // failed, and only on the refusal path. Deliberately
                    // re-derived rather than threaded out of `add_root`: it
                    // decides whether to *offer* a question, and the answer to
                    // that question re-checks everything from scratch.
                    Err(msg) => {
                        let missing = !create
                            && matches!(
                                std::fs::symlink_metadata(crate::config::expand_home(path.trim())),
                                Err(ref e) if e.kind() == std::io::ErrorKind::NotFound
                            );
                        serde_json::json!({ "t": "Error", "msg": msg, "missing": missing })
                    }
                }
            }
            Ok(RootsIntent::NewProject { rel, root }) => {
                let roots = crate::projects::roots();
                match new_project(&rel, root.as_deref(), &roots) {
                    // The key is what the URL and every storage path use, and
                    // it is derived here rather than by the client: the client
                    // typed the name, and a name is not a key.
                    Ok((dir, key)) => {
                        serde_json::json!({ "t": "Project", "key": key, "path": dir.display().to_string() })
                    }
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

    // ---- making a project ----

    fn is_repo(d: &Path) -> bool {
        d.join(".git").exists()
    }

    #[test]
    fn a_name_becomes_a_directory_with_a_repository_in_it() {
        // The report, in one test: type a name, get a folder. And `git init`,
        // because roost's project list *is* the git repositories under the
        // roots — a folder without one would not appear in the list it was
        // made from.
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("projects");
        fs::create_dir_all(&root).unwrap();
        let (dir, key) = new_project("mqtt-bridge", None, &[root.clone()]).unwrap();
        assert_eq!(dir, root.canonicalize().unwrap().join("mqtt-bridge"));
        assert_eq!(key, "mqtt-bridge", "the key is what the URL will use");
        assert!(dir.is_dir(), "the directory was not created");
        assert!(is_repo(&dir), "a project roost cannot list is not a project");
    }

    #[test]
    fn a_nested_name_is_keyed_by_its_path_under_the_root_not_its_last_segment() {
        // `file_name()` would call this `sub`, and the browser would then be
        // sent to a project that does not exist. Caught by writing the key
        // derivation twice and asking which one the URL uses.
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("projects");
        fs::create_dir_all(root.join("group")).unwrap();
        let (dir, key) = new_project("group/sub", None, &[root.clone()]).unwrap();
        assert_eq!(key, "group/sub");
        assert!(is_repo(&dir));
    }

    #[test]
    fn the_parent_must_already_exist_and_says_so() {
        // The restriction `safe_resolve_parent` carries, kept deliberately
        // rather than worked around: it canonicalises the parent, which is
        // what makes the confinement below possible at all.
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("projects");
        fs::create_dir_all(&root).unwrap();
        let e = new_project("nope/sub", None, &[root.clone()]).unwrap_err();
        assert!(e.contains("no such directory"), "{e}");
        assert!(!root.join("nope").exists(), "nothing was created on the way to refusing");
    }

    /// **The confinement test, written so that it reaches the confinement.**
    ///
    /// CLAUDE.md records why this matters in its own words: path-confinement
    /// tests "that failed with `ENOENT` before ever reaching the confinement
    /// check — which is why a symlink escape survived review". So the escape
    /// here is *possible*: `escape` really exists outside the root, and the
    /// parent of `../escape/evil` really canonicalises. Only the
    /// `starts_with` check stands between the input and a `mkdir` out there.
    ///
    /// Revert-checked by deleting that check from `safe_resolve_parent`: this
    /// test fails at its `unwrap_err`, because the call *succeeded* — a
    /// directory was created at `<tmp>/escape/evil`, outside every root, from
    /// a string a browser sent. Both halves fail, the plain `..` and the
    /// symlinked parent.
    #[test]
    fn a_relative_path_cannot_escape_its_root() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("projects");
        let outside = d.path().join("escape");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        // The fixture is only a fixture if the escape would otherwise land:
        // the parent exists, so nothing before the confinement can refuse it.
        assert!(root.join("../escape").canonicalize().unwrap() == outside.canonicalize().unwrap(),
                "setup: the escape target must really be reachable");

        let e = new_project("../escape/evil", None, &[root.clone()]).unwrap_err();
        assert!(e.contains("outside project"), "refused for the wrong reason: {e}");
        assert!(!outside.join("evil").exists(), "a directory was created outside the root");

        // A symlinked parent resolves to where it really points and is checked
        // from there — the same escape wearing a different coat.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
            let e = new_project("link/evil", None, &[root.clone()]).unwrap_err();
            assert!(e.contains("outside project"), "a symlinked parent escaped: {e}");
            assert!(!outside.join("evil").exists(), "a directory was created through the symlink");
        }
    }

    #[test]
    fn a_directory_that_is_already_there_is_named_and_left_alone() {
        // "It is already there" is a different sentence from "I made it", and
        // a `git init` over someone's existing directory is a change to a
        // repository this feature was not asked to touch. So the assertion is
        // on the directory's *contents*, not merely on the refusal.
        //
        // Revert-checked by running `git init` before returning the refusal:
        // the refusal is still returned, still says "already exists", and this
        // test still goes red — on `.git` appearing in a directory that was
        // supposed to be left alone. An assertion on the error string alone
        // would have stayed green.
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("projects");
        let mine = root.join("mine");
        fs::create_dir_all(&mine).unwrap();
        fs::write(mine.join("notes.txt"), "already here\n").unwrap();

        let e = new_project("mine", None, &[root.clone()]).unwrap_err();
        assert!(e.contains("already exists"), "{e}");
        assert!(!is_repo(&mine), "an existing directory was git initialised");
        assert_eq!(fs::read_to_string(mine.join("notes.txt")).unwrap(), "already here\n");
    }

    #[test]
    fn with_several_roots_the_chosen_one_is_used_and_an_unchosen_one_is_refused() {
        // With one root a server that ignored `root` entirely would pass every
        // other test in this file. This is the test that makes the field mean
        // something, so it asserts the project landed under the **second**
        // root — the one a version that always took `roots[0]` would miss.
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        let b = d.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let roots = vec![a.clone(), b.clone()];

        let (dir, _) = new_project("here", Some(b.to_str().unwrap()), &roots).unwrap();
        assert!(dir.starts_with(b.canonicalize().unwrap()), "landed in {dir:?}, not under {b:?}");
        assert!(!a.join("here").exists(), "it was also made under the root nobody chose");

        // A root the browser invented is refused, not adopted. The dialog that
        // offered it is a hint, not an authorisation.
        let e = new_project("x", Some(d.path().join("elsewhere").to_str().unwrap()), &roots).unwrap_err();
        assert!(e.contains("not one of this roost's project roots"), "{e}");

        // And with several roots, naming none is a missing answer rather than
        // a licence to pick.
        let e = new_project("x", None, &roots).unwrap_err();
        assert!(e.contains("choose which"), "{e}");
        assert!(!a.join("x").exists() && !b.join("x").exists(), "one was picked anyway");
    }

    #[test]
    fn with_no_roots_at_all_the_refusal_points_at_the_other_half_of_the_field() {
        // There is nothing for a relative path to be relative to, and the fix
        // is the same `+`: type an absolute path and it becomes a root.
        let e = new_project("anything", None, &[]).unwrap_err();
        assert!(e.contains("no project roots yet"), "{e}");
        assert!(e.contains("absolute path"), "the refusal must say what to do instead: {e}");
    }

    #[test]
    fn an_empty_name_is_refused_before_anything_is_touched() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("projects");
        fs::create_dir_all(&root).unwrap();
        for bad in ["", "   ", "/", "."] {
            assert!(new_project(bad, None, &[root.clone()]).is_err(), "accepted {bad:?}");
        }
    }

    // ---- creating a root ----

    #[test]
    fn a_missing_root_is_refused_without_the_confirmation_and_created_with_it() {
        // The confirmation is the whole of what stands where a confinement
        // cannot: a root is by definition an arbitrary directory, so the old
        // flat refusal became a question rather than an action. Both halves
        // are asserted, because only the pair shows the flag does anything.
        let d = tempfile::tempdir().unwrap();
        let want = d.path().join("brand/new");

        let e = add_root(want.to_str().unwrap(), &[], None, &global(&d)).unwrap_err();
        assert!(e.contains("no such directory"), "{e}");
        assert!(!want.exists(), "the unconfirmed call created it anyway");

        let list = add_root_maybe_creating(want.to_str().unwrap(), &[], None, &global(&d), true).unwrap();
        assert!(want.is_dir(), "the confirmed call did not create it");
        assert_eq!(list.len(), 1);
        assert!(fs::read_to_string(global(&d)).unwrap().contains("brand/new"), "and it was written to the config");
    }

    #[test]
    fn a_path_roost_cannot_stat_is_not_created_over_even_with_the_confirmation() {
        // `create` acts on *absent* only. "Cannot look" stays a refusal: a
        // create over something roost could not stat is how a directory that
        // really is there gets written into — the distinction CLAUDE.md's
        // whole table is about.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let d = tempfile::tempdir().unwrap();
            let locked = d.path().join("locked");
            fs::create_dir_all(&locked).unwrap();
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
            let target = locked.join("inside");
            let got = add_root_maybe_creating(target.to_str().unwrap(), &[], None, &global(&d), true);
            let readable = fs::read_dir(&locked).is_ok();
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            // Skipped rather than inverted when running as root, which can
            // stat through a 0000 directory: a test that quietly passes as
            // root is the "passes for the wrong reason" class by another name.
            if !readable {
                let e = got.unwrap_err();
                assert!(e.contains("cannot read") || e.contains("cannot create"), "{e}");
            }
        }
    }
}
