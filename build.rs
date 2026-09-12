//! Compiles `static/` into the binary as a sorted path -> bytes table.
//!
//! Generated into OUT_DIR rather than checked in: the table must track the
//! directory exactly, and a checked-in copy would drift the moment someone
//! edits an asset without running the generator.
use std::{env, fs, path::{Path, PathBuf}};

// Shared with the test suite so the channel logic is testable; see the
// comments in that file for why it is not simply a fn in here.
include!("src/channel.rs");

fn main() {
    println!("cargo:rerun-if-changed=static");
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("static");
    let mut files: Vec<(String, String)> = Vec::new();
    walk(&root, &root, &mut files);
    // Sorted so the lookup can binary-search. Sorting tuples orders by the
    // relative path, which is the key.
    files.sort();

    let mut out = String::from("pub static ASSETS: &[(&str, &[u8])] = &[\n");
    for (rel, abs) in &files {
        out.push_str(&format!("    ({rel:?}, include_bytes!({abs:?})),\n"));
    }
    out.push_str("];\n");

    let dest = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("assets_table.rs");
    fs::write(&dest, out).expect("write assets table");

    emit_build_info();
}

/// Records the commit this binary was built from, for the About panel.
///
/// roost is deployed by building it and copying a binary about, and this
/// project has been bitten twice by not knowing which build is in front of it
/// — CLAUDE.md carries both under "Verify, don't trust". Nothing in the binary
/// could answer "is this the thing I built?" except `CARGO_PKG_VERSION`, which
/// changes once a release.
///
/// Every failure here is "unknown", never a build failure and never a
/// plausible-looking empty string: a release tarball has no `.git`, and a
/// build box may have no `git` at all. A version display that can be quietly
/// wrong is worse than one that says it does not know.
fn emit_build_info() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    // Without these the hash goes stale the moment anyone commits without
    // touching `static/` — cargo would see no reason to re-run this script.
    // `.git/HEAD` covers checkouts and commits on a detached HEAD; the ref it
    // names covers an ordinary commit on a branch.
    let git_dir = root.join(".git");
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    if let Ok(head) = fs::read_to_string(git_dir.join("HEAD")) {
        if let Some(r) = head.strip_prefix("ref: ").map(str::trim) {
            println!("cargo:rerun-if-changed={}", git_dir.join(r).display());
        }
    }

    println!("cargo:rustc-env=ROOST_GIT_HASH={}", git_hash(&root));
    let ref_type = env::var("GITHUB_REF_TYPE").ok();
    println!(
        "cargo:rustc-env=ROOST_CHANNEL={}",
        channel_from(ref_type.as_deref(), &root)
    );
    // Or the channel goes stale: a tagged CI build and a local one differ only
    // in the environment, which cargo does not watch by itself.
    println!("cargo:rerun-if-env-changed=GITHUB_REF_TYPE");
    // Seconds since the epoch, formatted by the server rather than here: a
    // build script has no business picking a date format, and the raw number
    // survives being carried through an env var without a parser.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=ROOST_BUILD_EPOCH={secs}");
}

/// The short commit, with `-dirty` when the tree had uncommitted changes.
///
/// The dirty suffix is the part that earns its keep. "Built from a1b2c3d" is
/// false in the common case of a local build with edits in the tree, and this
/// panel exists to be trusted. `-dirty` is the honest form.
fn git_hash(root: &Path) -> String {
    let run = |args: &[&str]| -> Option<std::process::Output> {
        std::process::Command::new("git").args(args).current_dir(root).output().ok()
    };
    // `status.success()`, never `unwrap_or_default` on the bytes: a `git` that
    // ran and failed produces empty stdout, which would otherwise become an
    // empty hash that reads as a real one.
    let Some(out) = run(&["rev-parse", "--short=9", "HEAD"]) else { return "unknown".into() };
    if !out.status.success() {
        return "unknown".into();
    }
    let hash = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if hash.is_empty() {
        return "unknown".into();
    }
    // Three outcomes again: clean, dirty, and could-not-tell. A `git status`
    // that fails says nothing about the tree, so it must not say "clean".
    match run(&["status", "--porcelain", "--untracked-files=no"]) {
        Some(st) if st.status.success() && !st.stdout.is_empty() => format!("{hash}-dirty"),
        Some(st) if st.status.success() => hash,
        _ => format!("{hash}?"),
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    // `.flatten()` here would silently discard any entry that comes back
    // `Err` — an asset omitted from ASSETS with the build still green,
    // exactly the failure Cargo.toml's rationale for embedding says should
    // instead show up as a build break, not a bug report.
    for e in entries {
        let e = e.unwrap_or_else(|e| panic!("read entry in {}: {e}", dir.display()));
        let p = e.path();
        if p.is_dir() {
            walk(root, &p, out);
        } else {
            // Forward slashes: this is a URL path, not a host path.
            let rel = p
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, p.to_string_lossy().into_owned()));
        }
    }
}
