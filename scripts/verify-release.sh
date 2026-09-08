#!/usr/bin/env bash
# Reads every channel back from the outside. A green CI run says the jobs
# exited 0; it does not say the shipped binary can load on the machines it is
# aimed at. Only running the published artifact says that.
#
# Every `[ ]` below tests a variable, never a bare `$(...)`. Under
# `set -euo pipefail`, a command substitution that fails still lets the `[ ]`
# it sits inside run against whatever it produced (empty, or partial output) —
# errexit does not see a failing substitution unless it is the whole of a
# simple command. Splitting each into `VAR=$(cmd) || die ...` then `[ "$VAR" =
# ... ]` turns "gh/curl failed" into "script stops with a reason", not "check
# silently reads the failure as a pass". preflight.sh has the same rule, with
# the same three failures being the reason it exists there.
#
# This script opens the actual contents of exactly one of the release's
# artifacts: the x86_64 linux-musl tarball. The aarch64 linux-musl binary and
# both macOS binaries are checked by filename and by the sha256 the tap
# formula and sha256.sum claim for them, never by running them — aarch64
# would need qemu-user-static, and macOS cannot run here at all. A pass below
# is a verdict on the release's metadata and on one real binary, not a verdict
# that every published binary loads on its target.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib.sh

VERSION=${1:?usage: verify-release.sh <version>}
TAG="v$VERSION"
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT

phase verify
need gh "install the GitHub CLI"; need jq "apt install jq"
need curl "apt install curl"; need tar "apt install tar"
need sha256sum "apt install coreutils"; need base64 "apt install coreutils"
need readelf "apt install binutils"; need objdump "apt install binutils"

DRAFT=$(gh release view "$TAG" --json isDraft --jq .isDraft) \
  || die "could not read $TAG from GitHub — does it exist?"
[ "$DRAFT" = false ] || die "$TAG is still a draft"
ASSETS=$(gh release view "$TAG" --json assets --jq '.assets[].name') \
  || die "could not list assets on $TAG"
for want in \
  roost-x86_64-unknown-linux-musl.tar.xz roost-aarch64-unknown-linux-musl.tar.xz \
  roost-x86_64-apple-darwin.tar.xz roost-aarch64-apple-darwin.tar.xz \
  roost-installer.sh roost.rb sha256.sum; do
  echo "$ASSETS" | grep -qx "$want" || die "release is missing $want"
done
echo "$ASSETS" | grep -qE '\.deb$' || die "release has no .deb"
echo "$ASSETS" | grep -qE '\.rpm$' || die "release has no .rpm"
ok "release published with every expected asset"

FORMULA=$(gh api repos/PeterKnego/homebrew-tap/contents/Formula/roost.rb --jq .content | base64 -d) \
  || die "could not fetch the tap formula"
echo "$FORMULA" | grep -qx "  version \"$VERSION\"" || die "the tap formula is not at $VERSION"
echo "$FORMULA" | grep -qx '  depends_on "dtach"' || die "the formula lost depends_on dtach"
ok "tap formula at $VERSION, still declaring dtach"

# The version line and depends_on are not what `brew install` fetches — the
# per-platform `url`/`sha256` pairs are, and a stale or wrong sha256 there
# breaks installation for every Homebrew user while every check above still
# passes. Verified against the real cargo-dist-generated formula (fetched
# during this fix) that each `sha256` line is the line immediately following
# its `url` line, for all four platform blocks, with nothing else between
# them — so pairing "most recently seen url" with "next sha256" is exact for
# this generator's output, not a guess. If a future formula ever breaks that
# adjacency, an artifact would end up with no paired hash and the die below
# for a missing entry catches it, rather than silently pairing the wrong url.
declare -A FORMULA_SHA=()
URL_LINE=
while IFS= read -r LINE; do
  case "$LINE" in
    *'url "'*)
      URL_LINE=$LINE
      ;;
    *'sha256 "'*)
      if [ -n "$URL_LINE" ]; then
        ARTIFACT=${URL_LINE##*/}; ARTIFACT=${ARTIFACT%\"}
        SHA=${LINE#*sha256 \"}; SHA=${SHA%\"}
        FORMULA_SHA["$ARTIFACT"]=$SHA
        URL_LINE=
      fi
      ;;
  esac
done <<< "$FORMULA"

# sha256.sum is the release's own authoritative hash list, and downloading it
# — one small text asset — is what lets this check cover all four platform
# artifacts without fetching and re-hashing four tarballs just to check a
# hash the release already publishes.
gh release download "$TAG" -D "$TMP" -p 'sha256.sum' >/dev/null \
  || die "could not download sha256.sum from $TAG"
declare -A RELEASE_SHA=()
# `read` returns false at EOF rather than yielding one final empty $LINE, so
# a well-formed sha256.sum (trailing newline and all) never hits this guard.
# It is here for the file that is not well-formed: a hand-edited or
# corrupted sha256.sum with a genuine blank line would assign into
# ARRAY[""], which does abort the script under `set -e` — but with bash's
# own "bad array subscript", naming neither this script nor what was wrong
# with the input. The guard buys a clearer failure, not protection from one
# that would otherwise slip through.
while IFS= read -r LINE; do
  [ -n "$LINE" ] || continue
  HASH=${LINE%% *}
  RELEASE_SHA["${LINE#*\*}"]=$HASH
done < "$TMP/sha256.sum"

for artifact in \
  roost-x86_64-unknown-linux-musl.tar.xz roost-aarch64-unknown-linux-musl.tar.xz \
  roost-x86_64-apple-darwin.tar.xz roost-aarch64-apple-darwin.tar.xz; do
  WANT=${RELEASE_SHA[$artifact]:-}
  GOT=${FORMULA_SHA[$artifact]:-}
  [ -n "$WANT" ] || die "sha256.sum has no entry for $artifact"
  [ -n "$GOT" ] || die "tap formula has no url/sha256 pair for $artifact"
  [ "$WANT" = "$GOT" ] \
    || die "tap formula sha256 for $artifact is $GOT, but the release's sha256.sum says $WANT"
done
ok "tap formula hashes match the release's sha256.sum for all four platform artifacts"

# A prerelease publishes to neither crates.io nor the tap — release.sh only
# calls this script for non-prereleases, so running it by hand against a
# prerelease tag is expected to die here. That is the checks working, not a
# bug in them.
NEWEST=$(curl -sS -H 'User-Agent: roost-release (peter@knego.net)' \
  https://crates.io/api/v1/crates/roost | jq -r '.crate.newest_version') \
  || die "could not determine crates.io's newest version — curl or jq failed"
[ "$NEWEST" = "$VERSION" ] || die "crates.io newest is $NEWEST, not $VERSION"
ok "crates.io at $VERSION"

gh release download "$TAG" -D "$TMP" \
  -p 'roost-x86_64-unknown-linux-musl.tar.xz*' >/dev/null \
  || die "could not download the linux-musl tarball from $TAG"
( cd "$TMP" && sha256sum -c roost-x86_64-unknown-linux-musl.tar.xz.sha256 >/dev/null ) \
  || die "published checksum does not match the published tarball"
ok "published checksum verifies"

# gh 2.46 has no `attestation` subcommand, so this goes through the REST API.
# It confirms an attestation exists and covers this artifact; it does NOT
# verify the signature — that needs a newer gh or cosign, neither present here.
DIGEST=$(sha256sum "$TMP/roost-x86_64-unknown-linux-musl.tar.xz" | cut -d' ' -f1) \
  || die "could not hash the downloaded tarball"
# gh api exits nonzero on a 404, which is exactly what "no attestation for
# this digest" looks like — so a failed query and a genuinely missing
# attestation both land here. Both mean "cannot show this artifact came from
# the release workflow", so both die; there is no case where treating the
# query failure as a pass would be correct.
COUNT=$(gh api "repos/PeterKnego/roost/attestations/sha256:$DIGEST" --jq '.attestations | length') \
  || die "no attestation covers the published tarball (query failed or none exist)"
[ "$COUNT" -ge 1 ] || die "no attestation covers the published tarball"
ok "attestation covers the published tarball (existence, not signature)"

tar -xJf "$TMP/roost-x86_64-unknown-linux-musl.tar.xz" -C "$TMP" \
  || die "could not extract the published tarball"
BIN="$TMP/roost-x86_64-unknown-linux-musl/roost"
[ -f "$BIN" ] || die "extracted archive has no roost binary at the expected path"

# readelf -d exits 0 on a genuinely static binary (it just reports no dynamic
# section) and only exits nonzero when it could not read the file at all — so
# capturing its exit status distinguishes "checked, and it is static" from
# "could not check". Piping straight into `grep -q needed && die`, as an
# earlier draft of this script did, would not: a failed readelf produces no
# output, grep finds no "needed" in nothing, and `&&` never fires — a broken
# tool or an unreadable binary would silently read as "verified static".
READELF_OUT=$(readelf -d "$BIN" 2>&1) || die "readelf could not read the extracted binary"
echo "$READELF_OUT" | grep -qi needed && die "the published binary is dynamically linked"

# Measured on the published v0.5.1 roost-x86_64-unknown-linux-musl binary —
# the exact artifact this check examines: it is static-pie linked (`file`
# reports "static-pie linked"), and `objdump -T` on it exits 0 with an empty
# dynamic symbol table, same as `readelf -d` above. A plain (non-PIE) static
# binary is a different shape: the published aarch64 artifact, which this
# script does not check, is "statically linked" (no PIE) and `objdump -T`
# exits 1 on it ("not a dynamic object"), while `readelf -d` still exits 0.
# So objdump's exit status is not a reliable signal for "static" in general —
# it varies with how the binary was linked, not just whether it's dynamic —
# and `|| true` is defensive against that variance on artifacts this check
# might grow to cover, not a workaround for anything seen on this one. The
# readelf check just above already proved $BIN is a real, readable ELF file;
# `grep -q GLIBC` below is what actually decides this check.
OBJDUMP_OUT=$(objdump -T "$BIN" 2>&1) || true
echo "$OBJDUMP_OUT" | grep -q GLIBC && die "the published binary references GLIBC"

BIN_VERSION=$("$BIN" --version 2>&1) || die "the published binary failed to run: $BIN_VERSION"
[ "$BIN_VERSION" = "roost $VERSION" ] || die "the published binary reports the wrong version (got: $BIN_VERSION)"
ok "published binary is static, GLIBC-free, and reports $VERSION"
