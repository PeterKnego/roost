#!/usr/bin/env bash
# Opens the produced package rather than checking an exit code. A test that
# asserts only "the script exited 0" passes against an empty package, which is
# the failure mode this repository has been bitten by seven times.
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/lib.sh

need dpkg-deb "apt install dpkg"
need rpm "apt install rpm"

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
FIXTURE="$TMP/roost"
printf '#!/bin/sh\necho "roost 9.9.9"\n' > "$FIXTURE"
chmod 755 "$FIXTURE"

phase "deb"
DEB=$(scripts/package-deb.sh "$FIXTURE" 9.9.9 amd64 | tail -1)
[ -f "$DEB" ] || die "no .deb produced at $DEB"

# cargo-deb's default Architecture is the *host's* arch, not the arch passed
# in, so a build that never sets --target looks correct here — amd64 in,
# amd64 out — even with the bug fully restored, because this runner is
# amd64. That is exactly how this drifted invisibly: only amd64 was ever
# built. The arm64 build just below is what actually exercises the fix.
dpkg-deb -I "$DEB" | grep -qE '^ Architecture: amd64$' \
  || die "the .deb's declared Architecture does not match the amd64 arch passed in"
ok "declares Architecture: amd64"

# Same assertion, but for the one other Linux arch this project ships
# (dist-workspace.toml). This is the case that actually catches the drift:
# revert package-deb.sh's --target fix and this dies with "Architecture:
# amd64" while amd64 was requested — proven below, see verification output.
ARM_DEB=$(scripts/package-deb.sh "$FIXTURE" 9.9.9 arm64 | tail -1)
[ -f "$ARM_DEB" ] || die "no arm64 .deb produced at $ARM_DEB"
dpkg-deb -I "$ARM_DEB" | grep -qE '^ Architecture: arm64$' \
  || die "the arm64 .deb's declared Architecture does not match the arm64 arch passed in"
ok "declares Architecture: arm64"

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

phase "rpm"
RPM=$(scripts/package-rpm.sh "$FIXTURE" 9.9.9 x86_64 | tail -1)
[ -f "$RPM" ] || die "no .rpm produced at $RPM"

rpm -qp --requires "$RPM" 2>/dev/null | grep -qx 'dtach' \
  || die "the .rpm does not require dtach"
ok "requires dtach"

rpm -qpl "$RPM" 2>/dev/null | grep -qx '/usr/lib/systemd/user/roost.service' \
  || die "the unit is missing from the .rpm"
ok "ships the systemd user unit"

rpm -qpl "$RPM" 2>/dev/null | grep -qx '/usr/bin/roost' || die "no /usr/bin/roost in the .rpm"
ok "ships /usr/bin/roost"

# Same reasoning as the .deb's byte-identity check above, and the assertion
# that actually catches the staging bug this task's brief walks straight into:
# rpm -qpl only proves a path exists in the archive, not whose binary it is.
# One extraction covers both the binary and the unit checked below.
RPMEXTRACT="$TMP/rpmextract"; mkdir -p "$RPMEXTRACT"
RPMABS="$(pwd)/$RPM"
(cd "$RPMEXTRACT" && rpm2cpio "$RPMABS" | cpio -idm --quiet \
  ./usr/bin/roost ./usr/lib/systemd/user/roost.service) \
  || die "could not extract the .rpm payload"
[ "$(sha256sum < "$RPMEXTRACT/usr/bin/roost" | cut -d' ' -f1)" = "$(sha256sum < "$FIXTURE" | cut -d' ' -f1)" ] \
  || die "the .rpm contains a different binary than the one passed in"
ok "the packaged binary is byte-identical to the input"

# The unit could ship empty or with the default kill mode and every path check
# above would still pass. Both packages embed the same packaging/roost.service,
# but the embedding tool differs (cargo-deb vs cargo-generate-rpm), so this is
# not redundant with the deb block's equivalent check — each proves its own
# tool actually put the file's real content in, not just a path.
grep -q '^KillMode=process$' "$RPMEXTRACT/usr/lib/systemd/user/roost.service" \
  || die "the packaged unit lacks KillMode=process"
ok "the packaged unit sets KillMode=process"
