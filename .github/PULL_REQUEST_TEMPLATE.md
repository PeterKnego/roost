## What and why

<!-- What changes, and what problem it solves. The why matters more. -->

## How it was tested

<!-- Commands you ran and what they printed. If you added a test, did you
     watch it fail without your change? -->

## Hard constraints

<!-- CLAUDE.md lists constraints that are load-bearing rather than stylistic —
     among them: the loopback bind, Origin checks, path confinement, the caps, the
     settings-file rules. Does this PR touch any of them? If so, which, and
     why does the reason recorded there no longer hold? -->

- [ ] `cargo test --locked -- --test-threads=1` passes
- [ ] `make test-scripts` passes, or this PR touches nothing under `scripts/` or `packaging/`
