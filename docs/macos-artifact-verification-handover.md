# macOS artifact verification — handover

For a Claude running on the macOS machine. Everything below that can be checked
without a Mac has been checked; what remains needs one.

## 1. Purpose

roost v0.5.2 ships four release artifacts, two of them macOS. Both macOS
binaries have been **built, uploaded and checksummed, but never executed** —
`docs/roost-packaging-handover.md` says so in its own words ("this survey had no
macOS machine") and nothing has changed that. An artifact that exists and hashes
correctly is not an artifact that runs.

Two distinct shapes need settling, and the second is the one most likely to be
broken:

1. **Homebrew** — `brew install peterknego/tap/roost`.
2. **The released tarball** — download, verify, run. This is one of the five
   shapes issue #65 names, and it is the one where macOS can refuse a binary
   that is otherwise perfect, because **cargo-dist does not notarize**.

## 2. What is already established (checked 2026-09-12, from Linux)

Do not re-verify these; they are recorded so you know what is *not* in question.

- **The release exists.** `v0.5.2`, published `2026-09-09T09:03:58Z`. Assets:
  four `.tar.xz` with `.sha256` sidecars, `roost_0.5.2_amd64.deb`,
  `roost_0.5.2_arm64.deb`, `roost-0.5.2.x86_64.rpm`,
  `roost-0.5.2.aarch64.rpm`, `roost-installer.sh`, `dist-manifest.json`.
- **The tap is published.** `PeterKnego/homebrew-tap`, public, default branch
  `main`, pushed `2026-09-09T09:05:07Z` — about one minute after the release,
  i.e. that push *is* the publish job succeeding. It holds
  `Formula/roost.rb` (2,714 bytes).
- **The formula is correct.** `version "0.5.2"`, all four targets,
  `license any_of: ["MIT", "Apache-2.0"]`, and `depends_on "dtach"` — so the
  `stage = ["run"]` config in `dist-workspace.toml` did the right thing rather
  than putting a `brew install` on the build runner.
- **All four artifact URLs return 200**, and **all four sha256 values in the
  formula match the release's own `.sha256` sidecars** — the mismatch that
  would break `brew install` silently is ruled out.
- **The token blocker is cleared.** `HOMEBREW_TAP_TOKEN` is set
  (`total_count=1`), and `check-tap-token.yml` has run four times, all
  success, most recently `2026-09-09T12:17:19Z`.

**`docs/roost-packaging-handover.md` § "Step 2" is stale.** Its heading still
reads "done 2026-09-08, blocked on one secret" and its "Outstanding — needs a
browser" paragraph describes a state that ended on 2026-09-09. Correcting it is
part of this task (§6).

## 3. What only a Mac can settle

- Does either macOS binary **execute at all**? Cross-compiled from Linux
  runners, never run.
- Does **Gatekeeper** refuse the tarball binary? `brew` strips the quarantine
  attribute itself, so Homebrew working tells you nothing about the tarball.
- Does a **terminal actually work** — the PTY plus `dtach` on macOS? That needs
  a browser click; no CLI check reaches it.

## 4. Before you start

- **roost binds `127.0.0.1` only, and its websocket spawns a shell.** The
  loopback bind is the security boundary. Do not set `ROOST_BIND_ALL` — that
  exists for the container shape, where a network namespace substitutes for
  the boundary. On this machine it would only remove it.
- **Do not touch the user's real state.** Use a scratch `ROOST_STATE_DIR` and a
  port that is free. Never point a check at an instance the user is working in:
  roost sizes every session's PTY to the minimum over all attached clients, so
  an extra client clamps every terminal in that project.
- Check the port first: `lsof -nP -iTCP:8899 -sTCP:LISTEN`.
- `roost --version` and `-V` are supported and print `roost <version>`.
- An empty `ROOST_ROOTS` is **not** fatal any more — it logs a notice and the
  front page offers "Add path". So a missing roots list is not a failure.

## 5. The commands

### A. Homebrew shape

```sh
brew install peterknego/tap/roost

roost --version                                   # expect exactly: roost 0.5.2
which roost                                       # expect a Homebrew prefix
file "$(readlink -f "$(which roost)" 2>/dev/null || which roost)"
which dtach                                       # depends_on must have pulled it
```

`roost --version` is the whole point: the first execution of that binary on
macOS. If it fails, stop and report — nothing below matters.

Then prove it serves:

```sh
export ROOST_STATE_DIR="$(mktemp -d)"
export ROOST_ROOTS="$HOME/projects"               # or any directory holding repos
roost 8899 &
sleep 2
curl -s -o /dev/null -w 'HTTP %{http_code}\n' http://127.0.0.1:8899/   # expect HTTP 200
```

Then **open `http://127.0.0.1:8899/` in a browser and click a terminal**. That
is the only check that exercises the PTY and `dtach` together, and no CLI
substitute reaches it. Confirm a shell prompt appears and `ls` works, then:

```sh
kill %1
```

### B. Released-tarball shape, and Gatekeeper

```sh
cd "$(mktemp -d)"
ARCH=$(uname -m); case "$ARCH" in arm64) T=aarch64-apple-darwin;; *) T=x86_64-apple-darwin;; esac
URL=https://github.com/PeterKnego/roost/releases/download/v0.5.2/roost-$T.tar.xz
curl -fsSLO "$URL" && curl -fsSLO "$URL.sha256"
shasum -a 256 -c "roost-$T.tar.xz.sha256"         # expect: OK
tar xf "roost-$T.tar.xz"
find . -name roost -type f -perm -u+x
```

Run the extracted binary, then record its signing state regardless of whether
it ran:

```sh
./roost*/roost --version || true
xattr -l ./roost*/roost                           # com.apple.quarantine present?
codesign -dv ./roost*/roost 2>&1 | head -3        # expect: not signed at all
spctl -a -vv ./roost*/roost 2>&1                  # expect: rejected
```

**`curl` does not set the quarantine attribute; a browser does.** So this can
pass while a real user's download fails. To reproduce what a user hits,
download the same tarball in Safari or Chrome, extract it in `~/Downloads`, and
run it from there. That second run is the one worth reporting.

### C. Cleanup

```sh
kill %1 2>/dev/null
brew uninstall roost
brew untap peterknego/tap
rm -rf "$ROOST_STATE_DIR"
```

## 6. What to do with the result

**Report actual command output, not a summary of it.** This repository's
CLAUDE.md is explicit: never state that something works without the command and
its output in the same place, and "I could not check X" is a third outcome that
must never be written as "X is false". The three lines that matter are
`roost --version`, the `shasum -a 256 -c` line, and the `spctl` verdict.

Then update `docs/roost-packaging-handover.md`:

- § Step 2's heading — "blocked on one secret" is false as of 2026-09-09.
  Replace with the measured state, citing the tap push timestamp and the
  `check-tap-token.yml` runs.
- § 4 "Target set" — "The two macOS targets are **unverified**" becomes either
  verified-with-evidence or verified-as-broken, with the date.

And on issue #65 ("Tell me when there is a newer roost"), which lists the
released binary as one of its five supported shapes:

- If Gatekeeper refuses the tarball, that is a **new finding for #65**: the
  "download, verify, replace" shape needs notarization or a documented
  `xattr -d com.apple.quarantine` step before it can be offered as a supported
  way to run roost.
- If both shapes work, say so there — #65's step 3 ("offer the command, do not
  run it") becomes implementable for all three live channels.

## 7. Interpreting the outcomes

| Result | Meaning |
|---|---|
| A passes, B passes | Both shapes work. Close the "unverified macOS targets" caveat with the date and the output. |
| A passes, B refused by Gatekeeper | Most likely outcome. Homebrew is fine; the tarball and `roost-installer.sh` need notarization or a documented workaround. New item for #65. |
| A passes, browser terminal fails | The binary runs but the PTY or `dtach` path does not. The most serious of the three — report the browser console and roost's stderr. |
| `roost --version` fails | The cross-compiled macOS binaries are broken. This invalidates the Homebrew channel outright and is the single most valuable thing to learn. |

## 8. How the § 2 facts were checked

For anyone re-treading this: `gh api repos/PeterKnego/homebrew-tap`,
`.../contents/Formula`, and `.../contents/Formula/roost.rb` (base64-decoded) for
the tap and formula; `gh api repos/PeterKnego/roost/releases/tags/v0.5.2` for
the assets; `curl -sIL -o /dev/null -w '%{http_code}'` per formula URL; the
release's own `.tar.xz.sha256` sidecars compared against the formula's `sha256`
lines; `gh api repos/PeterKnego/roost/actions/secrets` for the token's presence
and `.../actions/workflows/check-tap-token.yml/runs` for its four successful
runs. None of that touches a Mac, which is why § 3 exists.
