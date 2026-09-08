#!/usr/bin/env bash
# Opens the produced package rather than checking an exit code. A test that
# asserts only "the script exited 0" passes against an empty package, which is
# the failure mode this repository has been bitten by seven times.
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/lib.sh

need dpkg-deb "apt install dpkg"

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
FIXTURE="$TMP/roost"
printf '#!/bin/sh\necho "roost 9.9.9"\n' > "$FIXTURE"
chmod 755 "$FIXTURE"

phase "deb"
DEB=$(scripts/package-deb.sh "$FIXTURE" 9.9.9 amd64 | tail -1)
[ -f "$DEB" ] || die "no .deb produced at $DEB"

dpkg-deb -I "$DEB" | grep -qE '^ Depends: .*\bdtach\b' \
  || die "the .deb does not declare Depends: dtach"
ok "declares Depends: dtach"

dpkg-deb -c "$DEB" | grep -q './usr/lib/systemd/user/roost.service' \
  || die "the unit is missing from the .deb"
ok "ships the systemd user unit"

dpkg-deb -c "$DEB" | grep -q './usr/bin/roost' || die "no /usr/bin/roost in the .deb"
ok "ships /usr/bin/roost"

# The unit could ship empty or with the default kill mode and every path check
# above would still pass. Assert the property, not the file's existence.
dpkg-deb --fsys-tarfile "$DEB" | tar -xO ./usr/lib/systemd/user/roost.service \
  | grep -q '^KillMode=process$' || die "the packaged unit lacks KillMode=process"
ok "the packaged unit sets KillMode=process"
