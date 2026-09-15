# Releases

What each release changed, newest first.

`dist` reads this file when it builds a release: it looks for the heading
matching the tag's version and uses that section as the GitHub release body.
So a release's entry is written *before* its tag is cut, not after — an entry
added later never reaches the release page.

Keep the newest section at the top, and say what a change means for someone
running roost rather than what the diff did.

## v0.6.0 — 2026-09-15 — roost on a phone, and a workspace you can carry

The largest release so far: 108 commits. Three themes dominate — roost became
usable from a phone, a workspace became something you can take a copy of, and
the About pane will now tell you what this binary actually is and how it got
there.

- **A phone layout: one pane at a time.** The four-pane workspace collapses to
  a single pane on a narrow screen, with a header that fits real project names
  and a front page that fits at all. Terminal keys reach Claude's menus, the
  close × is visible and attached and costs no row, and there are keys that do
  not summon the on-screen keyboard alongside a button that does. Scrolling a
  running Claude works, and the first tap is no longer swallowed.
- **A paste button, because iOS will not offer one.** Mobile Safari exposes no
  paste affordance to a terminal, so roost draws its own.
- **Backup and restore.** A workspace archive — conversations included —
  downloads from a new Backup pane, and a restore builds its whole plan before
  writing anything, so you read what it will do before it does it. The archive
  format cannot name its own destination, and the download declares `nosniff`.
- **An About pane that knows what it is.** Which binary this is, how this copy
  was installed, whether it may replace itself, and the upgrade command for
  that install shape — a package, a tarball, `cargo install --git`, or a
  checkout — rather than a single command that is wrong for most readers.
- **A ✻ menu on each terminal.** Start a fresh Claude, or pick up where a
  conversation left off. roost records which Claude is in which terminal from
  the hook that was already firing, marks those tabs, and attaches by `claude`.
- **Surviving a reboot.** A terminal comes back in the directory it was in, a
  tab whose shell did not survive says so instead of looking alive, a mode
  contract holds across a roost restart, and roost offers to resume the Claude
  a reboot interrupted. The agents roost launched restart when you open the
  project.
- **A container image**, with a test that proves it runs roost. Inside a
  namespace `127.0.0.1` is unreachable from the host, so `ROOST_BIND_ALL=1`
  lets a container bind every interface — a boolean named for its consequence,
  environment-only, and fatal on an unrecognised value.
- **Make a project from the front page.** The `+` creates one when you type a
  name; the project name opens the switcher.
- **Editor work.** Find within the buffer and go to a line; tab indents, Enter
  keeps the indent and brackets close themselves; every wrapped line gets one
  number, on the row it starts on; Alt+K mentions the files picked in the tree,
  and the tree follows the file you are looking at.
- **Tabs drag** between panes, and within one to reorder.
- **Gitignore filtering actually applies.** It had been silently off in every
  worktree and submodule. A negation now poisons the file it is merged with
  rather than only its own scope, and the rule cap can no longer hide one.
- **The workspace connection has a visible state**, and `send()` stops
  reporting success for messages it did not deliver.

Fixes worth naming, because each was invisible until it was not: a missing
plugin file took the whole workspace down; two browser tests leaked their
fixture and a live shell with it; every browser test that never set a viewport
had been testing the phone; and a session name containing a dot was excluding
metadata by name.

## v0.5.2 — 2026-09-09 — one command cuts a release

- **`make release VERSION=x.y.z`** — one command that bumps, tags, watches CI
  and reads every channel back. Its preflight refuses to tag when any of
  **eleven known failures** is present, each one a failure this project
  actually had while cutting v0.5.1. Re-running after a half-finished push
  resumes rather than dying, and it never deletes a tag or a release.
- **Linux packages that carry `dtach`.** A `.deb` and an `.rpm` that declare
  the dependency and ship the systemd unit with `KillMode=process` — without
  which a restart SIGKILLs every dtach session, defeating the reason dtach is
  used at all. The packaging tests prove the packages install, not merely that
  they describe themselves.
- **Publishing to crates.io from CI with no stored token**, via trusted
  publishing.
- **`verify-release`** reads a release back rather than trusting its green
  tick — exact asset filenames, and the hashes brew actually installs from.

## v0.5.1 — 2026-09-08 — project roots from the front page

- **Add a project root from the front page**, with an explanation when there
  are none. No roots is now a startable state rather than a refusal to start.
  A symlinked alias is recognised as the same root, and a roots value that is
  not a list of strings is refused rather than overwritten.
- **The first release built and published by CI** — four targets, with Linux
  as static musl so the binary does not fault on an older glibc.
- Editor highlighting extends to 300 KB, measured rather than assumed.

## v0.5.0 — 2026-09-06 — in-page dialogs and a settings pane

- **Every native browser dialog is gone.** Ten of them replaced by in-page
  dialogs on `<dialog>`: confirmations, text entry with the basename
  preselected, and a pointer-anchored context menu. Close Project is one
  dialog, disabled while buffers are dirty.
- **A settings dialog** with project and global scopes shown side by side,
  live theme preview that Save keeps and a scope switch undoes, and writes
  that go through `toml_edit` atomically — refusing a file that does not parse
  rather than rewriting it.
- **35 vendored daisyUI themes**, selectable by name.
- `roost --version`, and a non-port first argument is refused instead of being
  silently treated as 8444.

## v0.4.0 — 2026-09-04 — raw HTML in markdown, sanitized

- **Raw HTML in a markdown preview is sanitized by an allowlist** rather than
  neutralized to text, so a GitHub-style README renders as itself and nothing
  it contains executes. The tokenizer bounds its scans at the next `<`, which
  is what keeps the sanitizer linear — the unbounded version took minutes on a
  few hundred kilobytes.
- **Claude notification hooks**, toggled from the bell, written into the
  project's own `.claude/settings.local.json` and only into entries roost
  owns. A settings file roost cannot parse is refused rather than rewritten.
- **The owl** — the home mark, the header, and every favicon, with
  content-hashed URLs so a browser that cached "no icon" looks again.
- `SECURITY.md`, and a README that leads with what roost looks like.
