#!/usr/bin/env bash
# Opens the extension zip package-extension.sh produces and reads it back,
# rather than trusting its exit code — packages.sh says why.
#
# Revert-checks performed, all restored:
#   (a) the manifest stamp removed -> "manifest version" fails: the zip ships
#       the tree's development version.
#   (b) the missing-file check removed -> "a missing icon is refused" fails:
#       the zip is written with a manifest Chrome will refuse to load.
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/lib.sh

need python3 "install python3"
need unzip "apt install unzip"
need jq "apt install jq"

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT

phase "extension package"
scripts/package-extension.sh 9.8.7-rc.1 "$TMP/a" >/dev/null
ZIP="$TMP/a/roost-extension-9.8.7-rc.1.zip"
[ -f "$ZIP" ] || die "no zip at $ZIP"

MANIFEST=$(unzip -p "$ZIP" roost-extension/manifest.json)
[ "$(jq -r .version <<<"$MANIFEST")" = 9.8.7 ] || die "manifest version is $(jq -r .version <<<"$MANIFEST"), want 9.8.7"
[ "$(jq -r .version_name <<<"$MANIFEST")" = 9.8.7-rc.1 ] || die "manifest version_name is $(jq -r .version_name <<<"$MANIFEST"), want 9.8.7-rc.1"
ok "manifest version 9.8.7, shown as 9.8.7-rc.1"

# Every file the manifest names, plus the scripts its pages load, by exact
# path under the one top-level folder Chrome's "Load unpacked" is pointed at.
LISTING=$(unzip -Z1 "$ZIP")
for f in worker.js content.js popup.html popup.js options.html options.js consent.html consent.js \
         icons/icon-16.png icons/icon-32.png icons/icon-48.png icons/icon-128.png; do
  grep -qx "roost-extension/$f" <<<"$LISTING" || die "zip is missing roost-extension/$f"
done
[ "$(grep -cvx 'roost-extension/.*' <<<"$LISTING" || true)" = 0 ] || die "zip has entries outside roost-extension/"
# PNG magic, not just a name: an icon written as an empty file loads as no icon.
[ "$(unzip -p "$ZIP" roost-extension/icons/icon-128.png | head -c 8 | od -An -tx1 | tr -d ' \n')" = 89504e470d0a1a0a ] \
  || die "icon-128.png is not a PNG"
ok "every file the extension needs is in the zip"

scripts/package-extension.sh 9.8.7-rc.1 "$TMP/b" >/dev/null
cmp -s "$ZIP" "$TMP/b/roost-extension-9.8.7-rc.1.zip" || die "two packs of the same tree differ"
ok "packing is deterministic"

if OUT=$(scripts/package-extension.sh 9.8 "$TMP/c" 2>&1); then die "a two-part version was accepted"; fi
grep -q "not a release version" <<<"$OUT" || die "a bad version failed for the wrong reason: $OUT"
ok "a version Chrome cannot take is refused"

# A copy of the tree whose manifest names an icon that is not there.
mkdir -p "$TMP/tree/scripts"
cp -r extension "$TMP/tree/"
cp scripts/package-extension.sh scripts/lib.sh "$TMP/tree/scripts/"
rm "$TMP/tree/extension/icons/icon-48.png"
if OUT=$("$TMP/tree/scripts/package-extension.sh" 9.8.7 "$TMP/d" 2>&1); then die "a missing icon was packed"; fi
grep -q "icons/icon-48.png" <<<"$OUT" || die "a missing icon failed for the wrong reason: $OUT"
[ ! -e "$TMP/d/roost-extension-9.8.7.zip" ] || die "a zip was left behind anyway"
ok "a missing icon is refused, and names itself"
