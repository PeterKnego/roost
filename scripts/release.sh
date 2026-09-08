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
# The sed below matches the first `version = "..."` line and assumes it is
# the only one — the package's own. Checked here rather than assumed: a
# second match earlier in the file would make `0,/re/` touch the wrong line
# and this whole block would bump, commit and tag against unchanged content.
VERSION_LINES=$(grep -c '^version = "' Cargo.toml)
[ "$VERSION_LINES" = 1 ] || die "Cargo.toml has $VERSION_LINES lines matching '^version = \"...\"', expected exactly 1"

# Bump, commit and tag are each made idempotent so a re-run after a partial
# failure (tag push rejected, network drop, the operator's Ctrl-C) resumes
# instead of dying at a `git commit` with nothing staged or a `git tag` that
# already exists — which is the exact recovery story this script exists to
# provide. Each branch below says which it did.
CARGO_LINE=$(grep -m1 '^version = "' Cargo.toml)
if [ "$CARGO_LINE" = "version = \"$VERSION\"" ]; then
  ok "Cargo.toml already at $VERSION"
else
  sed -i "0,/^version = \".*\"$/s//version = \"$VERSION\"/" Cargo.toml
  grep -qx "version = \"$VERSION\"" Cargo.toml || die "the version bump did not take"
  ok "Cargo.toml bumped to $VERSION"
fi
cargo check --quiet
# `ci.yml` runs `cargo build --locked`, which fails on a lock file that has
# drifted from Cargo.toml — and by the time that happens the tag pushed below
# is already public. `cargo check` just above refreshes the lock as a side
# effect; this confirms it actually landed rather than trusting that it did.
cargo metadata --locked --format-version 1 >/dev/null 2>&1 || die "Cargo.lock did not stay in sync after the version bump"
ok "Cargo.lock in sync"

git add Cargo.toml Cargo.lock
if git diff --cached --quiet; then
  # Nothing staged: either a prior run already made the bump commit, or
  # something else put Cargo.toml at this version. Only the former is safe
  # to resume from, so check HEAD itself rather than assume.
  HEAD_CARGO_LINE=$(git show HEAD:Cargo.toml | grep -m1 '^version = "')
  if [ "$HEAD_CARGO_LINE" = "version = \"$VERSION\"" ]; then
    ok "release: $VERSION already committed"
  else
    die "Cargo.toml is at $VERSION but nothing is staged and HEAD is not the release commit — resolve by hand"
  fi
else
  git commit -q -m "release: $VERSION"
  ok "committed the version bump"
fi

if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  TAG_SHA=$(git rev-parse "$TAG^{commit}")
  HEAD_SHA=$(git rev-parse HEAD)
  if [ "$TAG_SHA" = "$HEAD_SHA" ]; then
    ok "tag $TAG already exists locally and points at HEAD"
  else
    die "tag $TAG already exists but points at $TAG_SHA, not HEAD ($HEAD_SHA) — re-pointing a tag is forbidden, resolve by hand"
  fi
else
  git tag -a "$TAG" -m "roost $VERSION"
  ok "tagged $TAG"
fi

# `git push` of a ref the remote already has at the same commit is a no-op
# (exit 0, "Everything up-to-date") rather than an error — confirmed against
# a real remote while writing this fix, not assumed. So re-running after
# master was already pushed does not fail here.
git push -q origin master
ok "master pushed"

phase ci
# `gh run list --limit 1` returns the newest run of the workflow, which need
# not be the one this push triggers — a concurrent or leftover run would be
# picked and its unrelated conclusion trusted instead. `headBranch` for a
# tag-triggered run is the tag itself (measured: `gh run list --json
# headBranch` against this repo's own v0.5.2-rc.1 run reports headBranch
# "v0.5.2-rc.1"), so filtering on it — not on when the run was created —
# ties every poll below to the run this specific tag caused, and does so
# whether that run was dispatched by this invocation or an earlier one.
#
# That second case is what makes this resumable: `git push` of a ref the
# remote already has at the same commit is a no-op (confirmed against a real
# remote), so on a re-run after the tag was already pushed, this push does
# not dispatch a new run — there is no new run to wait for. A DISPATCHED_AT
# timestamp captured now, after that earlier run started, would filter the
# existing run out and this would wait 3 minutes for one that never comes.
# Matching on the tag instead finds the run that already exists and
# re-attaches to it, whatever its status.
git push -q origin "$TAG"
ok "tag $TAG pushed (or already up to date)"

list_release_run() {
  gh run list --workflow=Release --limit 10 \
    --json databaseId,conclusion,status,createdAt,headBranch \
    --jq "[.[] | select(.headBranch == \"$TAG\")] | sort_by(.createdAt) | last"
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
