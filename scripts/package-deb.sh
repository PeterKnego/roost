#!/usr/bin/env bash
# Wraps an already-built binary as a .deb. It never compiles: if this script
# built its own binary, the .deb in a release would contain different bytes
# from the .tar.xz in that same release — same version, quietly different
# builds, noticed only when a bug reproduces on one and not the other.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib.sh

BIN=${1:?usage: package-deb.sh <binary> <version> <arch>}
VERSION=${2:?usage: package-deb.sh <binary> <version> <arch>}
ARCH=${3:?usage: package-deb.sh <binary> <version> <arch>}

need cargo-deb "cargo install cargo-deb"
[ -f "$BIN" ] || die "no binary at $BIN"

# The Cargo.toml asset "target/release/roost" is not a literal relative path:
# cargo-deb resolves it through `cargo metadata`'s real target directory, which
# on a host with a shared target-dir (see CLAUDE.md) is NOT ./target. Staging
# into ./target/release therefore both misses — cargo-deb packs whatever is
# already sitting in the real target dir, silently stale — and, on a host
# where ./target/release *is* the real one, clobbers a developer's own build
# (and a later `cargo build --release` won't repair it: cargo trusts its
# fingerprint, not file content). A private CARGO_TARGET_DIR sidesteps both.
STAGE=$(mktemp -d); trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/release"
install -m 755 "$BIN" "$STAGE/release/roost"

mkdir -p target/distrib
# --no-strip because the binary is already stripped ([profile.release] strip =
# true) and because the host `strip` cannot process a cross-built aarch64
# binary anyway.
CARGO_TARGET_DIR="$STAGE" cargo deb --no-build --no-strip \
  --deb-version "$VERSION" \
  --output "$(pwd)/target/distrib/roost_${VERSION}_${ARCH}.deb" \
  >/dev/null

ok "built roost_${VERSION}_${ARCH}.deb"
echo "target/distrib/roost_${VERSION}_${ARCH}.deb"
