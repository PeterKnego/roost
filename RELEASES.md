# Releases

What each release changed, newest first.

`dist` reads this file when it builds a release: it looks for the heading
matching the tag's version and uses that section as the GitHub release body.
So a release's entry is written *before* its tag is cut, not after — an entry
added later never reaches the release page.

Keep the newest section at the top, and say what a change means for someone
running roost rather than what the diff did.

## v0.7.0 — 2026-09-30 — roost tells you when there is a newer roost

roost now knows what the newest published version is, and says so. The
machinery to replace itself from a button is in this release too, but it stays
switched off until the release signing key exists (#87): a release built
without that key offers no button to anyone.

- **A Latest row in About.** roost reads the newest published version from the
  crates.io index and says one of: *0.x.y available*, *up to date*, *could not
  check*, *not checked yet*, or *version checks are off*. It asks when a browser
  opens a project — never on a timer, so a roost nobody is looking at makes no
  requests — at most once a day after a success, hourly after a failure, and it
  honours `HTTPS_PROXY`. `version_check = false` in the global config turns it
  off. "Could not check" is never folded into "up to date", including after a
  long run of failures.
- **An ↑ mark when a newer version is out.** Once a version newer than this one
  is published, the header shows `↑ 0.x.y` and a dialog offers *Later* (a day)
  or *Skip 0.x.y*, with the upgrade command for how this copy was installed.
  When the dialog opens by itself it focuses *Later*, so a keystroke meant for
  a terminal cannot start anything. A checkout build shows no mark.
- **The self-update, dormant.** For a release install that may replace itself,
  a future signed release adds an *Update* button: roost downloads the tarball
  and its signature, verifies the signature before opening anything, checks
  the new binary answers `--version`, swaps it in, and restarts in place so
  your terminals survive. It needs a key compiled in; this release has none.
- **A terminal you are looking at has nothing unread.** A notice from the
  terminal you are typing in is read on arrival and raises no desktop banner,
  and returning to the page or clicking into a terminal reads the ones waiting
  for it. `read_when_watching = false` keeps them unread until clicked.
- **Revert from the Changes pane and the Diff tab**, instead of the file-tree
  menu that appeared there.
- **Download a file from the tree** — the other half of upload.
- **Agents can ask what other agents are doing.** `roost sessions` lists them
  and `roost wait` blocks until one finishes.
- **Install roost to a phone's home screen**, and a JSON surface a native app
  can read instead of scraping HTML.
- **The Files pane and the terminal work by touch.**
- **A missing `dtach` says so** on the terminal, instead of "session ended".

Under the hood: roost now makes HTTPS requests itself, so the release binary
carries rustls and ring (pure webpki roots, no OpenSSL), and building it for
musl needs `musl-tools`.

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
