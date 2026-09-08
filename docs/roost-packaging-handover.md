# Roost — packaging and distribution handover

Date of survey: 2026-09-06. Last updated: 2026-09-08 (6.1 decided; steps 1 and 2
done bar one secret).
Repository: https://github.com/PeterKnego/roost (public, branch `master`)
Version at survey: 0.5.0

## 1. Purpose

This document records the state of Roost distribution on 2026-09-06. It also
records the decisions about the install channels to add before the Show HN
launch. Read section 2 for the facts. Read section 5 for the work to do.

## 2. Current state

### 2.1 Release artifacts

| Tag | Date | Assets |
|---|---|---|
| v0.5.0 | 2026-09-06 | `roost-v0.5.0-x86_64-unknown-linux-gnu.tar.gz` (1.8 MB) and its `.sha256` |
| v0.4.0 | 2026-09-04 | none |
| v0.3.0 | 2026-09-02 | `roost-v0.3.0-x86_64-unknown-linux-gnu.tar.gz` (1.6 MB) and its `.sha256` |

There is one target only: `x86_64-unknown-linux-gnu`.

### 2.2 CI

`.github/workflows/ci.yml` is the only workflow. It runs tests. It does not
build releases. Its steps are:

- Trigger: push to `master`, and pull requests.
- Runner: `ubuntu-latest`.
- Install `dtach` with `apt-get`.
- `cargo build --locked`.
- `cargo test --locked -- --test-threads=1` (single thread is deliberate).

The release tarballs are therefore uploaded by hand. The gap at v0.4.0 shows
the result.

### 2.3 crates.io

The crate name `roost` belongs to this project. Version 0.5.0 is published.
The crate was first published on 2026-09-04. `cargo install roost` works, but
it needs a Rust toolchain on the user machine.

### 2.4 Runtime prerequisites

Roost needs `dtach` and `git` on the `PATH` at run time.

### 2.5 Dependencies relevant to packaging

The release binary has no TLS or crypto dependency at all. `Cargo.lock` does
contain `rustls` and `ring`, but both are **dev-dependencies**, reached only
through `ureq` in the test suite:

```
rustls v0.23.43
└── ureq v2.12.1
    [dev-dependencies]
    └── roost v0.5.0
```

There is no `openssl`, `openssl-sys` or `native-tls` anywhere in the lock file.
The shipped v0.5.0 binary declares exactly three NEEDED libraries: `libc.so.6`,
`ld-linux-x86-64.so.2` and `libgcc_s.so.1`.

## 3. Constraints

These three facts control the packaging decisions.

**C1 — External runtime dependencies.**
A tarball or a shell installer cannot install `dtach`. It can only test for
`dtach` and report an error. A package manager can install it. Homebrew, AUR,
`deb` and `rpm` all declare dependencies. This makes Homebrew the correct
primary channel for macOS, not only the popular one.

**C2 — glibc floor. Measured, not estimated.**
The current build is a gnu build made on `ubuntu-latest`. The published v0.5.0
binary requires `GLIBC_2.39`, and below that it fails at *load* time — before
`main` runs — with:

```
roost: libc.so.6: version `GLIBC_2.39' not found (required by roost)
```

Observed by running the released binary under each distro's own loader
(`ld-linux-x86-64.so.2 --library-path`, libc extracted from that distro's
`libc6` `.deb`):

| glibc | Distro | Result |
|---|---|---|
| 2.43 | this host | `roost 0.5.0` — control |
| 2.36 | Debian 12 | fails to load |
| 2.35 | Ubuntu 22.04 LTS | fails to load |
| 2.34 | Amazon Linux 2023 | below both of the above; not run |

The entire incompatibility is **two symbols**, from Rust std's `posix_spawn`
pidfd path:

```
w DF *UND* (GLIBC_2.39) pidfd_spawnp
w DF *UND* (GLIBC_2.39) pidfd_getpid
```

Every other undefined symbol in the binary tops out at `GLIBC_2.34`. Both of
these are *weak* symbols, which would normally not abort a load — but the
loader checks `.gnu.version_r`, and that entry reads
`Name: GLIBC_2.39  Flags: none`. Absent `VER_FLG_WEAK` there, a missing version
is fatal. Do not assume weak means survivable.

Two consequences. Pinning the runner to `ubuntu-22.04` would drop the floor to
2.34 and clear all three distros today — an interim fix costing one line of CI.
And musl (C3) removes the question entirely, which is why it is the real answer.

Roost runs on servers, and servers are frequently older than the build machine.
This is the most probable bug report after launch.

**C3 — musl is available. Both targets verified building.**
Because of the fact in section 2.5 a static musl build has no blocker: there is
no `openssl-sys`, and nothing in the tree needs a C toolchain beyond libc
bindings. Both musl targets were built from this checkout on 2026-09-06 with
nothing installed but the rustup targets themselves:

| Target | Result |
|---|---|
| `x86_64-unknown-linux-musl` | clean; `static-pie linked`, no NEEDED entries; `roost --version` runs |
| `aarch64-unknown-linux-musl` | clean **only with `-C linker=rust-lld`**; `statically linked`, no NEEDED entries |

The aarch64 caveat is not optional. Cargo defaults to the host `cc` as the
linker driver, and `x86_64-linux-gnu-ld.bfd` then rejects the aarch64 objects:
`Relocations in generic ELF (EM: 183) ... file in wrong format`. Set the linker
explicitly in the release config. No cross-gcc, `zig` or `cross` is needed.

Both binaries carry the embedded `static/` assets, so `build.rs` cross-compiles
correctly.

Change the Linux targets to musl.

## 4. Target set

Build these four targets:

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`
- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-musl`

The two Linux targets are verified to build (see C3), and `aarch64` needs the
explicit `rust-lld` linker setting. The two macOS targets are **unverified** —
this survey had no macOS machine.

macOS has no binary today. The README says that macOS is in daily use. Mac
users must therefore install a Rust toolchain first. Close this gap first.

## 5. Work to do, in order

### Step 1 — Add a release workflow — **done 2026-09-08**

Landed as `dist-workspace.toml`, `.cargo/config.toml`, `[profile.dist]` in
`Cargo.toml` and a generated `.github/workflows/release.yml`. cargo-dist 0.32.0.
It is tag-triggered on `**[0-9]+.[0-9]+.[0-9]+*` and runs `dist plan` on pull
requests, so a configuration break shows up on the PR and not on the tag. This
removes the manual upload that produced the empty v0.4.0.

`dist init -y` **overwrites the `-t` flags you pass it** with its own default
target list — gnu, musl and `x86_64-pc-windows-msvc` all seven. The four
targets from section 4 were written into `dist-workspace.toml` by hand
afterwards. Check the file, not the command.

Configured: `checksum = "sha256"` (per artifact, plus a unified `sha256.sum`)
and `github-attestations = true`, which generated the `attestations: write` and
`id-token: write` permissions on the build job.

**Correction to C3: CI does not cross-compile, so it never needed the linker
setting.** dist assigns `aarch64-unknown-linux-musl` to a native
`ubuntu-22.04-arm` runner and `x86_64-unknown-linux-musl` to `ubuntu-22.04`,
each installing `musl-tools`. The `EM: 183` failure in C3 is a property of
building aarch64 *from this x86_64 host*, not of the target. `.cargo/config.toml`
sets `linker = "rust-lld"` for that target anyway, so a local build links the
way CI's does instead of failing outright — `linker` rather than
`[target.*].rustflags`, because a `rustflags` table is discarded wholesale as
soon as a `RUSTFLAGS` environment variable is set, which would drop the linker
with it and reinstate the error in CI alone.

Verified on 2026-09-08 under the `dist` profile, not `release`:

| Check | Result |
|---|---|
| `x86_64-unknown-linux-musl` | `static-pie linked`, no NEEDED entries, `roost 0.5.0` |
| `aarch64-unknown-linux-musl` | `statically linked`, no NEEDED entries |
| linker line commented out | fails with the exact `Relocations in generic ELF (EM: 183)` from C3, no binary produced |
| `dist build --artifacts=local` | `roost-x86_64-unknown-linux-musl.tar.xz` + `.sha256`; holds the binary, `README.md`, `LICENSE-MIT`, `LICENSE-APACHE`; extracted binary prints `roost 0.5.0` |

Still unverified, and unverifiable without pushing a tag: the two macOS targets
and the `ubuntu-22.04-arm` runner. The first tag is the test.

### Step 2 — Publish a Homebrew tap — **done 2026-09-08, blocked on one secret**

`PeterKnego/homebrew-tap` exists, is public, and holds a seed `README.md` — a
tap must not be an empty repository, because dist checks it out to push the
formula and an empty repository has no default branch to check out. The README
carries the 6.4 loopback warning and the 6.1 `PATH` collision, per the rule that
every package description states both.

`dist-workspace.toml` gained `installers = ["shell", "homebrew"]`,
`tap = "PeterKnego/homebrew-tap"`, `publish-jobs = ["homebrew"]`, and:

```toml
[dist.dependencies.homebrew]
dtach = { stage = ["run"] }
```

`stage = ["run"]` is the load-bearing part. Without it the entry becomes a
`brew install` on the *build* runner instead of a `depends_on` in the formula,
which is the opposite of what C1 asks for.

Verified by generating the formula, not by reading the configuration:
`dist build --artifacts=global` produced `target/distrib/roost.rb` containing
`depends_on "dtach"`, `license any_of: ["MIT", "Apache-2.0"]`, and macOS/Linux
× arm/intel URLs pointing at the four release artifacts. `dist generate` wrote
a `publish-homebrew-formula` job targeting `PeterKnego/homebrew-tap`.

**Outstanding — needs a browser, so it could not be done here.** That job
authenticates with `secrets.HOMEBREW_TAP_TOKEN`, and
`gh api repos/PeterKnego/roost/actions/secrets` returns `total_count: 0`. Until
it is set, a tagged release will build and upload every artifact and *then* fail
at the publish step. Mint a personal access token with `repo` scope on the tap,
then:

```sh
gh secret set HOMEBREW_TAP_TOKEN -R PeterKnego/roost
```

### Step 3 — Publish an install script

cargo-dist generates the shell installer. Add a preflight test for `dtach` to
it. Without this test, the user gets a failure at run time and not at install
time. Host the script on a stable URL. Do not send users to
`raw.githubusercontent.com` in the README.

### Step 4 — Add cargo-binstall to the README

`cargo binstall roost` reads the GitHub release artifacts. It works with no
extra configuration after step 1. It only needs a documentation line.

### Step 5 — Add an AUR package (after launch)

Create `roost-bin` — the plain AUR name `roost` is already taken by an
unrelated project (see 6.1). Declare `conflicts=('roost')`. That package's
PKGBUILD installs `/usr/bin/roost` and declares `provides=("roost")`, so
without the declaration pacman aborts on a file conflict at install time
instead of explaining itself.

Set `depends=('dtach' 'git')`, but note that
**`dtach` is not in the Arch official repositories**: it exists only as an AUR
package (`dtach 0.9-4`). An AUR package may depend on another AUR package, but
`makepkg` alone will not resolve it — only a helper such as `paru` or `yay`
will. Say so in the package description.

### Later, if there is demand

- `deb` and `rpm` files with `cargo-deb` and `cargo-generate-rpm`, attached to
  the releases. A hosted apt or yum repository is much more work. Use
  Cloudsmith or the openSUSE Build Service if this becomes necessary.
- A Nix flake in the repository, then a nixpkgs pull request.
- An OCI image on ghcr.io. Roost is a server-side tool, so some users will
  prefer a container to an install.

### Do not use

- **Flatpak.** Its sandbox model is for desktop GUI applications. Roost spawns
  PTYs. This is the wrong fit.
- **Snap.** Roost would need classic confinement. Classic confinement needs a
  manual review by Canonical.
- **Debian and Fedora official archives.** These need a distribution
  maintainer and a slow release cycle. Do not attempt this before 1.0.

## 6. Open items

**6.1 — Name collision. Decided: the name stays `roost`.**
The GitHub repository `navbytes/roost` is not a distant neighbour. Its README
describes it as "a session-native terminal multiplexer for AI agent CLIs (pi,
Claude Code, codex, gemini, opencode, shell) — no daemon, ever." Same audience,
same language, same two platforms. It was at **v0.1.20 on 2026-09-03**, with
six releases in the five days to 2026-09-06 — not v0.1.11, and not slowing.

The package namespaces are **not** unclaimed:

- **Homebrew.** `navbytes/roost` already ships
  `brew install navbytes/tap/roost` from an existing `navbytes/homebrew-tap`.
  Taps are namespaced, so `PeterKnego/tap/roost` is still available, but a user
  who taps both gets a formula-name collision. homebrew-core `roost` is free
  and now contested.
- **AUR.** `roost` is taken — by a *third* project: `roost 0.2.5-1`, a
  "TUI-first, Rust-based dotfiles manager with git-sync" (`mt-22/roost`).
  `roost-bin` is free.
- **crates.io.** Secure. `roost` is this project, created 2026-09-04. Note that
  their README tells readers the crates.io name "belongs to an unrelated 2018
  crate", which is false — a reader following it concludes this project is the
  impostor.

**Decided on 2026-09-08: the name stays `roost`.** The rename was considered
and cancelled. Nothing above changes; what follows records what the decision
costs, so that the package metadata can be honest about it rather than
discovering each cost at submission time.

- **`PATH` conflict.** Both binaries are called `roost`. A user with both gets
  whichever comes first on `PATH`. This is now permanent — state it in every
  package description.
- **Homebrew: no action.** `PeterKnego/tap/roost` is namespaced and unaffected.
  A user who has already tapped `navbytes/tap` must install by the
  fully-qualified name.
- **AUR: needs `conflicts=('roost')`.** See Step 5. The existing package
  installs `/usr/bin/roost` and declares `provides=("roost")`, so this is a
  hard file conflict, not a naming preference.
- **crates.io: no action.** `cargo install roost` and `cargo binstall roost`
  both resolve to this project.
- **Show HN.** Comparison questions will come in the first comments. Prepare an
  answer, not a correction.

**6.2 — License metadata.**
`Cargo.toml` declares `MIT OR Apache-2.0`. Both `LICENSE-MIT` and
`LICENSE-APACHE` are in the repository. The GitHub API reports the license as
Apache-2.0 only. Distribution packagers read the GitHub field. Correct the
detection, or the packages will state the wrong license.

**6.3 — macOS Gatekeeper.**
Binaries that a browser downloads are quarantined. Binaries that `curl` fetches
and binaries that Homebrew installs mostly avoid this. The two primary channels
therefore avoid the need for an Apple Developer account (approximately 100 EUR
per year). If you later publish a download link on a web page, this becomes
necessary.

**6.4 — Security note for package descriptions.**
Roost binds to `127.0.0.1` and has no authentication of its own. Keep this
statement in every package description and in the post-install message. A
package manager install can make a user assume that the service is safe to
expose.

## 7. How this was checked

Re-verified on 2026-09-06 against live sources rather than from memory:

- Release assets — `gh release view <tag> --json assets`. CI — the working
  tree's `.github/workflows/`, which contains only `ci.yml`.
- crates.io and AUR — their JSON APIs. Homebrew — the `formulae.brew.sh` API
  (`dtach` is present at 0.9; `roost` returns 404).
- Licence detection — `gh api repos/PeterKnego/roost` reports `Apache-2.0`,
  confirming 6.2.
- glibc floor — `objdump -T` and `readelf -V` on the published tarball, then
  the binary run under Ubuntu 22.04 and Debian 12 loaders extracted from their
  `libc6` `.deb` packages. No container is needed for this:
  `ld-linux-x86-64.so.2 --library-path <dir> ./roost` runs a binary against any
  libc, and running the host loader first proves the harness is not what broke
  it.
- musl — `cargo build --locked --release --target <triple>` from this checkout.
- The AUR file conflict in 6.1/Step 5 — the `roost` PKGBUILD fetched from
  `aur.archlinux.org/cgit`, which installs `/usr/bin/${_appname}` with
  `pkgname=${_appname}` and declares `provides=("roost")`.

Still unverified: the two macOS targets, and the Amazon Linux 2023 row in C2
(2.34 is below both loaders that were tested, so it follows by inspection, but
it was not run).
