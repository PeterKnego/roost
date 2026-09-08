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

# cargo-deb reads assets relative to the target dir, so the binary is placed
# where the metadata says rather than passed as a path.
STAGE=target/release
mkdir -p "$STAGE"
install -m 755 "$BIN" "$STAGE/roost"

mkdir -p target/distrib
# --no-strip because the binary is already stripped ([profile.release] strip =
# true) and because the host `strip` cannot process a cross-built aarch64
# binary anyway.
cargo deb --no-build --no-strip \
  --deb-version "$VERSION" \
  --output "target/distrib/roost_${VERSION}_${ARCH}.deb" \
  >/dev/null

ok "built roost_${VERSION}_${ARCH}.deb"
echo "target/distrib/roost_${VERSION}_${ARCH}.deb"
