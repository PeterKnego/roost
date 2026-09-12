#!/usr/bin/env bash
# A second roost, so a browser never has to point at the one you are working in.
#
# Two reasons this exists rather than "just open the live instance":
#
#   - roost sizes every session's PTY to the minimum over all attached clients
#     (session.rs, min_geometry). A headless 1280x720 tab attached to a live
#     project clamped every terminal in it to 107x33, reported as "Claude is
#     using only half the terminal" — from a browser tab nobody could find.
#   - The live instance's sessions are real work. A test instance that shares
#     its state directory is one reconcile away from reaping them.
#
# It is deliberately on-demand and foreground. A leftover test instance was
# found on this box still serving on :8901 twelve days after its state
# directory had been deleted; one that dies with the terminal that started it
# cannot do that.
#
# Usage: scripts/testroost.sh [PORT] [--no-build]
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib.sh

PORT=8445
BUILD=1
for a in "$@"; do
  case "$a" in
    --no-build) BUILD=0 ;;
    *[!0-9]*|'') die "unknown argument: $a (usage: scripts/testroost.sh [PORT] [--no-build])" ;;
    *) PORT=$a ;;
  esac
done

# Its own roots, not the live instance's, and not only to keep test sessions
# out of real checkouts: cwds.rs finds shells by walking /proc for
# ROOST_PROJECT/ROOST_SESSION and is blind to which instance they belong to, so
# two instances sharing a project key can hand each other a sampled cwd. With
# separate roots the keys never collide.
STATE=${ROOST_TEST_STATE:-$HOME/.local/state/roost-test}
ROOTS=${ROOST_TEST_ROOTS:-$HOME/roost-test/roots}

phase "test roost"
need cargo "install rust"; need dtach "apt install dtach"; need git "install git"

# The state directory is the whole isolation. Two instances sharing one is
# CLAUDE.md's "a key not resolving under *these* roots" row: the second
# instance's roots differ, so the first instance's projects read as gone and
# its live sessions are reaped. Rather than hardcode the live path — which
# would go stale the moment the unit changes — ask every running roost what
# its own state directory is, and refuse to collide with any of them.
for p in /proc/[0-9]*; do
  [ "${p#/proc/}" = "$$" ] && continue
  # `2>/dev/null` BEFORE the input redirect, and no `[ -r ]` pre-check. Two
  # separate traps: bash applies redirections left to right, so a `2>/dev/null`
  # written after `< file` is installed too late to swallow that file's own
  # "Permission denied"; and `-r` answers from the file mode via access(2)
  # while the read itself goes through the kernel's ptrace check, so environ
  # can test readable and still refuse — which is this project's own rule, that
  # a check succeeding is not the operation succeeding. Either way an
  # unreadable environ leaves `other` empty and is skipped: could not look is
  # not evidence of anything.
  # `|| continue` is not decoration: under `pipefail` this pipeline reports
  # tr's failure even though head succeeded, and `set -e` then takes the whole
  # sweep down on the first process whose environ we may not read — pid 1, every
  # time. One unreadable process must skip that process, not abandon the check.
  other=$(tr '\0' '\n' 2>/dev/null < "$p/environ" |
          sed -n 's/^ROOST_STATE_DIR=//p;s/^RESH_STATE_DIR=//p' | head -1) || continue
  [ -n "$other" ] || continue
  [ "$other" = "$STATE" ] &&
    die "pid ${p#/proc/} is already running with ROOST_STATE_DIR=$STATE — sharing it would let one instance reap the other's shells"
done

command -v ss >/dev/null 2>&1 &&
  ss -ltn 2>/dev/null | grep -q "127.0.0.1:$PORT " &&
  die "127.0.0.1:$PORT is already listening — pass a different port"

if [ "$BUILD" = 1 ]; then
  cargo build >/dev/null 2>&1 || die "cargo build failed — run it yourself to see why"
  ok "built"
else
  warn "skipping the build: serving whatever target/debug/roost currently is"
fi
BIN=$(cargo metadata --format-version 1 --no-deps |
      python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/roost
[ -x "$BIN" ] || die "$BIN is not there — run cargo build"

mkdir -p "$STATE" "$ROOTS"
if [ ! -d "$ROOTS/scratch/.git" ]; then
  mkdir -p "$ROOTS/scratch"
  git -C "$ROOTS/scratch" init -q
  printf '# scratch\n\nA fixture project for the test instance.\n' > "$ROOTS/scratch/README.md"
  printf 'fn main() {\n    println!("hello");\n}\n' > "$ROOTS/scratch/main.rs"
  ok "created a fixture project at $ROOTS/scratch"
fi

# Its own global config, so a settings change made while testing cannot reach
# ~/.config/roost/config.toml and follow you back to the instance you work in.
[ -f "$STATE/config.toml" ] || : > "$STATE/config.toml"

# Stops the sessions this instance started, and nothing else.
#
# Two hazards, and the second one bit while this script was being written.
# `pkill -f` is out because it matches the invoking shell's own command line,
# which carries the pattern as an argument, so it kills the caller — observed
# three times while the browser harness was built. But reading /proc and
# matching the socket path in a cmdline has exactly the same blast radius by a
# different route: the measuring shell that first exercised this teardown had
# the path in its own command line and was duly SIGTERMed, and the log claimed
# to have stopped "1 session process" for an instance with no sessions.
#
# So a path match is a candidate, never a verdict. A session is confirmed by
# what the process *is*: /proc/<pid>/exe resolving to dtach. A shell, an editor
# or a grep that merely mentions the path is not dtach and is left alone. Its
# shell children are taken with it deliberately — roost's own Close Project
# kills only the master, which is how a HUP-ignoring Claude survives it, and
# for a throwaway instance that is litter rather than mercy.
#
# Anything unreadable is skipped rather than guessed at: could not look is not
# evidence that it is ours.
sweep() {
  local sock="$STATE/sock" masters=() kids=() p pid exe cmd ppid left=0
  for p in /proc/[0-9]*; do
    pid=${p#/proc/}
    [ "$pid" = "$$" ] && continue
    cmd=$(tr '\0' ' ' 2>/dev/null < "$p/cmdline") || continue
    case "$cmd" in *"$sock"*) ;; *) continue ;; esac
    exe=$(readlink "$p/exe" 2>/dev/null) || continue
    [ "${exe##*/}" = "dtach" ] || continue
    masters+=("$pid")
  done
  [ ${#masters[@]} -eq 0 ] && return 0
  for p in /proc/[0-9]*; do
    pid=${p#/proc/}
    ppid=$(awk '{print $4}' "$p/stat" 2>/dev/null) || continue
    for m in "${masters[@]}"; do
      [ "$ppid" = "$m" ] && kids+=("$pid")
    done
  done
  say "stopping ${#masters[@]} session(s) and ${#kids[@]} shell(s) under $sock"
  kill -TERM "${masters[@]}" ${kids[@]+"${kids[@]}"} 2>/dev/null || true
  # A moment for TERM to be taken, or the escalation below is not a backstop —
  # it is the only thing that ever runs, and "needed SIGKILL" stops meaning
  # anything. The harness found the opposite of TERM-proof: a master abandoned
  # for days took a later TERM first try.
  sleep 1
  for p in "${masters[@]}" ${kids[@]+"${kids[@]}"}; do
    kill -0 "$p" 2>/dev/null && { kill -KILL "$p" 2>/dev/null || true; left=$((left + 1)); }
  done
  [ "$left" -gt 0 ] && warn "$left needed SIGKILL"
  return 0
}

ROOST_PID=
cleanup() {
  trap - INT TERM EXIT
  if [ -n "$ROOST_PID" ]; then kill -TERM "$ROOST_PID" 2>/dev/null || true; fi
  sweep
  ok "test roost stopped"
}
trap cleanup INT TERM EXIT

say "port    $PORT"
say "state   $STATE"
say "roots   $ROOTS"
say "static  $PWD/static  (edit app.js or style.css and just reload — no rebuild)"
say "binary  $BIN"
printf '\n  %sopen http://127.0.0.1:%s/%s   — ctrl-c stops it and its sessions\n\n' "$BOLD" "$PORT" "$OFF"

# ROOST_CONFIG and ROOST_STATE_DIR are what keep this instance off the live
# one's data; ROOST_STATIC is what makes it worth using for UI work at all
# (routes.rs: the overlay is consulted for any class, per file, falling through
# to the embedded copy).
ROOST_STATE_DIR="$STATE" \
ROOST_ROOTS="$ROOTS" \
ROOST_CONFIG="$STATE/config.toml" \
ROOST_STATIC="$PWD/static" \
  "$BIN" "$PORT" &
ROOST_PID=$!
wait "$ROOST_PID"
