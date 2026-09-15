# Contributing to roost

Thanks for considering it. roost is a single Rust binary with no async runtime,
hand-rolled HTTP, and server-rendered HTML — small enough to read in an
afternoon, and opinionated enough that the reasons matter.

## Branches

- `develop` is the default branch and where all development happens.
- `master` holds releases only. Every tag is cut from it.

Branch off `develop`, open a pull request against `develop`. Accepted PRs are
squash-merged. Releases are cut by the maintainer.

`develop` requires a pull request and green CI, and that applies to the
maintainer too — including the merge of `master` back into `develop` after each
release, which therefore goes through a PR like anything else. That merge-back
PR is merged with a merge commit, never squashed: squashing it would put
master's content on `develop` under a new SHA, and every later `develop` →
`master` release merge would see it twice and conflict.

## Before you open a PR

Install `dtach` and `git` — the test suite needs both, because two integration
tests deliberately run the real `dtach` rather than a substitute:

```sh
apt install dtach    # or: brew install dtach
```

Then:

```sh
cargo test --locked -- --test-threads=1
```

**The `--test-threads=1` is not optional.** The suite shares a process-wide
session registry and a bare `cargo test` has hung on it.

If you touch anything under `scripts/` or `packaging/`, also run:

```sh
make test-scripts
```

If you touch `static/app.js`, also run the browser tests — no Rust test can
reach that file, so `cargo test` alone verifies nothing about a change there.
They need `deno` on `PATH`; see <https://docs.deno.com/runtime/getting_started/installation/>
if you don't have it:

```sh
deno run -A tests/browser/reconnect.mjs
deno run -A tests/browser/upload.mjs
```

See [tests/browser/README.md](tests/browser/README.md) for the rest of the
suite. They drive a real Chromium against a real roost with real `dtach`, and
they skip silently rather than failing when no browser is found — a skip is
not a pass.

## Poking at it by hand: a second instance

Never point a browser — and especially not browser automation — at the roost
you are working in. roost sizes every session's PTY to the minimum over all
attached clients, so one headless 1280x720 tab clamps every terminal in that
project. It has happened, and the symptom ("Claude is using only half the
terminal") gives no hint that a stray tab is the cause.

Start a throwaway one instead:

```sh
scripts/testroost.sh            # :8445, or pass a port
scripts/testroost.sh 9001 --no-build
```

It gets its own state directory, its own roots with a fixture project, and its
own global config, so nothing it does can reach the instance you work in.
`ROOST_STATIC` points at your checkout, so an edit to `static/app.js` or
`static/style.css` needs only a browser reload — no rebuild. Ctrl-C stops it
and the sessions it started.

Override `ROOST_TEST_STATE` or `ROOST_TEST_ROOTS` if you want it somewhere
else. Keep its roots away from the roots the live instance uses: the cwd
sampler in `src/cwds.rs` finds shells by walking `/proc` for
`ROOST_PROJECT`/`ROOST_SESSION`, which is instance-blind, so with overlapping
project keys a new session in one instance can start in a cwd sampled from the
other's shell.

## Read CLAUDE.md first

[CLAUDE.md](CLAUDE.md) lists this project's hard constraints — the loopback
bind, the `Origin` checks on every browser-facing websocket, path confinement,
the caps. They are load-bearing: breaking one is a defect rather than a style
choice, and each entry says why. Several look like cleanups and are not.

A PR that changes one of those is welcome, but say so explicitly and explain
why the reason recorded there no longer holds.

## What makes a PR easy to accept

- One change per PR.
- A test that fails without your change. If you are not sure it does, revert
  your fix, watch the test fail, and put that in the PR description — this
  project has been bitten repeatedly by tests that passed for the wrong reason.
- Commit messages that explain *why*. The existing history is the model.

## Licence

Contributions are dual-licensed under [MIT](LICENSE-MIT) and
[Apache-2.0](LICENSE-APACHE), the same terms as roost itself. There is no CLA.
