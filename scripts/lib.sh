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

# Translates this project's version string (semver, e.g. "0.5.2-rc.2") into
# the encoding both Linux package formats require for a prerelease. Neither
# format tolerates a bare '-' the way semver uses it: RPM's version grammar
# forbids '-' outright (cargo-generate-rpm's own error: "invalid character
# (allowed: alphanumeric, '.', '_', '+', '%', '{', '}', '~', '^')" — no
# hyphen), and Debian reserves '-' as the separator before the debian
# revision, so "0.5.2-rc.2" parses as upstream "0.5.2-rc" revision "2" —
# measured with `dpkg --compare-versions '0.5.2-rc.2' gt '0.5.2'`, which is
# true, i.e. the prerelease sorts AFTER the release it precedes. Both formats
# document '~' for exactly this case, and both sort it before the string it's
# attached to: measured with `dpkg --compare-versions '0.5.2~rc.2' lt
# '0.5.2'` (true) and `rpmdev-vercmp 0.5.2~rc.2 0.5.2` (reports "<"). Semver
# puts at most one '-' in a version, introducing the prerelease, so a
# blanket substitution is exact — there is nothing else in this project's
# version strings a '-' could mean.
pkg_version() { printf '%s' "${1//-/\~}"; }
