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
need jq "apt install jq"; need dtach "apt install dtach"; need curl "apt install curl"
need cargo "install rust"
ok "required tools present"

# release.sh's bump/commit/tag block is idempotent so a re-run after a
# partial push resumes instead of dying — but only if preflight lets it
# reach that block at all. This tells the two checks below (master-sync and
# the tag check) "is $1 a commit release.sh itself already made for exactly
# this version bump", so a resume can tell that apart from a genuine
# collision, without weakening either check for a fresh release.
is_release_bump_commit() {
  [ -n "$VERSION" ] || return 1
  git show "$1:Cargo.toml" 2>/dev/null | grep -qx "version = \"$VERSION\"" \
    && [ "$(git log -1 --format=%s "$1" 2>/dev/null)" = "release: $VERSION" ]
}

[ "$(git rev-parse --abbrev-ref HEAD)" = master ] || die "not on master"

# Assigned first, then tested: `[ -z "$(cmd)" ]` does NOT abort under `set -e`
# when cmd fails — a failing command substitution nested inside `[ ]` is not a
# simple command, so errexit does not see it, and the test just runs against
# an empty string. A plain assignment DOES abort on a failing substitution, so
# splitting the assignment out of the `[ ]` turns "git failed" into "script
# stops", not "check silently reads as passing". Do not fold this back into
# `[ -z "$(git status --porcelain)" ]` — that reintroduces the fail-open bug.
DIRTY=$(git status --porcelain)
[ -z "$DIRTY" ] || die "working tree is dirty"
git fetch --quiet origin
HEAD_SHA=$(git rev-parse HEAD)
ORIGIN_SHA=$(git rev-parse origin/master)
if [ "$HEAD_SHA" = "$ORIGIN_SHA" ]; then
  ok "on master, clean, in sync with origin"
elif is_release_bump_commit "$HEAD_SHA"; then
  # HEAD is ahead of origin/master by exactly $VERSION's own bump commit —
  # the state release.sh leaves things in if it dies before "master pushed"
  # completes. That is the master-push partial failure this check must let
  # through; anything else ahead of origin for any other reason still dies.
  ok "on master, HEAD is $VERSION's own unpushed release commit — resuming"
else
  die "master differs from origin/master"
fi

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
ACTUAL=$(dist plan --output-format=json | jq -r '[.artifacts[] | select(.kind=="executable-zip") | .target_triples[]] | sort | join(" ")') \
  || die "could not determine dist's target list — dist plan failed"
[ "$ACTUAL" = "$EXPECTED_TARGETS" ] \
  || die "dist is building [$ACTUAL], expected [$EXPECTED_TARGETS] — check dist-workspace.toml"
ok "dist builds exactly the four expected targets"

if [ -n "$VERSION" ]; then
  # A local tag is not automatically a collision: release.sh creates the tag
  # before either push, so both partial-failure windows (master push failed,
  # tag push failed) leave one behind for the release currently in
  # progress. Distinguish that from a genuine collision by what the tag
  # actually points at, not by whether it exists.
  TAG_IS_OWN_BUMP=false
  if git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
    TAG_SHA=$(git rev-parse "refs/tags/v$VERSION^{commit}")
    if is_release_bump_commit "$TAG_SHA"; then
      TAG_IS_OWN_BUMP=true
    else
      die "tag v$VERSION already exists locally but is not this release's own bump commit — re-pointing a tag is forbidden, resolve by hand"
    fi
  fi

  REMOTE_TAG=$(git ls-remote --tags origin "refs/tags/v$VERSION")
  if [ -n "$REMOTE_TAG" ] && [ "$TAG_IS_OWN_BUMP" != true ]; then
    # Origin has this tag but our local repo either has no tag at all or one
    # that didn't match above (and already died there) — either way this
    # push did not come from resuming a release in this checkout.
    die "tag v$VERSION already exists on origin — resolve by hand"
  fi

  if [ "$TAG_IS_OWN_BUMP" = true ]; then
    ok "tag v$VERSION already exists and is this release's own bump commit — resuming"
  else
    ok "tag v$VERSION is free"
  fi

  # A crates.io version can never be reused, not even after a yank.
  PUBLISHED=$(curl -sS -H 'User-Agent: roost-release (peter@knego.net)' \
    "https://crates.io/api/v1/crates/roost" | jq -r '.versions[].num') \
    || die "could not determine published crates.io versions — curl or jq failed"
  # Whole-line match, not substring: `case "$PUBLISHED" in *"$VERSION"*)` would
  # match VERSION=0.5.1 against a published 0.5.10, refusing a version that is
  # genuinely free. jq emits one version per line, so grep -Fxq (fixed string,
  # whole line) is exact.
  echo "$PUBLISHED" | grep -Fxq "$VERSION" && die "$VERSION is already on crates.io"
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
#
# This whole block is unreachable from a branch other than master:
# workflow_dispatch 404s unless the workflow is on the repository's default
# branch. It cannot be exercised until this branch merges.
DISPATCHED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
gh workflow run check-tap-token.yml >/dev/null
say "dispatched tap token check…"
sleep 20

# `gh run list --limit 1` returns the newest run of the workflow, which need
# not be the one just dispatched — a concurrent or leftover run would be
# picked and its stale conclusion trusted instead. Filtering to runs created
# at or after DISPATCHED_AT, then taking the newest of those, ties the poll
# to the run this invocation actually triggered.
list_dispatched_run() {
  gh run list --workflow=check-tap-token.yml --limit 10 \
    --json databaseId,conclusion,status,createdAt \
    --jq "[.[] | select(.createdAt >= \"$DISPATCHED_AT\")] | sort_by(.createdAt) | last"
}
RUN=$(list_dispatched_run)
if [ -z "$RUN" ] || [ "$RUN" = null ]; then die "could not find the dispatched tap token check run"; fi

# Bounded: an unattended release (Task 10) must not hang forever if the run
# never completes. 60 tries at 5s apart is 5 minutes.
TRIES=0
while [ "$(echo "$RUN" | jq -r .status)" != completed ]; do
  TRIES=$((TRIES + 1))
  [ "$TRIES" -le 60 ] || die "tap token check did not complete within 5 minutes — see run $(echo "$RUN" | jq -r .databaseId)"
  sleep 5
  RUN=$(list_dispatched_run)
  if [ -z "$RUN" ] || [ "$RUN" = null ]; then die "could not find the dispatched tap token check run"; fi
done
[ "$(echo "$RUN" | jq -r .conclusion)" = success ] || die "tap token check failed — see run $(echo "$RUN" | jq -r .databaseId)"
ok "tap token valid"
