//! Updating roost from the button. See
//! `docs/superpowers/specs/2026-09-13-self-update-design.md`.

include!("pubkey.rs");

/// The download cap. The musl tarball is 1.4 MB; a server answering with
/// gigabytes is one to walk away from, and the check happens twice — against
/// `Content-Length` before the body is buffered, and while reading it.
pub const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
/// The `--version` probe's bound. The macOS lesson from #65: a hang is a
/// fourth outcome that exit status cannot see.
pub const PROBE_SECS: u64 = 5;
/// How long `Later` keeps the dialog from opening by itself. The mark stays.
pub const DEFER_SECS: u64 = 24 * 60 * 60;

/// The public key `build.rs` baked from `keys/roost.pub`, or `None` for a
/// build made without one — which offers nothing, rather than trusting
/// anything.
pub fn public_key() -> Option<&'static str> {
    match option_env!("ROOST_UPDATE_PUBKEY") {
        Some(k) if !k.is_empty() => Some(k),
        _ => None,
    }
}

/// `<repository>/releases/download/v<version>`, the directory dist hosts a
/// release's artifacts under. Derived from `BuildInfo.repository`, never
/// spelled and never configured: a setting that let a repository name the
/// host roost fetches a binary from would be the hole the version-check spec
/// exists to avoid.
pub fn release_base(repository: &str, version: &str) -> String {
    format!("{}/releases/download/v{version}", repository.trim_end_matches('/'))
}

/// `release_base`, unless `ROOST_UPDATE_BASE` names another for this process.
/// Environment only, never config, and a test hook rather than a feature:
/// the manual run in this plan's last task serves a locally signed tarball
/// from it. It is not a hole because the signature is the boundary, not the
/// URL — a redirected download without the private key fails at `verify`
/// and the executable is untouched.
pub fn download_base(repository: &str, version: &str) -> String {
    match std::env::var("ROOST_UPDATE_BASE") {
        Ok(b) if !b.is_empty() => b.trim_end_matches('/').to_string(),
        _ => release_base(repository, version),
    }
}

/// The tarball and its detached signature, named the way dist names them
/// (`roost-<target>.tar.xz`) and the way `minisign -S` names its output.
pub fn asset_urls(base: &str, target: &str) -> (String, String) {
    let tarball = format!("{base}/roost-{target}.tar.xz");
    let sig = format!("{tarball}.minisig");
    (tarball, sig)
}

/// Verified over the tarball bytes, **before** decompression touches them:
/// nothing unsigned is parsed. `allow_legacy` is false — the release signs
/// with a current minisign, and the legacy format is one more thing to
/// accept for no reason. The three messages are distinct on purpose: a
/// signature that does not verify is the one someone should hear about.
pub fn verify(tarball: &[u8], minisig: &str, pubkey_b64: &str) -> Result<(), String> {
    let pk = minisign_verify::PublicKey::from_base64(pubkey_b64)
        .map_err(|e| format!("the compiled-in key could not be read: {e}"))?;
    let sig = minisign_verify::Signature::decode(minisig)
        .map_err(|e| format!("the signature file could not be read: {e}"))?;
    pk.verify(tarball, &sig, false)
        .map_err(|_| "signature did not verify".to_string())
}

/// Bounds the *cumulative* decompressed output `xz_decompress` writes across
/// the XZ blocks that make up one stream — `lzma_rs`'s XZ entry point takes no
/// size limit of its own (that only exists on the raw LZMA API's
/// `Options::memlimit`, which the XZ container format doesn't expose), so
/// this is the only bound between block N+1 and the running total.
///
/// It does **not** bound a single block: `lzma_rs` 0.3.0's `read_block`
/// (`src/decode/xz.rs:196-284`) decodes one whole block into a local,
/// unbounded `tmpbuf` and only calls `output.write_all(tmpbuf)` once that
/// block is fully decoded (`xz.rs:282`) — so a one-block bomb is fully
/// materialized in memory before `Capped::write` ever runs, no matter how
/// small `cap` is. That gap is accepted here rather than worked around: this
/// function only ever runs on bytes that already passed `verify` against the
/// compiled-in key (the pipeline enforces that order), so producing a
/// tarball that exploits it requires the release signing key. The bounds
/// that do apply regardless of the key are the 64 MB download cap enforced
/// while the tarball is fetched (`MAX_ARCHIVE_BYTES`, checked against
/// `Content-Length` and again while reading the body) and, below, the
/// per-member `entry.size()` check against the same constant.
struct Capped<'a> {
    buf: &'a mut Vec<u8>,
    cap: u64,
}

impl std::io::Write for Capped<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.buf.len() as u64 + data.len() as u64 > self.cap {
            return Err(std::io::Error::other("decompressed archive exceeds the size cap"));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The one binary out of a release tarball. dist writes `<target>/roost`
/// beside a README and two licences, so the rule is *exactly one regular
/// member whose file name is `roost`*, at any depth. A symlink there would
/// be followed by nothing here but is refused anyway: the file that gets
/// probed and swapped has to be the bytes that were signed. Nothing here
/// ever touches disk — the result is bytes in memory, staged and swapped by
/// a later step — so there is no path to confine and no traversal to guard
/// against; the only hostile shapes that matter at this stage are ones that
/// would make this function allocate or return the wrong bytes.
pub fn extract_binary(tar_xz: &[u8]) -> Result<Vec<u8>, String> {
    let mut tar = Vec::new();
    lzma_rs::xz_decompress(&mut &tar_xz[..], &mut Capped { buf: &mut tar, cap: MAX_ARCHIVE_BYTES })
        .map_err(|e| format!("the archive could not be decompressed: {e:?}"))?;
    let mut archive = tar::Archive::new(&tar[..]);
    let entries = archive.entries().map_err(|e| format!("the archive could not be read: {e}"))?;
    let mut found: Option<Vec<u8>> = None;
    let mut count = 0usize;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("the archive could not be read: {e}"))?;
        let is_roost = entry
            .path()
            .ok()
            .and_then(|p| p.file_name().map(|n| n == "roost"))
            .unwrap_or(false);
        if !is_roost {
            continue;
        }
        count += 1;
        match entry.header().entry_type() {
            tar::EntryType::Regular | tar::EntryType::Continuous => {}
            tar::EntryType::Directory => return Err("the roost member is a directory".into()),
            tar::EntryType::Symlink | tar::EntryType::Link => return Err("the roost member is a symlink".into()),
            other => return Err(format!("the roost member is not a regular file ({other:?})")),
        }
        if entry.size() > MAX_ARCHIVE_BYTES {
            return Err("the roost member is larger than the download cap".into());
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        std::io::Read::read_to_end(&mut entry, &mut bytes)
            .map_err(|e| format!("the roost member could not be read: {e}"))?;
        found = Some(bytes);
    }
    match (count, found) {
        (0, _) => Err("the archive has no roost member".into()),
        (1, Some(b)) => Ok(b),
        (n, _) => Err(format!("the archive has {n} roost members")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two URLs, from the three facts the binary already carries. The
    /// tag is `v<version>`, as `gh release list` shows for every release.
    #[test]
    fn the_asset_urls_are_derived_from_repository_version_and_target() {
        let base = release_base("https://github.com/PeterKnego/roost", "0.5.3");
        assert_eq!(base, "https://github.com/PeterKnego/roost/releases/download/v0.5.3");
        let (tarball, sig) = asset_urls(&base, "x86_64-unknown-linux-musl");
        assert_eq!(tarball, "https://github.com/PeterKnego/roost/releases/download/v0.5.3/roost-x86_64-unknown-linux-musl.tar.xz");
        assert_eq!(sig, format!("{tarball}.minisig"));
        // A trailing slash on the repository must not double up.
        assert_eq!(release_base("https://github.com/PeterKnego/roost/", "0.5.3"), base);
    }

    /// The test hook, and its limit: it replaces the base for this process
    /// and nothing else. The signature is the boundary, not the URL.
    #[test]
    fn the_base_can_be_overridden_by_env_for_a_manual_run() {
        let _g = crate::config::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_UPDATE_BASE", "http://127.0.0.1:8999/");
        assert_eq!(download_base("https://github.com/PeterKnego/roost", "9.9.9"), "http://127.0.0.1:8999");
        std::env::remove_var("ROOST_UPDATE_BASE");
        assert_eq!(download_base("https://github.com/PeterKnego/roost", "9.9.9"),
            "https://github.com/PeterKnego/roost/releases/download/v9.9.9");
    }

    /// A `.pub` file is an untrusted-comment line and a base64 line; only the
    /// second is the key. `build.rs` bakes that line, so a comment change on
    /// the file must not change the binary's trust.
    #[test]
    fn the_pub_file_s_second_line_is_the_key() {
        let text = "untrusted comment: minisign public key 1234ABCD\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\n";
        assert_eq!(pubkey_line(text).as_deref(), Some("RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"));
        assert_eq!(pubkey_line("untrusted comment: nothing else\n"), None, "a comment alone is no key");
        assert_eq!(pubkey_line(""), None);
        assert_eq!(pubkey_line("RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"), Some("RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3".into()),
            "a bare base64 line is accepted too");
        assert_eq!(pubkey_line("not base64 at all !!\n"), None);
    }

    /// Whatever `build.rs` baked either decodes as a minisign key or is
    /// absent. Absent is what this checkout has until #87 commits the key,
    /// and absent must never read as "trust anything".
    #[test]
    fn the_compiled_in_key_is_absent_or_a_real_key() {
        match public_key() {
            None => {}
            Some(k) => {
                minisign_verify::PublicKey::from_base64(k).expect("keys/roost.pub holds a minisign key");
            }
        }
    }

    fn test_key() -> (String, minisign::SecretKey) {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        (kp.pk.to_base64(), kp.sk)
    }

    fn sign_bytes(sk: &minisign::SecretKey, data: &[u8]) -> String {
        minisign::sign(None, sk, data, Some("roost test signature"), None).unwrap().into_string()
    }

    /// Revert-checked: a `verify` that decodes both inputs and returns `Ok`
    /// without calling `pk.verify` fails here at the flipped-byte row with
    /// `called Result::unwrap_err() on an Ok value`.
    ///
    /// A valid signature passes; a flipped byte, another key, and garbage
    /// each fail with their own message — "signature did not verify" is the
    /// one someone should hear about, and it must not be confused with a
    /// signature file that could not be read.
    #[test]
    fn the_signature_is_checked_over_the_tarball_bytes() {
        let (pk, sk) = test_key();
        let data = b"not really a tarball, but the bytes are what is signed".to_vec();
        let sig = sign_bytes(&sk, &data);
        assert_eq!(verify(&data, &sig, &pk), Ok(()));

        let mut flipped = data.clone();
        flipped[7] ^= 0x01;
        assert_eq!(verify(&flipped, &sig, &pk).unwrap_err(), "signature did not verify");

        let (other_pk, _) = test_key();
        assert_eq!(verify(&data, &sig, &other_pk).unwrap_err(), "signature did not verify");

        let e = verify(&data, "this is not a minisig file", &pk).unwrap_err();
        assert!(e.starts_with("the signature file could not be read"), "{e}");

        let e = verify(&data, &sig, "not a key").unwrap_err();
        assert!(e.starts_with("the compiled-in key could not be read"), "{e}");
    }

    /// Three more refusals the row above doesn't exercise: an empty key (not
    /// merely a wrong one), a `.minisig` truncated mid-file rather than pure
    /// garbage, and a trusted-comment tamper. The last matters because
    /// minisign's global signature covers `signature || trusted_comment`
    /// specifically so the comment can't be forged independently of the
    /// payload signature it describes — flip a byte there with the data and
    /// data-signature untouched, and `verify` must still refuse it.
    #[test]
    fn empty_key_truncated_signature_and_a_tampered_trusted_comment_are_all_refused() {
        let (pk, sk) = test_key();
        let data = b"not really a tarball, but the bytes are what is signed".to_vec();
        let sig = sign_bytes(&sk, &data);

        let e = verify(&data, &sig, "").unwrap_err();
        assert!(e.starts_with("the compiled-in key could not be read"), "{e}");

        let e = verify(&data, &sig[..sig.len() / 2], &pk).unwrap_err();
        assert!(e.starts_with("the signature file could not be read"), "{e}");

        let mut lines: Vec<String> = sig.lines().map(str::to_string).collect();
        assert!(lines[2].starts_with("trusted comment: "), "line 2 was: {}", lines[2]);
        lines[2].push_str("-tampered");
        let tampered = lines.join("\n");
        assert_eq!(verify(&data, &tampered, &pk).unwrap_err(), "signature did not verify");
    }

    enum Member<'a> {
        File(&'a str, &'a [u8]),
        Dir(&'a str),
        Link(&'a str, &'a str),
    }

    /// An archive the way dist writes one, built in memory: a top directory
    /// named for the target, files under it. `xz_compress` produces a
    /// stream `xz_decompress` reads; the release's is `xz -9`, same format.
    fn tar_xz(members: &[Member]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for m in members {
            let mut h = tar::Header::new_gnu();
            match m {
                Member::File(path, data) => {
                    h.set_mode(0o755);
                    h.set_size(data.len() as u64);
                    b.append_data(&mut h, path, *data).unwrap();
                }
                Member::Dir(path) => {
                    h.set_entry_type(tar::EntryType::Directory);
                    h.set_mode(0o755);
                    h.set_size(0);
                    b.append_data(&mut h, path, &b""[..]).unwrap();
                }
                Member::Link(path, target) => {
                    h.set_entry_type(tar::EntryType::Symlink);
                    h.set_size(0);
                    b.append_link(&mut h, path, target).unwrap();
                }
            }
        }
        let tar = b.into_inner().unwrap();
        let mut out = Vec::new();
        lzma_rs::xz_compress(&mut &tar[..], &mut out).unwrap();
        out
    }

    const T: &str = "roost-x86_64-unknown-linux-musl";

    /// The layout `tar -tJf` printed for v0.5.2 on 2026-09-14: a directory
    /// named for the target, and under it README.md, roost, and two
    /// licences. The member is `<dir>/roost`, not `roost`.
    #[test]
    fn the_release_layout_yields_the_one_binary() {
        let a = tar_xz(&[
            Member::Dir(&format!("{T}/")),
            Member::File(&format!("{T}/README.md"), b"# roost"),
            Member::File(&format!("{T}/roost"), b"\x7fELF fake"),
            Member::File(&format!("{T}/LICENSE-MIT"), b"MIT"),
            Member::File(&format!("{T}/LICENSE-APACHE"), b"Apache"),
        ]);
        assert_eq!(extract_binary(&a).unwrap(), b"\x7fELF fake");
    }

    #[test]
    fn every_other_shape_is_refused_by_name() {
        let none = tar_xz(&[Member::File(&format!("{T}/README.md"), b"x")]);
        assert_eq!(extract_binary(&none).unwrap_err(), "the archive has no roost member");

        let two = tar_xz(&[Member::File(&format!("{T}/roost"), b"a"), Member::File("other/roost", b"b")]);
        assert_eq!(extract_binary(&two).unwrap_err(), "the archive has 2 roost members");

        let link = tar_xz(&[Member::Link(&format!("{T}/roost"), "/bin/sh")]);
        assert_eq!(extract_binary(&link).unwrap_err(), "the roost member is a symlink");

        let dir = tar_xz(&[Member::Dir(&format!("{T}/roost/"))]);
        assert_eq!(extract_binary(&dir).unwrap_err(), "the roost member is a directory");

        let e = extract_binary(b"definitely not xz").unwrap_err();
        assert!(e.starts_with("the archive could not be decompressed"), "{e}");
    }

    /// Tests `Capped`'s running counter in isolation, calling `write`
    /// directly rather than through `xz_decompress` — it does not, and
    /// cannot, exercise the single-block gap `Capped`'s doc comment
    /// describes: `lzma_rs` 0.3.0 hands a whole decoded block to `write` in
    /// one call, so this test's two separate `write` calls model
    /// cross-block accumulation only, never a single oversized write.
    /// Revert-checked: deleting the cap check turns `write` into a plain
    /// append and the second `unwrap_err()` below fails with
    /// `Ok(4)` instead of a size-cap error.
    #[test]
    fn the_capped_writer_counts_across_writes() {
        let mut buf = Vec::new();
        let mut w = Capped { buf: &mut buf, cap: 4 };
        assert_eq!(std::io::Write::write(&mut w, b"ab").unwrap(), 2);
        let e = std::io::Write::write(&mut w, b"cdef").unwrap_err();
        assert_eq!(e.to_string(), "decompressed archive exceeds the size cap");
        assert_eq!(buf, b"ab", "the over-cap write must not be partially applied");
    }
}
