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

files = sorted(p for p in src.rglob("*") if p.is_file() and p.name != "manifest.json")
dest = out / f"roost-extension-{version}.zip"
with zipfile.ZipFile(dest, "w", zipfile.ZIP_DEFLATED) as z:
    def put(name, data):
        info = zipfile.ZipInfo(f"roost-extension/{name}", date_time=(1980, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        info.external_attr = 0o644 << 16
        z.writestr(info, data)
    put("manifest.json", json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
    for p in files:
        put(p.relative_to(src).as_posix(), p.read_bytes())
print(f"  ✓ {dest} ({len(files) + 1} files, manifest version {manifest['version']}, shown as {version})")
EOF
