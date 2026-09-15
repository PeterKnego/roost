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
#
# "Looks like a release bump" (right subject, right Cargo.toml) is not
# narrow enough on its own: a commit with that shape sitting on top of other
# unpushed work would still read as resumable, and `git push origin master`
# would carry that other work along with it — exactly the collision this
# check exists to prevent, just moved one commit deeper. So $1 must be
# EITHER origin/master itself OR exactly one commit ahead of it (its parent
# must BE origin/master, not merely some ancestor of it) — not "some commit
# whose tip happens to look right".
#
# Both shapes are real: the tag-side caller can be asked about a commit
# after master's own push already succeeded (tag push is what failed), at
# which point origin/master (fetched fresh above) now points AT the bump
# commit rather than at its parent — "$1 == $ORIGIN_SHA" catches that case.
# Checked by reverting to a parent-only comparison and reproducing: it dies
# claiming a genuine resume "is not this release's own bump commit" the
# moment master has already been pushed, which is exactly the resumable
# state this whole check exists to let through.
is_release_bump_commit() {
  [ -n "$VERSION" ] || return 1
  if [ "$1" != "$ORIGIN_SHA" ]; then
    PARENT_SHA=$(git rev-parse -q --verify "$1^" 2>/dev/null) || return 1
    [ "$PARENT_SHA" = "$ORIGIN_SHA" ] || return 1
  fi
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
  # Distinguish "HEAD looks like a release commit but fails the ancestry
  # test" from a plain unrelated divergence, so the message names what was
  # actually found instead of the generic mismatch both cases share.
  if [ -n "$VERSION" ] && git show "$HEAD_SHA:Cargo.toml" 2>/dev/null | grep -qx "version = \"$VERSION\"" \
     && [ "$(git log -1 --format=%s "$HEAD_SHA" 2>/dev/null)" = "release: $VERSION" ]; then
    AHEAD=$(git rev-list --count "origin/master..$HEAD_SHA")
    die "HEAD looks like $VERSION's release commit but is $AHEAD commit(s) ahead of origin/master, not 1 — something else is riding along, resolve by hand"
  fi
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

# NOT CHECKED HERE: that the crates.io Trusted Publisher is registered for
# this repo/workflow. crates.io does have an API for it
# (/api/v1/trusted_publishing/github_configs?crate=roost), but unlike the
# plain version-lookup above it requires an authenticated request — measured:
# unauthenticated, it 403s with "this action requires authentication".
# Automating this check would mean preflight holding a crates.io API token,
# which is exactly the stored, mistypeable secret Trusted Publishing was
# adopted to avoid for the publish job itself. Confirm this by hand, in the
# crates.io UI, before cutting a release.

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
# `--ref master` is load-bearing, and its absence fails in a way that reads as
# something else. check-tap-token.yml carries `environment: release`, and that
# environment allows only master, v* and *.*.* — deliberately, since develop
# permits a self-merged PR at zero approvals and so is not a trusted ref for a
# token. A bare `gh workflow run` targets the repository's *default* branch,
# which is develop, so the run is refused at the environment gate before a
# single step executes. It then produces no logs at all — `gh run view --log`
# returns nothing — and the only evidence is a run annotation reading "Branch
# develop is not allowed to deploy to release". Measured on this repo: run
# 34977527251 (no --ref) came back headBranch=develop, refused; run
# 34977766127 (--ref master) came back headBranch=master and actually ran.
#
# The comment this replaces claimed the block was "unreachable from a branch
# other than master: workflow_dispatch 404s unless the workflow is on the
# repository's default branch". That was true while master *was* the default.
# The two-branch flow made develop the default and inverted the failure: the
# dispatch now succeeds and runs on the wrong ref instead of 404ing.
DISPATCHED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
gh workflow run check-tap-token.yml --ref master >/dev/null
say "dispatched tap token check…"
sleep 20

# `gh run list --limit 1` returns the newest run of the workflow, which need
# not be the one just dispatched — a concurrent or leftover run would be
# picked and its stale conclusion trusted instead. Filtering to runs created
# at or after DISPATCHED_AT, then taking the newest of those, ties the poll
# to the run this invocation actually triggered.
list_dispatched_run() {
  gh run list --workflow=check-tap-token.yml --limit 10 \
    --json databaseId,conclusion,status,createdAt,url \
    --jq "[.[] | select(.createdAt >= \"$DISPATCHED_AT\")] | sort_by(.createdAt) | last"
}
RUN=$(list_dispatched_run)
if [ -z "$RUN" ] || [ "$RUN" = null ]; then die "could not find the dispatched tap token check run"; fi

# Bounded: an unattended release (Task 10) must not hang forever if the run
# never completes. 180 tries at 5s apart is 15 minutes.
#
# It was 5, which was right for a run that starts executing the moment it is
# dispatched. The `release` environment now requires a review, so the run sits
# in `waiting` until a human approves it, and the bound became a race against
# the operator's attention rather than a guard against a hung job. Fifteen
# minutes is still a bound — this must not wait forever — but it is long
# enough that approving is not a sprint.
#
# A `waiting` run is announced once, with its URL, because the failure it
# otherwise produces is indistinguishable from a broken token: preflight would
# die saying the check "did not complete" while the run was merely unapproved,
# and the operator would go looking at the tap credential.
TRIES=0
ANNOUNCED_WAIT=false
while [ "$(echo "$RUN" | jq -r .status)" != completed ]; do
  if [ "$(echo "$RUN" | jq -r .status)" = waiting ] && [ "$ANNOUNCED_WAIT" = false ]; then
    ANNOUNCED_WAIT=true
    say "  the release environment needs your approval before this can run:"
    say "  $(echo "$RUN" | jq -r .url)"
  fi
  TRIES=$((TRIES + 1))
  [ "$TRIES" -le 180 ] || die "tap token check did not complete within 15 minutes — see run $(echo "$RUN" | jq -r .databaseId)"
  sleep 5
  RUN=$(list_dispatched_run)
  if [ -z "$RUN" ] || [ "$RUN" = null ]; then die "could not find the dispatched tap token check run"; fi
done
[ "$(echo "$RUN" | jq -r .conclusion)" = success ] || die "tap token check failed — see run $(echo "$RUN" | jq -r .databaseId)"
ok "tap token valid"
