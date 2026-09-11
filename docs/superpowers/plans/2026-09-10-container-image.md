# A container image for roost

*2026-09-10. Issue #53. Spec: `docs/superpowers/specs/2026-09-10-container-image-design.md`.*
*Status: **Part one implemented and verified 2026-09-10**; Part two is the next*
*commit on the same branch.*
*Order set by Dean: **a usable image first, then the security that fits this
kind of project**. That is not a sequencing preference — it changes answers. A
hardening-first reading picks Alpine, and Alpine cannot run `claude`.*

## Decisions already settled, so the tasks do not reopen them

| Question | Settled |
|---|---|
| Base | glibc. `claude` is an ELF wanting `/lib64/ld-linux-x86-64.so.2`; measured cost of Debian over Alpine is 53 *unfixable* HIGH/CRITICAL and **the same 2 fixable ones** |
| Bind | `0.0.0.0` in the namespace, gated on `ROOST_BIND_ALL=1`, port published to host loopback only |
| Project mounts | **the host's own absolute path**, mirrored — git worktrees, `.origin` markers and Claude's transcript directories all encode absolute paths |
| `claude` | not in the image; installed once into the persistent `$HOME` |
| Writable root | yes, no `read_only:`; `sudo` stays and `no-new-privileges` goes |
| Sessions across restart | shells die; #18 step 2 (PR #68) brings the conversation back |

## Part one — a usable image

### T1. `ROOST_BIND_ALL` (Rust, no container needed)

`src/main.rs` binds `("127.0.0.1", port)` today, and `SECURITY.md` says the bind
"is deliberately not configurable". One branch, as a pure function so the whole
table is a unit test:

| Value | Behaviour |
|---|---|
| unset / `0` | `127.0.0.1`, as today |
| `1` | `0.0.0.0`, plus one stderr line naming the substitution |
| anything else | exit 2, naming the value |

Not an address-shaped variable — `ROOST_BIND=0.0.0.0` is a thing someone types
on a host. An unrecognised value is **fatal, not a fallback**: falling back to
loopback is the safe direction and the wrong one, because `ROOST_BIND_ALL=true`
would then start a container listening where nothing can reach it and say
nothing. That is CLAUDE.md's table exactly — a check that failed, read as an
answer.

`src/ide.rs` keeps its unconditional `127.0.0.1`; its client is a `claude` in
the same container.

Tests: the table; that the parse is not `unwrap_or(false)`; that `ide` is
untouched.

### T2. `scripts/test/container.sh`, written before the Dockerfile

CLAUDE.md's dev/prod substitution table has four rows and three are this class.
A container is a fourth substitution nothing existing can see: `cargo test` runs
on the host, `tests/browser/*.mjs` drives a host roost. Written first because
several assertions will fail on the first honest attempt, and each failure is a
finding the spec should have contained.

Skips with a message when there is no docker daemon, in the same shape as the
browser tests. `make test-container`.

### T3. `Dockerfile` + `.dockerignore`

Builder: Rust, static musl, cross-compiled in a `--platform=$BUILDPLATFORM`
stage (`.cargo/config.toml` already sets `linker = "rust-lld"` for
`aarch64-unknown-linux-musl`, added for exactly this). Runtime:
`debian:trixie-slim` + `dtach`, `bash`, `git`, `curl`, `ca-certificates`,
`ripgrep`, `less`, `openssh-client`, `sudo`. Digest-pinned with the refresh
command in a comment; `APP_USER`/`APP_UID`/`APP_GID`/`HOME` build args; OCI
labels; `HEALTHCHECK` on `/static/favicon-32.png`, **not** `/` — the fragments
that page fetches reach `known_projects_inner`, whose first line is
`reconcile_throttled`, and reconcile kills sessions.

A comment against every relaxation, naming it. An unmarked relaxation is how
"hardened" becomes a claim rather than a fact.

### T4. `docker-compose.yml`

Three mounts (roots at the mirrored host path, `$HOME` volume, state volume),
port on `127.0.0.1` only, its own network with one service on it, `init: true`,
`ROOST_BIND_ALL=1`, `ROOST_ROOTS` matching the mount.

### T5. `docs/deploy.md`

A container section, honest about what it gives up: shells across a restart
above all, the mirrored paths and why, the UID, `$HOME` as the only writable
place that survives an upgrade, and — in bold — that the container's state dir
must never be shared with a host roost, because the container's `ps` cannot see
the host's dtach masters and reconcile would unlink live sockets.

## Part two — the security that fits

Applied to a working image, argued from the threat model rather than the
reference checklist: **someone with a shell in this container is not an
attacker, it is the user.** So the controls that count bound the blast radius
*outward*.

- T6. compose posture: `cap_drop: [ALL]` (verified against a real PTY, not
  asserted), `pids_limit`, `mem_limit`, `cpus`, bounded logging; `sudo` kept and
  `no-new-privileges` dropped, said out loud.
- T7. SBOM in the image (syft, CycloneDX), `security/roost.openvex.json`,
  `security/README.md` with the measured numbers, the CIS table, and the
  refusals **listed as refusals**: no `read_only`, no removed package manager,
  no `nologin` shell, no `noexec` on `/tmp`.
- T8. `.github/workflows/publish-image.yml` → GHCR, buildx `sbom`/`provenance:
  mode=max`, keyless cosign, weekly rebuild, trivy gating on *fixable*
  HIGH/CRITICAL, every action pinned by SHA.
- T9. `SECURITY.md` — "binds `127.0.0.1` only, and that bind is deliberately not
  configurable" is now false of one deployment; two paragraphs instead.
  CLAUDE.md's first hard constraint gains the container case in the same shape
  as the `ide.rs` bullet: two rules that are opposites because their deployments
  are, with reconciling them named as the mistake.

## Out of scope

Pinning the *existing* workflows' actions (its own change; `release.yml` is
hand-edited and exempted from `dist` regeneration), CodeQL/Scorecard/fuzzing
(about the repository, not the image), a Home Assistant add-on, and rootless
Docker (a host posture, documented not configured).


## What Part one actually proved, on the real thing

`scripts/test/container.sh` builds the image and drives it. Everything below was
observed, not argued:

- a websocket opened a session, the shell ran a command, and the file it wrote
  landed in a bind-mounted checkout **owned by the host uid** — the PTY works
  with `--cap-drop ALL` (`CapEff=0000000000000000`), which the spec said to
  verify rather than assert;
- a real `dtach` holds a real socket under the state volume — the `ROOST_CMD=cat`
  row of CLAUDE.md's table, reproduced in the new environment;
- `git worktree list` inside the container sees a worktree created on the host,
  which is the whole reason the mount mirrors the host path;
- `ROOST_BIND_ALL` unset ⇒ the published port refuses the connection;
  `=yes` ⇒ exit 2 naming the value;
- no setuid binaries left apart from `sudo`;
- `claude` installs into the persistent `$HOME` (exit 0), and **a brand-new
  container on the same volume still finds it on a login shell's PATH and runs
  it** — `claude --version` → 2.1.268. That is the base-image decision confirmed
  end to end: it is a glibc binary and this is why the image is not Alpine.

Two defects the test found by being written first, both in the test rather than
the image, and both invisible to a green run:

- **`set -e` killed the script at the first failing assertion**, so the first
  failure hid every one after it. `check <message> <command…>` now runs the
  assertion as the helper's own argument.
- **keystrokes are `Message::Binary`; `Message::Text` is the resize channel.**
  The first client sent text, which `term.rs` correctly ignored — so the session
  spawned, dtach held its socket, everything looked healthy, and nothing was
  ever typed. Exactly the shape of a test that passes for the wrong reason.
