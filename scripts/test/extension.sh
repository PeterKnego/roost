#!/usr/bin/env bash
# Opens the extension zip package-extension.sh produces and reads it back,
# rather than trusting its exit code — packages.sh says why.
#
# Revert-checks performed, all restored:
#   (a) the manifest stamp removed -> "manifest version" fails: the zip ships
#       the tree's development version.
#   (b) the missing-file check removed -> "a missing icon is refused" fails:
#       the zip is written with a manifest Chrome will refuse to load.
#   (c) the `store` exclusion removed from package-extension.sh -> "the store
#       listing stays out" fails: the listing's screenshots ship in every zip.
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
for f in worker.js page.js drive-cdp.js drive-page.js content.js popup.html popup.js options.html options.js consent.html consent.js \
         icons/icon-16.png icons/icon-32.png icons/icon-48.png icons/icon-128.png; do
  grep -qx "roost-extension/$f" <<<"$LISTING" || die "zip is missing roost-extension/$f"
done
[ "$(grep -cvx 'roost-extension/.*' <<<"$LISTING" || true)" = 0 ] || die "zip has entries outside roost-extension/"
# PNG magic, not just a name: an icon written as an empty file loads as no icon.
[ "$(unzip -p "$ZIP" roost-extension/icons/icon-128.png | head -c 8 | od -An -tx1 | tr -d ' \n')" = 89504e470d0a1a0a ] \
  || die "icon-128.png is not a PNG"
ok "every file the extension needs is in the zip"

[ "$(grep -c '^roost-extension/store/' <<<"$LISTING" || true)" = 0 ] || die "the store listing's files leaked into the extension zip"
ok "the store listing stays out of the extension zip"

# The store's copy: the same files with manifest.json at the root, which is
# the only layout the Chrome Web Store accepts.
STORE="$TMP/a/roost-extension-9.8.7-rc.1-webstore.zip"
[ -f "$STORE" ] || die "no web store zip at $STORE"
SLIST=$(unzip -Z1 "$STORE")
grep -qx manifest.json <<<"$SLIST" || die "the web store zip has no manifest.json at its root"
[ "$(unzip -p "$STORE" manifest.json | jq -r .version)" = 9.8.7 ] || die "the web store zip's manifest is not stamped"
[ "$(grep -c / <<<"$SLIST" | tr -d ' ')" = "$(grep -c '^icons/' <<<"$SLIST" | tr -d ' ')" ] || die "the web store zip has a folder other than icons/: $SLIST"
# Every entry in the plain zip starts with that prefix (checked above), so
# stripping it everywhere strips exactly the leading folder.
[ "$(grep -vx README.md <<<"${LISTING//roost-extension\//}" | sort)" = "$(sort <<<"$SLIST")" ] \
  || die "the web store zip holds different files from the extension zip"
ok "the web store zip is the same files, at the root"

# Firefox's copy: the same files minus the Chrome-only backend, at the root,
# with a manifest Firefox and AMO accept. Read back, not trusted.
FOX="$TMP/a/roost-extension-9.8.7-rc.1-firefox.zip"
[ -f "$FOX" ] || die "no Firefox zip at $FOX"
FLIST=$(unzip -Z1 "$FOX")
FM=$(unzip -p "$FOX" manifest.json)
[ "$(jq -c .background <<<"$FM")" = '{"scripts":["page.js","drive-page.js","worker.js"]}' ] \
  || die "Firefox background is $(jq -c .background <<<"$FM"), want page.js, drive-page.js, worker.js as scripts"
jq -e '.permissions | index("debugger") | not' <<<"$FM" >/dev/null || die "the Firefox manifest asks for debugger, which Firefox does not have"
[ "$(jq -r .browser_specific_settings.gecko.id <<<"$FM")" = roost-bridge@roost ] || die "the Firefox manifest's gecko id is wrong"
[ "$(jq -r .browser_specific_settings.gecko.strict_min_version <<<"$FM")" = 140.0 ] || die "the Firefox manifest's minimum version is wrong"
[ "$(jq -c .browser_specific_settings.gecko.data_collection_permissions.required <<<"$FM")" = '["websiteContent","browsingActivity"]' ] \
  || die "the Firefox manifest does not declare what it sends, which AMO requires"
[ "$(jq -c .optional_host_permissions <<<"$FM")" = '["<all_urls>"]' ] || die "Firefox needs <all_urls> by that name for tabs.captureTab"
[ "$(jq -r .version <<<"$FM")" = 9.8.7 ] || die "the Firefox manifest's version is $(jq -r .version <<<"$FM"), want 9.8.7"
[ "$(jq -r '.version_name // "none"' <<<"$FM")" = none ] || die "the Firefox manifest carries version_name, which Firefox does not show"
grep -qx drive-cdp.js <<<"$FLIST" && die "the Firefox zip ships drive-cdp.js, the Chrome-only backend AMO's linter flags"
[ "$(grep -vx manifest.json <<<"$FLIST" | sort)" = "$(grep -vx -e manifest.json -e drive-cdp.js <<<"$SLIST" | sort)" ] \
  || die "the Firefox zip holds different files from the web store zip (minus drive-cdp.js)"
ok "the Firefox zip: event page, no debugger, gecko settings, no Chrome-only backend"

scripts/package-extension.sh 9.8.7-rc.1 "$TMP/b" >/dev/null
cmp -s "$ZIP" "$TMP/b/roost-extension-9.8.7-rc.1.zip" || die "two packs of the same tree differ"
cmp -s "$STORE" "$TMP/b/roost-extension-9.8.7-rc.1-webstore.zip" || die "two web store packs of the same tree differ"
cmp -s "$FOX" "$TMP/b/roost-extension-9.8.7-rc.1-firefox.zip" || die "two Firefox packs of the same tree differ"
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
