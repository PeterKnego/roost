#!/usr/bin/env bash
# One command, four phases. Never builds a shipped artifact and never deletes a
# tag or a release: tags are mirrored and cached outside this repository, so a
# stale release is recoverable and a re-pointed tag is not.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib.sh

case "${1:-}" in
  check)  exec scripts/preflight.sh ;;
  verify) shift; exec scripts/verify-release.sh "${1:?usage: release.sh verify <version>}" ;;
esac

VERSION=${1:?usage: release.sh <version> | check | verify <version>}
TAG="v$VERSION"
case "$VERSION" in *-*) PRERELEASE=true ;; *) PRERELEASE=false ;; esac

scripts/preflight.sh "$VERSION"

phase release
# Matches the first `version = "..."` line regardless of what it currently
# holds — Cargo.toml has exactly one (checked above), the package's own, so
# there is nothing here that assumes a particular prior version. The grep
# below is the real guarantee: it re-reads the file afterward instead of
# trusting the sed matched.
sed -i "0,/^version = \".*\"$/s//version = \"$VERSION\"/" Cargo.toml
cargo check --quiet
grep -qx "version = \"$VERSION\"" Cargo.toml || die "the version bump did not take"
# `ci.yml` runs `cargo build --locked`, which fails on a lock file that has
# drifted from Cargo.toml — and by the time that happens the tag pushed below
# is already public. `cargo check` just above refreshes the lock as a side
# effect; this confirms it actually landed rather than trusting that it did.
cargo metadata --locked --format-version 1 >/dev/null 2>&1 || die "Cargo.lock did not stay in sync after the version bump"
ok "Cargo.toml and Cargo.lock at $VERSION"
git add Cargo.toml Cargo.lock
git commit -q -m "release: $VERSION"
git tag -a "$TAG" -m "roost $VERSION"
git push -q origin master
ok "master pushed"

phase ci
# `gh run list --limit 1` returns the newest run of the workflow, which need
# not be the one this push triggers — a concurrent or leftover run would be
# picked and its unrelated conclusion trusted instead. Recording the time
# before the tag goes up, then filtering to runs created at or after it and
# taking the newest of those, ties every poll below to the run this push
# actually caused. Same defect and same fix as preflight.sh's tap-token check.
DISPATCHED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
git push -q origin "$TAG"
ok "tagged $TAG and pushed"

list_release_run() {
  gh run list --workflow=Release --limit 10 \
    --json databaseId,conclusion,status,createdAt \
    --jq "[.[] | select(.createdAt >= \"$DISPATCHED_AT\")] | sort_by(.createdAt) | last"
}

sleep 10
RUN=$(list_release_run)
TRIES=0
while [ -z "$RUN" ] || [ "$RUN" = null ]; do
  TRIES=$((TRIES + 1))
  [ "$TRIES" -le 18 ] || die "no Release run appeared for $TAG within 3 minutes — check the Actions tab"
  sleep 10
  RUN=$(list_release_run)
done
RUN_ID=$(echo "$RUN" | jq -r .databaseId)
say "run $RUN_ID"

# Bounded: an unattended release must not hang forever if the run never
# completes. 180 tries at 30s apart is 90 minutes.
STATUS=$(echo "$RUN" | jq -r .status)
TRIES=0
while [ "$STATUS" != completed ]; do
  TRIES=$((TRIES + 1))
  [ "$TRIES" -le 180 ] || die "release run $RUN_ID did not complete within 90 minutes — see: gh run view $RUN_ID"
  sleep 30
  RUN=$(list_release_run)
  if [ -z "$RUN" ] || [ "$RUN" = null ]; then die "lost track of release run $RUN_ID"; fi
  STATUS=$(echo "$RUN" | jq -r .status)
done
CONCLUSION=$(echo "$RUN" | jq -r .conclusion)
if [ "$CONCLUSION" != success ]; then
  gh run view "$RUN_ID" --json jobs --jq '.jobs[] | select(.conclusion=="failure") | "  failed: \(.name)"'
  say ""
  say "The tag and any uploaded assets are left in place on purpose."
  say "Fix the cause, then re-run only what failed:"
  say "  gh run rerun $RUN_ID --failed"
  die "release run $RUN_ID failed"
fi
ok "release run succeeded"

if [ "$PRERELEASE" = true ]; then
  phase verify
  say "prerelease: the tap and crates.io are skipped by design, so verification is partial"
  ok "prerelease $VERSION built; artifacts are on the release page"
  exit 0
fi

scripts/verify-release.sh "$VERSION"
printf '\n%srelease %s live on all channels%s\n' "$BOLD" "$VERSION" "$OFF"
