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
