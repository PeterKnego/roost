/// The base64 line of a minisign `.pub` file, or `None`. The file is one
/// `untrusted comment:` line and one base64 line; only the second is the
/// key, so a comment edit must not change what a binary trusts.
///
/// Shared with `build.rs` through `include!`, like `channel.rs`: the build
/// script bakes the line, and `update.rs` tests the rule. `update.rs`'s own
/// production code never calls it — only `build.rs`'s separate compilation
/// and the test module do — so a non-test build of the lib sees it as dead,
/// same as `channel_from` in `channel.rs`.
#[allow(dead_code)]
fn pubkey_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
        .filter(|l| l.len() >= 40 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='))
        .map(str::to_string)
}
