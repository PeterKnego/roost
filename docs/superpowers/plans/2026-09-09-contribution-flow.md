# Contribution Flow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make roost safe to open for outside contributions — a `develop`/`master` flow with protection that does not break `make release`, the files a stranger needs, and a test suite that does not fail three times on their first run.

**Architecture:** Five tasks in a deliberate order. The confinement-test fix comes first because everything after it invites people to run a suite that currently fails on any machine with a deep `TMPDIR`. Community files and the CI trigger change land next, on `master`, so they exist before `develop` is branched from it. Branch creation, the default-branch switch and protection come last, when CI has reported on `develop` and the required check names are known to be real.

**Tech Stack:** Rust 2021, `tempfile`, GitHub Actions, `gh` CLI 2.46.0, GitHub branch protection API.

**Spec:** [docs/superpowers/specs/2026-09-09-contribution-flow-design.md](../specs/2026-09-09-contribution-flow-design.md)

## Global Constraints

- **`master` protection is "restrict who can push" only** — no PR requirement, no required status checks. Both break `make release`, which pushes the version bump directly at `scripts/release.sh:82`.
- **`develop` protection is: require a pull request, require both CI checks** (`cargo test (Linux)`, `shellcheck + package tests`). Not "require branches up to date".
- **Merge strategy differs by direction:** PR→`develop` squash; `develop`→`master` merge commit, never squash.
- **Tests run `cargo test --locked -- --test-threads=1`.** Never `cargo test --release`, never a bare `cargo test`.
- **The confinement tests must pass under BOTH a shallow and a deep `TMPDIR`.** A fix verified only under `/tmp` proves nothing — that is the case that already works.
- **Comments give rationale, never mechanics.** Describe only what you have measured.
- **Do not modify `scripts/release.sh` or `scripts/preflight.sh`.** The spec lists restructuring them as a non-goal.

---

### Task 1: Fix the three confinement tests

The prerequisite. These are the first thing a contributor runs and the first thing that fails.

`safe_resolve` (`src/projects.rs:326-337`) canonicalises `project_dir.join(rel)` **before** checking confinement, so a target that does not exist returns `Err("not found: …")` and the `canon.starts_with(&base)` check is never reached. The three tests escape with `../../etc/passwd` from a `tempfile::tempdir()` root, which only lands on a real `/etc` when that temp dir happens to sit exactly two levels above one.

**Files:**
- Modify: `src/hub.rs` — three tests at 4704, 4739, 5540

**Interfaces:**
- Consumes: `tempfile::tempdir`, `Hub::new(name: &str, dir: PathBuf)`, `safe_resolve` behaviour.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Reproduce the failure**

```bash
cd /home/claude/projects/roost
cargo test --locked --lib -- --test-threads=1 hub::tests::open_at_line_refuses_a_path_outside_the_project hub::tests::share_selection 2>&1 | tail -20
```

Expected: 3 failures. Note the message — `must refuse by confinement and say so, got ["{\"t\":\"Error\",\"msg\":\"couldn't open ../../etc/passwd\"}"]`. That is `safe_resolve`'s `not found` arm, not its confinement arm. Paste this output into your report; it is the "before" half of the proof.

- [ ] **Step 2: Confirm the same tests pass under a shallow TMPDIR**

```bash
TMPDIR=/tmp cargo test --locked --lib -- --test-threads=1 hub::tests::open_at_line_refuses_a_path_outside_the_project hub::tests::share_selection 2>&1 | tail -6
```

Expected: 3 passed. This is what proves the tests are environment-dependent rather than simply broken, and it is why fixing them cannot be verified under `/tmp` alone.

- [ ] **Step 3: Apply the house pattern to the first test**

The repository already solves this, in `src/projects.rs:801-806` — a parent temp dir, the project root *inside* it, and a real file beside the root:

```rust
let parent = tempfile::tempdir().unwrap();
let root = parent.path().join("proj");
std::fs::create_dir_all(&root).unwrap();
let outside = parent.path().join("secret.txt");
std::fs::write(&outside, b"real file, really there").unwrap();
```

`../secret.txt` from `root` always resolves to a file that exists and is always outside the project, wherever the temp dir sits.

In `src/hub.rs`, `share_selection_outside_the_project_is_refused_before_reaching_ide` (at 4704), replace the fixture and the escape path:

```rust
    fn share_selection_outside_the_project_is_refused_before_reaching_ide() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // The project root sits INSIDE a parent temp dir, with a real file
        // beside it. `../secret.txt` therefore always names something that
        // exists and is always outside the project — where the old
        // `../../etc/passwd` only reached a real file when the temp dir
        // happened to sit two levels above `/etc`, so `safe_resolve` returned
        // `not found` and the confinement arm this test exists for was never
        // executed. Same shape as `projects.rs`'s
        // `terminal_path_refuses_a_real_file_outside_the_project`.
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(parent.path().join("secret.txt"), b"real file, really there").unwrap();
        std::env::set_var("ROOST_STATE_DIR", parent.path().join("state"));
        let mut h = Hub::new("shareselectionescape", root);
        let (asker, rx) = h.subscribe();

        h.handle(&asker, Intent::ShareSelection {
            rel: "../secret.txt".into(),
            text: "root:x:0:0".into(),
            start_line: 0,
            start_col: 0,
            end_line: 0,
            end_col: 10,
        });

        let got: Vec<String> = rx.try_iter().collect();
        assert!(
            got.iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("outside project")),
            "an escaping path must be refused by safe_resolve before ide::selection_changed \
             ever sees it: {got:?}"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }
```

- [ ] **Step 4: Apply it to the second test**

`share_selection_refusal_reaches_only_the_client_that_asked` (at 4739). Keep both subscribers — CLAUDE.md records why: with one, a leak to every subscriber and a reply to only the asker are indistinguishable.

```rust
    fn share_selection_refusal_reaches_only_the_client_that_asked() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(parent.path().join("secret.txt"), b"real file, really there").unwrap();
        std::env::set_var("ROOST_STATE_DIR", parent.path().join("state"));
        let mut h = Hub::new("shareselectionrefuse", root);
        let (asker, rx_asker) = h.subscribe();
        let (_other, rx_other) = h.subscribe();

        h.handle(&asker, Intent::ShareSelection {
            rel: "../secret.txt".into(),
            text: "secret".into(),
            start_line: 0,
            start_col: 0,
            end_line: 0,
            end_col: 1,
        });

        let got: Vec<String> = rx_asker.try_iter().collect();
        assert!(got.iter().any(|m| m.contains("outside project")), "the asking client got no refusal: {got:?}");
        let others: Vec<String> = rx_other.try_iter().collect();
        assert!(others.is_empty(), "a refusal must leave the other subscriber's inbox empty, got: {others:?}");
        std::env::remove_var("ROOST_STATE_DIR");
    }
```

- [ ] **Step 5: Apply it to the third test, and keep its second assertion meaningful**

`open_at_line_refuses_a_path_outside_the_project` (at 5540) has a second assertion that no message may contain the escaped file's name. That still holds and still means something: `src/hub.rs:1801-1802` deliberately replaces a confinement message with the bare string `"path outside project"` rather than echoing the caller's path back. The assertion is what keeps that behaviour. Change the name it looks for from `passwd` to `secret.txt`.

```rust
    fn open_at_line_refuses_a_path_outside_the_project() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(parent.path().join("secret.txt"), b"real file, really there").unwrap();
        std::env::set_var("ROOST_STATE_DIR", parent.path().join("state"));
        let mut h = Hub::new("proj", root);
        let (c, rx) = h.subscribe();
        drain(&rx);

        h.handle(&c, Intent::OpenAtLine {
            pane: proto::MIDDLE,
            rel: "../secret.txt".into(),
            line: 1,
        });

        let got = drain(&rx);
        assert!(
            got.iter().any(|m| m.contains(r#""t":"Error""#) && m.contains("outside project")),
            "must refuse by confinement and say so, got {got:?}"
        );
        // `hub.rs`'s confinement arm replaces the message with the bare
        // "path outside project" instead of echoing the caller's path. This
        // assertion is what holds that in place.
        assert!(
            !got.iter().any(|m| m.contains("secret.txt")),
            "nothing outside the project may reach the layout, got {got:?}"
        );
        std::env::remove_var("ROOST_STATE_DIR");
    }
```

- [ ] **Step 6: Verify under BOTH a deep and a shallow TMPDIR**

```bash
cargo test --locked --lib -- --test-threads=1 hub::tests::open_at_line_refuses_a_path_outside_the_project hub::tests::share_selection 2>&1 | tail -6
TMPDIR=/tmp cargo test --locked --lib -- --test-threads=1 hub::tests::open_at_line_refuses_a_path_outside_the_project hub::tests::share_selection 2>&1 | tail -6
```

Expected: 3 passed both times. The first run is the one that was failing; if you only run the second you have proved nothing.

- [ ] **Step 7: Revert-and-watch-it-fail, per CLAUDE.md**

Confirm each test still fails when the confinement check is removed — otherwise the fix has produced three tests that pass for a new wrong reason. In `src/projects.rs`, back up with `cp` first, then change `safe_resolve`'s confinement arm so everything is allowed:

```rust
    if canon.starts_with(&base) {
        Ok(canon)
    } else {
        Ok(canon) // TEMPORARY: confinement removed to prove the tests detect it
    }
```

Run the three tests. Expected: all three fail. Restore `src/projects.rs` from the copy — **not** with `git checkout`, which has discarded uncommitted work in this repo before. Re-run and confirm 3 passed. Paste the failing output into your report.

- [ ] **Step 8: Full suite, then commit**

```bash
cargo test --locked -- --test-threads=1 2>&1 | grep -E "^test result"
git add src/hub.rs
git commit -m "tests: confinement tests that reach the confinement check"
```

The full suite must pass **without** `TMPDIR=/tmp` — that is the whole point. If any other test needs it, say so in your report rather than adding the variable back.

---

### Task 2: CONTRIBUTING.md and CODE_OF_CONDUCT.md

**Files:**
- Create: `CONTRIBUTING.md`
- Create: `CODE_OF_CONDUCT.md`

**Interfaces:**
- Consumes: Task 1's fix — CONTRIBUTING tells contributors to run the suite, which must pass.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Write CONTRIBUTING.md**

```markdown
# Contributing to roost

Thanks for considering it. roost is a single Rust binary with no async runtime,
hand-rolled HTTP, and server-rendered HTML — small enough to read in an
afternoon, and opinionated enough that the reasons matter.

## Branches

- `develop` is the default branch and where all development happens.
- `master` holds releases only. Every tag is cut from it.

Branch off `develop`, open a pull request against `develop`. Accepted PRs are
squash-merged. Releases are cut by the maintainer.

## Before you open a PR

Install `dtach` and `git` — the test suite needs both, because one integration
test deliberately runs the real `dtach` rather than a substitute:

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
```

- [ ] **Step 2: Add CODE_OF_CONDUCT.md**

Use Contributor Covenant 2.1 verbatim from https://www.contributor-covenant.org/version/2/1/code_of_conduct/ — do not paraphrase it, the text is the point. Set the enforcement contact to `peter@knego.net`.

- [ ] **Step 3: Verify every command in CONTRIBUTING actually works**

Run each one and paste the result. A CONTRIBUTING file that tells a newcomer to run something that fails is worse than none:

```bash
cargo test --locked -- --test-threads=1 2>&1 | grep -E "^test result" | tail -3
make test-scripts 2>&1 | tail -3
```

- [ ] **Step 4: Commit**

```bash
git add CONTRIBUTING.md CODE_OF_CONDUCT.md
git commit -m "docs: what a contributor needs before their first PR"
```

---

### Task 3: PR and issue templates

**Files:**
- Create: `.github/PULL_REQUEST_TEMPLATE.md`
- Create: `.github/ISSUE_TEMPLATE/bug.yml`
- Create: `.github/ISSUE_TEMPLATE/feature.yml`
- Create: `.github/ISSUE_TEMPLATE/config.yml`

- [ ] **Step 1: PR template**

```markdown
## What and why

<!-- What changes, and what problem it solves. The why matters more. -->

## How it was tested

<!-- Commands you ran and what they printed. If you added a test, did you
     watch it fail without your change? -->

## Hard constraints

<!-- CLAUDE.md lists constraints that are load-bearing rather than stylistic:
     the loopback bind, Origin checks, path confinement, the caps, the
     settings-file rules. Does this PR touch any of them? If so, which, and
     why does the reason recorded there no longer hold? -->

- [ ] `cargo test --locked -- --test-threads=1` passes
- [ ] `make test-scripts` passes, or this PR touches nothing under `scripts/` or `packaging/`
```

- [ ] **Step 2: Bug template**

The install-method field matters: roost ships through five channels and they differ in ways that produce different bugs — only Homebrew and the Linux packages install `dtach`, and only the packages ship the systemd unit.

```yaml
name: Bug report
description: Something roost does wrong
labels: [bug]
body:
  - type: textarea
    id: what
    attributes:
      label: What happened
      description: What you did, what you expected, what you got.
    validations:
      required: true
  - type: dropdown
    id: install
    attributes:
      label: How did you install roost?
      description: The channels differ — only Homebrew and the .deb/.rpm install dtach for you, and only the packages ship the systemd unit.
      options:
        - .deb or .rpm package
        - Homebrew (peterknego/tap/roost)
        - Shell installer (roost-installer.sh)
        - cargo install / cargo binstall
        - Downloaded tarball
        - Built from source
    validations:
      required: true
  - type: input
    id: version
    attributes:
      label: Output of `roost --version`
    validations:
      required: true
  - type: input
    id: dtach
    attributes:
      label: Output of `dtach -V` (or "not installed")
    validations:
      required: true
  - type: input
    id: os
    attributes:
      label: OS and version
      placeholder: Debian 12, Ubuntu 24.04, macOS 15…
    validations:
      required: true
  - type: textarea
    id: logs
    attributes:
      label: Anything roost printed
      render: text
```

- [ ] **Step 3: Feature template**

```yaml
name: Feature request
description: Something roost should do
labels: [enhancement]
body:
  - type: textarea
    id: problem
    attributes:
      label: The problem
      description: What are you trying to do that roost makes hard? Describe the problem before the solution — the best answer is often not the one either of us thought of first.
    validations:
      required: true
  - type: textarea
    id: idea
    attributes:
      label: What you had in mind
    validations:
      required: false
```

- [ ] **Step 4: Point security reports away from the issue tracker**

```yaml
blank_issues_enabled: false
contact_links:
  - name: Security vulnerability
    url: https://github.com/PeterKnego/roost/security/policy
    about: Please report security issues privately, not as a public issue.
```

- [ ] **Step 5: Verify the YAML parses as GitHub expects**

```bash
for f in .github/ISSUE_TEMPLATE/*.yml; do
  python3 -c "import yaml,sys; yaml.safe_load(open('$f')); print('  $f parses')"
done
```

GitHub rejects a malformed issue form silently — the template simply does not appear — so this check is the only local signal.

- [ ] **Step 6: Commit**

```bash
git add .github/PULL_REQUEST_TEMPLATE.md .github/ISSUE_TEMPLATE
git commit -m "docs: templates that ask the questions we actually need answered"
```

---

### Task 4: CI on develop, and cache the packaging tools

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add `develop` to the push trigger**

`ci.yml` currently triggers on `push: branches: [master]` and `pull_request`. Without `develop`, a direct push there runs nothing.

```yaml
on:
  push:
    branches: [master, develop]
  pull_request:
```

- [ ] **Step 2: Cache the two cargo tools**

The `scripts` job takes 4m32s, most of it compiling `cargo-deb` and `cargo-generate-rpm` from source on every run. Replace the bare `cargo install` step with a cached one:

```yaml
      - name: Cache cargo-deb and cargo-generate-rpm
        id: cache-pkg-tools
        uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/bin/cargo-deb
            ~/.cargo/bin/cargo-generate-rpm
          key: pkg-tools-${{ runner.os }}-cargo-deb-3.8.0-generate-rpm-0.21.0

      - name: Install cargo-deb and cargo-generate-rpm
        if: steps.cache-pkg-tools.outputs.cache-hit != 'true'
        run: cargo install cargo-deb cargo-generate-rpm --locked
```

The key names both versions, so a version bump misses the cache and rebuilds rather than silently serving a stale binary.

- [ ] **Step 3: Verify the workflow still parses**

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml')); print('parses')"
```

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: run on develop, and stop rebuilding the packaging tools every run"
```

- [ ] **Step 5: Push and confirm CI is still green**

```bash
git push origin master
gh run list --branch master --limit 1 --json databaseId --jq '.[0].databaseId'
```

Watch that run to completion and confirm both jobs pass. The cache is cold on this first run, so it will not be faster yet — that is expected; the second run is the one to compare. Report both durations if you can get them.

---

### Task 5: Create develop, switch the default, apply protection

Last on purpose: the branch must exist and CI must have reported on it before required checks can be named, and the default-branch switch changes where `workflow_dispatch` resolves.

**Files:** none — GitHub settings only.

**Interfaces:**
- Consumes: Task 4's CI run, which establishes the exact check names.

- [ ] **Step 1: Confirm the exact check names**

Required status checks must be named exactly as GitHub reports them:

```bash
gh pr checks 5 2>&1 | grep -v skipping || gh run view --json jobs --jq '.jobs[].name' $(gh run list --branch master --workflow=CI --limit 1 --json databaseId --jq '.[0].databaseId')
```

Expected: `cargo test (Linux)` and `shellcheck + package tests`. Use whatever this prints, not what this plan guesses.

- [ ] **Step 2: Create `develop` from `master`**

```bash
git checkout master && git pull --ff-only
git checkout -b develop
git push -u origin develop
```

- [ ] **Step 3: Confirm CI runs on it**

```bash
gh run list --branch develop --limit 2 --json workflowName,status,conclusion --jq '.[] | "\(.workflowName) \(.status) \(.conclusion // "-")"'
```

Expected: a `CI` run. If nothing appears, Task 4 Step 1 did not take — stop and fix that before protecting anything.

- [ ] **Step 4: Make `develop` the default branch**

```bash
gh api -X PATCH repos/PeterKnego/roost -f default_branch=develop --jq .default_branch
```

Expected output: `develop`.

Then confirm the thing this could break — `check-tap-token.yml` is dispatchable only because it exists on the default branch:

```bash
gh workflow list --all --json name,path --jq '.[] | "\(.name)\t\(.path)"'
```

Expected: all five workflows still listed, including `Check tap token`.

- [ ] **Step 5: Protect `master` — restrict pushes only**

No PR requirement and no required status checks: both reject the version-bump commit `scripts/release.sh:82` pushes directly, after preflight has passed and the tag exists locally.

```bash
gh api -X PUT repos/PeterKnego/roost/branches/master/protection \
  --input - <<'JSON'
{
  "required_status_checks": null,
  "enforce_admins": false,
  "required_pull_request_reviews": null,
  "restrictions": { "users": ["PeterKnego"], "teams": [], "apps": [] },
  "allow_force_pushes": false,
  "allow_deletions": false
}
JSON
```

`enforce_admins: false` is deliberate and load-bearing — with it true, the maintainer's own release push is rejected too.

- [ ] **Step 6: Protect `develop` — PR plus both checks**

```bash
gh api -X PUT repos/PeterKnego/roost/branches/develop/protection \
  --input - <<'JSON'
{
  "required_status_checks": {
    "strict": false,
    "contexts": ["cargo test (Linux)", "shellcheck + package tests"]
  },
  "enforce_admins": false,
  "required_pull_request_reviews": { "required_approving_review_count": 0 },
  "restrictions": null,
  "allow_force_pushes": false,
  "allow_deletions": false
}
JSON
```

`"strict": false` is the "do not require branches to be up to date" decision — on a low-traffic repository, strict forces a rebase for every unrelated merge.

`required_approving_review_count: 0` requires a PR without requiring someone else to approve it, which matters for a solo maintainer: with 1, you could never merge your own work.

- [ ] **Step 7: Verify protection took, on both branches**

```bash
gh api repos/PeterKnego/roost/branches/master/protection --jq '{restrictions: .restrictions.users[].login, pr: .required_pull_request_reviews, checks: .required_status_checks}'
gh api repos/PeterKnego/roost/branches/develop/protection --jq '{pr: .required_pull_request_reviews.required_approving_review_count, checks: .required_status_checks.contexts, strict: .required_status_checks.strict}'
```

Expected: master shows the user restriction with `pr: null` and `checks: null`; develop shows `0`, both check names, and `strict: false`.

- [ ] **Step 8: Prove `make release` still works against the protected master**

This is the check that matters — the protection was designed around it, and a wrong setting only shows up mid-release.

```bash
scripts/preflight.sh
```

Expected: it dies at `✗ not on master` if you are on develop, which is correct. Switch to master and run it again; expected: every check green, including the tap-token dispatch. Do **not** cut a release — preflight alone is the test, because the push it protects happens later in `release.sh`.

Then confirm a direct push to master is still possible for you, without pushing anything real:

```bash
git checkout master && git push --dry-run origin master
```

Expected: `Everything up-to-date` — not a rejection. A rejection means the protection is wrong and `make release` will fail at `release.sh:82`.

- [ ] **Step 9: Update CONTRIBUTING with the now-real flow**

Task 2 wrote CONTRIBUTING before `develop` existed. Confirm every branch name in it matches reality, and commit any correction on `develop`:

```bash
git checkout develop
grep -n "develop\|master" CONTRIBUTING.md
```

---

## Self-review

**Spec coverage.** Every spec section maps to a task: *The flow* → Tasks 2 and 5; *Branch protection* → Task 5 Steps 5-7; *Merge strategy* → documented in CONTRIBUTING (Task 2) and enforced by the squash setting, which is a repository-level option the maintainer sets in the GitHub UI — **noted as a gap**, see below; *Fix the confinement tests first* → Task 1; *Files to add* → Tasks 2 and 3; *CI* → Task 4; *Order* → the task order itself; *Testing* → Task 1 Steps 6-7.

**One gap I could not close in a task:** the merge-strategy decision (squash for PRs into `develop`, merge commit for `develop`→`master`) is partly a repository setting — "Allow squash merging" / "Allow merge commits" under Settings → General → Pull Requests. The API can set `allow_squash_merge` and `allow_merge_commit`, but leaving both enabled and choosing per-merge is what the flow actually needs, and that is the default. No task changes it; CONTRIBUTING states the convention and the maintainer applies it at merge time.

**Placeholders.** None. Every step carries real content; the one instruction to fetch external text (Contributor Covenant, Task 2 Step 2) names the exact URL and says not to paraphrase.

**Type consistency.** All three tests in Task 1 use the same fixture shape — `parent` / `root` / `secret.txt` — and the same escape path `../secret.txt`. `Hub::new` takes the `root` `PathBuf` in all three. The check names in Task 5 Steps 6 are the ones Task 5 Step 1 instructs you to confirm rather than assume.
