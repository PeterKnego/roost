//! Adding a project root from the front page.
//!
//! A root is a directory roost scans for projects; today they come only from
//! `ROOST_ROOTS` or the global config's `roots`. This is the one place a
//! browser may extend that list, behind the same Origin check every
//! shell-spawning socket has. Validation reads metadata and canonicalises;
//! it never creates, lists or follows into anything — the scan that follows
//! is the existing one.
use std::path::{Path, PathBuf};

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
    let mut list: Vec<String> = match crate::config::raw_setting(global, "roots") {
        Some(SettingValue::List(l)) => l,
        _ => Vec::new(),
    };
    list.push(canon.display().to_string());
    crate::config::with_global_write_lock(|| crate::config::write_setting(global, "roots", Some(&SettingValue::List(list))))?;
    let mut out = current.to_vec();
    out.push(canon);
    Ok(out)
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
        // `~/` expands against HOME.
        let home = d.path().join("home");
        fs::create_dir_all(home.join("work")).unwrap();
        std::env::set_var("HOME", &home);
        let list = add_root("~/work", &[canon.clone()], None, &global(&d)).unwrap();
        assert_eq!(list.last().unwrap(), &home.join("work").canonicalize().unwrap());
    }
    // Revert-check (recorded, not reproduced here): swapping `canon` for
    // `expanded` in `list.push(...)` — the entry written to the file — did
    // NOT fail this test on this host. Rust's `Path` equality already
    // discards a mid-path "." component (confirmed: `PathBuf::from("/tmp/foo")
    // == PathBuf::from("/tmp/./foo")`), and this host's temp directory is not
    // itself a symlink, so `expanded` and `canon` agree here even without a
    // real `canonicalize()`. The substitution would surface on a host where
    // the temp root is a symlink (e.g. macOS's `/tmp` -> `/private/tmp`),
    // where only `canonicalize()` resolves it. A separate ad hoc probe with a
    // real symlink (not checked in) confirmed canonicalize is load-bearing:
    // without it, a symlink alias of an existing root is not caught as a
    // duplicate.

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
