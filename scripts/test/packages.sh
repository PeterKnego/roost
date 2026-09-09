#!/usr/bin/env bash
# Opens the produced package rather than checking an exit code. A test that
# asserts only "the script exited 0" passes against an empty package, which is
# the failure mode this repository has been bitten by seven times.
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/lib.sh

need dpkg-deb "apt install dpkg"
need rpm "apt install rpm"
need tar "apt install tar"
need sha256sum "apt install coreutils"
need rpm2cpio "apt install rpm2cpio"
need cpio "apt install cpio"

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
FIXTURE="$TMP/roost"
printf '#!/bin/sh\necho "roost 9.9.9"\n' > "$FIXTURE"
chmod 755 "$FIXTURE"

phase "deb"
DEB=$(scripts/package-deb.sh "$FIXTURE" 9.9.9 amd64 | tail -1)
[ -f "$DEB" ] || die "no .deb produced at $DEB"

# dpkg-deb -I's output embeds the package Description, which pulls in the
# full README (Cargo.toml's `description` metadata) — long enough that
# `dpkg-deb -I | grep -q ...` races grep's early exit (it matches
# "Architecture" on line 7) against dpkg-deb still writing the rest.
# Reproduced directly: under `set -o pipefail`, ~1 run in 15 died with
# pipeline exit 141 (SIGPIPE) even though the .deb on disk was correct and
# grep had already found its match — a false failure, not an architecture
# bug. Capturing the output into a variable first reads it to completion
# before anything greps it, so there is nothing left to race.
DEB_INFO=$(dpkg-deb -I "$DEB")

# cargo-deb's default Architecture is the *host's* arch, not the arch passed
# in, so a build that never sets --target looks correct here — amd64 in,
# amd64 out — even with the bug fully restored, because this runner is
# amd64. That is exactly how this drifted invisibly: only amd64 was ever
# built. The arm64 build just below is what actually exercises the fix.
echo "$DEB_INFO" | grep -qE '^ Architecture: amd64$' \
  || die "the .deb's declared Architecture does not match the amd64 arch passed in"
ok "declares Architecture: amd64"

# Same assertion, but for the one other Linux arch this project ships
# (dist-workspace.toml). This is the case that actually catches the drift:
# revert package-deb.sh's --target fix and this dies with "Architecture:
# amd64" while amd64 was requested — proven below, see verification output.
ARM_DEB=$(scripts/package-deb.sh "$FIXTURE" 9.9.9 arm64 | tail -1)
[ -f "$ARM_DEB" ] || die "no arm64 .deb produced at $ARM_DEB"
ARM_DEB_INFO=$(dpkg-deb -I "$ARM_DEB")
echo "$ARM_DEB_INFO" | grep -qE '^ Architecture: arm64$' \
  || die "the arm64 .deb's declared Architecture does not match the arm64 arch passed in"
ok "declares Architecture: arm64"

echo "$DEB_INFO" | grep -qE '^ Depends: .*\bdtach\b' \
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
#
# rpm2cpio | cpio, not rpm2archive: measured on this host (rpm 6.0.1),
# `rpm2archive t.rpm` writes its archive to stdout and creates no file —
# convenient for piping, but ubuntu-22.04's older rpm2archive does the
# opposite (CI run 34325648104): it writes `t.rpm.tgz` beside the input and
# puts nothing on stdout, so piping it into tar there reads an empty stream
# ("gzip: stdin: unexpected end of file"). Two rpm versions, two different
# defaults, no invocation works on both. `rpm2cpio t.rpm`, measured on this
# host, writes its cpio stream to stdout — that interface has been the
# documented behaviour for a very long time and is what this project already
# depended on before the rpm2archive detour, so this reverts to it and pays
# for `cpio` as an explicit apt dependency instead.
#
# rpm2cpio to a file, and not gated on rpm2cpio's own exit status: measured
# in a container running ubuntu:22.04 (rpm 4.17.0), `rpm2cpio t.rpm` exits 1
# on every package cargo-generate-rpm produces — with no pipe involved at
# all, redirected straight to a file — while writing the complete, correct
# payload (confirmed: `cpio -tv` on that same file lists all five archive
# members at their right sizes). rpm2cpio's own source explains why
# (tools/rpm2cpio.c in the rpm-4.17.0-release tag): it compares the bytes it
# copied against header tag LONGARCHIVESIZE, defaulting to 0 if the tag is
# absent, and returns EXIT_FAILURE on any mismatch. `rpm -qp --qf` on our
# .rpm shows both ARCHIVESIZE and LONGARCHIVESIZE as "(none)" — cargo-
# generate-rpm never sets either — so that comparison is `actual_bytes == 0`,
# unconditionally false, on every package it builds, independent of whether
# the payload is actually intact. (A real rpmbuild-produced .rpm, fetched
# and tried the same way, sets both tags and rpm2cpio exits 0 against it.)
# rpm2cpio's exit status is therefore not a usable signal for this project's
# own packages; a genuine read failure (tried against a non-.rpm file) still
# shows up honestly as an empty payload, and cpio finding both requested
# paths below plus the byte-identity check further down are what actually
# prove the payload is good.
RPMEXTRACT="$TMP/rpmextract"; mkdir -p "$RPMEXTRACT"
RPMABS="$(pwd)/$RPM"
PAYLOAD="$TMP/payload.cpio"
rpm2cpio "$RPMABS" > "$PAYLOAD" || true
[ -s "$PAYLOAD" ] || die "rpm2cpio produced no output reading the .rpm payload"
(cd "$RPMEXTRACT" && cpio -idm --quiet \
  ./usr/bin/roost ./usr/lib/systemd/user/roost.service < "$PAYLOAD") \
  || die "cpio could not extract the expected paths from the .rpm payload"
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

phase "prerelease version encoding"
# Every case above uses 9.9.9, which has no '-' to mistranslate — it is the
# one version shape that never exercised the bug this suite is here to
# catch. dist hands both scripts a real prerelease like "0.5.2-rc.2"
# (scripts/release.sh's own PRERELEASE check matches on '-'), so build one
# of each package from that shape and read back what each format actually
# declares, not just whether the script exited 0.
PRE_VERSION="0.5.2-rc.2"

PRE_DEB=$(scripts/package-deb.sh "$FIXTURE" "$PRE_VERSION" amd64 | tail -1)
[ -f "$PRE_DEB" ] || die "no prerelease .deb produced at $PRE_DEB"

# Filenames are for humans matching artifacts across one release (the
# .tar.xz and the git tag are both literally "0.5.2-rc.2"); only the field
# a package manager parses needs the '~' escape. See package-deb.sh.
[ "$(basename "$PRE_DEB")" = "roost_${PRE_VERSION}_amd64.deb" ] \
  || die "prerelease .deb filename does not carry the project's own version ($PRE_VERSION)"
ok "prerelease .deb filename keeps the project version ($PRE_VERSION)"

PRE_DEB_VERSION=$(dpkg-deb -I "$PRE_DEB" | grep -E '^ Version:' | awk '{print $2}')
[ "$PRE_DEB_VERSION" = "0.5.2~rc.2" ] \
  || die "prerelease .deb declares Version: $PRE_DEB_VERSION, want 0.5.2~rc.2"
ok "prerelease .deb declares Version: 0.5.2~rc.2"

PRE_RPM=$(scripts/package-rpm.sh "$FIXTURE" "$PRE_VERSION" x86_64 | tail -1)
[ -f "$PRE_RPM" ] || die "no prerelease .rpm produced at $PRE_RPM"

[ "$(basename "$PRE_RPM")" = "roost-${PRE_VERSION}.x86_64.rpm" ] \
  || die "prerelease .rpm filename does not carry the project's own version ($PRE_VERSION)"
ok "prerelease .rpm filename keeps the project version ($PRE_VERSION)"

PRE_RPM_VERSION=$(rpm -qp --qf '%{VERSION}' "$PRE_RPM" 2>/dev/null)
[ "$PRE_RPM_VERSION" = "0.5.2~rc.2" ] \
  || die "prerelease .rpm declares Version: $PRE_RPM_VERSION, want 0.5.2~rc.2"
ok "prerelease .rpm declares Version: 0.5.2~rc.2"
