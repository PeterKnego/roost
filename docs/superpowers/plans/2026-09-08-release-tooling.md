# Release Tooling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One command cuts a release — preflight, tag, watch CI, verify every channel — and the release produces `.deb` and `.rpm` packages that install `dtach` for the user.

**Architecture:** Shell scripts under `scripts/`, indexed by a Makefile that holds no build rules. Two of those scripts (`package-deb.sh`, `package-rpm.sh`) are also the body of a CI job, so packaging logic runs identically locally and in the release. cargo-dist keeps ownership of tarballs, checksums, attestations, the installer and the formula; three new reusable workflows extend it.

**Tech Stack:** bash, GNU make, `cargo-deb` 3.8.0, `cargo-generate-rpm` 0.21.0, cargo-dist 0.32.0, GitHub Actions, `gh` 2.46.0, shellcheck.

**Spec:** [docs/superpowers/specs/2026-09-08-release-tooling-design.md](../specs/2026-09-08-release-tooling-design.md)

## Global Constraints

- **Scripts never build a shipped binary.** Package scripts take an already-built binary and pack it. `cargo deb --no-build`; `cargo generate-rpm` never builds at all.
- **Scripts never delete a tag or a release.** On CI failure, report and print `gh run rerun <id> --failed`.
- **The four targets are exactly:** `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`.
- **Packages declare `dtach`:** `Depends: dtach` (deb), `Requires: dtach` (rpm).
- **The packaged unit contains `KillMode=process`** and is not enabled on install.
- **Tests run `cargo test --locked -- --test-threads=1`.** Never `cargo test --release`, never a bare `cargo test`.
- **Tag format is `v<version>`**, matching `release.yml`'s `**[0-9]+.[0-9]+.[0-9]+*` trigger.
- **`set -euo pipefail`** at the top of every script.
- **Every script is `shellcheck`-clean** with no suppressions unless the suppression carries a comment saying why.

---

### Task 1: Prove the global-artifacts upload contract

The spec names this as the one unproven assumption, and says to settle it before writing anything else. If a custom global-artifacts job's uploads are not collected by `host`, the packages build green and never reach the release — a silent omission, not an error. A stub answers it in one prerelease.

**Files:**
- Create: `.github/workflows/build-linux-packages.yml` (stub, replaced in Task 5)
- Modify: `dist-workspace.toml`

**Interfaces:**
- Consumes: nothing.
- Produces: a proven (or disproven) answer recorded in the plan, and the job name `build-linux-packages` referenced by Task 5.

- [ ] **Step 1: Write the stub job**

`.github/workflows/build-linux-packages.yml`:

```yaml
# Stub. Task 1 exists only to answer one question: are a custom
# global-artifacts job's uploads collected by dist's `host` job and attached to
# the release? The documented contract ("uploaded to an artifact named
# artifacts") is stated for `build-local-artifacts = false`, which is not our
# configuration, so it is assumed rather than known.
name: Build Linux packages

on:
  workflow_call:
    inputs:
      plan:
        required: true
        type: string

jobs:
  build-linux-packages:
    runs-on: ubuntu-22.04
    steps:
      - name: Produce a marker file
        run: |
          mkdir -p target/distrib
          echo "contract probe" > target/distrib/CONTRACT-PROBE.txt

      - uses: actions/upload-artifact@v4
        with:
          name: artifacts-linux-packages
          path: target/distrib/CONTRACT-PROBE.txt
```

- [ ] **Step 2: Wire it into the global phase**

In `dist-workspace.toml`, after the `publish-jobs` line:

```toml
# Runs in the global phase, not the local one: the local phase is a matrix, so
# a job there sees one target's output. The global phase is the first point at
# which all four binaries exist and can be downloaded.
global-artifacts-jobs = ["./build-linux-packages"]
```

- [ ] **Step 3: Regenerate and confirm the job is called**

```bash
dist generate
grep -n "build-linux-packages" .github/workflows/release.yml
```

Expected: a `uses: ./.github/workflows/build-linux-packages.yml` entry.

- [ ] **Step 4: Commit and cut a throwaway prerelease**

```bash
git add dist-workspace.toml .github/workflows/build-linux-packages.yml
git commit -m "release: probe whether a custom global-artifacts job reaches the release"
```

Then bump `Cargo.toml` + `Cargo.lock` to `0.5.2-rc.1`, commit, tag `v0.5.2-rc.1`, push the tag.

- [ ] **Step 5: Read the answer off the release**

```bash
gh release view v0.5.2-rc.1 --json assets --jq '.assets[].name'
```

Expected if the contract holds: `CONTRACT-PROBE.txt` is among the assets.

**If it is absent**, the contract does not hold as assumed. Do not proceed to Task 5 as written. Investigate how `host` collects artifacts (read the generated `host` job's `download-artifact` pattern in `release.yml`) and adjust the artifact name to match, then re-probe. Record the finding in this file before continuing.

- [ ] **Step 6: Record the outcome in this plan**

Add a line under this task: `Outcome (YYYY-MM-DD): contract holds / does not hold — <evidence>`.

---

### Task 2: Shared script scaffolding, the unit file, and shellcheck

Folded into one task because none of these is independently reviewable: the unit file has no consumer yet, `lib.sh` has no caller, and shellcheck has nothing to check until a script exists. Together they are the floor everything else stands on.

**Files:**
- Create: `packaging/roost.service`
- Create: `scripts/lib.sh`
- Create: `Makefile`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Produces: `lib.sh` exporting `say`, `ok`, `warn`, `die`, `phase`, `need`; `packaging/roost.service` consumed by Tasks 3 and 4.

- [ ] **Step 1: Write the unit file**

`packaging/roost.service`. Derived from the unit running in production on the deploy host, with the three host-specific lines removed: `Environment=ROOST_ROOTS=…` (a package cannot know your directories, and roots can now be added on the front page), `Environment=ROOST_STATE_DIR=…` (a one-host migration pin, documented in deploy.md), and `WorkingDirectory=…`.

```ini
[Unit]
Description=roost — remote web workspace
Documentation=https://github.com/PeterKnego/roost
After=network.target

[Service]
# KillMode=process is load-bearing: roost spawns dtach sessions as children so
# shells survive a restart. systemd's default control-group KillMode kills the
# whole cgroup on stop and takes every session with it, defeating the entire
# reason dtach is used. Verified in production: with the default, a restart
# took `pgrep -c dtach` to 0.
KillMode=process
ExecStart=/usr/bin/roost
Restart=on-failure

[Install]
WantedBy=default.target
```

- [ ] **Step 2: Write `scripts/lib.sh`**

```bash
# Shared vocabulary so every script speaks with one voice: a release is read at
# a glance or not at all.
# shellcheck shell=bash

RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; BOLD=$'\033[1m'; OFF=$'\033[0m'
[ -t 1 ] || { RED=; GREEN=; YELLOW=; BOLD=; OFF=; }

phase() { printf '%s[%s]%s\n' "$BOLD" "$1" "$OFF"; }
ok()    { printf '  %s✓%s %s\n' "$GREEN" "$OFF" "$1"; }
warn()  { printf '  %s!%s %s\n' "$YELLOW" "$OFF" "$1"; }
say()   { printf '  %s\n' "$1"; }
die()   { printf '  %s✗%s %s\n' "$RED" "$OFF" "$1" >&2; exit 1; }

# `need <cmd> <hint>` — a missing tool is a third outcome, not a failed check.
need() { command -v "$1" >/dev/null 2>&1 || die "$1 not found on PATH — $2"; }
```

- [ ] **Step 3: Write the Makefile**

Deliberately a command index. No build rules: the dependency edges differ between local (build, then pack) and CI (download the attested binary, then pack), and encoding them here would create a second, wrong description of how a package is made.

```make
# A command index, not a build graph. See the spec: the real dependency edges
# differ between a local run and CI, so they live in the scripts, once.
.PHONY: help preflight release verify deb rpm test-scripts

help:
	@echo 'make preflight              run every pre-tag check'
	@echo 'make release VERSION=0.5.2  cut a release end to end'
	@echo 'make verify VERSION=0.5.2   read back every channel'
	@echo 'make deb BIN=<path> VERSION=<v> ARCH=<amd64|arm64>'
	@echo 'make rpm BIN=<path> VERSION=<v> ARCH=<x86_64|aarch64>'
	@echo 'make test-scripts           shellcheck + package tests'

preflight:
	@scripts/preflight.sh

release:
	@test -n '$(VERSION)' || { echo 'VERSION= is required'; exit 1; }
	@scripts/release.sh '$(VERSION)'

verify:
	@test -n '$(VERSION)' || { echo 'VERSION= is required'; exit 1; }
	@scripts/verify-release.sh '$(VERSION)'

deb:
	@scripts/package-deb.sh '$(BIN)' '$(VERSION)' '$(ARCH)'

rpm:
	@scripts/package-rpm.sh '$(BIN)' '$(VERSION)' '$(ARCH)'

test-scripts:
	@shellcheck scripts/*.sh scripts/test/*.sh
	@scripts/test/packages.sh
```

- [ ] **Step 4: Add the scripts job to CI**

In `.github/workflows/ci.yml`, after the existing `test` job:

```yaml
  scripts:
    name: shellcheck + package tests
    runs-on: ubuntu-22.04
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@v7

      # rpm is not on ubuntu runners by default; dpkg-deb is.
      - name: Install packaging tools
        run: sudo apt-get update -q && sudo apt-get install -y -q shellcheck rpm

      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2

      - name: Install cargo-deb and cargo-generate-rpm
        run: cargo install cargo-deb cargo-generate-rpm --locked

      # None of this bash is reachable by `cargo test`, and a quoting bug in a
      # release script surfaces at the worst possible moment.
      - name: shellcheck
        run: shellcheck scripts/*.sh scripts/test/*.sh

      - name: Package tests
        run: scripts/test/packages.sh
```

- [ ] **Step 5: Verify shellcheck passes on what exists**

```bash
shellcheck scripts/lib.sh
```

Expected: no output, exit 0.

- [ ] **Step 6: Commit**

```bash
git add packaging/roost.service scripts/lib.sh Makefile .github/workflows/ci.yml
git commit -m "release: the packaged unit, shared script vocabulary, and a command index"
```

---

### Task 3: `package-deb.sh`

**Files:**
- Create: `scripts/package-deb.sh`
- Create: `scripts/test/packages.sh`
- Modify: `Cargo.toml` (add `[package.metadata.deb]`)

**Interfaces:**
- Consumes: `scripts/lib.sh` (`phase`, `ok`, `die`, `need`), `packaging/roost.service`.
- Produces: `scripts/package-deb.sh <binary-path> <version> <arch>` writing `target/distrib/roost_<version>_<arch>.deb` and printing that path on stdout as its last line.

- [ ] **Step 1: Write the failing test**

`scripts/test/packages.sh`:

```bash
#!/usr/bin/env bash
# Opens the produced package rather than checking an exit code. A test that
# asserts only "the script exited 0" passes against an empty package, which is
# the failure mode this repository has been bitten by seven times.
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/lib.sh

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
FIXTURE="$TMP/roost"
printf '#!/bin/sh\necho "roost 9.9.9"\n' > "$FIXTURE"
chmod 755 "$FIXTURE"

phase "deb"
DEB=$(scripts/package-deb.sh "$FIXTURE" 9.9.9 amd64 | tail -1)
[ -f "$DEB" ] || die "no .deb produced at $DEB"

dpkg-deb -I "$DEB" | grep -qE '^ Depends: .*\bdtach\b' \
  || die "the .deb does not declare Depends: dtach"
ok "declares Depends: dtach"

dpkg-deb -c "$DEB" | grep -q './usr/lib/systemd/user/roost.service' \
  || die "the unit is missing from the .deb"
ok "ships the systemd user unit"

dpkg-deb -c "$DEB" | grep -q './usr/bin/roost' || die "no /usr/bin/roost in the .deb"
ok "ships /usr/bin/roost"

# The unit could ship empty or with the default kill mode and every path check
# above would still pass. Assert the property, not the file's existence.
dpkg-deb --fsys-tarfile "$DEB" | tar -xO ./usr/lib/systemd/user/roost.service \
  | grep -q '^KillMode=process$' || die "the packaged unit lacks KillMode=process"
ok "the packaged unit sets KillMode=process"
```

```bash
chmod +x scripts/test/packages.sh
```

- [ ] **Step 2: Run it to verify it fails**

Run: `scripts/test/packages.sh`
Expected: FAIL — `scripts/package-deb.sh: No such file or directory`.

- [ ] **Step 3: Add the deb metadata**

Append to `Cargo.toml`:

```toml
# The package exists to do the one thing a tarball cannot: install dtach. Of
# roost's four Linux install paths only Homebrew declares it today, and
# Linuxbrew is a rounding error on Linux — so every realistic Linux install
# currently leaves the dependency unmet until a terminal fails to spawn.
[package.metadata.deb]
depends = "dtach"
section = "utils"
priority = "optional"
assets = [
    ["target/release/roost", "usr/bin/", "755"],
    ["README.md", "usr/share/doc/roost/README.md", "644"],
    ["LICENSE-MIT", "usr/share/doc/roost/LICENSE-MIT", "644"],
    ["LICENSE-APACHE", "usr/share/doc/roost/LICENSE-APACHE", "644"],
    ["packaging/roost.service", "usr/lib/systemd/user/roost.service", "644"],
]
maintainer-scripts = "packaging/debian/"
```

- [ ] **Step 4: Write the postinst**

`packaging/debian/postinst`:

```bash
#!/bin/sh
set -e
# Not enabled on install, deliberately: roost binds a port and spawns the
# invoking user's shell. A package install that starts that without being asked
# is a surprise nobody wants.
cat <<'MSG'
roost binds 127.0.0.1 and has no authentication of its own — put an auth layer
in front of it before exposing it (see /usr/share/doc/roost/README.md).

Start it with:  systemctl --user enable --now roost
MSG
MSG_EXIT=0
exit $MSG_EXIT
```

```bash
chmod +x packaging/debian/postinst
```

- [ ] **Step 5: Write the script**

`scripts/package-deb.sh`:

```bash
#!/usr/bin/env bash
# Wraps an already-built binary as a .deb. It never compiles: if this script
# built its own binary, the .deb in a release would contain different bytes
# from the .tar.xz in that same release — same version, quietly different
# builds, noticed only when a bug reproduces on one and not the other.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib.sh

BIN=${1:?usage: package-deb.sh <binary> <version> <arch>}
VERSION=${2:?usage: package-deb.sh <binary> <version> <arch>}
ARCH=${3:?usage: package-deb.sh <binary> <version> <arch>}

need cargo-deb "cargo install cargo-deb"
[ -f "$BIN" ] || die "no binary at $BIN"

# cargo-deb reads assets relative to the target dir, so the binary is placed
# where the metadata says rather than passed as a path.
STAGE=target/release
mkdir -p "$STAGE"
install -m 755 "$BIN" "$STAGE/roost"

mkdir -p target/distrib
# --no-strip because the binary is already stripped ([profile.release] strip =
# true) and because the host `strip` cannot process a cross-built aarch64
# binary anyway.
cargo deb --no-build --no-strip \
  --deb-version "$VERSION" \
  --output "target/distrib/roost_${VERSION}_${ARCH}.deb" \
  >/dev/null

ok "built roost_${VERSION}_${ARCH}.deb"
echo "target/distrib/roost_${VERSION}_${ARCH}.deb"
```

```bash
chmod +x scripts/package-deb.sh
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `scripts/test/packages.sh`
Expected: PASS — four `✓` lines under `[deb]`.

- [ ] **Step 7: Revert one property and watch the test fail**

Change `KillMode=process` to `KillMode=control-group` in `packaging/roost.service`, run `scripts/test/packages.sh`, and confirm it fails with "the packaged unit lacks KillMode=process". Restore the file. This proves the assertion reads the packaged content rather than passing vacuously.

- [ ] **Step 8: Commit**

```bash
git add scripts/package-deb.sh scripts/test/packages.sh packaging/debian/postinst Cargo.toml
git commit -m "release: a .deb that declares dtach and ships the unit with KillMode=process"
```

---

### Task 4: `package-rpm.sh`

**Files:**
- Create: `scripts/package-rpm.sh`
- Modify: `scripts/test/packages.sh`
- Modify: `Cargo.toml` (add `[package.metadata.generate-rpm]`)

**Interfaces:**
- Consumes: `scripts/lib.sh`, `packaging/roost.service`.
- Produces: `scripts/package-rpm.sh <binary-path> <version> <arch>` writing `target/distrib/roost-<version>.<arch>.rpm` and printing that path on stdout as its last line.

- [ ] **Step 1: Add the failing test**

Append to `scripts/test/packages.sh`:

```bash
phase "rpm"
RPM=$(scripts/package-rpm.sh "$FIXTURE" 9.9.9 x86_64 | tail -1)
[ -f "$RPM" ] || die "no .rpm produced at $RPM"

rpm -qp --requires "$RPM" 2>/dev/null | grep -qx 'dtach' \
  || die "the .rpm does not require dtach"
ok "requires dtach"

rpm -qpl "$RPM" 2>/dev/null | grep -qx '/usr/lib/systemd/user/roost.service' \
  || die "the unit is missing from the .rpm"
ok "ships the systemd user unit"

rpm -qpl "$RPM" 2>/dev/null | grep -qx '/usr/bin/roost' || die "no /usr/bin/roost in the .rpm"
ok "ships /usr/bin/roost"
```

- [ ] **Step 2: Run it to verify it fails**

Run: `scripts/test/packages.sh`
Expected: the `[deb]` block passes, then FAIL — `scripts/package-rpm.sh: No such file or directory`.

- [ ] **Step 3: Add the rpm metadata**

Append to `Cargo.toml`:

```toml
[package.metadata.generate-rpm]
assets = [
    { source = "target/release/roost", dest = "/usr/bin/roost", mode = "755" },
    { source = "README.md", dest = "/usr/share/doc/roost/README.md", mode = "644" },
    { source = "LICENSE-MIT", dest = "/usr/share/doc/roost/LICENSE-MIT", mode = "644" },
    { source = "LICENSE-APACHE", dest = "/usr/share/doc/roost/LICENSE-APACHE", mode = "644" },
    { source = "packaging/roost.service", dest = "/usr/lib/systemd/user/roost.service", mode = "644" },
]

[package.metadata.generate-rpm.requires]
dtach = "*"
```

- [ ] **Step 4: Write the script**

`scripts/package-rpm.sh`:

```bash
#!/usr/bin/env bash
# Wraps an already-built binary as an .rpm. cargo-generate-rpm needs no
# --no-build flag because it never builds: its README states that `cargo build`
# and `strip` "are not run upon `cargo generate-rpm` as of now". The
# consequence is that it finds the binary by convention, so this script places
# the artifact rather than passing a path — getting that wrong packages a stale
# local build, quietly.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib.sh

BIN=${1:?usage: package-rpm.sh <binary> <version> <arch>}
VERSION=${2:?usage: package-rpm.sh <binary> <version> <arch>}
ARCH=${3:?usage: package-rpm.sh <binary> <version> <arch>}

need cargo-generate-rpm "cargo install cargo-generate-rpm"
[ -f "$BIN" ] || die "no binary at $BIN"

STAGE=target/release
mkdir -p "$STAGE"
install -m 755 "$BIN" "$STAGE/roost"

mkdir -p target/distrib
OUT="target/distrib/roost-${VERSION}.${ARCH}.rpm"
cargo generate-rpm --payload-compress none \
  --set-metadata "version = '${VERSION}'" \
  --arch "$ARCH" \
  --output "$OUT" \
  >/dev/null

ok "built $(basename "$OUT")"
echo "$OUT"
```

```bash
chmod +x scripts/package-rpm.sh
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `scripts/test/packages.sh`
Expected: PASS — four `✓` under `[deb]`, three under `[rpm]`.

- [ ] **Step 6: Revert one property and watch it fail**

Remove the `[package.metadata.generate-rpm.requires]` block from `Cargo.toml`, run the test, confirm it fails with "the .rpm does not require dtach". Restore.

- [ ] **Step 7: Commit**

```bash
git add scripts/package-rpm.sh scripts/test/packages.sh Cargo.toml
git commit -m "release: an .rpm that requires dtach and ships the same unit"
```

---

### Task 5: Wire the packages into the release

Replaces Task 1's stub with the real job, using the artifact name Task 1 proved.

**Files:**
- Modify: `.github/workflows/build-linux-packages.yml`

**Interfaces:**
- Consumes: `scripts/package-deb.sh`, `scripts/package-rpm.sh`, and the artifact-name finding recorded in Task 1 Step 6.
- Produces: `.deb` and `.rpm` assets on every non-prerelease and prerelease GitHub release.

- [ ] **Step 1: Replace the stub with the real job**

```yaml
# Builds the .deb and .rpm from the binaries the release already produced —
# never from a fresh compile. A package built separately would contain
# different bytes from the tarball of the same version.
name: Build Linux packages

on:
  workflow_call:
    inputs:
      plan:
        required: true
        type: string

jobs:
  build-linux-packages:
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2

      - name: Install packaging tools
        run: cargo install cargo-deb cargo-generate-rpm --locked

      - uses: actions/download-artifact@v8
        with:
          pattern: artifacts-*
          path: dist-in/
          merge-multiple: true

      - name: Build packages from the released binaries
        run: |
          set -euo pipefail
          VERSION=$(echo '${{ inputs.plan }}' | jq -r '.releases[0].app_version')

          for pair in "x86_64-unknown-linux-musl amd64 x86_64" \
                      "aarch64-unknown-linux-musl arm64 aarch64"; do
            set -- $pair
            triple=$1; debarch=$2; rpmarch=$3
            tar -xJf "dist-in/roost-${triple}.tar.xz" -C dist-in
            bin="dist-in/roost-${triple}/roost"
            scripts/package-deb.sh "$bin" "$VERSION" "$debarch"
            scripts/package-rpm.sh "$bin" "$VERSION" "$rpmarch"
          done

          ls -la target/distrib/

      - uses: actions/upload-artifact@v4
        with:
          name: artifacts-linux-packages
          path: |
            target/distrib/*.deb
            target/distrib/*.rpm
```

- [ ] **Step 2: Commit**

```bash
git add .github/workflows/build-linux-packages.yml
git commit -m "release: build the deb and rpm from the release's own binaries"
```

- [ ] **Step 3: Cut a prerelease and verify the packages land**

Bump to `0.5.2-rc.2`, tag, push, then:

```bash
gh release view v0.5.2-rc.2 --json assets --jq '.assets[].name' | grep -E '\.(deb|rpm)$'
```

Expected: four names — two `.deb`, two `.rpm`.

- [ ] **Step 4: Verify a downloaded package, not just its name**

```bash
gh release download v0.5.2-rc.2 -p '*amd64.deb' -D /tmp/relcheck
dpkg-deb -I /tmp/relcheck/*.deb | grep -E 'Depends|Version'
dpkg-deb -c /tmp/relcheck/*.deb | grep -E 'roost.service|usr/bin/roost'
```

Expected: `Depends: dtach`, the release version, and both paths.

---

### Task 6: crates.io via Trusted Publishing

**Files:**
- Create: `.github/workflows/publish-crates-io.yml`
- Modify: `dist-workspace.toml`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: crates.io publication on every non-prerelease tag; a `--dry-run` on prereleases.

- [ ] **Step 1: Register the trusted publisher on crates.io**

On crates.io, under the `roost` crate's settings, add a GitHub trusted publisher:
- Repository owner: `PeterKnego`
- Repository name: `roost`
- Workflow filename: **`release.yml`**

Not `publish-crates-io.yml`. crates.io matches the OIDC `workflow_ref` claim — its `GitHubClaims` struct carries `workflow_ref` and has no `job_workflow_ref` — and GitHub sets `workflow_ref` to the *top-level* workflow, not the reusable one that runs. Registering the file that does the publishing is the intuitive choice and it is rejected at exchange time.

- [ ] **Step 2: Write the workflow**

```yaml
# dist has no built-in crates.io job — its PublishStyle enum is Homebrew, Npm
# and User(String) only — so this is a custom publish-jobs entry.
#
# No API token. crates.io Trusted Publishing exchanges GitHub's OIDC identity
# for a 30-minute credential that its own post-step revokes, so nothing
# long-lived is stored and there is no secret to mangle on the way in — which
# is exactly how the v0.5.1 formula push failed (`Bad credentials`, a 401 on a
# secret whose value was not a valid token).
name: Publish to crates.io

on:
  workflow_call:
    inputs:
      plan:
        required: true
        type: string

jobs:
  publish-crates-io:
    runs-on: ubuntu-22.04
    permissions:
      contents: read
      # dist grants custom publish jobs id-token: write by default; stated here
      # so the job cannot silently lose it if those defaults change.
      id-token: write
    env:
      PLAN: ${{ inputs.plan }}
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable

      # A crates.io version can never be reused — not after a yank, not ever.
      # So a prerelease verifies the package the whole way up to the upload and
      # stops, rather than burning a version to discover that `include` missed
      # a file. Skipping outright would leave this path untested until the
      # release that matters, which is precisely the blind spot that let the
      # tap token through.
      - name: Decide publish or dry run
        id: mode
        run: |
          if [ "$(echo "$PLAN" | jq -r '.announcement_is_prerelease')" = "true" ]; then
            echo "dry_run=true" >> "$GITHUB_OUTPUT"
          else
            echo "dry_run=false" >> "$GITHUB_OUTPUT"
          fi

      - name: Verify the package builds (prerelease)
        if: steps.mode.outputs.dry_run == 'true'
        run: cargo publish --locked --dry-run

      - uses: rust-lang/crates-io-auth-action@v1
        if: steps.mode.outputs.dry_run == 'false'
        id: auth

      - name: Publish
        if: steps.mode.outputs.dry_run == 'false'
        run: cargo publish --locked --token '${{ steps.auth.outputs.token }}'
```

- [ ] **Step 3: Add it to publish-jobs**

In `dist-workspace.toml`, change the `publish-jobs` line to:

```toml
publish-jobs = ["homebrew", "./publish-crates-io"]
```

- [ ] **Step 4: Regenerate and confirm**

```bash
dist generate
grep -n "publish-crates-io" .github/workflows/release.yml
```

Expected: a `uses: ./.github/workflows/publish-crates-io.yml` entry.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/publish-crates-io.yml dist-workspace.toml .github/workflows/release.yml
git commit -m "release: publish to crates.io from CI with no stored token"
```

- [ ] **Step 6: Verify on the next prerelease**

On the Task 5 prerelease run (or a fresh one), confirm the job ran the dry run and did **not** publish:

```bash
gh run view <run-id> --json jobs --jq '.jobs[] | select(.name|test("crates-io")) | .conclusion'
curl -sS -H "User-Agent: roost-release-check" https://crates.io/api/v1/crates/roost \
  | python3 -c "import json,sys; print(json.load(sys.stdin)['crate']['newest_version'])"
```

Expected: job `success`, and `newest_version` unchanged.

---

### Task 7: The tap-token preflight workflow

**Files:**
- Create: `.github/workflows/check-tap-token.yml`

**Interfaces:**
- Produces: a `workflow_dispatch` workflow named `check-tap-token.yml`, consumed by Task 8's preflight.

- [ ] **Step 1: Write the workflow**

```yaml
# Answers one question in about fifteen seconds: is HOMEBREW_TAP_TOKEN a
# credential the tap will accept?
#
# It exists because a credential used only by CI can only be tested by CI, and
# the release script needs that answer before a tag exists. On v0.5.1 the
# answer arrived after all four targets had built and the GitHub release had
# been created: `Bad credentials`, a 401 on the token value itself. Everything
# upstream of the publish jobs is content-addressed and re-runnable; the
# publish steps are the only ones carrying a human-entered secret, and they are
# the only ones that failed.
#
# Never part of a release. workflow_dispatch only.
name: Check tap token

on:
  workflow_dispatch:

jobs:
  check-tap-token:
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v6
        with:
          repository: "peterknego/homebrew-tap"
          token: ${{ secrets.HOMEBREW_TAP_TOKEN }}
          path: tap

      # Checkout proves read. The formula push needs write, and a fine-grained
      # token can carry one without the other, so probe write without leaving
      # anything behind: a push of an unchanged ref is accepted by a token with
      # Contents: write and rejected by a read-only one.
      - name: Probe write access
        working-directory: tap
        run: git push origin HEAD:refs/heads/"$(git branch --show-current)"
```

- [ ] **Step 2: Commit**

```bash
git add .github/workflows/check-tap-token.yml
git commit -m "release: a fifteen-second answer to whether the tap token works"
```

- [ ] **Step 3: Run it and confirm it passes**

```bash
gh workflow run check-tap-token.yml
sleep 20
gh run list --workflow=check-tap-token.yml --limit 1 --json conclusion --jq '.[0].conclusion'
```

Expected: `success`.

- [ ] **Step 4: Prove it can fail**

Temporarily set the secret to a bogus value, re-run, and confirm the run fails with `Bad credentials`. Restore the real token immediately afterwards and re-run to confirm `success`. Without this, the check is a green light that has never been red.

---

### Task 8: `preflight.sh`

**Files:**
- Create: `scripts/preflight.sh`

**Interfaces:**
- Consumes: `scripts/lib.sh`, `check-tap-token.yml`.
- Produces: `scripts/preflight.sh [version]` — exits 0 if every check passes, non-zero naming the first failure. With no version argument it skips the version-specific checks.

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Every check here is a failure this project actually had while cutting v0.5.1.
# Nothing mutates: a preflight failure means nothing has happened yet.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib.sh

VERSION=${1:-}
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

# dist init -y silently replaces the configured targets with its own
# seven-target default, gnu and windows included. A gnu Linux build reintroduces
# the GLIBC_2.39 load failure on Debian 12, Ubuntu 22.04 and Amazon Linux 2023.
ACTUAL=$(dist plan --output-format=json | jq -r '[.artifacts[].target_triples[]?] | unique | join(" ")')
for t in $EXPECTED_TARGETS; do
  case " $ACTUAL " in *" $t "*) ;; *) die "dist is not building $t — check dist-workspace.toml" ;; esac
done
ok "dist builds the four expected targets"

# `cargo build --locked` rejects a lock file that has drifted from Cargo.toml,
# and in CI that failure lands on the tag, after the tag is public.
cargo metadata --locked --format-version 1 >/dev/null 2>&1 || die "Cargo.lock is out of sync — run cargo check"
ok "Cargo.lock in sync"

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
```

```bash
chmod +x scripts/preflight.sh
```

- [ ] **Step 2: Run it and confirm it passes on a clean master**

Run: `scripts/preflight.sh`
Expected: every line a `✓`, exit 0.

- [ ] **Step 3: Prove each check can fail**

For each of these, make the change, run `scripts/preflight.sh`, confirm it dies naming that check, then revert:

1. `git checkout -b tmp` → "not on master"
2. `touch dirty.txt` → "working tree is dirty"
3. Add `"x86_64-pc-windows-msvc"` to `targets` in `dist-workspace.toml` — this must **still pass**, since the check asserts the four are present; then remove `"aarch64-unknown-linux-musl"` → "dist is not building aarch64-unknown-linux-musl"
4. Edit `Cargo.toml`'s version without running `cargo check` → "Cargo.lock is out of sync"
5. Comment out `linker = "rust-lld"` in `.cargo/config.toml` → "aarch64-unknown-linux-musl does not build"

A check that has never been red is a green light, not a check.

- [ ] **Step 4: Commit**

```bash
git add scripts/preflight.sh
git commit -m "release: refuse to tag when any of eleven known failures is present"
```

---

### Task 9: `verify-release.sh`

**Files:**
- Create: `scripts/verify-release.sh`

**Interfaces:**
- Consumes: `scripts/lib.sh`.
- Produces: `scripts/verify-release.sh <version>` — exits 0 when every channel serves that version.

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Reads every channel back from the outside. A green CI run says the jobs
# exited 0; it does not say the shipped binary can load on the machines it is
# aimed at. Only running the published artifact says that.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib.sh

VERSION=${1:?usage: verify-release.sh <version>}
TAG="v$VERSION"
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT

phase verify
need gh "install the GitHub CLI"; need jq "apt install jq"

DRAFT=$(gh release view "$TAG" --json isDraft --jq .isDraft)
[ "$DRAFT" = false ] || die "$TAG is still a draft"
ASSETS=$(gh release view "$TAG" --json assets --jq '.assets[].name')
for want in \
  roost-x86_64-unknown-linux-musl.tar.xz roost-aarch64-unknown-linux-musl.tar.xz \
  roost-x86_64-apple-darwin.tar.xz roost-aarch64-apple-darwin.tar.xz \
  roost-installer.sh roost.rb sha256.sum; do
  echo "$ASSETS" | grep -qx "$want" || die "release is missing $want"
done
echo "$ASSETS" | grep -qE '\.deb$' || die "release has no .deb"
echo "$ASSETS" | grep -qE '\.rpm$' || die "release has no .rpm"
ok "release published with every expected asset"

FORMULA=$(gh api repos/PeterKnego/homebrew-tap/contents/Formula/roost.rb --jq .content | base64 -d)
echo "$FORMULA" | grep -qx "  version \"$VERSION\"" || die "the tap formula is not at $VERSION"
echo "$FORMULA" | grep -qx '  depends_on "dtach"' || die "the formula lost depends_on dtach"
ok "tap formula at $VERSION, still declaring dtach"

NEWEST=$(curl -sS -H 'User-Agent: roost-release (peter@knego.net)' \
  https://crates.io/api/v1/crates/roost | jq -r '.crate.newest_version')
[ "$NEWEST" = "$VERSION" ] || die "crates.io newest is $NEWEST, not $VERSION"
ok "crates.io at $VERSION"

gh release download "$TAG" -D "$TMP" \
  -p 'roost-x86_64-unknown-linux-musl.tar.xz*' >/dev/null
( cd "$TMP" && sha256sum -c roost-x86_64-unknown-linux-musl.tar.xz.sha256 >/dev/null ) \
  || die "published checksum does not match the published tarball"
ok "published checksum verifies"

# gh 2.46 has no `attestation` subcommand, so this goes through the REST API.
# It confirms an attestation exists and covers this artifact; it does not
# verify the signature.
DIGEST=$(sha256sum "$TMP/roost-x86_64-unknown-linux-musl.tar.xz" | cut -d' ' -f1)
COUNT=$(gh api "repos/PeterKnego/roost/attestations/sha256:$DIGEST" --jq '.attestations | length')
[ "$COUNT" -ge 1 ] || die "no attestation covers the published tarball"
ok "attestation covers the published tarball (existence, not signature)"

tar -xJf "$TMP/roost-x86_64-unknown-linux-musl.tar.xz" -C "$TMP"
BIN="$TMP/roost-x86_64-unknown-linux-musl/roost"
readelf -d "$BIN" 2>/dev/null | grep -qi needed && die "the published binary is dynamically linked"
objdump -T "$BIN" 2>/dev/null | grep -q GLIBC && die "the published binary references GLIBC"
[ "$("$BIN" --version)" = "roost $VERSION" ] || die "the published binary reports the wrong version"
ok "published binary is static, GLIBC-free, and reports $VERSION"
```

```bash
chmod +x scripts/verify-release.sh
```

- [ ] **Step 2: Run it against the existing v0.5.1**

Run: `scripts/verify-release.sh 0.5.1`
Expected: every check passes **except** the `.deb`/`.rpm` assertions, which must fail — v0.5.1 predates the packages. That failure is the proof the assertion is real rather than vacuous.

- [ ] **Step 3: Run it against the Task 5 prerelease**

Run: `scripts/verify-release.sh 0.5.2-rc.2`
Expected: the asset and binary checks pass; the crates.io and tap checks fail, because a prerelease publishes to neither. Note this in the script's comments if it is confusing — it is correct behaviour, and `release.sh` only calls verify for non-prereleases.

- [ ] **Step 4: Commit**

```bash
git add scripts/verify-release.sh
git commit -m "release: verify a release by reading it back, not by trusting its green tick"
```

---

### Task 10: `release.sh`

**Files:**
- Create: `scripts/release.sh`
- Modify: `docs/deploy.md` (add a short "Cutting a release" section)

**Interfaces:**
- Consumes: `preflight.sh`, `verify-release.sh`, `lib.sh`.
- Produces: `scripts/release.sh <version>` and the subcommands `release.sh check`, `release.sh verify <version>`.

- [ ] **Step 1: Write the script**

```bash
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
sed -i "0,/^version = \".*\"$/s//version = \"$VERSION\"/" Cargo.toml
cargo check --quiet
grep -qx "version = \"$VERSION\"" Cargo.toml || die "the version bump did not take"
git add Cargo.toml Cargo.lock
git commit -q -m "release: $VERSION"
git tag -a "$TAG" -m "roost $VERSION"
git push -q origin master
git push -q origin "$TAG"
ok "tagged $TAG and pushed"

phase ci
sleep 10
RUN=$(gh run list --workflow=Release --limit 1 --json databaseId --jq '.[0].databaseId')
say "run $RUN"
until [ "$(gh run view "$RUN" --json status --jq .status)" = completed ]; do sleep 20; done
CONCLUSION=$(gh run view "$RUN" --json conclusion --jq .conclusion)
if [ "$CONCLUSION" != success ]; then
  gh run view "$RUN" --json jobs --jq '.jobs[] | select(.conclusion=="failure") | "  failed: \(.name)"'
  say ""
  say "The tag and any uploaded assets are left in place on purpose."
  say "Fix the cause, then re-run only what failed:"
  say "  gh run rerun $RUN --failed"
  die "release run $RUN failed"
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
```

```bash
chmod +x scripts/release.sh
```

- [ ] **Step 2: Verify the subcommands dispatch without doing anything**

```bash
scripts/release.sh check   # must run preflight only
scripts/release.sh         # must exit non-zero with the usage line
```

Expected: the first runs preflight; the second prints the usage message and exits non-zero.

- [ ] **Step 3: Verify the failure path prints recovery, not destruction**

Read the `[ci]` block back and confirm by inspection that no path calls `git tag -d`, `git push --delete`, or `gh release delete`:

```bash
grep -nE 'tag -d|push .*--delete|release delete' scripts/release.sh || echo "no destructive path — correct"
```

Expected: `no destructive path — correct`.

- [ ] **Step 4: Document it**

Add to `docs/deploy.md`, after the existing build/install section:

```markdown
## Cutting a release

```sh
make release VERSION=0.5.2
```

Preflight refuses to tag when any of eleven known failures is present — the
wrong branch, a dirty tree, drifted `dist` targets, a stale `Cargo.lock`, a
version already on crates.io, a broken cross-link, failing tests, or a tap
token the tap will not accept. It then bumps, tags, pushes, watches the release
run, and reads every channel back.

It never deletes a tag or a release. If CI fails after the tag is pushed it
prints `gh run rerun <id> --failed`, which is what recovered v0.5.1.

A version with a `-` in it is a prerelease: it builds all four targets and the
packages, and publishes to neither the tap nor crates.io. Use one to exercise
a change to the release itself.
```

- [ ] **Step 5: Commit**

```bash
git add scripts/release.sh docs/deploy.md
git commit -m "release: one command, and a failure path that reports rather than destroys"
```

- [ ] **Step 6: Cut the real 0.5.2 with it**

```bash
make release VERSION=0.5.2
```

Expected: preflight passes, the run succeeds, verify reports every channel at 0.5.2 including the `.deb` and `.rpm`.

---

## Self-review

**Spec coverage.** Every spec section maps to a task: *Shape* → Tasks 2–10; *Preflight checks* → Task 8; *Prereleases* → Tasks 6 and 10; *Failure handling* → Task 10 Step 3; *Verification* → Task 9; *The packages* → Tasks 2–5; *crates.io* → Task 6; *Testing* → Tasks 2 (shellcheck), 3, 4 (package tests, both with a revert-and-watch-it-fail step) and 8 Step 3 (preflight failure paths). The spec's *Open* item 1 is Task 1; *Open* items 2 and 3 are recorded as accepted limits in Task 9's comments rather than being implemented.

**Placeholders.** None. Every code step carries the actual content.

**Type consistency.** The three scripts that produce files all print the produced path as their last stdout line and are consumed with `| tail -1`. `lib.sh` defines `phase`, `ok`, `warn`, `say`, `die`, `need`, and only those are called. The artifact name `artifacts-linux-packages` is used identically in Tasks 1 and 5. `EXPECTED_TARGETS` in Task 8 matches the four in *Global Constraints* and in `dist-workspace.toml`.

**One ordering constraint worth stating plainly:** Task 5 depends on Task 1's *outcome*, not merely its code. If Task 1 Step 5 shows the probe file absent, Task 5's upload step is wrong as written and must be corrected before it is committed — otherwise the packages build green and never reach the release, which no later task would catch.
