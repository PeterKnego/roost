#!/usr/bin/env bash
# Every check here is a failure this project actually had while cutting v0.5.1.
# Nothing mutates: a preflight failure means nothing has happened yet.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib.sh

VERSION=${1:-}
# Must stay alphabetically sorted — compared byte-for-byte against ACTUAL below, which is sorted.
EXPECTED_TARGETS='aarch64-apple-darwin aarch64-unknown-linux-musl x86_64-apple-darwin x86_64-unknown-linux-musl'

phase preflight
need git "install git"; need gh "install the GitHub CLI"; need dist "cargo install cargo-dist"
need jq "apt install jq"; need dtach "apt install dtach"
ok "required tools present"

[ "$(git rev-parse --abbrev-ref HEAD)" = master ] || die "not on master"
[ -z "$(git status --porcelain)" ] || die "working tree is dirty"
git fetch --quiet origin
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/master)" ] || die "master differs from origin/master"
ok "on master, clean, in sync with origin"

# `cargo build --locked` rejects a lock file that has drifted from Cargo.toml,
# and in CI that failure lands on the tag, after the tag is public.
#
# This must run before `dist plan` below: `dist plan` shells out to its own
# unlocked `cargo metadata`, which silently rewrites a drifted Cargo.lock as a
# side effect — proven while writing this check, where adding a dependency
# and running `dist plan` first made this check pass over real drift. Locked
# first, so nothing has touched the lock file yet when it runs.
cargo metadata --locked --format-version 1 >/dev/null 2>&1 || die "Cargo.lock is out of sync — run cargo check"
ok "Cargo.lock in sync"

# dist init -y silently replaces the configured targets with its own
# seven-target default, gnu and windows included. A gnu Linux build reintroduces
# the GLIBC_2.39 load failure on Debian 12, Ubuntu 22.04 and Amazon Linux 2023.
#
# Filtered to kind=="executable-zip": the unfiltered artifact list also
# includes the shell/homebrew installers, each of which lists every triple it
# can *serve* rather than the ones actually built — a plain `unique` over all
# artifacts returns ten triples, gnu and musl variants both, and the check
# would pass with a gnu Linux binary in the build, which is the exact
# regression it exists to catch. This is compared for exact set equality, not
# "each expected target is present", so it also catches an *added* target.
ACTUAL=$(dist plan --output-format=json | jq -r '[.artifacts[] | select(.kind=="executable-zip") | .target_triples[]] | sort | join(" ")')
[ "$ACTUAL" = "$EXPECTED_TARGETS" ] \
  || die "dist is building [$ACTUAL], expected [$EXPECTED_TARGETS] — check dist-workspace.toml"
ok "dist builds exactly the four expected targets"

if [ -n "$VERSION" ]; then
  git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null && die "tag v$VERSION already exists locally"
  [ -z "$(git ls-remote --tags origin "refs/tags/v$VERSION")" ] || die "tag v$VERSION already exists on origin"
  ok "tag v$VERSION is free"

  # A crates.io version can never be reused, not even after a yank.
  PUBLISHED=$(curl -sS -H 'User-Agent: roost-release (peter@knego.net)' \
    "https://crates.io/api/v1/crates/roost" | jq -r '.versions[].num')
  case "$PUBLISHED" in *"$VERSION"*) die "$VERSION is already on crates.io" ;; esac
  ok "$VERSION is free on crates.io"
fi

# The aarch64 cross-link dies without .cargo/config.toml's linker = "rust-lld",
# and that file looks exactly like dead configuration to anyone tidying up.
for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
  cargo build --locked --profile dist --target "$t" >/dev/null 2>&1 || die "$t does not build"
done
ok "both musl targets build"

# Single-threaded is deliberate: the suite shares a process-wide session
# registry and a bare `cargo test` has hung on it.
TMPDIR=/tmp cargo test --locked -- --test-threads=1 >/dev/null 2>&1 || die "tests failed"
ok "tests pass"

# A credential used only by CI can only be tested by CI.
gh workflow run check-tap-token.yml >/dev/null
say "dispatched tap token check…"
sleep 20
RUN=$(gh run list --workflow=check-tap-token.yml --limit 1 --json databaseId,conclusion,status --jq '.[0]')
while [ "$(echo "$RUN" | jq -r .status)" != completed ]; do
  sleep 5
  RUN=$(gh run list --workflow=check-tap-token.yml --limit 1 --json databaseId,conclusion,status --jq '.[0]')
done
[ "$(echo "$RUN" | jq -r .conclusion)" = success ] || die "tap token check failed — see run $(echo "$RUN" | jq -r .databaseId)"
ok "tap token valid"
