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

# cargo-deb has no --arch flag (checked `cargo deb --help`: the only
# architecture-shaped option is `--target <triple>`). Its Architecture field
# comes solely from that triple — cargo-deb's own
# debian_architecture_from_rust_triple, confirmed by reading cargo-deb 3.8.0's
# source — so omitting --target defaults to the host's own architecture
# regardless of what $ARCH says, which is exactly the bug this script had:
# every .deb came out "Architecture: amd64" because the host that ran it is
# amd64. This project ships exactly two Linux targets (dist-workspace.toml),
# so the map below is exhaustive, not a guess.
case "$ARCH" in
  amd64) TRIPLE=x86_64-unknown-linux-musl ;;
  arm64) TRIPLE=aarch64-unknown-linux-musl ;;
  *) die "package-deb.sh: unsupported arch '$ARCH' (want amd64 or arm64)" ;;
esac

# The Cargo.toml asset "target/release/roost" is not a literal relative path:
# cargo-deb resolves it through `cargo metadata`'s real target directory, which
# on a host with a shared target-dir (see CLAUDE.md) is NOT ./target. Staging
# into ./target/release therefore both misses — cargo-deb packs whatever is
# already sitting in the real target dir, silently stale — and, on a host
# where ./target/release *is* the real one, clobbers a developer's own build
# (and a later `cargo build --release` won't repair it: cargo trusts its
# fingerprint, not file content). A private CARGO_TARGET_DIR sidesteps both.
# Passing --target also changes where cargo-deb looks for the binary, from
# <target-dir>/release/roost to <target-dir>/<triple>/release/roost (measured:
# omitting the triple subdirectory here makes cargo-deb fail with "Static file
# asset has not been built"), so the staging path below has to match.
STAGE=$(mktemp -d); trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/$TRIPLE/release"
install -m 755 "$BIN" "$STAGE/$TRIPLE/release/roost"

mkdir -p target/distrib
# --no-strip because the binary is already stripped ([profile.release] strip =
# true) and because the host `strip` cannot process a cross-built aarch64
# binary anyway.
CARGO_TARGET_DIR="$STAGE" cargo deb --no-build --no-strip --target "$TRIPLE" \
  --deb-version "$VERSION" \
  --output "$(pwd)/target/distrib/roost_${VERSION}_${ARCH}.deb" \
  >/dev/null

ok "built roost_${VERSION}_${ARCH}.deb"
echo "target/distrib/roost_${VERSION}_${ARCH}.deb"
