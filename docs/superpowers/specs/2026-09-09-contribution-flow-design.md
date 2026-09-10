# Opening roost to outside contributions

*2026-09-09. Status: designed, not implemented. Decided with Peter on 2026-09-09.*

## What and why

roost is public and released. 0.5.2 ships on five channels and the release is one
command. What it does not have is any of the machinery a stranger needs to
contribute: no `develop` branch, no branch protection at all (`master` is
currently writable by anyone with access), no `CONTRIBUTING.md`, no PR or issue
templates, and a test suite that fails three times on a first run for reasons
that have nothing to do with the contributor's change.

This sets up a two-branch flow and the files that make a first contribution
succeed.

## The flow

```
contributor fork ──PR──▶ develop ──release PR──▶ master ──tag──▶ release
                            ▲                       │
                            └───── merge back ──────┘
```

1. `master` is protected and used only for releases.
2. `develop` is the main development branch and the repository default.
3. Contributions branch off `develop`.
4. All contributions arrive as pull requests.
5. An accepted PR is squash-merged into `develop`.
6. At release time **everything on `develop` ships** — there is no selection.
7. `develop` is merged into `master` by a release PR, and the tag is cut from
   `master` by `make release`.
8. `master` is merged back into `develop` after each release.

**Step 6 is the load-bearing decision.** The original proposal was to *select*
contributions from `develop` at release time. Selection means cherry-picking,
which duplicates a commit under a new SHA, so every later `master → develop`
merge sees the change twice and conflicts — permanently, and worse each release.
Work that is not ready to ship stays on its own branch until it is. If selection
ever becomes genuinely necessary, the answer is a release branch cut from
`develop` with the unwanted work removed there, never a cherry-pick.

**Step 8 exists because of where the version bump happens.** `scripts/release.sh`
bumps `Cargo.toml`, commits, and pushes that commit to `master` directly. That
commit has to reach `develop` or the next release re-conflicts on the version
line. The alternative — restructuring `release.sh` so the bump happens on
`develop` and `master` only ever receives merges and tags — is cleaner history
and was rejected: that script and `preflight.sh` were hardened over a full day
and have now cut a working release. A merge-back is one command. Revisit only if
it is forgotten twice.

## Branch protection

**`master`, as implemented: `enforce_admins: true`, force-push and branch
deletion blocked. No push restriction, no PR requirement, no required status
checks.**

This section originally specified "restrict who can push (maintainer only)"
instead of `enforce_admins`. The plan overrode that during implementation:
with fork-only contributors, nobody but the maintainer has push access to
begin with, so a push restriction defends against nobody. The actual risk is
the maintainer's own mistake — a stray force-push or branch deletion — and
GitHub's own docs are explicit that branch protection rules do not apply to
users with admin permissions unless `enforce_admins` is on, so a push
restriction alone would not even have covered that risk for the maintainer's
own admin account. `enforce_admins` plus blocking force-push and deletion
covers it directly.

A PR requirement and required status checks are still absent, deliberately:
`release.sh:82` pushes the version bump straight to `master` as a fast-forward,
and that commit has never been through CI at the moment it is pushed. Turning
either on breaks `make release` after preflight has passed and the tag exists
locally.

**`develop`: require a pull request, and require both CI checks
(`cargo test (Linux)`, `shellcheck + package tests`) to pass.**

Not "require branches to be up to date" — on a low-traffic repository that forces
a rebase for every unrelated merge and buys little.

## Merge strategy, which differs by direction

- **PR → `develop`: squash.** One commit per change, and the message can be
  rewritten at merge time, which matters given this repository's commit-message
  standard.
- **`develop` → `master`: merge commit, never squash.** Squashing produces a
  commit on `master` that `develop` does not have, and every merge-back then
  conflicts. That is the same duplication trap as cherry-picking, arrived at from
  the other direction.
- **`master` → `develop` (the post-release merge-back): merge commit, never
  squash**, for the same reason in the other direction — squashing it puts
  master's content on `develop` under a new SHA, and the next release merge
  sees it twice.

## Fix the confinement tests first

Three tests in `src/hub.rs` build the project root with `tempfile::tempdir()` and escape with
`../../etc/passwd`. They only reach the confinement check when the temp dir
happens to sit exactly two levels above a real `/etc`. Under a deeper `TMPDIR`
the path resolves to something that does not exist, `safe_resolve` returns
`ENOENT`, and the assertion fails with `must refuse by confinement and say so,
got [… "couldn't open ../../etc/passwd"]`.

| Test | defined at | asserts at |
|---|---|---|
| `share_selection_outside_the_project_is_refused_before_reaching_ide` | 4704 | 4721 |
| `share_selection_refusal_reaches_only_the_client_that_asked` | 4739 | 4757 |
| `open_at_line_refuses_a_path_outside_the_project` | 5540 | 5555 |

Measured: with `TMPDIR=/tmp` all three pass; under this host's default
`TMPDIR=/home/claude/scratch/tmp` all three fail.

This is the first row of CLAUDE.md's own testing table — a path-confinement test
that fails with `ENOENT` before ever reaching the confinement check — still live.
CI passes only because GitHub runners use `/tmp`.

**These are fixed, not documented.** A contributor's first `cargo test` must not
produce three failures that have nothing to do with their change; telling them to
set `TMPDIR` makes the project's own suite a gotcha and leaves a test that
asserts a security property it does not actually exercise. The fix makes the
escape target independent of where the temp dir sits.

## Files to add

| File | Contents that matter |
|---|---|
| `CONTRIBUTING.md` | The branch flow; `cargo test --locked -- --test-threads=1` and *why* single-threaded (the suite shares a process-wide session registry and a bare `cargo test` has hung); `dtach` and `git` are required to run the tests; contributions are dual MIT/Apache-2.0; releases are cut by the maintainer only |
| `CODE_OF_CONDUCT.md` | Contributor Covenant 2.1 |
| `.github/PULL_REQUEST_TEMPLATE.md` | What changed and why; how it was tested; whether it touches any hard constraint in CLAUDE.md |
| `.github/ISSUE_TEMPLATE/bug.yml` | **Install method** (there are now five channels), OS and version, `roost --version`, `dtach --version`, whether it reproduces from a fresh state |
| `.github/ISSUE_TEMPLATE/feature.yml` | Problem before solution |

No CLA and no DCO: friction without benefit at this scale.

The bug template asks for the install method because roost now ships through five
channels — tarball, shell installer, Homebrew, `cargo install`, and `.deb`/`.rpm`
— and they differ in ways that produce different bugs: only Homebrew and the
Linux packages install `dtach`, and only the packages ship the systemd unit.

## CI

`ci.yml` triggers on `push: branches: [master]` and `pull_request`. Add
`develop`, or direct pushes there run no tests.

The `scripts` job takes 4m32s, most of it compiling `cargo-deb` and
`cargo-generate-rpm` from source on every run. Cache those two binaries. Do **not**
add a `paths:` filter: a required status check that does not run reads as pending
and blocks the merge, which needs more machinery than the saved minutes are
worth at this traffic.

## Non-goals

- **Restructuring `release.sh`.** See step 8.
- **A `paths:` filter on the packaging job.** See above.
- **Automating the merge-back.** Document it in the release checklist first; automate
  only if it is actually forgotten.
- **Changing what `master` means for users.** README and install instructions
  continue to reference tags and releases, not a branch.

## Order

1. Fix the three confinement tests. They are the prerequisite: everything else
   invites people to run a suite that fails.
2. Add the community files and the CI trigger change, on `develop`.
3. Create `develop` from `master`, make it the default, apply protection to both.

Protection comes last so the branch exists and CI has reported on it before the
required checks are named.

## Testing

The confinement tests get the treatment CLAUDE.md prescribes: revert the fix,
watch each of the three fail, restore. They must also pass under **both** a
shallow and a deep `TMPDIR` — the second is the case that is broken today, so a
fix verified only under `/tmp` proves nothing.

`shellcheck` and the package suite are unaffected. The CI trigger change is
verified by pushing to `develop` and observing a run, not by reading the YAML.

## Open

`check-tap-token.yml` is dispatchable only because it exists on the default
branch. Changing the default from `master` to `develop` keeps that true — both
branches share the file — but the dependency is worth knowing, since
`preflight.sh` dispatches it during every release and a release runs from
`master`.
