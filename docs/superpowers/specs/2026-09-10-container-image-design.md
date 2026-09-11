# roost in a container

*2026-09-10. Status: designed, not implemented. Issue
[#53](https://github.com/PeterKnego/roost/issues/53). Decisions from Dean in
that issue's comment and in conversation on 2026-09-10, which reversed two of
the issue's own starting assumptions — see* Two reversals *below.*

## What and why

roost is installed today by putting a binary on a box and pointing a systemd
user unit at it. That is five install channels and one way to *run*. The ask is
an image, to the standard of
[dgprivate/kronoterm2mqtt](https://github.com/dgprivate/kronoterm2mqtt).

## Two reversals from the issue as filed

**Order.** The issue is written as a hardening checklist with roost's frictions
listed after it. Dean reversed that: *first a usable image, then the security
that makes sense for this kind of project.* This document follows the new order,
because it turns out to change the answers rather than just their sequence — a
hardening-first reading picks Alpine, and Alpine cannot run `claude`.

**Base.** The issue's reference chose Alpine on measured evidence. For roost the
base is decided by a functional requirement instead: **`claude` is a glibc
binary.** On this machine `/usr/bin/claude` resolves to an ELF with interpreter
`/lib64/ld-linux-x86-64.so.2`; it cannot run on musl. Dean: *"uporabimo base, ki
lahko claude normalno namesti."* So the base is glibc, and the evidence gets
recorded rather than used to pick — measured 2026-09-10 with
`trivy --severity HIGH,CRITICAL`:

| Base | fixable HIGH/CRIT | all HIGH/CRIT |
|---|---|---|
| `debian:trixie-slim` | **2** | **55** |
| `alpine:3.22` | **2** | **2** |

The reference's argument reproduces in shape and not in the number that matters.
Debian carries 53 more HIGH/CRITICAL findings, and **all 53 are unfixable** —
exactly the class the reference names, packages the app never executes whose
advisories Debian will not patch. But the count a CI gate acts on is *identical*:
two fixable findings each, and both are cleared by rebuilding.

So the cost of the glibc base is precise, and it is not a weaker gate. It is 53
findings that every scan of this image will report forever, which someone has to
read past every time — the reason `--ignore-unfixed` is the gate in Part two and
the reason the VEX document earns its place. Named here so nobody re-opens this
in six months on a scanner screenshot.

Both numbers are the *base* images. The plan re-measures the built roost images,
which add `dtach`, `git`, `curl`, `sudo` and the rest, and records those in
`security/README.md` with the date and versions.

## Part one — a usable image

### The threat model this project actually has

Stated first, because every later decision leans on it and because it is the
thing that makes roost different from the reference bridge.

**Someone with a shell in this container is not an attacker. It is the user.**
roost's websocket spawns a shell by design; the agent running in it installs
toolchains, compiles, and rewrites the mounted checkout, because that is the
product. So the controls worth having are the ones that bound the blast radius
*outward* — what is mounted, what the network reaches, whether container root
becomes host root. Controls that assume the inside is hostile — a read-only
rootfs, a removed package manager, a `nologin` shell — buy nothing here and cost
the product. They are refused in Part two by name, not omitted quietly.

### The user, and why the container mirrors the host

`APP_USER`, `APP_UID`, `APP_GID` and `HOME` are build args. The container's user
mirrors the host user that owns the checkouts, and **project roots are
bind-mounted at the same absolute path they have on the host** — `/home/dean/projects`
→ `/home/dean/projects`, not `/projects`.

That last part is the answer to *"pazit mormo kako projects notr prpelemo"*, and
it is not cosmetic. Three separate things record absolute paths and break under
a remapped mount:

- **git worktrees.** A worktree's `.git` is a file containing
  `gitdir: /abs/path/.git/worktrees/<name>`, and the repository's
  `worktrees/<name>/gitdir` points back the same way. roost discovers worktrees
  by asking `git worktree list` (`docs/deploy.md`), so under a remapped path the
  entire worktree feature — the header strip, the launch flow, the overview
  children — sees nothing, and git itself refuses the worktree.
- **roost's `.origin` markers**, which record the absolute path a project
  resolved to and are what stops reaping from killing a live session it cannot
  otherwise account for.
- **`claude`'s own transcript directories**, keyed by an encoding of the working
  directory (`~/.claude/projects/-home-dean-projects-karpie`). A remapped path
  silently orphans every past conversation for that checkout.

Mirroring costs nothing and removes all three at once. A UID that does not match
is the other half: files roost writes into a mounted checkout must be owned by
whoever owns it on the host, or every file the agent touches comes back
unwritable.

### What persists, and where

One rule, because a rule with exceptions is one people get wrong: **everything
that must survive an upgrade lives in a mount; the image holds only the roost
binary and the distro packages.**

| Mount | Contents | Why it cannot be in the image |
|---|---|---|
| project roots, at the host path | the checkouts | the product |
| `$HOME` (volume) | `~/.claude` — auth, transcripts, `ide/*.lock`; `~/.local/bin/claude`; `~/.cargo`, `~/go`, `~/.nvm`; shell history | auth and installed toolchains must survive `docker compose up` after an image change |
| `$ROOST_STATE_DIR` (volume) | workspace layouts, buffers, `notifications.json`, `error.log`, `sock/` | layouts and unsaved buffers are the user's work |

**`$HOME` as one volume is the whole answer to Claude's auth surviving a
restart.** The credential is an ordinary file — `~/.claude/.credentials.json`,
mode 0600 — so persisting the directory persists the login. Nothing clever is
needed and nothing about the container runtime is involved.

It also disposes of the issue's open question about *where `claude` comes from*,
better than either option that was on the table there:

- **not baked into the image** — that pins a version that is stale within days;
- **not mounted from the host** — the host binary is glibc-linked to the *host's*
  glibc and, more to the point, mounting the host's `~/.claude` hands over the
  OAuth token and every transcript;
- **installed once, into the persistent `$HOME`**, from a terminal inside the
  container (`curl -fsSL https://claude.ai/install.sh | bash` →
  `~/.local/bin/claude`). It survives every restart and upgrade, it
  self-updates, so it is never the image's stale copy, and `claude` and roost
  share one `$HOME` — which is exactly the condition `idelock.rs` needs, since
  it writes `$CLAUDE_CONFIG_DIR/ide/<port>.lock` and the CLI scans that same
  directory.

The plan must *run* that install line in the real image and record the result;
an example nobody executed is the kind of documentation that is wrong for a
year. The image ships `curl` and `ripgrep` so it can succeed.

`$HOME` is a **container volume, not the host's home directory.** Mounting the
host's real `$HOME` would share auth and history, and is offered as a documented
opt-in with the grant spelled out — it hands the container the user's account.
The default should not back anyone into that.

### Installing toolchains

Dean: *"pustit, da je root writable, da si claude lahk namesti stvari kot je
golang / rust."* Taken, with one correction that matters more than the setting:

**A writable rootfs is not what makes an install survive.** Anything written to
`/usr` in a running container is gone at the next `docker compose up` after an
image change — which is the one moment a user is most likely to expect their
toolchain to still be there. The durable location is `$HOME`, and `rustup`, the
Go tarball, `nvm`, `npm --prefix`, `uv` and the `claude` installer all install
there natively, as the app user, with no root at all.

So both, with the order stated:

1. `read_only: true` is **not** set. The rootfs is writable, as asked.
2. `$HOME` is where toolchains are *meant* to go, and the docs say why: it is
   the only writable place that survives an image upgrade.
3. `apt-get install` needs root. Part one gives the app user passwordless
   `sudo`; Part two revisits it, because it is the single control that trades
   most against `no-new-privileges`. Reproducibly, the answer is a derived image
   (`FROM ghcr.io/peterknego/roost` + your own `RUN apt-get install …`), and
   that gets documented as the supported route rather than as a scolding.

### The bind

CLAUDE.md's first hard constraint is that roost binds `127.0.0.1` and that the
loopback bind *is* the security boundary. Inside a container `127.0.0.1` is the
*container's* loopback, so a host publishing `127.0.0.1:8123:8123` cannot reach
it.

**Decided (Dean, issue #53): bind `0.0.0.0` inside the container, with the
network namespace as the substituted boundary.** A substitution, not a
relaxation, and it holds only with all three of:

1. the port published to host loopback only — `127.0.0.1:8123:8123`, never
   `8123:8123`, which Docker renders as `0.0.0.0` and on most daemons also
   punches through the host firewall;
2. no other reachable service on the container's network. The compose file
   declares its own network with one service on it. The browser-facing socket's
   `Origin` check does not stop a client that sends *no* `Origin` — a curl from
   a second container on the same network gets a shell;
3. the non-loopback bind gated explicitly, so no host build reaches it by
   accident.

`src/main.rs` gains one branch, `ROOST_BIND_ALL`:

| Value | Behaviour |
|---|---|
| unset or `0` | bind `127.0.0.1`, as today |
| `1` | bind `0.0.0.0`, and say so on stderr in one line that names the substitution |
| anything else | refuse to start, exit 2, naming the value |

Not an address-shaped variable: `ROOST_BIND=0.0.0.0` is a thing someone types on
a host, and a boolean named for its consequence can only be set on purpose. And
an unrecognised value is **fatal, not a fallback** — falling back to loopback is
the safe direction and the wrong one, because `ROOST_BIND_ALL=true` in a compose
file would then produce a container that starts, listens where nothing can reach
it, and says nothing. That is the shape of every row in CLAUDE.md's "absence of
evidence" table: a check that failed, read as an answer.

`src/ide.rs` keeps its unconditional `127.0.0.1`. Its client is a `claude`
inside the same container, so container loopback is exactly right, and it
authenticates by token besides. The gate does not reach it.

Published on host loopback, the browser's `Host` and `Origin` are both
`127.0.0.1:8123`, which `origin.rs` passes unlisted — so **the container needs
no `allowed_origins` entry to work**, and needs one for exactly the same reason
a host does: putting a name in front of it. Worth stating, because it is the
first thing that will look broken and will not be.

### Sessions across a restart — a mount is not the mechanism, #18 is

Dean asked for a mount *"kamor lahko roost persista seje, da jih ne zgubi po
restartu dockerja / upgradu"*, then asked whether doing
[#18](https://github.com/PeterKnego/roost/issues/18) first would let a container
restore normally. The second question is the right one, and the answer is yes.

**No mount can persist a shell.** A dtach socket is worthless without its master
process: the file survives on the volume, the process dies with the container.
`KillMode=process` in `packaging/roost.service` exists to stop systemd doing
exactly this on the host, and there is no container equivalent — when roost is
the container's main process, roost exiting *is* the container exiting.

**But a container restart is, from roost's point of view, the same event as a
host reboot** — no process outlives either. #17 says so about reboots, the
project has already accepted that cost on the host, and #17/#18 is already its
chosen answer. So this is not a container-specific deal-breaker. It is that a
container reboots more often, and the fix is the one already designed.

What each mechanism brings back after a container restart, with the mounts above:

| | Restored by | Status today |
|---|---|---|
| workspace layout, panes, tabs | the persisted state dir | ships |
| unsaved buffers | the persisted state dir | ships |
| the terminal's working directory | `cwds.rs` — written for reboots | ships |
| that this terminal was running an agent | `relaunch.rs` (#17 step 3, opt-in, global-only) | ships |
| **the Claude conversation** | **`claude --resume <id>`** | **shipped** — #18 step 2, PR #68 |
| the shell process | nothing | impossible |
| scrollback | nothing — `screen::Screens` is an in-memory ring per process | lost |

**#18 step 1 already shipped** (`888b66e`, `src/claudesess.rs`): `SessionStart`
is in `claudehooks::EVENTS`, `record_from_hook` runs before the notification
gate precisely so a session that never finishes a turn is still recorded, and
`claudesess::recorded()` returns `Option` so absence means *unknown* rather than
"there was no session".

**Nothing in production read it**, until this. `recorded()` had exactly one
caller outside its own module and that caller was a test (`session.rs`, inside
`#[cfg(test)]`): the id was written on every hook event and consumed by nobody.
Step 2 closed that on 2026-09-10 (PR #68) — the placeholder of a terminal whose
shell is gone now offers to continue its conversation, and `docs/deploy.md` says
what it does and does not restore. So the gap between "a container restart loses
everything" and "a container restart loses the shell and keeps the work" is
closed before the image exists, which is the order this was worth doing in.

Two things reinforce each other here and are worth naming, because getting
either wrong breaks the other silently:

- **The mirrored mount path is what makes resume work at all.** Claude Code
  keys its transcripts by an encoding of the absolute working directory
  (`~/.claude/projects/-home-dean-projects-karpie`). Mount the checkouts at
  `/projects` and `--resume` finds nothing — no error, just no history. The
  same decision that keeps git worktrees working keeps resume working.
- **The persisted `$HOME` is where the transcripts live.** They are Claude
  Code's files under `~/.claude/projects/`, so the volume that keeps the login
  keeps the conversations.

**Consequence for this design: the supervisor PID 1 is withdrawn.** It was the
only arrangement that kept shells across a roost restart, at the cost of an
unusual PID 1 and — for upgrades — a roost binary mounted from outside the
image, which trades away the reproducibility that makes an image worth having.
With #18 step 2 the thing users actually want back comes back, so the exotic
mechanism buys the difference between a live shell and a fresh shell in the
right directory with the conversation resumed. Not worth it.

**Recommendation: #18 step 2 is a prerequisite of Part one, not a follow-up.**
It is small, it is useful on the host as well (a reboot is the case it was
written for), and without it the container's restart story is materially worse
than the host's rather than equal to it.

What still does not come back, stated so the docs can say it plainly:

- **only Claude terminals.** A plain shell running a build, a `tail -f`, an
  editor — nothing restores those, on the host after a reboot either;
- **scrollback**, which is per-process and in memory. A resumed Claude reprints
  its own history, so this bites the non-Claude terminals hardest;
- **projects without the bell on.** Hooks are per-project and opt-in, so there
  is no record to resume from — and `claudesess.rs`'s own doc is explicit that
  this must never be rendered as "there was no session";
- and resume is **offered, never automatic**. #17 is explicit and `relaunch.rs`
  quotes it: resuming continues a conversation whose last turn may have been
  mid-edit, which is worse than a fresh start, not better.

One hazard that must be in the docs in bold, because it is the CLAUDE.md
destruction class exactly: **the container's state dir must never be shared with
a roost running on the host.** The container's `ps` cannot see the host's dtach
masters, so reconcile reads the host's live sockets as held by nothing and
unlinks them — leaving live shells alive and unreachable forever.

### Healthcheck

`HEALTHCHECK` hits `http://127.0.0.1:$PORT/static/favicon-32.png` with busybox-
or curl-level HTTP, every 30 s.

Not `/`. `serve_index` is cheap today — it renders the overview shell and
nothing else — but the shell's job is to fetch fragments, and *those* reconcile:
`/frag/_overview_projects` reaches `registry::project_rows`, `/frag/_projects`
and `/frag/_worktrees` reach `known_projects`, and all three land on
`known_projects_inner`, whose first line is `reconcile_throttled`. Reconcile
kills sessions whose project directory is confirmed gone and unlinks sockets
whose holder is confirmed dead.

The objection is not that `/` reconciles now; it is that `/` is the page most
likely to grow, and a healthcheck is the one caller nobody watches. Putting that
judgement on an unattended 30-second timer is what `health.rs`'s module doc
refuses to do, for the reason it gives. A static asset proves what a healthcheck
should — process alive, listener accepting, HTTP loop parsing, host check
passing, asset table intact — spawns no subprocess, takes no lock, and cannot
acquire either later.

### Build

Two stages. The builder is a Rust image producing a **static musl binary**
(`cargo build --locked --profile dist --target <triple>`); the runtime is
`debian:trixie-slim` plus `dtach`, `bash`, `git`, `curl`, `ca-certificates`,
`ripgrep`, `less`, `openssh-client`, `sudo`.

Static musl on a glibc base is deliberate, not an oversight: it makes the
runtime base a free choice, so the base can change — for the measured reasons in
Part two, or when trixie ages out — without touching the build. It is also
already the shape `dist` produces for release. Verified safe for this binary:
roost resolves the home directory from `$HOME` only and calls no NSS function,
which is the usual thing that breaks static musl.

Multi-arch `linux/amd64` + `linux/arm64`, cross-compiled in a
`--platform=$BUILDPLATFORM` builder rather than emulated. Close to free here
only because the groundwork exists: `.cargo/config.toml` already sets
`linker = "rust-lld"` for `aarch64-unknown-linux-musl`, added for exactly this
cross-link with a comment saying it needs no cross-gcc, `cross` or `zig`. QEMU
is the fallback, and the plan measures before choosing.

`build.rs` bakes absolute asset paths into a generated table (CLAUDE.md, *Build
from one checkout*). Inside the image that path is `/app/static` and the context
is isolated, so the shared-target-dir hazard cannot arise — but the build must
not be handed the host's target directory as a cache mount, for the same reason.

### Part one deliverables

1. `Dockerfile` — two stages, bases pinned by digest with the refresh command in
   a comment, `APP_USER`/`APP_UID`/`APP_GID`/`HOME`/`VCS_REF`/`APP_VERSION`/
   `BUILD_DATE` as build args, OCI labels, `HEALTHCHECK`.
2. `.dockerignore` — `/target`, `.git`, `tests/browser/tmp`, `docs/img`,
   `.claude/worktrees`.
3. `docker-compose.yml` — the three mounts, the port on `127.0.0.1` only, its
   own network, `init: true`, `ROOST_BIND_ALL=1`, `ROOST_ROOTS` matching the
   mirrored mount path.
4. `src/main.rs` — the `ROOST_BIND_ALL` gate, as a pure function with the whole
   table under test, plus the startup line.
5. `docs/deploy.md` — a container section: sessions across restarts above all,
   the mirrored paths and why, the UID, `$HOME` as the durable place for
   toolchains and auth, and the shared-state-dir hazard.
6. `scripts/test/container.sh` — Part one's assertions, below.

## Part two — the security that fits this project

Applied to a working image, and argued from the threat model above rather than
from the reference's checklist. Each item is either kept, kept-with-a-reason, or
**refused by name**.

**Kept, and they are the ones that matter** — every one bounds the blast radius
outward:

- non-root app user, with the host's UID (already Part one);
- `cap_drop: [ALL]`. To be verified, not asserted: `portable-pty` opens a PTY
  through `/dev/ptmx`, which Docker provides in every container by default, and
  the belief that this needs no capability is exactly the kind of claim this
  codebase's dev/prod table says to test rather than believe;
- `no-new-privileges: true` — **but it is incompatible with the `sudo` from Part
  one**, since sudo is setuid. This is the one genuine trade in the whole
  design and it is a decision for review: passwordless `sudo` in the container,
  or `no-new-privileges` plus the derived-image route for anything needing root.
  Recommendation: keep `sudo`, drop `no-new-privileges`, and say so out loud —
  in a container whose purpose is running the user's agent, root *inside the
  namespace* is not the boundary anyone is relying on; the namespace is;
- `pids_limit`, `mem_limit`, `cpus` — a runaway build in an agent's terminal is
  the most likely way this container hurts its host, and it is not hypothetical;
- bounded json-file logging;
- its own network, one service on it, port on host loopback only;
- setuid/setgid bits stripped image-wide except where `sudo` needs its own;
- application binary root-owned, not writable by the app user.

**Refused, by name, with the reason in the file:**

- `read_only: true` — the product installs toolchains and compiles;
- removing the package manager — the user's purpose is to install tooling, and
  anyone with a shell here is the user;
- a `nologin` shell — the websocket spawns `$SHELL -l` by design, so it would be
  a claim that reads as a control and is not one;
- `noexec` on `/tmp` — breaks language installers that unpack and execute there,
  and the thing it defends against is the user's own agent.

**Supply chain**, which is orthogonal to all of the above and is kept whole:

- CycloneDX SBOM inside the image, generated by syft from the copied filesystem;
- registry-side SBOM and `provenance: mode=max` from buildx;
- keyless Sigstore signing, and a signed SBOM attached to the release;
- an OpenVEX document in `security/` for findings that cannot be fixed and do
  not apply — with the reference repo's own warning carried over, that a purl
  must match the SBOM exactly or the statement is silently ignored, and that a
  VEX statement is a claim about the code, not a mute button. The two fixable
  HIGH/CRITICAL measured above are not VEX candidates: they are fixed by
  rebuilding;
- weekly rebuild; trivy gating on *fixable* HIGH/CRITICAL only;
- every action in the new workflow pinned by commit SHA.

**Registry: GHCR** (`ghcr.io/peterknego/roost`). The release already lives on
GitHub and `GITHUB_TOKEN` needs no new secret; the reference's Docker Hub setup
carries a token, a description-sync action and a `vars.` opt-in that exist to
work around not being there. Keep the opt-in shape so a fork publishes nothing
by accident.

**Documents**: `SECURITY.md`'s sentence *"roost binds `127.0.0.1` only, and that
bind is deliberately not configurable"* is now false of one deployment and
becomes two paragraphs — the boundary on a host, and the substituted boundary in
a container with the three conditions it rests on. CLAUDE.md's first hard
constraint gains the container case in the same shape as the `ide.rs` bullet:
two rules that are opposites because their deployments are, with reconciling
them named as the mistake. `security/README.md` carries the verify-it-yourself
commands, the CIS table with roost's refusals marked as refusals, and what is
deliberately not claimed.

## Testing: what a green suite cannot see

CLAUDE.md's dev/prod substitution table has four rows and three are this class —
`ROOST_CMD=cat` hiding that the socket directory was never created, no systemd
hiding that `KillMode` killed every session, no browser hiding that saving was
broken. A container is a fourth substitution of the same kind, and nothing
existing can see it: `cargo test` runs on the host and `tests/browser/*.mjs`
drives a host roost.

`scripts/test/container.sh` builds the image and asserts against the real thing.
Part one:

| Assertion | Why it is not obvious |
|---|---|
| a websocket opens a session and a command's output comes back | the PTY works in this namespace — the one thing no host test substitutes for |
| the dtach socket exists under the state volume and `dtach` holds it | the `ROOST_CMD=cat` row, reproduced in the new environment |
| a file written from a terminal into a mounted root is owned by the host UID | the whole reason the UID is a build arg |
| `git worktree list` inside the container sees a worktree created on the host | the mirrored mount path, which is the one thing a remapped mount breaks silently |
| a toolchain installed into `$HOME` survives `docker compose down && up` | the durability claim, which is the part users will rely on |
| `ROOST_BIND_ALL` unset ⇒ the published port refuses the connection | the gate is real, and a host build cannot reach `0.0.0.0` |
| `ROOST_BIND_ALL=yes` ⇒ exit 2, message names the value | the third outcome is not folded into "false" |
| `docker inspect` reports `healthy` within the start period | that the probe works at all in this image is not obvious. That it is a static asset rather than `/` is a review property, asserted in a Dockerfile comment |

Part two adds `id -u` ≠ 0, `CapEff` all zeroes, `find / -xdev -perm /6000` empty
apart from `sudo`, and an SBOM that parses and names `dtach` and `git`.

Not part of `cargo test` — it needs a docker daemon and takes minutes — and it
skips with a message when there is none, in the same shape as the browser tests.
`make test-container`.

Write it *first* when implementing: several of those assertions will fail on the
first honest attempt, and each failure is a finding this document should have
contained.

## Deliberately out of scope

- **Pinning the existing workflows' actions by SHA.** `ci.yml` and the rest use
  floating tags. The new workflow is pinned; the others are a separate change
  with their own review, and `release.yml` is hand-edited and exempted from
  `dist` regeneration (`allow-dirty = ["ci"]`), so touching it carries a cost
  this change should not.
- **CodeQL, OSSF Scorecard, fuzzing.** About the repository, not the image.
  Their own issue.
- **A Home Assistant add-on.** Where the reference's `ha-addon/` comes from;
  nothing about roost wants it.
- **Rootless Docker / userns-remap.** A host posture, documented in
  `security/README.md` as the reference does, not configured here.

## Open, for the review to settle

1. ~~`sudo` versus `no-new-privileges`.~~ **Taken as settled by "pustit, da je
   root writable":** an image whose point is that the agent can install a
   toolchain needs a way to reach root, and every route to it is a setuid
   binary. So `sudo` stays and `no-new-privileges` goes, said out loud in the
   compose file rather than quietly omitted. Reopen it if the answer should
   instead be "derived image only".
2. ~~Whether #18 step 2 lands first.~~ **Settled: it did**, as PR #68. (The
   supervisor PID 1 that appeared in an earlier draft of this document is
   withdrawn; #18 replaces it.)
3. **Tag policy.** Recommendation: `latest` from `master` only, `develop`
   published as `edge`.
4. **Whether the weekly rebuild also runs `scripts/test/container.sh`.** It is
   the only thing that would catch a distro update breaking dtach or the PTY.
   Recommendation: yes.
