#!/usr/bin/env bash
# Wraps an already-built binary as an .rpm. It never compiles: if this script
# built its own binary, the .rpm in a release would contain different bytes
# from the .tar.xz in that same release — same version, quietly different
# builds, noticed only when a bug reproduces on one and not the other.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib.sh

BIN=${1:?usage: package-rpm.sh <binary> <version> <arch>}
VERSION=${2:?usage: package-rpm.sh <binary> <version> <arch>}
ARCH=${3:?usage: package-rpm.sh <binary> <version> <arch>}

need cargo-generate-rpm "cargo install cargo-generate-rpm"
[ -f "$BIN" ] || die "no binary at $BIN"

# The Cargo.toml asset "target/release/roost" is not a literal relative path:
# cargo-generate-rpm resolves it through cargo's real target directory, which
# on a host with a shared target-dir (see CLAUDE.md) is NOT ./target. Staging
# into ./target/release therefore both misses — the tool packs whatever is
# already sitting in the real target dir, silently stale — and, on a host
# where ./target/release *is* the real one, clobbers a developer's own build.
# A private CARGO_TARGET_DIR sidesteps both, the same fix package-deb.sh uses.
STAGE=$(mktemp -d); trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/release"
install -m 755 "$BIN" "$STAGE/release/roost"

mkdir -p target/distrib
OUT="target/distrib/roost-${VERSION}.${ARCH}.rpm"
CARGO_TARGET_DIR="$STAGE" cargo generate-rpm --payload-compress none \
  --set-metadata "version = '${VERSION}'" \
  --arch "$ARCH" \
  --output "$(pwd)/$OUT" \
  >/dev/null

ok "built $(basename "$OUT")"
echo "$OUT"
