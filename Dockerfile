# syntax=docker/dockerfile:1

# A container image for roost. See docs/deploy.md for what it gives up compared
# with the systemd unit — shells across a restart, above all — and
# docs/superpowers/specs/2026-09-10-container-image-design.md for why each of
# the decisions below is the one it is.
#
# **Every relaxation of the usual hardening advice is marked, with its reason.**
# An unmarked relaxation is how "hardened" becomes a claim rather than a fact,
# and this image relaxes several on purpose, because roost's job — spawn a
# shell, edit the files, run whatever the user asks — is the list of things a
# hardened container is designed to prevent.
#
# The threat model, since every decision leans on it: **someone with a shell in
# this container is not an attacker, it is the user.** The controls worth having
# bound the blast radius *outward* — what is mounted, what the network reaches,
# whether container root becomes host root. Controls that assume the inside is
# hostile buy nothing here and cost the product.
#
# Refresh a digest with:
#   docker pull debian:trixie-slim && docker image inspect debian:trixie-slim \
#     --format '{{index .RepoDigests 0}}'

# Debian, not Alpine, and the reason is functional rather than a preference:
# `claude` is a glibc binary (interpreter /lib64/ld-linux-x86-64.so.2) and does
# not run on musl, so an Alpine image cannot host the thing most people install
# here. Measured cost, 2026-09-10, `trivy --severity HIGH,CRITICAL`: Debian
# trixie-slim carries 53 more findings than alpine:3.22 and **every one of them
# is unfixable** — while the count a CI gate acts on is identical, two each.
ARG RUNTIME_IMAGE=debian@sha256:d7e12182ce18b85b93007c1dedf31f2d29e01ccf3182cc4017c709b6259bc132
# rust 1.98.1
ARG BUILDER_IMAGE=rust@sha256:bce1476d4be4d78b83705bc5f428b86d640eeeea33e9dadafbc037b5703a53bf

# Files roost writes into a mounted checkout must be owned by whoever owns that
# checkout on the host, or every file the agent touches comes back unwritable.
# So this is a build arg and there is no fixed 65532: `docker compose build
# --build-arg APP_UID=$(id -u)`.
ARG APP_UID=1000
ARG APP_GID=1000
ARG APP_USER=roost
ARG APP_HOME=/home/roost

ARG VCS_REF=unknown
ARG APP_VERSION=unknown
ARG BUILD_DATE=unknown


# --------------------------------------------------------------------------
# Builder: a static musl binary, cross-compiled rather than emulated
# --------------------------------------------------------------------------
# On $BUILDPLATFORM on purpose. The binary has no C dependencies and
# `.cargo/config.toml` already sets `linker = "rust-lld"` for
# aarch64-unknown-linux-musl — added for exactly this cross-link, with a comment
# saying it needs no cross-gcc, `cross` or `zig`. Emulating an arm64 Rust build
# under QEMU costs tens of minutes for the same bytes.
FROM --platform=$BUILDPLATFORM ${BUILDER_IMAGE} AS builder
ARG TARGETARCH

WORKDIR /app
COPY Cargo.toml Cargo.lock build.rs ./
COPY .cargo ./.cargo
COPY src ./src
COPY static ./static

# Static musl on a glibc runtime is deliberate, not an oversight: it makes the
# runtime base a free choice, so it can change — when trixie ages out, or if the
# measurement above ever changes — without touching this stage. It is also
# already the shape `dist` produces for release. Safe for this binary because
# roost resolves the home directory from $HOME and calls no NSS function, which
# is the usual thing that breaks a static musl build.
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) triple=x86_64-unknown-linux-musl ;; \
      arm64) triple=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported TARGETARCH=$TARGETARCH" >&2; exit 1 ;; \
    esac; \
    rustup target add "$triple"; \
    cargo build --locked --profile dist --target "$triple"; \
    install -m 0755 "target/$triple/dist/roost" /roost


# --------------------------------------------------------------------------
# Runtime
# --------------------------------------------------------------------------
FROM ${RUNTIME_IMAGE} AS runtime
ARG APP_UID
ARG APP_GID
ARG APP_USER
ARG APP_HOME
ARG VCS_REF
ARG APP_VERSION
ARG BUILD_DATE

# What the product *is*, not a minimal set. A terminal with no shell is not a
# terminal.
#
#   dtach              why sessions survive at all; a hard runtime prerequisite
#                      already (Cargo.toml declares it for deb and rpm)
#   bash               session::default_command runs `$SHELL -l`
#   git                the tree, the diff pane and the changes pane shell out
#   curl, ca-certs     how `claude` gets installed into the persistent HOME
#   ripgrep            Claude Code wants it
#   less, ssh, procps  what a shell in a project needs to be worth having;
#                      procps also lets roost's registry read `ps`, which is how
#                      it tells a live session from an orphaned socket
#   sudo               RELAXATION, see below
RUN set -eux; \
    apt-get update; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      bash ca-certificates curl dtach git less openssh-client procps ripgrep sudo tini; \
    rm -rf /var/lib/apt/lists/*

# RELAXATION 1 — the app user gets a real shell, not `nologin`.
# The websocket spawns `$SHELL -l` as this user by design. A `nologin` in
# /etc/passwd would not prevent that (dtach is handed the shell explicitly), so
# it would be a claim that reads as a control and is not one.
#
# RELAXATION 2 — passwordless sudo, and therefore no `no-new-privileges`.
# The point of this image is that the agent can install a toolchain, and every
# route to root is a setuid binary. Said out loud rather than quietly omitted:
# in a container whose purpose is running the user's own agent, root *inside the
# namespace* is not the boundary anyone relies on — the namespace is. The
# reproducible alternative is a derived image (`FROM this` + your own
# `apt-get install`), which docs/deploy.md documents as the supported route.
RUN set -eux; \
    groupadd -g "${APP_GID}" -o "${APP_USER}"; \
    useradd -u "${APP_UID}" -g "${APP_GID}" -o -m -d "${APP_HOME}" -s /bin/bash "${APP_USER}"; \
    printf '%s ALL=(ALL) NOPASSWD:ALL\n' "${APP_USER}" > /etc/sudoers.d/roost; \
    chmod 0440 /etc/sudoers.d/roost; \
    mkdir -p /var/lib/roost; \
    chown "${APP_UID}:${APP_GID}" /var/lib/roost "${APP_HOME}"

# Setuid/setgid bits stripped image-wide *except* sudo, which is useless without
# one. Nothing else in here needs to escalate.
RUN find / -xdev -perm /6000 -type f ! -path /usr/bin/sudo -exec chmod a-s {} + || true

# Root-owned and not writable by the app user, so the process cannot rewrite its
# own code. This is one of the controls that is *not* relaxed, and it survives
# the writable rootfs below because ownership, not mount flags, is what enforces
# it.
COPY --from=builder --chown=root:root /roost /usr/local/bin/roost

# RELAXATION 3 — no `read_only: true` (set in the compose file, noted here).
# The product installs toolchains and compiles. Note that a writable rootfs is
# *not* what makes an install survive: anything written to /usr is gone at the
# next `docker compose up` after an image change. $HOME is the durable place,
# and rustup, the Go tarball, nvm, npm and the `claude` installer all install
# there natively, as this user, with no root at all.
#
# RELAXATION 4 — the package manager stays.
# The reference practice removes it so an attacker cannot install tooling. Here
# the *user's* purpose is to install tooling, and anyone who can complete a
# websocket handshake already has a shell as this user. Removing apt protects
# nothing from them and costs the product.

# `claude` is deliberately absent. Baking it pins a version that is stale within
# days; installing it into the persistent $HOME instead means it survives every
# restart and upgrade, updates itself, and shares one $HOME with roost — which
# is the condition idelock.rs needs, since it writes
# $CLAUDE_CONFIG_DIR/ide/<port>.lock and the CLI scans that same directory.
# See docs/deploy.md for the one line that installs it.

ENV SHELL=/bin/bash \
    HOME=${APP_HOME} \
    ROOST_STATE_DIR=/var/lib/roost \
    LANG=C.UTF-8

LABEL org.opencontainers.image.title="roost" \
      org.opencontainers.image.description="A per-project remote workspace in one binary: browser IDE layout over real terminals that survive the tab" \
      org.opencontainers.image.source="https://github.com/PeterKnego/roost" \
      org.opencontainers.image.documentation="https://github.com/PeterKnego/roost/blob/master/docs/deploy.md" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0" \
      org.opencontainers.image.version="${APP_VERSION}" \
      org.opencontainers.image.revision="${VCS_REF}" \
      org.opencontainers.image.created="${BUILD_DATE}"

USER ${APP_UID}:${APP_GID}
WORKDIR ${APP_HOME}
EXPOSE 8123

# A static asset, NOT `/`. The overview shell fetches fragments that reach
# `known_projects_inner`, whose first line is `reconcile_throttled` — the sweep
# that kills sessions whose directory is gone and unlinks sockets whose holder
# is dead. Putting that judgement on an unattended 30-second timer driven by the
# container runtime is what health.rs's module doc refuses to do. This proves
# the process is alive, the listener accepts, the HTTP loop parses, the host
# check passes and the asset table is intact — and it takes no lock and spawns
# no subprocess.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8123/static/favicon-32.png -o /dev/null || exit 1

# tini as PID 1: dtach masters reparent to it by design, so without an init they
# accumulate as zombies in a long-lived container.
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/roost"]
CMD ["8123"]
