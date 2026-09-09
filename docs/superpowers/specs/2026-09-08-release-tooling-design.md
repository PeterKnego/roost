# Release tooling: one command, and packages that carry their dependency

*2026-09-08. Status: designed, not implemented. Decided with Peter on 2026-09-08.*

## What and why

v0.5.1 was the first release built by CI rather than uploaded by hand. Cutting
it took eleven manual steps and failed once, and every failure mode was
avoidable with a check that nobody had written down:

- `dist init -y` silently replaced the four configured targets with its own
  seven-target default. Caught by reading the file; nothing would have caught
  it later except a user on Debian 12.
- Bumping the version desynchronised `Cargo.lock`, which `cargo build --locked`
  rejects. Caught locally; in CI it would have failed *on the tag*.
- `HOMEBREW_TAP_TOKEN` held a value GitHub rejected with `Bad credentials` — a
  401 on the token itself, not a permissions problem. Discovered after all four
  targets had built, the GitHub release had been created, and the artifacts
  uploaded. The release sat half-published for the length of a diagnosis.
- `cargo publish` was a separate manual step, so crates.io sat on 0.5.0 while
  every other channel served 0.5.1. `cargo binstall` reads crates.io for the
  version and GitHub for the binary, so for that window it resolved a version
  whose only artifact was the glibc-2.39 build — a load-time failure on Debian
  12, Ubuntu 22.04 and Amazon Linux 2023 that would have read as a roost bug.

Separately, roost has **no Linux packages at all**. Of the four Linux install
paths that exist — tarball, shell installer, Homebrew, `cargo binstall` — only
Homebrew declares `dtach`, and Linuxbrew is a rounding error on Linux. So every
realistic Linux install today leaves roost's one hard runtime dependency unmet
and unmentioned, and the user finds out when a terminal fails to spawn. `dtach`
has been in Debian since wheezy and is in every current Ubuntu (0.9-5 in jammy
and noble, 0.9-8 in questing), so a `.deb` declaring `Depends: dtach` makes the
problem disappear rather than merely reporting it.

This spec covers a `scripts/` directory driven by one command, and `.deb` and
`.rpm` artifacts produced inside the release.

## Non-goals

- **Building release artifacts locally.** The scripts never produce a shipped
  binary. Hand-built artifacts are what left v0.4.0 with no assets, and
  CLAUDE.md's shared-`target-dir` warning makes a local build actively unsafe
  here: a `cargo build` from a second checkout rewrites the shared asset table
  and leaves the binary built from the wrong tree, reporting `Fresh roost`.
- **AUR.** Still worth doing (`roost-bin`, `conflicts=('roost')` — the existing
  AUR `roost` is a dotfiles manager that installs `/usr/bin/roost` and declares
  `provides=("roost")`), but it is a different distribution model and does not
  belong in this pass.
- **npm.** The bare name is taken on npm — an unrelated "System provisioning
  toolkit", published 2013, last touched 2022 — so it could only ship as
  `@peterknego/roost`, and `npx roost` never works. That removes the only
  ergonomic reason for the channel.
- **A hosted apt or yum repository.** Attached `.deb`/`.rpm` files first; a
  repository is much more work and its demand is unestablished.
- **Replacing anything cargo-dist owns.** Tarballs, checksums, attestations,
  the shell installer and the formula stay generated.

## Shape

```
Makefile                       # command index only; see below
scripts/lib.sh                 # shared logging, die, the ✓/✗ vocabulary
scripts/preflight.sh           # every pre-tag check; mutates nothing
scripts/package-deb.sh         # <binary> <version> <arch> -> .deb
scripts/package-rpm.sh         # <binary> <version> <arch> -> .rpm
scripts/verify-release.sh      # <version> -> reads back every channel
scripts/release.sh             # the one command; calls the above in order
.github/workflows/publish-crates-io.yml     # reusable; Trusted Publishing
.github/workflows/build-linux-packages.yml  # reusable; calls the package scripts
.github/workflows/check-tap-token.yml       # workflow_dispatch; preflight only
```

**The package scripts run in both places.** CI invokes `package-deb.sh` during
the release; a developer invokes the same file locally to change it. Today every
piece of release logic lives inside generated YAML and can be executed nowhere
but a tag push, which is the reason the token defect cost a full build cycle to
find. One definition, two callers.

**The Makefile is a command index, not a build graph.** `.PHONY` targets that
exec scripts, plus `help`. The dependency edges genuinely differ between local
(build the binary, then pack) and CI (download the attested binary, then pack),
so encoding them in make would create two descriptions of how a package is
made — and the CI one is the one that ships. A graph that is right in one
context and wrong in the other is worse than no graph.

## Behaviour

### `release.sh <version>`

Four phases, run in order. Each is also a subcommand (`release.sh check`,
`release.sh verify <version>`) so a phase can be run alone.

```
[preflight]   every check below; refuses to continue on any failure
[release]     bump Cargo.toml + Cargo.lock, commit, tag, push
[ci]          watch the tag's release run to completion
[verify]      read back every channel independently
```

The version argument is the plain version (`0.5.2`, `0.5.2-rc.1`); the tag is
`v` + that, matching the existing tags and `release.yml`'s trigger pattern.

### Preflight checks

Each row is a failure this project has actually had, or the direct cause of one.
Any failure stops the run before a tag exists.

| Check | The failure it prevents |
|---|---|
| On `master`, tree clean, in sync with `origin` | Tagging a commit nobody else has, or one carrying unrelated work |
| Tag `v<version>` free, locally and on the remote | A second release announcing the same version |
| `<version>` absent from crates.io | An irrecoverable half-release: a crates.io version can never be reused, not even after a yank |
| `dist plan` names exactly the four expected targets | `dist init -y` replacing them with its gnu + windows default; a gnu Linux build reintroduces the `GLIBC_2.39` load failure |
| `Cargo.lock` in sync with `Cargo.toml` | `cargo build --locked` failing on the tag, after the tag is public |
| `cargo test --locked -- --test-threads=1` passes | The obvious one. Single-threaded is deliberate; a bare `cargo test` has hung on the shared registry |
| `dtach` and `git` on `PATH` | Tests that silently substitute their way past a missing runtime dependency |
| Tap token valid (dispatched, ~15s) | Exactly v0.5.1: four targets built, release created, then `Bad credentials` |
| Working tree builds for both musl targets | A cross-compile break found on the tag rather than before it |

The last check is the one with a real cost — two musl builds — and it is
deliberate: the `aarch64` cross-link fails outright on an x86_64 host without
`.cargo/config.toml`'s `linker = "rust-lld"`, and that file is exactly the kind
of thing a refactor deletes as unused.

**Not automated: crates.io trusted publisher registered.** crates.io does
expose `/api/v1/trusted_publishing/github_configs?crate=<name>`, which could in
principle answer this — but it requires an authenticated request (measured:
unauthenticated, it returns 403 "this action requires authentication", not a
public read). Automating it means preflight holding a crates.io API token,
which is exactly the stored, mistypeable secret Trusted Publishing was chosen
over for the publish job itself (see "crates.io" above — the whole point was
no secret to mistype). Confirm this by hand, in the crates.io UI, before
cutting a release; `scripts/preflight.sh` documents this in a comment rather
than silently omitting the row.

### Prereleases

`release.sh 0.5.2-rc.1` runs the same preflight and the same build, and the
publish steps gate themselves off, as they already do: `release.yml` guards the
formula push on `!announcement_is_prerelease || publish_prereleases`, and
`publish_prereleases` is false.

crates.io behaves differently on purpose. Rather than skipping, a prerelease runs
`cargo publish --locked --dry-run`. A crates.io version number can never be
reused, so a prerelease must not burn one — but skipping entirely leaves the path
untested until the release that matters, which is precisely the blind spot that
let the tap token through. The dry run exercises everything up to the upload,
including the verification build from the *packaged* tree, which is the only
thing that catches `Cargo.toml`'s `include` list drifting away from what
`build.rs` needs.

### Failure handling

**The scripts never delete a tag or a release.** If CI fails after the tag is
pushed, `release.sh` reports which job failed and prints the recovery command
(`gh run rerun <id> --failed`), which is what recovered v0.5.1 with no re-tag.
Tags are mirrored and cached by things outside this repository; a stale release
is recoverable, a re-pointed tag is not. This is CLAUDE.md's rule about
destruction needing positive evidence, applied to git objects.

A preflight failure is different: nothing has happened yet, so it simply exits
non-zero naming the check.

### Verification

`verify-release.sh <version>` does not trust the run's own green tick. It reads
back, from the outside:

- the release exists, is not a draft, and carries every expected asset — four
  tarballs and their `.sha256`, the installer, `roost.rb`, `sha256.sum`, the
  source tarball, and now the `.deb` and `.rpm`;
- the published `Formula/roost.rb` in the tap declares this version;
- crates.io reports this version as `newest`;
- an attestation exists whose subject digest matches the downloaded tarball;
- the downloaded x86_64 binary is static — no `NEEDED` entries, no `GLIBC`
  references — and prints the expected version when run.

That last one is the check that would have caught the C2 defect at the source.
Nothing in a green CI run says the shipped binary can load on the machines it is
aimed at; only running it does, and running the *published* artifact rather than
the local build is the whole point.

## The packages

### Contents

```
/usr/bin/roost
/usr/lib/systemd/user/roost.service
/usr/share/doc/roost/README.md
/usr/share/doc/roost/copyright          # MIT OR Apache-2.0, both texts
```

`Depends: dtach` on the deb, `Requires: dtach` on the rpm.

**The unit ships with `KillMode=process` and is not enabled.** That property is
load-bearing and this is the first place it can stop being something a user has
to know: systemd's default `KillMode=control-group` SIGKILLs the whole cgroup on
stop, taking every dtach master with it and defeating the entire reason dtach is
used. It is verified in production (with the default, a restart took
`pgrep -c dtach` to 0; with `KillMode=process`, a shell variable set before the
restart survived it), it is in CLAUDE.md's dev/prod substitution table as a
defect that shipped, and deploy.md spends a page on it. A package that carries
it correctly removes a whole class of "roost lost my shell" reports.

It is a **user** unit, not a system one, and it is not enabled on install. roost
spawns the invoking user's shell and binds loopback; a system service running as
root would be a different and much worse program, and a package install that
starts a shell-spawning listener without being asked is a surprise nobody wants.
`postinst` prints the loopback/no-auth warning and the one command to start it.

### The no-build contract

Both scripts take an already-built binary and never compile. `cargo deb
--no-build` is documented for exactly this. `cargo generate-rpm` needs no such
flag because it never builds at all — its README is explicit that `cargo build`
and `strip` "are not run upon `cargo generate-rpm` as of now" and must be done
in advance. The consequence is a detail the script has to get right: it locates
the binary by convention, under `target/<triple>/release/`, so
`package-rpm.sh` must place the downloaded artifact there rather than pass a
path. Getting that wrong packages either nothing or a stale local build, and
both are quiet.

This is not an optimisation. If a package script built its own binary, the
`.deb` in a release would contain different bytes from the `.tar.xz` in that same
release — same version, same provenance story, quietly different builds. Nobody
notices until a bug reproduces on one and not the other, and by then the two
have been treated as interchangeable for months.

### Where they are built

A custom job in cargo-dist's **global-artifacts** phase. The local-artifacts
phase runs as a matrix, so a job there sees one target's output; the global phase
is the first point at which all four binaries exist and can be downloaded. The
job pulls the `artifacts-*` bundle, extracts the two musl tarballs, runs the
package scripts against those exact binaries, and uploads the results into the
same bundle so `host` attaches them to the release.

**This contract is the one thing in this design that has not been exercised, and
it should be proved before anything else is written.** The documented requirement
("archives and checksums are produced and uploaded to an artifact named
`artifacts`") is stated for the case where `build-local-artifacts = false`, which
is not our case. If a custom global job's uploads are not collected the same way,
the packages build green and never reach the release — a silent omission, not an
error. Prove it with a throwaway prerelease tag carrying a stub package job
before building the real one.

## crates.io

Trusted Publishing, not a stored token: crates.io trades GitHub's OIDC identity
for a 30-minute credential that its own post-step revokes. Given that this
release cycle has already lost a run to a mistyped long-lived secret, a design
with no secret to mistype is the point rather than a nicety. cargo-dist grants
custom publish jobs `id-token: write` by default; the job states it anyway so it
cannot silently lose it.

**Register the trusted publisher against `release.yml`, not
`publish-crates-io.yml`.** crates.io matches the OIDC `workflow_ref` claim — its
`GitHubClaims` struct carries `workflow_ref` and has no `job_workflow_ref` — and
GitHub sets `workflow_ref` to the *top-level* workflow, not the reusable one that
actually runs. Registering the file that does the publishing is the intuitive
choice and it is rejected at exchange time.

## Testing

**The package tests open the artifact.** `dpkg-deb -I` must show `Depends: dtach`
in the control file; `dpkg-deb -c` must list the unit at
`/usr/lib/systemd/user/roost.service`; `rpm -qp --requires` must name `dtach`.
Asserting that a script exited 0 would pass against an empty package, which is
the "test that cannot fail" this repository has been bitten by seven times.

**The unit file is asserted on, not just shipped.** A test greps the packaged
unit for `KillMode=process`. Without a glyph-level assertion this is a strip test
with no glyph: the unit could ship empty, or with the default kill mode, and
every test stays green while the packaged property is exactly inverted.

**`shellcheck` on `scripts/` in `ci.yml`.** None of this bash is reachable by
`cargo test`, and quoting bugs in a release script surface at the worst moment.

**Revert-and-watch-it-fail applies to the preflight checks.** Each check gets its
failure path exercised — a deliberately desynchronised `Cargo.lock`, a
`dist-workspace.toml` with a fifth target — because a check that cannot fail is
worse than no check: it reports a property it never examined.

## Open

1. The global-artifacts upload contract above. Prove first.
2. Whether `verify-release.sh` should run the published **aarch64** binary. It
   cannot on this host; `qemu-user-static` would let it, at the cost of a
   dependency. The macOS artifacts cannot be run from here at all, and that gap
   stays — the spec says so rather than pretending otherwise.
3. `gh` on the deploy host is 2.46.0, which has no `attestation` subcommand, so
   `verify-release.sh` checks the attestation through the REST API. That confirms
   an attestation exists and covers the artifact; it does **not** verify the
   signature. Worth revisiting when `gh` ≥ 2.49 is available.
