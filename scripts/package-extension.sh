#!/usr/bin/env bash
# Packs extension/ as roost-extension-<version>.zip, the browser bridge a
# release ships beside the binaries. Unzipped, it is a folder Chrome loads with
# "Load unpacked".
#
# The release's version is stamped into the manifest here rather than kept in
# step by hand: extension/manifest.json in the tree is a development copy,
# and an extension whose version disagrees with the roost it talks to is the
# first question a bug report should be able to answer from chrome://extensions.
# Chrome's `version` takes one to four dot-separated integers and nothing else,
# so a prerelease's suffix goes in `version_name`, which is what Chrome shows.
#
# The zip is deterministic — sorted entries, one fixed timestamp — so the same
# commit and version always give the same bytes, as the other release
# artifacts do.
#
# Two zips from the one tree. roost-extension-<version>.zip keeps everything
# under a roost-extension/ folder, because unzipping it should give a folder to
# point "Load unpacked" at. The Chrome Web Store refuses exactly that layout
# ("manifest file is missing"): it wants manifest.json at the root. So
# roost-extension-<version>-webstore.zip is the same files at the root, minus
# the README, which a store package has no reader for. extension/store/ — the
# listing's text and pictures — goes in neither.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib.sh

VERSION=${1:?usage: package-extension.sh <version> [out-dir]}
OUT=${2:-target/distrib}

need python3 "install python3"
mkdir -p "$OUT"

python3 -I - "$VERSION" "$OUT" <<'EOF'
import json, re, sys, zipfile
from pathlib import Path

version, out = sys.argv[1], Path(sys.argv[2])
m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?", version)
if not m:
    sys.exit(f"  ✗ {version!r} is not a release version (want 1.2.3 or 1.2.3-rc.1)")
if any(int(p) > 65535 for p in m.groups()):
    sys.exit(f"  ✗ {version}: Chrome caps each version part at 65535")

src = Path("extension")
manifest = json.loads((src / "manifest.json").read_text())
manifest["version"] = ".".join(m.groups())
manifest["version_name"] = version

# Every file the manifest names must be in the zip: Chrome refuses to load an
# extension with a missing icon or page, and says so only to whoever installs it.
named = set(manifest.get("icons", {}).values())
named |= set(manifest.get("action", {}).get("default_icon", {}).values())
named |= {manifest["background"]["service_worker"], manifest["action"]["default_popup"], manifest["options_ui"]["page"]}
missing = sorted(n for n in named if not (src / n).is_file())
if missing:
    sys.exit(f"  ✗ the manifest names files extension/ does not have: {', '.join(missing)}")

files = sorted(
    p for p in src.rglob("*")
    if p.is_file() and p.name != "manifest.json" and p.relative_to(src).parts[0] != "store"
)
stamped = json.dumps(manifest, indent=2, ensure_ascii=False) + "\n"

def pack(dest, prefix, skip=()):
    n = 0
    with zipfile.ZipFile(dest, "w", zipfile.ZIP_DEFLATED) as z:
        def put(name, data):
            info = zipfile.ZipInfo(f"{prefix}{name}", date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            z.writestr(info, data)
        put("manifest.json", stamped)
        n += 1
        for p in files:
            rel = p.relative_to(src).as_posix()
            if rel in skip:
                continue
            put(rel, p.read_bytes())
            n += 1
    return n

dest = out / f"roost-extension-{version}.zip"
n = pack(dest, "roost-extension/")
print(f"  ✓ {dest} ({n} files, manifest version {manifest['version']}, shown as {version})")
store = out / f"roost-extension-{version}-webstore.zip"
n = pack(store, "", skip={"README.md"})
print(f"  ✓ {store} ({n} files at the root, for the Chrome Web Store)")
EOF
