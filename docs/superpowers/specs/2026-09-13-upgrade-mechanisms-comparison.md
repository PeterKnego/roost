# How four things update themselves, and what that means for roost

*2026-09-13. Status: research input for #65 step 4 ("only then, consider
running it for the shapes where it is safe"). Not a design; the design that
picks an option follows it. Sources: the Tauri v2 updater docs, the
`self_update` and `self-replace` crate docs, the Chromium updater and
Courgette design docs, and the Firefox source docs on in-app and background
updates, all read on 2026-09-13. Anything marked* (memory) *was not verified
against a document this session.*

## The four, in one table

| | `self_update` (Rust crate) | Tauri updater plugin | Firefox | Chrome |
|---|---|---|---|---|
| **Who replaces the file** | The running binary, in-process | The running app, in-process | A helper (`updater.exe`), plus the Maintenance Service on Windows for privileged installs | A separate updater (Omaha 4; formerly Google Update / Keystone) running as its own service |
| **Runs when the app is closed** | No | No | Yes, a background task, if installed by the installer and BITS is on | Yes, roughly every five hours |
| **Where "is there a newer" comes from** | A release list from GitHub / GitLab / Gitea / Gitee / S3-compatible storage | A static JSON manifest, or a dynamic server that resolves per client | Balrog: XML tailored to OS, channel, locale, version; can throttle a rollout | Omaha protocol: per-install IDs, cohorts, staged rollout, closed server |
| **Who decides "newer"** | The client, by semver against `current_version` | Static: the client. Dynamic: the server | The server | The server |
| **Payload** | The whole release asset (tar/zip), unpacked | The whole installer or bundle | A partial MAR against the previous version, complete MAR as fallback | A Courgette diff against the exact binary the client reports, full as fallback |
| **Trust in the payload** | TLS to the host; optional `signatures` feature verifies `.zip` / `.tar.gz` | A detached signature per artifact, checked against a public key baked into the app. Losing the private key ends updates for existing installs | MAR files are signed; verified by the updater* (memory) | Payloads signed; verified by the updater* (memory) |
| **When the new version takes effect** | Next launch. The running process keeps the old inode; nothing restarts it | Immediately: install, then `relaunch()` | Staged beside the install; swapped at next start, or the background task restarts to finalise | Staged; swapped at next launch |
| **Rollback** | None | None | Keeps the previous files until the swap succeeds* (memory) | Keeps the previous version alongside* (memory) |
| **Install location it can serve** | Wherever the process can write; fails at the rename otherwise | Its own bundle formats only: NSIS/MSI, `.app`, AppImage | Any install the installer made; the Maintenance Service supplies privilege | Any; the updater is the privileged part |
| **Package-manager installs** | Not its problem; it will overwrite one if it can | Not supported: `.deb` / `.rpm` / distro packages are outside the plugin | A distro Firefox has the updater disabled by the distro | Chrome ships its own apt/yum repo, so the package manager *is* the updater* (memory) |
| **Platforms** | Unix natively; Windows through `self-replace`'s delete-on-close trick | Windows, macOS, Linux (AppImage) | All three | All three |

## The axis that matters most for a single binary

Reading down the table, the four separate into two families:

- **In-process** (`self_update`, Tauri). Small, no second component, no
  schedule, and the update can only succeed where the running process can
  already write. Both push the "who owns this install" question onto the
  developer: Tauri by supporting only bundles it produces, `self_update` by
  not asking.
- **Privileged helper** (Firefox, Chrome). A second process with its own
  rights answers the ownership question by construction, and pays for it with
  a service, an installer that registers it, and a background schedule.

Diffs, staging, cohorts and rollback are all things the helper family can
afford *because* it already has a second process. They are not the reason the
two families differ; write access is.

## Where roost already stands

roost has answered the ownership question a third way, in `src/install.rs`
and About: **probe whether the binary's directory is writable, sniff who
installed it, and offer the command rather than run it.** That is #65 step 3,
merged in #77 and #78. This document is about step 4, which #65 scopes to
"probably systemd-plus-signed-artifact and nothing else".

Facts that constrain any option, each with where it comes from:

1. **Five shapes, and only two are roost's to replace.** `dialog.js`'s
   `upgradesLabel` already says "roost can replace this copy" for exactly one
   condition: channel `release`, owner not Homebrew and not a system package,
   probe `Yes`. That is the shell installer (`~/.cargo/bin`) and the tarball
   (`Other`). Homebrew, `.deb` / `.rpm`, `cargo install`, a checkout, and the
   container are all somebody else's, and About already says so.
2. **All release channels ship the same bytes**, one sha256 across brew, the
   tarball and the `.deb` (measured 2026-09-12, recorded in `install.rs`). So
   a self-update that fetches the release tarball for `ROOST_TARGET` installs
   the same file every other channel would.
3. **What "signed" means here.** `dist-workspace.toml` sets
   `checksum = "sha256"` and `github-attestations = true`. So each artifact
   has a `.sha256` beside it on the release, and a SLSA provenance
   attestation verifiable with `gh attestation verify`. There is no
   application-level signature of the Tauri kind, and no cosign step in the
   workflow despite #65's wording. macOS binaries are ad-hoc signed and not
   notarized. Verifying a Sigstore attestation without `gh` means a Sigstore
   client in Rust, which is a large dependency for one check.
4. **The running process must not be replaced while it serves** (#65). On
   Linux and macOS a rename-over swap is safe for the running process, which
   keeps its old inode; the hazard is the *display*: About would report the
   old version over a new file until restart. `self_update` does exactly this
   swap and leaves the restart to the user.
5. **A restart must keep the sessions.** dtach makes that true already: the
   README promises terminals survive a roost restart, and
   `packaging/roost.service` carries `KillMode=process` so systemd does not
   take the sessions with the service. But `Restart=on-failure` means a
   *clean* exit is not restarted, and a roost started by hand under `nohup`
   or in a terminal is not restarted by anyone. "Restart" therefore means
   something different per shape, and the container has no restart at all
   (`docs/deploy.md`: the container's main process exiting is the container
   exiting).
6. **The Linux packages install to `/usr/bin`**, which the probe reads as
   `No` for a non-root user, and the unit's `ExecStart` is that path. A
   self-update never applies to them, by the rule in point 1.
7. **A browser download is poisoned on macOS** (#65 comment, 2026-09-12): the
   quarantine attribute makes the binary hang without output. A fetch by
   roost itself carries no attribute. Anything that shells out to the *new*
   binary to read its version needs a timeout, because "hangs" is a fourth
   outcome that exit status cannot see.
8. **The version check lands first** (`2026-09-12-version-check-design.md`):
   crates.io sparse index, three-way `Latest`, 24h / 1h intervals, stored as
   a fact not a verdict. A self-update consumes that answer; it does not
   check again.

## What each model would look like on roost

Sketched only far enough to compare. None is chosen here.

**`self_update`-shaped: a `roost upgrade` subcommand.** Fetch
`releases/latest/download/roost-<target>.tar.xz` and its `.sha256`, verify,
unpack to a temp file in the binary's directory, rename over the executable,
print "restart roost to run <version>". Applies only where About already says
"roost can replace this copy"; refuses, naming the owner, elsewhere. No UI,
no restart, runs in the user's terminal so failure is visible. Closest to what
step 3 already offers, with the shell replaced by roost.

**Tauri-shaped: an `[Update]` button that installs and relaunches.** The same
fetch and swap, triggered from About over the websocket, followed by roost
restarting itself. Needs a restart mechanism that works in every shape the
button appears in, and point 5 says there is not one: under systemd a clean
exit stops the service. A re-exec of the new binary in place, inheriting
the listening socket, is the technique that works everywhere, and it is the
piece roost does not have.

**Firefox-shaped: download and stage now, swap at next start.** Fetch into
the state directory, verify, and have the *next* roost start notice the
staged file and install it before serving. Preserves the running display,
survives a half-finished download, and never touches the executable from the
serving process. Costs a startup step in the binary and a second place a
binary can live.

**Chrome-shaped: a separate updater process on a schedule.** Not on the
table. roost's whole security posture is one process on loopback, and the
only shape that could use an unattended updater is the systemd one, where
the package manager is the correct updater anyway.

## What is not a differentiator for roost

- **Diffs.** The musl tarball is a few megabytes; a diff engine is not worth
  its code.
- **Rollout control.** One publisher, one channel, no cohorts.
- **Windows.** Not a target (`dist-workspace.toml`: "roost spawns dtach").
