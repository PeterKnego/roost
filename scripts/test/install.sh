#!/usr/bin/env bash
# Installs the packages the way a user would, in containers.
#
# scripts/test/packages.sh reads metadata; this reads consequences. A `Depends:`
# field can name a package that does not exist, an architecture that does not
# match, or a version nothing satisfies — all of which that file reports green.
# Only a real `apt install` resolves it.
#
# debian:12 is chosen, not arbitrary: it ships glibc 2.36, the version the
# hand-built v0.5.0 gnu binary failed to *load* on. So this doubles as a standing
# regression test on the static-musl decision, in the exact environment that
# measured it.
#
# Calls `docker` directly. On a machine whose login predates its docker group
# membership, run it as `sg docker -c scripts/test/install.sh` — that belongs in
# the caller, not in here, or the script breaks everywhere docker works normally.
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/lib.sh

need docker "install docker, and be in the docker group"
need cargo "install rust"

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
VERSION=9.9.9

phase "build"
# A real static binary, because the assertions below run it.
cargo build --locked --profile dist --target x86_64-unknown-linux-musl >/dev/null
BIN=$(cargo metadata --format-version 1 --no-deps \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/x86_64-unknown-linux-musl/dist/roost
[ -f "$BIN" ] || die "no musl binary at $BIN"
ok "built the x86_64 musl binary"

# `roost --version` prints CARGO_PKG_VERSION, baked in at compile time from
# Cargo.toml — it has no idea what version string this script hands the
# packaging tools. VERSION above is deb/rpm package metadata only, deliberately
# not the crate version, so the "binary runs" assertion below has to check for
# the version the binary actually reports, not the packaging version.
PKG_VERSION=$(cargo metadata --format-version 1 --no-deps \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["packages"][0]["version"])')

DEB=$(scripts/package-deb.sh "$BIN" "$VERSION" amd64 | tail -1)
RPM=$(scripts/package-rpm.sh "$BIN" "$VERSION" x86_64 | tail -1)
cp "$DEB" "$RPM" "$TMP/"
ok "packaged both"

phase "debian:12"
docker run --rm -v "$TMP:/pkgs:ro" debian:12 sh -euc '
  apt-get update -q >/dev/null
  # apt, not dpkg -i: apt resolves the package'"'"'s dependencies from the
  # configured repositories, which is the whole thing under test.
  apt-get install -y -q /pkgs/*.deb >/dev/null
  command -v dtach >/dev/null || { echo "dtach was not installed"; exit 1; }
  test -f /usr/lib/systemd/user/roost.service || { echo "unit missing"; exit 1; }
  grep -qx "KillMode=process" /usr/lib/systemd/user/roost.service || { echo "unit lost KillMode"; exit 1; }
  roost --version
' | tail -1 | grep -qx "roost $PKG_VERSION" || die "debian:12 install failed"
ok "apt resolved dtach, unit placed, binary runs on glibc 2.36"

phase "fedora"
docker run --rm -v "$TMP:/pkgs:ro" fedora:41 sh -euc '
  dnf install -y -q /pkgs/*.rpm >/dev/null
  command -v dtach >/dev/null || { echo "dtach was not installed"; exit 1; }
  test -f /usr/lib/systemd/user/roost.service || { echo "unit missing"; exit 1; }
  roost --version
' | tail -1 | grep -qx "roost $PKG_VERSION" || die "fedora install failed"
ok "dnf resolved dtach, unit placed, binary runs"
