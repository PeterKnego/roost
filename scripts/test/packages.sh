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

# Path existence cannot distinguish the right binary from a stale one sitting
# wherever cargo-deb actually resolved its target dir to. This is the
# assertion that would have caught the shared-target-dir bug: the packed
# binary must be the exact bytes handed to package-deb.sh, not merely present.
dpkg-deb --fsys-tarfile "$DEB" | tar -xO ./usr/bin/roost > "$TMP/packed"
[ "$(sha256sum < "$TMP/packed" | cut -d' ' -f1)" = "$(sha256sum < "$FIXTURE" | cut -d' ' -f1)" ] \
  || die "the .deb contains a different binary than the one passed in"
ok "the packaged binary is byte-identical to the input"

# The unit could ship empty or with the default kill mode and every path check
# above would still pass. Assert the property, not the file's existence.
dpkg-deb --fsys-tarfile "$DEB" | tar -xO ./usr/lib/systemd/user/roost.service \
  | grep -q '^KillMode=process$' || die "the packaged unit lacks KillMode=process"
ok "the packaged unit sets KillMode=process"
