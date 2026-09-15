#!/usr/bin/env bash
# Builds the image and asserts, against the real thing, the claims that are
# otherwise prose in the Dockerfile.
#
# CLAUDE.md's dev/prod substitution table has four rows and three are this
# class: `ROOST_CMD=cat` hid that the dtach socket directory was never created,
# no systemd hid that `KillMode` killed every session, no browser hid that
# saving was completely broken. A container is a fourth substitution of the same
# kind, and nothing existing can see it — `cargo test` runs on the host and
# `tests/browser/*.mjs` drives a host roost.
#
# So the assertions here are chosen for what only a real container can answer:
# whether a PTY opens with no capabilities, whether dtach actually holds its
# socket on the state volume, whether a file written from a terminal comes back
# owned by the host user, and whether a worktree created on the host is visible
# through the mount. The rest — that a Dockerfile says `USER`, that a compose
# file says `cap_drop` — is readable and is not worth a test.
#
# Not part of `cargo test`: it needs a docker daemon and takes minutes. Skips
# with a message when there is none, in the same shape as the browser tests.
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/lib.sh

if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
  echo "SKIP: no usable docker daemon. Install docker and be in the docker group."
  exit 0
fi

IMAGE=roost:container-test
NET=roost-test-net
NAME=roost-test-$$
UID_N=$(id -u); GID_N=$(id -g)
APP_HOME=/home/roost
PORT=${ROOST_TEST_PORT:-18123}
TMP=$(mktemp -d)
fail=0
bad() { printf '  %s✗%s %s\n' "$RED" "$OFF" "$1"; fail=$((fail+1)); }
# `check <message> <command...>` rather than `command; check $?`: this script
# runs under `set -e`, where a bare failing assertion kills the run before it
# can be reported — so the first failure hid every one after it, which is the
# opposite of what a test script is for. Running the command as the helper's
# own argument keeps it out of `set -e`'s reach.
check() { local msg=$1; shift; if "$@" >/dev/null 2>&1; then ok "$msg"; else bad "$msg"; fi; }
# For assertions that need shell syntax (pipes, globs, negation).
checksh() { local msg=$1; shift; if sh -c "$*" >/dev/null 2>&1; then ok "$msg"; else bad "$msg"; fi; }

# Inline rather than a named function, matching `install.sh`: shellcheck reads
# a function reached only through `trap` as unreachable (SC2317), and `make
# test-scripts` treats any finding as a failure. The bind-mounted tree is
# written by a process running as this user, so none of this needs root.
trap 'docker rm -f "$NAME" >/dev/null 2>&1 || true;
      docker network rm "$NET" >/dev/null 2>&1 || true;
      docker volume rm "${NAME}-home" "${NAME}-state" >/dev/null 2>&1 || true;
      rm -rf "$TMP"' EXIT

phase "build"
docker build -q \
  --build-arg APP_UID="$UID_N" --build-arg APP_GID="$GID_N" \
  -t "$IMAGE" . >/dev/null
ok "image built for uid $UID_N"

phase "fixture"
# A real git repository with a real worktree, created on the *host*. The
# worktree is the point: git records absolute paths in both directions, so a
# checkout mounted anywhere but its own path has no worktrees at all.
PROJ=$TMP/projects/demo
mkdir -p "$PROJ"
git -C "$PROJ" init -q
git -C "$PROJ" -c user.email=t@t -c user.name=t commit -q --allow-empty -m init
git -C "$PROJ" worktree add -q "$TMP/projects/demo/.claude/worktrees/wt-1" -b wt-1 2>/dev/null
ok "a repository with a worktree, at $TMP/projects"

phase "run"
docker network create "$NET" >/dev/null
docker run -d --name "$NAME" \
  --network "$NET" \
  --init \
  --cap-drop ALL \
  -p "127.0.0.1:$PORT:8123" \
  -e ROOST_BIND_ALL=1 \
  -e "ROOST_ROOTS=$TMP/projects" \
  -e ROOST_STATE_DIR=/var/lib/roost \
  -v "$TMP/projects:$TMP/projects" \
  -v "${NAME}-home:/home/roost" \
  -v "${NAME}-state:/var/lib/roost" \
  "$IMAGE" 8123 >/dev/null
for _ in $(seq 1 60); do
  curl -fsS "http://127.0.0.1:$PORT/static/favicon-32.png" -o /dev/null 2>/dev/null && break
  sleep 0.5
done
check "the published port answers on the host's loopback" \
  curl -fsS "http://127.0.0.1:$PORT/static/favicon-32.png" -o /dev/null

phase "who it runs as"
got=$(docker exec "$NAME" id -u)
check "runs as the host's uid ($got)" test "$got" = "$UID_N"
check "and not as root" test "$got" != 0

phase "capabilities"
# `cap_drop: ALL` reaching the process, read from the process rather than from
# the compose file. A PTY needs none of them, and that belief is exactly the
# kind this project's own table says to test rather than believe.
eff=$(docker exec "$NAME" sh -c "grep CapEff /proc/1/status | awk '{print \$2}'")
check "no effective capabilities (CapEff=$eff)" test "$eff" = "0000000000000000"

phase "a terminal, through a real PTY and a real dtach"
# The websocket is the only way to start one, and it checks Origin — so this
# speaks it, with the Origin a browser on the published port would send.
check "a websocket opened a session and typed into it" \
  python3 scripts/test/wsterm.py "127.0.0.1:$PORT" demo "echo ROOST-PTY-OK > $PROJ/from-terminal.txt"
check "the command's effect is visible on the host" \
  grep -q ROOST-PTY-OK "$PROJ/from-terminal.txt"

owner=$(stat -c '%u:%g' "$PROJ/from-terminal.txt" 2>/dev/null || echo "missing")
check "and the file it wrote is owned by the host user ($owner)" test "$owner" = "$UID_N:$GID_N"

# The `ROOST_CMD=cat` row of CLAUDE.md's table, reproduced in the new
# environment: a session that looks fine until you ask what is holding it.
check "a dtach socket exists under the state volume" \
  docker exec "$NAME" sh -c 'ls /var/lib/roost/sock/*/ | grep -q .'
check "and a live dtach is holding it" \
  docker exec "$NAME" sh -c 'ps -Aww -o args= | grep -q "[d]tach.*/var/lib/roost/sock/"' 

phase "the mount path is the host's own"
# A worktree is discovered by asking git, and git stores absolute paths in the
# `.git` file and in the repository's `worktrees/<name>/gitdir`. Mounted
# anywhere but its own path, this reports nothing — silently.
n=$(docker exec "$NAME" sh -c "cd '$PROJ' && git worktree list | wc -l" 2>/dev/null || echo 0)
check "git inside the container sees the host's worktree ($n entries)" test "${n:-0}" -ge 2

phase "the bind gate"
docker rm -f "$NAME" >/dev/null 2>&1
# Without the gate the process must keep its loopback bind, so the published
# port reaches nothing. This is what stops a host build being reachable by
# accident.
docker run -d --name "$NAME" -p "127.0.0.1:$PORT:8123" \
  -e "ROOST_ROOTS=$TMP/projects" -v "$TMP/projects:$TMP/projects" \
  "$IMAGE" 8123 >/dev/null
sleep 3
checksh "with ROOST_BIND_ALL unset the published port refuses the connection" \
  "! curl -fsS --max-time 2 http://127.0.0.1:$PORT/static/favicon-32.png -o /dev/null"
docker rm -f "$NAME" >/dev/null 2>&1

out=$(docker run --rm -e ROOST_BIND_ALL=yes "$IMAGE" 8123 2>&1 || true)
check "an unrecognised value exits naming the value, rather than guessing" \
  grep -q '"yes"' <<<"$out"

phase "the image itself"
left=$(docker run --rm --entrypoint sh "$IMAGE" -c 'find / -xdev -perm /6000 -type f 2>/dev/null | grep -v "^/usr/bin/sudo$" | tr "\n" " "')
check "no setuid binaries left apart from sudo (found: ${left:-none})" test -z "$left"
# One at a time: `command -v` takes a single operand in POSIX sh, so asking it
# for four names reports only the first — which is how "git is present" failed
# against an image that has had git all along.
for t in dtach git bash sudo; do
  check "$t is on PATH" docker run --rm --entrypoint sh "$IMAGE" -c "command -v $t"
done
checksh "and claude deliberately is not — it is installed into the persistent HOME" \
  "! docker run --rm --entrypoint sh '$IMAGE' -c 'command -v claude'"

# Opt-in: it downloads the Claude Code installer, so it needs network and a few
# hundred megabytes, which is more than a test should take on every run. It is
# here rather than only in the docs because it is the claim the whole base-image
# decision rests on — `claude` is a glibc binary and cannot run on musl — and
# because docs/deploy.md tells people to run exactly this line.
#
# Verified by hand 2026-09-10: installer exit 0, `~/.local/bin/claude` →
# `~/.local/share/claude/versions/2.1.268`, still there and still running in a
# brand-new container on the same volume, with `~/.local/bin` on a login
# shell's PATH via Debian's own ~/.profile.
if [ "${ROOST_TEST_CLAUDE:-0}" = 1 ]; then
  phase "claude installs into the persistent HOME and survives the container"
  check "the installer succeeds in this image" \
    docker run --rm -u "$UID_N:$GID_N" -v "${NAME}-home:$APP_HOME" -e "HOME=$APP_HOME" \
      --entrypoint bash "$IMAGE" -lc 'curl -fsSL https://claude.ai/install.sh | bash'
  # A *different* container on the same volume: this is the durability claim,
  # and running it in the same one would prove nothing about an upgrade.
  check "and a brand-new container still finds it on a login shell's PATH" \
    docker run --rm -u "$UID_N:$GID_N" -v "${NAME}-home:$APP_HOME" -e "HOME=$APP_HOME" \
      --entrypoint bash "$IMAGE" -lc 'command -v claude && claude --version'
fi

echo
if [ "$fail" = 0 ]; then printf '%sPASS%s\n' "$GREEN" "$OFF"; else printf '%sFAIL (%s)%s\n' "$RED" "$fail" "$OFF"; fi
exit "$fail"
