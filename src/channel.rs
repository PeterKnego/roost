// How a build decides which channel produced it — shared by `build.rs` and
// the test suite, which is the whole reason it is a file of its own.
//
// A build script is not compiled into the crate, so nothing in `cargo test`
// can reach a function that lives only in `build.rs`. This one decides which
// upgrade command a user is eventually shown, and the `cargo` arm cannot be
// exercised any other way short of publishing a release — so the logic is
// pure, takes its inputs as arguments, and is `include!`d by both.
//
// Order is load-bearing and the reason this is not three independent tests: a
// tagged CI build happens *inside* a git checkout, so the tag must be tested
// before `.git` or every published artifact would report `checkout`.
#[allow(dead_code)]
fn channel_from(ref_type: Option<&str>, root: &std::path::Path) -> &'static str {
    // Every published artifact comes from a tag build of release.yml.
    if ref_type == Some("tag") {
        return "release";
    }
    // `cargo install` compiles an unpacked crate out of the registry cache.
    // Both components are required: a project of one's own called `registry`
    // should not read as a crates.io install.
    let has = |name: &str| root.components().any(|c| c.as_os_str() == name);
    if has("registry") && has("src") {
        return "cargo";
    }
    // A working tree — which includes `cargo install --git`, since that clones
    // first. Both mean "you built this", and both have the same answer for
    // upgrading: not roost's business.
    if root.join(".git").exists() {
        return "checkout";
    }
    // A source tarball with neither. Says so rather than picking the likeliest.
    "unknown"
}
