//! Compiles `static/` into the binary as a sorted path -> bytes table.
//!
//! Generated into OUT_DIR rather than checked in: the table must track the
//! directory exactly, and a checked-in copy would drift the moment someone
//! edits an asset without running the generator.
use std::{env, fs, path::{Path, PathBuf}};

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
