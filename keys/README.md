# keys

`roost.pub` is the minisign public key every release tarball is signed with.
`build.rs` bakes its base64 line into the binary (`ROOST_UPDATE_PUBKEY`), and
`src/update.rs` verifies a downloaded tarball against it before the archive is
opened. It is generated once, offline, by the maintainer — see item 6.5 in
`docs/roost-packaging-handover.md` — and **the secret key is never committed**.

While the file is absent, a build carries no key: About offers no `Update`
button and the `Update` intent is refused with "this build carries no release
key". Rotation is a new file here and a release: a binary trusts only the key
it was built with.
