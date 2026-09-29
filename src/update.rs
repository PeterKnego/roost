//! Updating roost from the button. See
//! `docs/superpowers/specs/2026-09-13-self-update-design.md`.

include!("pubkey.rs");

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::hub::{ConnId, Hub};
use crate::proto::Event;

/// The download cap. The musl tarball is 1.4 MB; a server answering with
/// gigabytes is one to walk away from, and the check happens twice — against
/// `Content-Length` before the body is buffered, and while reading it.
pub const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
/// The `--version` probe's bound. The macOS lesson from #65: a hang is a
/// fourth outcome that exit status cannot see.
pub const PROBE_SECS: u64 = 5;
/// How long `Later` keeps the dialog from opening by itself. The mark stays.
pub const DEFER_SECS: u64 = 24 * 60 * 60;

/// The public key `build.rs` baked from `keys/roost.pub`, or `None` for a
/// build made without one — which offers nothing, rather than trusting
/// anything.
pub fn public_key() -> Option<&'static str> {
    match option_env!("ROOST_UPDATE_PUBKEY") {
        Some(k) if !k.is_empty() => Some(k),
        _ => None,
    }
}

/// `<repository>/releases/download/v<version>`, the directory dist hosts a
/// release's artifacts under. Derived from `BuildInfo.repository`, never
/// spelled and never configured: a setting that let a repository name the
/// host roost fetches a binary from would be the hole the version-check spec
/// exists to avoid.
pub fn release_base(repository: &str, version: &str) -> String {
    format!("{}/releases/download/v{version}", repository.trim_end_matches('/'))
}

/// `release_base`, unless `ROOST_UPDATE_BASE` names another for this process.
/// Environment only, never config, and a test hook rather than a feature:
/// the manual run in this plan's last task serves a locally signed tarball
/// from it. It is not a hole because the signature is the boundary, not the
/// URL — a redirected download without the private key fails at `verify`
/// and the executable is untouched.
pub fn download_base(repository: &str, version: &str) -> String {
    match std::env::var("ROOST_UPDATE_BASE") {
        Ok(b) if !b.is_empty() => b.trim_end_matches('/').to_string(),
        _ => release_base(repository, version),
    }
}

/// The tarball and its detached signature, named the way dist names them
/// (`roost-<target>.tar.xz`) and the way `minisign -S` names its output.
pub fn asset_urls(base: &str, target: &str) -> (String, String) {
    let tarball = format!("{base}/roost-{target}.tar.xz");
    let sig = format!("{tarball}.minisig");
    (tarball, sig)
}

/// Verified over the tarball bytes, **before** decompression touches them:
/// nothing unsigned is parsed. `allow_legacy` is false — the release signs
/// with a current minisign, and the legacy format is one more thing to
/// accept for no reason. The three messages are distinct on purpose: a
/// signature that does not verify is the one someone should hear about.
pub fn verify(tarball: &[u8], minisig: &str, pubkey_b64: &str) -> Result<(), String> {
    let pk = minisign_verify::PublicKey::from_base64(pubkey_b64)
        .map_err(|e| format!("the compiled-in key could not be read: {e}"))?;
    let sig = minisign_verify::Signature::decode(minisig)
        .map_err(|e| format!("the signature file could not be read: {e}"))?;
    pk.verify(tarball, &sig, false)
        .map_err(|_| "signature did not verify".to_string())
}

/// Bounds the *cumulative* decompressed output `xz_decompress` writes across
/// the XZ blocks that make up one stream — `lzma_rs`'s XZ entry point takes no
/// size limit of its own (that only exists on the raw LZMA API's
/// `Options::memlimit`, which the XZ container format doesn't expose), so
/// this is the only bound between block N+1 and the running total.
///
/// It does **not** bound a single block: `lzma_rs` 0.3.0's `read_block`
/// (`src/decode/xz.rs:196-284`) decodes one whole block into a local,
/// unbounded `tmpbuf` and only calls `output.write_all(tmpbuf)` once that
/// block is fully decoded (`xz.rs:282`) — so a one-block bomb is fully
/// materialized in memory before `Capped::write` ever runs, no matter how
/// small `cap` is. That gap is accepted here rather than worked around: this
/// function only ever runs on bytes that already passed `verify` against the
/// compiled-in key (the pipeline enforces that order), so producing a
/// tarball that exploits it requires the release signing key. The bounds
/// that do apply regardless of the key are the 64 MB download cap enforced
/// while the tarball is fetched (`MAX_ARCHIVE_BYTES`, checked against
/// `Content-Length` and again while reading the body) and, below, the
/// per-member `entry.size()` check against the same constant.
struct Capped<'a> {
    buf: &'a mut Vec<u8>,
    cap: u64,
}

impl std::io::Write for Capped<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.buf.len() as u64 + data.len() as u64 > self.cap {
            return Err(std::io::Error::other("decompressed archive exceeds the size cap"));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The one binary out of a release tarball. dist writes `<target>/roost`
/// beside a README and two licences, so the rule is *exactly one regular
/// member whose file name is `roost`*, at any depth. A symlink there would
/// be followed by nothing here but is refused anyway: the file that gets
/// probed and swapped has to be the bytes that were signed. Nothing here
/// ever touches disk — the result is bytes in memory, staged and swapped by
/// a later step — so there is no path to confine and no traversal to guard
/// against; the only hostile shapes that matter at this stage are ones that
/// would make this function allocate or return the wrong bytes.
pub fn extract_binary(tar_xz: &[u8]) -> Result<Vec<u8>, String> {
    let mut tar = Vec::new();
    lzma_rs::xz_decompress(&mut &tar_xz[..], &mut Capped { buf: &mut tar, cap: MAX_ARCHIVE_BYTES })
        .map_err(|e| format!("the archive could not be decompressed: {e:?}"))?;
    let mut archive = tar::Archive::new(&tar[..]);
    let entries = archive.entries().map_err(|e| format!("the archive could not be read: {e}"))?;
    let mut found: Option<Vec<u8>> = None;
    let mut count = 0usize;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("the archive could not be read: {e}"))?;
        let is_roost = entry
            .path()
            .ok()
            .and_then(|p| p.file_name().map(|n| n == "roost"))
            .unwrap_or(false);
        if !is_roost {
            continue;
        }
        count += 1;
        match entry.header().entry_type() {
            tar::EntryType::Regular | tar::EntryType::Continuous => {}
            tar::EntryType::Directory => return Err("the roost member is a directory".into()),
            tar::EntryType::Symlink | tar::EntryType::Link => return Err("the roost member is a symlink".into()),
            other => return Err(format!("the roost member is not a regular file ({other:?})")),
        }
        if entry.size() > MAX_ARCHIVE_BYTES {
            return Err("the roost member is larger than the download cap".into());
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        std::io::Read::read_to_end(&mut entry, &mut bytes)
            .map_err(|e| format!("the roost member could not be read: {e}"))?;
        found = Some(bytes);
    }
    match (count, found) {
        (0, _) => Err("the archive has no roost member".into()),
        (1, Some(b)) => Ok(b),
        (n, _) => Err(format!("the archive has {n} roost members")),
    }
}

/// Run the staged file with `--version` and require stdout to be exactly
/// `roost <want>` — what `main.rs` prints — with exit 0, inside `timeout`.
/// The outer shape is `gitio::run_git`'s: stdout drained on its own thread so
/// a full pipe cannot wedge the poll, `try_wait` against a deadline, then
/// `kill` + `wait` so a hung child is reaped rather than leaked.
///
/// The wait for output is *itself* bounded by the same deadline the wait for
/// exit is, via an `mpsc` channel instead of a bare `.join()` on the reader
/// thread (the shape `launch::probe` and `gitio::run_git_within` both use).
/// That matters here specifically: a child can fork a grandchild and then
/// exit on time, leaving the pipe's write end open in a process this
/// function never sees and so never kills. `read_to_string` only returns on
/// EOF, so an unconditional `.join()` would block on that lingering
/// descendant — the process exited, `try_wait` reports success, and the
/// probe still hangs. Bounding the join with `recv_timeout` means that case
/// instead comes back as "did not answer in time", which is the correct
/// failure direction for a check gating a binary swap: no positive evidence
/// within the bound is a refusal, not a success taken on faith.
pub fn probe(exe: &Path, want: &str, timeout: std::time::Duration) -> Result<(), String> {
    use std::io::Read;
    let spawn = || {
        std::process::Command::new(exe)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
    };
    // `stage` closes its write handle before this runs, but a thread that
    // forked while that handle was open leaves a child holding an inherited
    // copy until it execs — and while it does, exec of the staged file is
    // ETXTBSY. The window is microseconds, and failing is safe, but it would
    // cost the user a whole re-download for nothing, so one retry after the
    // child has certainly exec'd. Exactly one: a file still busy after that
    // is someone else's writer, and a probe failure is the right answer.
    let mut child = match spawn() {
        Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
            std::thread::sleep(std::time::Duration::from_millis(100));
            spawn()
        }
        r => r,
    }
    .map_err(|e| format!("the new binary could not be started: {e}"))?;
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait().map_err(|e| format!("could not wait for the new binary: {e}"))? {
            Some(st) => break st,
            None if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("the new binary did not answer --version within {}s", timeout.as_secs()));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    };
    // Bounded by whatever is left of `timeout`, not `timeout` again: the
    // exit wait above may already have used most of it.
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    let out = rx
        .recv_timeout(remaining)
        .map_err(|_| format!("the new binary did not answer --version within {}s", timeout.as_secs()))?;
    if !status.success() {
        return Err(format!("the new binary exited {status} on --version"));
    }
    let got = out.trim();
    if got != format!("roost {want}") {
        return Err(format!("the new binary reports {got:?}, not \"roost {want}\""));
    }
    Ok(())
}

/// Where the pipeline was when it stopped. The dialog names it, About names
/// it, and `errlog` records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Download,
    Verify,
    Unpack,
    Probe,
    Swap,
    Exec,
    /// The thread panicked. Reported like any other failure, and it clears
    /// the guard like any other failure.
    Internal,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Download => "download",
            Phase::Verify => "verify",
            Phase::Unpack => "unpack",
            Phase::Probe => "probe",
            Phase::Swap => "swap",
            Phase::Exec => "exec",
            Phase::Internal => "internal",
        }
    }
}

/// The injection seam, like `version::FetchFn`: a plain `fn` so it crosses
/// into the detached thread without a lifetime.
pub type FetchBytes = fn(&str) -> Result<Vec<u8>, String>;

/// Everything the pipeline needs, decided before the thread starts. `exe`
/// is captured first of all: on Linux `current_exe` reads `/proc/self/exe`,
/// which after the rename names the deleted inode with ` (deleted)`.
pub struct Plan {
    pub exe: PathBuf,
    pub tarball_url: String,
    pub sig_url: String,
    pub pubkey: String,
    pub want: String,
    pub probe_timeout: std::time::Duration,
}

/// The new bytes, as `.roost-update.<pid>` in the executable's own directory
/// — the same directory because the swap is a rename, and a rename is atomic
/// only within one filesystem.
///
/// `create_new` (`O_EXCL`) because anything already at that name is not
/// ours: it may be a symlink `fs::write` would follow and write through, and
/// it is not ours to delete either — the error leaves it where it is. The
/// file is created 0600 and marked 0755 only once every byte is written, so
/// nothing can execute a half-written file. A failure *after* the create
/// removes the one path this call created, and nothing else.
pub fn stage(dir: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    use std::io::Write;
    let p = dir.join(format!(".roost-update.{}", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&p).map_err(|e| format!("could not create {}: {e}", p.display()))?;
    let written = f
        .write_all(bytes)
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("could not write {}: {e}", p.display()))
        .and_then(|()| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(std::fs::Permissions::from_mode(0o755))
                    .map_err(|e| format!("could not mark {} executable: {e}", p.display()))?;
            }
            Ok(())
        });
    // Closed before anything can exec it: an open write handle is ETXTBSY.
    drop(f);
    if let Err(e) = written {
        return Err(unstage(&p, e));
    }
    Ok(p)
}

/// Remove the file `stage` created, by its exact path, and fold a failure to
/// do so into the error rather than dropping it: a staged file left behind
/// is worth a sentence in the dialog.
fn unstage(staged: &Path, why: String) -> String {
    match std::fs::remove_file(staged) {
        Ok(()) => why,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => why,
        Err(e) => format!("{why} (and {} could not be removed: {e})", staged.display()),
    }
}

/// The one irreversible step, and a single syscall. The running process
/// keeps its old inode until it execs.
pub fn swap(staged: &Path, exe: &Path) -> Result<(), String> {
    std::fs::rename(staged, exe).map_err(|e| format!("rename refused: {e}"))
}

/// Fetch, verify, unpack, probe, swap. Every step before the rename fails
/// safe by construction: the executable is untouched and the staged file is
/// removed. `report` is called as each phase *starts*, so the dialog reads
/// what is happening rather than what just did.
///
/// Verify runs on the downloaded bytes before `extract_binary` sees them.
/// That order is what makes `extract_binary`'s unbounded single XZ block
/// acceptable (see `Capped`), so it is not an order to rearrange.
pub fn run_pipeline(plan: &Plan, fetch: FetchBytes, report: &mut dyn FnMut(Phase)) -> Result<(), (Phase, String)> {
    report(Phase::Download);
    let tarball = fetch(&plan.tarball_url).map_err(|e| (Phase::Download, e))?;
    let sig = fetch(&plan.sig_url).map_err(|e| (Phase::Download, e))?;
    let sig = String::from_utf8(sig).map_err(|_| (Phase::Download, "the signature file is not text".to_string()))?;

    report(Phase::Verify);
    verify(&tarball, &sig, &plan.pubkey).map_err(|e| (Phase::Verify, e))?;

    report(Phase::Unpack);
    let bytes = extract_binary(&tarball).map_err(|e| (Phase::Unpack, e))?;
    let dir = plan
        .exe
        .parent()
        .ok_or((Phase::Unpack, "the executable has no parent directory".to_string()))?;
    let staged = Staged(Some(stage(dir, &bytes).map_err(|e| (Phase::Unpack, e))?));

    report(Phase::Probe);
    if let Err(e) = probe(staged.path(), &plan.want, plan.probe_timeout) {
        return Err((Phase::Probe, staged.fail(e)));
    }

    report(Phase::Swap);
    if let Err(e) = swap(staged.path(), &plan.exe) {
        return Err((Phase::Swap, staged.fail(e)));
    }
    staged.disarm();
    Ok(())
}

/// Owns the staged path from `stage` until the rename. A failure returns
/// through `fail`, which folds a failed removal into the message; a panic
/// between the two (in `probe`, or in a `report` that takes the hub lock)
/// unwinds through `Drop`, which removes it too. Without this one leak
/// blocks every later update in the process: the name is pid-fixed and
/// `stage` refuses a path that exists. Disarmed only once `swap` returns
/// `Ok`, when the path names the executable and is no longer ours to remove.
struct Staged(Option<PathBuf>);

impl Staged {
    fn path(&self) -> &Path {
        self.0.as_deref().expect("a Staged is armed until it is consumed")
    }
    fn fail(mut self, why: String) -> String {
        match self.0.take() {
            Some(p) => unstage(&p, why),
            None => why,
        }
    }
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = unstage(&p, String::new());
        }
    }
}

/// Read at most `cap` bytes, and say so if there were more: the header check
/// in `http_get_bytes` is what a well-behaved server passes, and this is what
/// a lying one hits.
pub fn read_capped(r: impl std::io::Read, cap: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut buf = Vec::new();
    r.take(cap + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("the download stopped: {e}"))?;
    if buf.len() as u64 > cap {
        return Err(format!("the download exceeded the {cap} byte cap"));
    }
    Ok(buf)
}

/// The real fetch: the version check's agent (ten-second bound, the
/// User-Agent crates.io asks for, `HTTPS_PROXY` honoured), redirects
/// followed — `releases/download/` answers 302 to a CDN — and the cap checked
/// against `Content-Length` before a byte of body is buffered. A non-2xx is
/// an error in `ureq` 2 (`Error::Status`), whose message carries the code.
pub fn http_get_bytes(url: &str) -> Result<Vec<u8>, String> {
    http_get_capped(url, MAX_ARCHIVE_BYTES)
}

/// `http_get_bytes` with the cap as a parameter, so a test can reach both
/// checks over a real socket without sending 64 MB.
fn http_get_capped(url: &str, cap: u64) -> Result<Vec<u8>, String> {
    let resp = crate::version::agent().get(url).call().map_err(|e| e.to_string())?;
    if let Some(len) = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok()) {
        if len > cap {
            return Err(format!("the server announced {len} bytes, above the {cap} byte cap"));
        }
    }
    read_capped(resp.into_reader(), cap)
}

/// What the user said to the dialog. Two choices, two lifetimes: `Later`
/// defers the self-opening for `DEFER_SECS` and keeps the mark; `Skip` is
/// per version, and a later version un-skips by itself because the stored
/// value is the version string, not a flag.
///
/// **A separate file from the version check's**, on purpose: that one is
/// written by the check thread when a fetch completes, this one by the intent
/// handler when a user clicks. One file with two writers is a rename race in
/// which a completed check silently discards a click.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choices {
    pub skipped: Option<String>,
    pub deferred_until: Option<u64>,
}

pub fn choices_path() -> PathBuf {
    crate::version::state_dir_for_update().join("choices.json")
}

/// Missing, truncated or unreadable is **no choice was made** — which errs
/// toward showing the dialog, the recoverable direction. This is the one
/// reader in the feature where folding "could not look" into the default is
/// right, because the default is the safe one.
pub fn read_choices_from(path: &Path) -> Choices {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Pid-unique temp file, then `rename`, like every piece of persistent
/// evidence here — and like `version::write_state_to`, a failed rename must
/// not leave the tmp file behind.
pub fn write_choices_to(path: &Path, c: &Choices) -> Result<(), String> {
    let dir = path.parent().ok_or_else(|| "no parent directory".to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string(c).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".choices.{}.json.tmp", std::process::id()));
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(())
}

/// Deferred until a moment still ahead — and not further ahead than `Later`
/// can write, so a stepped clock or a copied file cannot silence the dialog
/// for a year.
pub fn deferred(c: &Choices, now: u64) -> bool {
    c.deferred_until.is_some_and(|t| t > now && t - now <= DEFER_SECS)
}

pub fn skipped(c: &Choices, latest: &str) -> bool {
    c.skipped.as_deref() == Some(latest)
}

/// `Later`: the deferral moves, the skip stays.
pub fn defer_in(path: &Path, now: u64) -> Result<(), String> {
    let mut c = read_choices_from(path);
    c.deferred_until = Some(now + DEFER_SECS);
    write_choices_to(path, &c)
}

/// `Skip <version>`: only the version the last check actually saw, so a
/// click from a stale page cannot skip a version it never showed. A skip
/// clears the deferral — there is nothing left to defer.
pub fn skip_in(path: &Path, latest: Option<&str>, version: &str) -> Result<(), String> {
    match latest {
        Some(l) if l == version => {}
        Some(l) => return Err(format!("{version} is not the version the last check saw ({l})")),
        None => return Err(format!("{version} is not the version the last check saw (no check yet)")),
    }
    let mut c = read_choices_from(path);
    c.skipped = Some(version.to_string());
    c.deferred_until = None;
    write_choices_to(path, &c)
}

/// The last attempt's failure, for the version it was for. Not persisted:
/// an exec is the success case, and a crash is a different problem.
static LAST_FAILURE: Mutex<Option<(String, Phase, String)>> = Mutex::new(None);
/// The version swapped in when the exec afterwards failed. The one state
/// where the file and the display disagree, so About says "restart roost".
static INSTALLED: Mutex<Option<String>> = Mutex::new(None);

pub fn record_failure(latest: &str, phase: Phase, msg: &str) {
    *LAST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()) = Some((latest.to_string(), phase, msg.to_string()));
}

pub fn record_installed(version: &str) {
    *INSTALLED.lock().unwrap_or_else(|e| e.into_inner()) = Some(version.to_string());
}

/// The button's condition, from the same four strings `upgradesLabel` in
/// `dialog.js` reads for "roost can replace this copy" — so the server and
/// the label cannot disagree. Each refusal names why, for the log.
pub fn replaceable_by(b: &crate::proto::BuildInfo) -> Result<(), String> {
    if b.channel != "release" {
        return Err(format!("channel {}", b.channel));
    }
    if b.owner == "homebrew" || b.owner == "system-package" {
        return Err(format!("owned by {}", b.owner));
    }
    if b.replaceable != "yes" {
        return Err("not writable by roost".into());
    }
    Ok(())
}

pub fn replaceable_here() -> Result<(), String> {
    replaceable_by(&crate::config::build_info())
}

/// What the dialog and the About row render from: step 2's two fields, plus
/// the choices and this process's last outcome. Read by
/// `config::settings_view`, so on every settings-cache miss — which
/// `RequestState`, a connecting client, `SetSetting` and every choice's
/// `hub::broadcast_settings_all` each cause.
///
/// Its I/O is one small read of `choices.json` per call. `check.json` is
/// read once per process by `version::current` and cached after, and
/// `build_info`'s write probe is a `OnceLock`. The two statics are leaf
/// locks held for a clone, never across I/O.
/// The `[Update]` button's whole condition: a copy roost may replace *and* a
/// key to verify with. Pure, so the key clause is testable in a checkout
/// that has none.
pub fn offer_for(build: &crate::proto::BuildInfo, key: Option<&str>) -> bool {
    replaceable_by(build).is_ok() && key.is_some()
}

pub fn view() -> crate::proto::UpdateView {
    let mut v = crate::version::view();
    v.offer = offer_for(&crate::config::build_info(), public_key());
    let now = crate::errlog::now_secs();
    let c = read_choices_from(&choices_path());
    v.skipped = c.skipped.clone().unwrap_or_default();
    v.deferred_until = if deferred(&c, now) { c.deferred_until.unwrap_or(0) } else { 0 };
    v.installed = INSTALLED.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default();
    v.failure = match &*LAST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()) {
        Some((for_version, phase, msg)) if *for_version == v.latest => format!("{}: {msg}", phase.as_str()),
        _ => String::new(),
    };
    v
}

/// One update for the whole process. Taken on the calling thread before the
/// pipeline thread starts, so two clicks in the same instant fetch once.
/// Cleared on every path except a successful exec, where the process is
/// about to be replaced — by `Flight`'s `Drop`, so a panic clears it too.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Owns the taken `IN_FLIGHT`. Moved into the pipeline thread, so the flag
/// is released when that thread finishes by any route — a return, a panic
/// past both `catch_unwind`s, or the thread never starting (the closure
/// that owns it is dropped by the failed `spawn`). The one route that never
/// drops it is the exec that worked, and there nothing is left to release.
struct Flight;

impl Drop for Flight {
    fn drop(&mut self) {
        IN_FLIGHT.store(false, Ordering::SeqCst);
    }
}

/// The exec seam: a plain `fn`, like `FetchBytes`, so a test can drive the
/// whole flight without replacing the test runner.
pub type ExecFn = fn(&Path) -> String;

/// Replace this process with the file at `exe`, same PID, same arguments,
/// same environment: `ROOST_ROOTS`, `ROOST_STATE_DIR`, `ROOST_BIND_ALL` and
/// the port argument ride along unchanged. systemd sees nothing;
/// `KillMode=process` is irrelevant; the dtach masters stay children of the
/// same PID; a hand-run roost keeps its terminal. The listening socket needs
/// no hand-off: Rust opens sockets close-on-exec, and the new process binds
/// the same port a few milliseconds later. Returns only on failure.
pub fn exec(exe: &Path) -> String {
    use std::os::unix::process::CommandExt;
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let err = std::process::Command::new(exe).args(args).exec();
    err.to_string()
}

/// The three checks, in the order a user would want to hear about them:
/// is this copy roost's to replace, does this build carry a key, and is the
/// last check's version actually newer than this one. `Ok` carries the
/// version to install.
pub fn eligible() -> Result<String, String> {
    replaceable_here()?;
    if public_key().is_none() {
        return Err("this build carries no release key".into());
    }
    let s = crate::version::current().ok_or("no version check has run yet")?;
    let latest = s.latest.ok_or("the last check did not name a version")?;
    match crate::version::verdict(env!("CARGO_PKG_VERSION"), Some(&latest)) {
        crate::version::Latest::Newer(v) => Ok(v),
        _ => Err(format!("{latest} is not newer than {}", env!("CARGO_PKG_VERSION"))),
    }
}

fn progress(phase: &str, detail: &str) -> Event {
    Event::UpdateProgress { phase: phase.to_string(), detail: detail.to_string() }
}

/// Never under a hub lock: it appends to a file.
fn log(text: &str) {
    crate::errlog::record(&format!("update: {text}"), crate::errlog::now_secs());
}

/// One message to the requester alone. The lock lives for this statement.
fn tell(hub: &Arc<Mutex<Hub>>, to: &ConnId, phase: &str, detail: &str) {
    Hub::lock(hub).send_to(to, &progress(phase, detail));
}

/// This executable, as the path the rename must replace: absolute and with
/// every symlink resolved. A symlink (`~/bin/roost -> ~/.cargo/bin/roost`)
/// would make the rename replace the *link* and leave the real file old;
/// a relative path (macOS `current_exe` can return one) would stage in the
/// cwd, possibly another filesystem, where the rename is not atomic or
/// refused. On Linux this is `/proc/self/exe`, which after an earlier swap
/// in this process names the deleted inode with " (deleted)" — canonicalize
/// then fails, which refuses a second update over a swapped file that is
/// waiting for a restart, rather than guessing at a path.
fn this_exe() -> Result<PathBuf, String> {
    let p = std::env::current_exe().map_err(|e| format!("this executable's path could not be read: {e}"))?;
    std::fs::canonicalize(&p).map_err(|e| format!("{} could not be resolved: {e}", p.display()))
}

/// The `Update` intent. Decides, then hands off to `drive` with the real
/// exec. Nothing here holds the hub lock for longer than one `send_to`.
pub fn start(hub: Arc<Mutex<Hub>>, from: ConnId, fetch: FetchBytes) {
    let plan = eligible().and_then(|latest| {
        let exe = this_exe()?;
        let b = crate::config::build_info();
        let (tarball_url, sig_url) = asset_urls(&download_base(&b.repository, &latest), &b.target);
        Ok(Plan {
            exe,
            tarball_url,
            sig_url,
            pubkey: public_key().unwrap_or_default().to_string(),
            want: latest,
            probe_timeout: std::time::Duration::from_secs(PROBE_SECS),
        })
    });
    match plan {
        Ok(plan) => drive(hub, from, plan, fetch, exec),
        Err(why) => {
            log(&format!("refused: {why}"));
            tell(&hub, &from, "refused", &why);
        }
    }
}

/// A failure message that names what it was working on: the tarball URL
/// for the network phases, the executable for the rest — unless the
/// message already carries it, as an HTTP error carries its URL (and the
/// signature's URL begins with the tarball's).
fn located(plan: &Plan, phase: Phase, msg: &str) -> String {
    let subject = match phase {
        Phase::Download | Phase::Verify => plan.tarball_url.clone(),
        _ => plan.exe.display().to_string(),
    };
    if msg.contains(&subject) {
        msg.to_string()
    } else {
        format!("{msg} ({subject})")
    }
}

/// The pipeline on a detached thread, reporting each phase to `from` alone.
/// Split from `start` so a test can drive it with a `Plan` pointing at a
/// tempdir and an exec that does not replace the test runner.
pub fn drive(hub: Arc<Mutex<Hub>>, from: ConnId, plan: Plan, fetch: FetchBytes, exec_fn: ExecFn) {
    // Before the thread, deliberately: a guard taken inside it would let
    // two clicks in the same instant both start.
    if IN_FLIGHT.swap(true, Ordering::SeqCst) {
        log("refused: already updating");
        tell(&hub, &from, "refused", "already updating");
        return;
    }
    let flight = Flight;
    *LAST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    log(&format!("starting: {} -> {}", plan.tarball_url, plan.exe.display()));
    let want = plan.want.clone();
    let (hub2, from2) = (hub.clone(), from.clone());
    let spawned = std::thread::Builder::new().name("roost-update".into()).spawn(move || {
        let _flight = flight;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_pipeline(&plan, fetch, &mut |p: Phase| {
                log(p.as_str());
                tell(&hub2, &from2, p.as_str(), "");
            })
        }));
        // A second net for the reporting itself: nothing may unwind out of
        // this thread, and `_flight` releases the guard either way.
        let settled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            settle(&hub2, &from2, &plan, outcome, exec_fn)
        }));
        if settled.is_err() {
            eprintln!("roost: update: reporting the outcome panicked");
        }
    });
    if let Err(e) = spawned {
        // The closure, `flight` with it, was dropped by the failed spawn.
        let msg = format!("the update thread could not start: {e}");
        log(&format!("failed at internal: {msg}"));
        record_failure(&want, Phase::Internal, &msg);
        tell(&hub, &from, "failed", &format!("internal: {msg}"));
        crate::hub::broadcast_settings_all();
    }
}

/// What the pipeline's outcome means, told to the requester, logged, and
/// kept for About — then pushed to every hub, since About's row changed.
/// Called with no hub lock held, which `broadcast_settings_all` needs.
fn settle(
    hub: &Arc<Mutex<Hub>>,
    from: &ConnId,
    plan: &Plan,
    outcome: std::thread::Result<Result<(), (Phase, String)>>,
    exec_fn: ExecFn,
) {
    let (phase, msg) = match outcome {
        Ok(Ok(())) => {
            log(&format!("swapped {} for {}; exec", plan.exe.display(), plan.want));
            tell(hub, from, "restarting", &plan.want);
            // A moment for the frame to leave the socket before the
            // process that holds it is replaced.
            std::thread::sleep(std::time::Duration::from_millis(300));
            let err = exec_fn(&plan.exe);
            // Only reached when exec returned, which it does only on
            // failure: the new file is in place, this process is not it.
            // That is `installed`, not a failure: the file is right, and
            // About says "restart roost" rather than "update failed".
            let msg = located(plan, Phase::Exec, &err);
            log(&format!("failed at exec: {msg}"));
            record_installed(&plan.want);
            tell(hub, from, "failed", &format!("exec: {msg}"));
            crate::hub::broadcast_settings_all();
            return;
        }
        Ok(Err((phase, msg))) => (phase, msg),
        Err(_) => (Phase::Internal, "the updater panicked".to_string()),
    };
    let msg = located(plan, phase, &msg);
    log(&format!("failed at {}: {msg}", phase.as_str()));
    record_failure(&plan.want, phase, &msg);
    tell(hub, from, "failed", &format!("{}: {msg}", phase.as_str()));
    crate::hub::broadcast_settings_all();
}

#[cfg(test)]
pub fn reset_for_test() {
    IN_FLIGHT.store(false, Ordering::SeqCst);
    *LAST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *INSTALLED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real executable — a fake `roost`, standing in for a staged binary
    /// under test. Has to be a real, runnable file rather than a mock: the
    /// thing `probe` exercises is `Command::spawn` against an actual path,
    /// the same reason `launch.rs`'s `fake_shell` and `claudes.rs`'s
    /// `fake_proc` build real scripts instead of stubbing the process layer.
    fn fake_exe(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    /// Positive evidence that the file is a working roost of the version
    /// claimed: stdout exactly `roost <latest>`, exit 0, inside the bound.
    #[test]
    fn the_probe_wants_the_exact_version_line_within_the_timeout() {
        let d = tempfile::tempdir().unwrap();
        let t = std::time::Duration::from_secs(1);
        let good = fake_exe(d.path(), "good", "echo 'roost 9.9.9'");
        assert_eq!(probe(&good, "9.9.9", t), Ok(()));

        let wrong = fake_exe(d.path(), "wrong", "echo 'roost 9.9.8'");
        assert_eq!(
            probe(&wrong, "9.9.9", t).unwrap_err(),
            "the new binary reports \"roost 9.9.8\", not \"roost 9.9.9\""
        );

        let noisy = fake_exe(d.path(), "noisy", "echo 'roost 9.9.9'; echo extra");
        assert!(probe(&noisy, "9.9.9", t).is_err(), "a second line is not the exact answer");

        let failing = fake_exe(d.path(), "failing", "exit 3");
        assert!(probe(&failing, "9.9.9", t).unwrap_err().contains("exited"), "a non-zero exit is reported as such");

        // The macOS case: a binary that hangs. Timed, because a hang would
        // otherwise pass by never returning.
        let hang = fake_exe(d.path(), "hang", "sleep 30");
        let started = std::time::Instant::now();
        let e = probe(&hang, "9.9.9", t).unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(4), "the probe returned in {:?}", started.elapsed());
        assert_eq!(e, "the new binary did not answer --version within 1s");

        let missing = d.path().join("missing");
        assert!(probe(&missing, "9.9.9", t).unwrap_err().starts_with("the new binary could not be started"));
    }

    /// Positive evidence for the timeout branch's "reaped, not leaked" claim:
    /// the process itself — not just `probe`'s return value — is gone from
    /// `/proc` immediately after `probe` returns, i.e. `kill` was followed by
    /// a `wait` that actually collected it rather than leaving a zombie.
    ///
    /// Revert-checked: with the `child.wait()` call after `child.kill()`
    /// commented out, this test fails — `/proc/<pid>` (now a zombie entry)
    /// still exists — while `the_probe_wants_the_exact_version_line_within_the_timeout`'s
    /// hang row keeps passing regardless, since it only ever checks `probe`'s
    /// return value and timing, not the process table. That is the gap this
    /// test closes.
    #[test]
    fn a_timed_out_child_is_actually_reaped_not_left_as_a_zombie() {
        let d = tempfile::tempdir().unwrap();
        let pidfile = d.path().join("hang.pid");
        let hang = fake_exe(d.path(), "hang", &format!("echo $$ > '{}'\nsleep 30\n", pidfile.display()));
        let t = std::time::Duration::from_millis(300);

        let started = std::time::Instant::now();
        let e = probe(&hang, "9.9.9", t).unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(4), "the probe returned in {:?}", started.elapsed());
        assert_eq!(e, "the new binary did not answer --version within 0s");

        let pid: u32 = std::fs::read_to_string(&pidfile)
            .expect("the fake exe writes its own pid before sleeping")
            .trim()
            .parse()
            .unwrap();
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "pid {pid} must be gone from /proc once probe returns: kill without a \
             successful wait leaves a zombie behind, which /proc still lists"
        );
    }

    /// A child can print the right line and exit on time while leaving a
    /// *descendant* holding the stdout pipe open — this fakes that by
    /// backgrounding a long sleep that inherits the same stdout fd before the
    /// fake exe itself exits. `read_to_string` only returns on EOF, so that
    /// descendant, which `probe` never sees and so never kills, would keep
    /// the pipe open for the full 30s. The assertion is purely about time:
    /// `probe` must come back within its own bound regardless, which is what
    /// distinguishes a bounded `recv_timeout` from a bare `.join()` on the
    /// reader thread — the latter would hang here for ~30s despite the
    /// child having exited successfully with the right output already
    /// flushed to the pipe.
    ///
    /// Revert-checked: replacing the `mpsc`/`recv_timeout` read with a plain
    /// `thread::spawn(...).join()` (the shape this function's own doc comment
    /// says *not* to use) makes this test hang for ~30s instead of returning
    /// within the bound — confirmed by running it with that change in place
    /// and a 35s wall-clock timeout, then reverting from the `/tmp` backup.
    #[test]
    fn a_grandchild_holding_stdout_open_does_not_hang_the_probe() {
        let d = tempfile::tempdir().unwrap();
        let leaky = fake_exe(d.path(), "leaky", "echo 'roost 9.9.9'\nsleep 30 &\n");
        let t = std::time::Duration::from_secs(1);

        let started = std::time::Instant::now();
        let result = probe(&leaky, "9.9.9", t);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(4),
            "the probe returned in {:?}, must not wait out the lingering grandchild",
            started.elapsed()
        );
        // No positive evidence arrived within the bound (the pipe never saw
        // EOF), so this must fail closed rather than guess at success.
        assert!(result.is_err(), "{result:?}");
    }

    /// The two URLs, from the three facts the binary already carries. The
    /// tag is `v<version>`, as `gh release list` shows for every release.
    #[test]
    fn the_asset_urls_are_derived_from_repository_version_and_target() {
        let base = release_base("https://github.com/PeterKnego/roost", "0.5.3");
        assert_eq!(base, "https://github.com/PeterKnego/roost/releases/download/v0.5.3");
        let (tarball, sig) = asset_urls(&base, "x86_64-unknown-linux-musl");
        assert_eq!(tarball, "https://github.com/PeterKnego/roost/releases/download/v0.5.3/roost-x86_64-unknown-linux-musl.tar.xz");
        assert_eq!(sig, format!("{tarball}.minisig"));
        // A trailing slash on the repository must not double up.
        assert_eq!(release_base("https://github.com/PeterKnego/roost/", "0.5.3"), base);
    }

    /// The test hook, and its limit: it replaces the base for this process
    /// and nothing else. The signature is the boundary, not the URL.
    #[test]
    fn the_base_can_be_overridden_by_env_for_a_manual_run() {
        let _g = crate::config::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROOST_UPDATE_BASE", "http://127.0.0.1:8999/");
        assert_eq!(download_base("https://github.com/PeterKnego/roost", "9.9.9"), "http://127.0.0.1:8999");
        std::env::remove_var("ROOST_UPDATE_BASE");
        assert_eq!(download_base("https://github.com/PeterKnego/roost", "9.9.9"),
            "https://github.com/PeterKnego/roost/releases/download/v9.9.9");
    }

    /// A `.pub` file is an untrusted-comment line and a base64 line; only the
    /// second is the key. `build.rs` bakes that line, so a comment change on
    /// the file must not change the binary's trust.
    #[test]
    fn the_pub_file_s_second_line_is_the_key() {
        let text = "untrusted comment: minisign public key 1234ABCD\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\n";
        assert_eq!(pubkey_line(text).as_deref(), Some("RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"));
        assert_eq!(pubkey_line("untrusted comment: nothing else\n"), None, "a comment alone is no key");
        assert_eq!(pubkey_line(""), None);
        assert_eq!(pubkey_line("RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"), Some("RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3".into()),
            "a bare base64 line is accepted too");
        assert_eq!(pubkey_line("not base64 at all !!\n"), None);
    }

    /// Whatever `build.rs` baked either decodes as a minisign key or is
    /// absent. Absent is what this checkout has until #87 commits the key,
    /// and absent must never read as "trust anything".
    #[test]
    fn the_compiled_in_key_is_absent_or_a_real_key() {
        match public_key() {
            None => {}
            Some(k) => {
                minisign_verify::PublicKey::from_base64(k).expect("keys/roost.pub holds a minisign key");
            }
        }
    }

    fn test_key() -> (String, minisign::SecretKey) {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        (kp.pk.to_base64(), kp.sk)
    }

    fn sign_bytes(sk: &minisign::SecretKey, data: &[u8]) -> String {
        minisign::sign(None, sk, data, Some("roost test signature"), None).unwrap().into_string()
    }

    /// Revert-checked: a `verify` that decodes both inputs and returns `Ok`
    /// without calling `pk.verify` fails here at the flipped-byte row with
    /// `called Result::unwrap_err() on an Ok value`.
    ///
    /// A valid signature passes; a flipped byte, another key, and garbage
    /// each fail with their own message — "signature did not verify" is the
    /// one someone should hear about, and it must not be confused with a
    /// signature file that could not be read.
    #[test]
    fn the_signature_is_checked_over_the_tarball_bytes() {
        let (pk, sk) = test_key();
        let data = b"not really a tarball, but the bytes are what is signed".to_vec();
        let sig = sign_bytes(&sk, &data);
        assert_eq!(verify(&data, &sig, &pk), Ok(()));

        let mut flipped = data.clone();
        flipped[7] ^= 0x01;
        assert_eq!(verify(&flipped, &sig, &pk).unwrap_err(), "signature did not verify");

        let (other_pk, _) = test_key();
        assert_eq!(verify(&data, &sig, &other_pk).unwrap_err(), "signature did not verify");

        let e = verify(&data, "this is not a minisig file", &pk).unwrap_err();
        assert!(e.starts_with("the signature file could not be read"), "{e}");

        let e = verify(&data, &sig, "not a key").unwrap_err();
        assert!(e.starts_with("the compiled-in key could not be read"), "{e}");
    }

    /// Three more refusals the row above doesn't exercise: an empty key (not
    /// merely a wrong one), a `.minisig` truncated mid-file rather than pure
    /// garbage, and a trusted-comment tamper. The last matters because
    /// minisign's global signature covers `signature || trusted_comment`
    /// specifically so the comment can't be forged independently of the
    /// payload signature it describes — flip a byte there with the data and
    /// data-signature untouched, and `verify` must still refuse it.
    #[test]
    fn empty_key_truncated_signature_and_a_tampered_trusted_comment_are_all_refused() {
        let (pk, sk) = test_key();
        let data = b"not really a tarball, but the bytes are what is signed".to_vec();
        let sig = sign_bytes(&sk, &data);

        let e = verify(&data, &sig, "").unwrap_err();
        assert!(e.starts_with("the compiled-in key could not be read"), "{e}");

        let e = verify(&data, &sig[..sig.len() / 2], &pk).unwrap_err();
        assert!(e.starts_with("the signature file could not be read"), "{e}");

        let mut lines: Vec<String> = sig.lines().map(str::to_string).collect();
        assert!(lines[2].starts_with("trusted comment: "), "line 2 was: {}", lines[2]);
        lines[2].push_str("-tampered");
        let tampered = lines.join("\n");
        assert_eq!(verify(&data, &tampered, &pk).unwrap_err(), "signature did not verify");
    }

    enum Member<'a> {
        File(&'a str, &'a [u8]),
        Dir(&'a str),
        Link(&'a str, &'a str),
    }

    /// An archive the way dist writes one, built in memory: a top directory
    /// named for the target, files under it. `xz_compress` produces a
    /// stream `xz_decompress` reads; the release's is `xz -9`, same format.
    fn tar_xz(members: &[Member]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for m in members {
            let mut h = tar::Header::new_gnu();
            match m {
                Member::File(path, data) => {
                    h.set_mode(0o755);
                    h.set_size(data.len() as u64);
                    b.append_data(&mut h, path, *data).unwrap();
                }
                Member::Dir(path) => {
                    h.set_entry_type(tar::EntryType::Directory);
                    h.set_mode(0o755);
                    h.set_size(0);
                    b.append_data(&mut h, path, &b""[..]).unwrap();
                }
                Member::Link(path, target) => {
                    h.set_entry_type(tar::EntryType::Symlink);
                    h.set_size(0);
                    b.append_link(&mut h, path, target).unwrap();
                }
            }
        }
        let tar = b.into_inner().unwrap();
        let mut out = Vec::new();
        lzma_rs::xz_compress(&mut &tar[..], &mut out).unwrap();
        out
    }

    const T: &str = "roost-x86_64-unknown-linux-musl";

    /// The layout `tar -tJf` printed for v0.5.2 on 2026-09-14: a directory
    /// named for the target, and under it README.md, roost, and two
    /// licences. The member is `<dir>/roost`, not `roost`.
    #[test]
    fn the_release_layout_yields_the_one_binary() {
        let a = tar_xz(&[
            Member::Dir(&format!("{T}/")),
            Member::File(&format!("{T}/README.md"), b"# roost"),
            Member::File(&format!("{T}/roost"), b"\x7fELF fake"),
            Member::File(&format!("{T}/LICENSE-MIT"), b"MIT"),
            Member::File(&format!("{T}/LICENSE-APACHE"), b"Apache"),
        ]);
        assert_eq!(extract_binary(&a).unwrap(), b"\x7fELF fake");
    }

    #[test]
    fn every_other_shape_is_refused_by_name() {
        let none = tar_xz(&[Member::File(&format!("{T}/README.md"), b"x")]);
        assert_eq!(extract_binary(&none).unwrap_err(), "the archive has no roost member");

        let two = tar_xz(&[Member::File(&format!("{T}/roost"), b"a"), Member::File("other/roost", b"b")]);
        assert_eq!(extract_binary(&two).unwrap_err(), "the archive has 2 roost members");

        let link = tar_xz(&[Member::Link(&format!("{T}/roost"), "/bin/sh")]);
        assert_eq!(extract_binary(&link).unwrap_err(), "the roost member is a symlink");

        let dir = tar_xz(&[Member::Dir(&format!("{T}/roost/"))]);
        assert_eq!(extract_binary(&dir).unwrap_err(), "the roost member is a directory");

        let e = extract_binary(b"definitely not xz").unwrap_err();
        assert!(e.starts_with("the archive could not be decompressed"), "{e}");
    }

    /// Tests `Capped`'s running counter in isolation, calling `write`
    /// directly rather than through `xz_decompress` — it does not, and
    /// cannot, exercise the single-block gap `Capped`'s doc comment
    /// describes: `lzma_rs` 0.3.0 hands a whole decoded block to `write` in
    /// one call, so this test's two separate `write` calls model
    /// cross-block accumulation only, never a single oversized write.
    /// Revert-checked: deleting the cap check turns `write` into a plain
    /// append and the second `unwrap_err()` below fails with
    /// `Ok(4)` instead of a size-cap error.
    #[test]
    fn the_capped_writer_counts_across_writes() {
        let mut buf = Vec::new();
        let mut w = Capped { buf: &mut buf, cap: 4 };
        assert_eq!(std::io::Write::write(&mut w, b"ab").unwrap(), 2);
        let e = std::io::Write::write(&mut w, b"cdef").unwrap_err();
        assert_eq!(e.to_string(), "decompressed archive exceeds the size cap");
        assert_eq!(buf, b"ab", "the over-cap write must not be partially applied");
    }

    use std::io::Read as _;
    use std::sync::Mutex as StdMutex;
    /// Shared by every pipeline test, so they are correct only one at a
    /// time: this module relies on `--test-threads=1`, as the suite does.
    static SERVED: StdMutex<Option<(Vec<u8>, String)>> = StdMutex::new(None);

    fn serve(tarball: Vec<u8>, sig: String) {
        *SERVED.lock().unwrap_or_else(|e| e.into_inner()) = Some((tarball, sig));
    }

    /// Answers the two URLs the plan names from `SERVED`, anything else 404.
    fn fetch_served(url: &str) -> Result<Vec<u8>, String> {
        let g = SERVED.lock().unwrap_or_else(|e| e.into_inner());
        let (t, s) = g.as_ref().ok_or("nothing served")?;
        if url.ends_with(".tar.xz") {
            Ok(t.clone())
        } else if url.ends_with(".minisig") {
            Ok(s.clone().into_bytes())
        } else {
            Err(format!("{url}: status code 404"))
        }
    }

    fn fetch_404(url: &str) -> Result<Vec<u8>, String> {
        Err(format!("{url}: status code 404"))
    }

    /// A "release": a fake roost that answers `--version` with `want`,
    /// packed the way dist packs one, signed with `sk`.
    fn release(sk: &minisign::SecretKey, want: &str) -> (Vec<u8>, String) {
        let script = format!("#!/bin/sh\necho 'roost {want}'\n");
        let a = tar_xz(&[Member::Dir(&format!("{T}/")), Member::File(&format!("{T}/roost"), script.as_bytes())]);
        let sig = sign_bytes(sk, &a);
        (a, sig)
    }

    fn plan_in(d: &Path, pk: &str, want: &str) -> Plan {
        let exe = fake_exe(d, "roost", "echo 'roost 0.0.1'");
        Plan {
            exe,
            tarball_url: format!("http://test/roost-{T}.tar.xz"),
            sig_url: format!("http://test/roost-{T}.tar.xz.minisig"),
            pubkey: pk.to_string(),
            want: want.to_string(),
            probe_timeout: std::time::Duration::from_secs(2),
        }
    }

    fn leftovers(d: &Path) -> Vec<String> {
        std::fs::read_dir(d)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".roost-update."))
            .collect()
    }

    /// The whole pipeline, start to swapped file, with every phase reported
    /// in order and the old file gone. Deleting the `swap` call leaves the
    /// old bytes in place and fails `assert_ne!`; deleting any `report`
    /// call fails the phase list.
    #[test]
    fn a_signed_release_replaces_the_executable_in_place() {
        let d = tempfile::tempdir().unwrap();
        let (pk, sk) = test_key();
        let (a, sig) = release(&sk, "9.9.9");
        serve(a, sig);
        let plan = plan_in(d.path(), &pk, "9.9.9");
        let before = std::fs::read(&plan.exe).unwrap();
        let mut phases = Vec::new();
        run_pipeline(&plan, fetch_served, &mut |p| phases.push(p)).unwrap();
        assert_eq!(phases, [Phase::Download, Phase::Verify, Phase::Unpack, Phase::Probe, Phase::Swap]);
        let after = std::fs::read(&plan.exe).unwrap();
        assert_ne!(after, before);
        assert!(String::from_utf8_lossy(&after).contains("roost 9.9.9"), "the swapped file is the new one");
        assert_eq!(leftovers(d.path()), Vec::<String>::new(), "no staged file remains");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&plan.exe).unwrap().permissions().mode() & 0o777, 0o755);
        }
    }

    /// Revert-checked: dropping the `unstage` after a failed probe fails
    /// here at "wrong version: a staged file remains" with
    /// `left: [".roost-update.<pid>"], right: []`.
    ///
    /// Every failure before the rename leaves the executable byte-identical
    /// and the staged file gone. Each row names the phase it fails in, which
    /// is what the dialog shows.
    #[test]
    fn every_failure_leaves_the_executable_untouched_and_nothing_staged() {
        let d = tempfile::tempdir().unwrap();
        let (pk, sk) = test_key();
        let plan = plan_in(d.path(), &pk, "9.9.9");
        let before = std::fs::read(&plan.exe).unwrap();
        let check = |why: &str| {
            assert_eq!(std::fs::read(&plan.exe).unwrap(), before, "{why}: the executable changed");
            assert_eq!(leftovers(d.path()), Vec::<String>::new(), "{why}: a staged file remains");
        };

        let (phase, msg) = run_pipeline(&plan, fetch_404, &mut |_| {}).unwrap_err();
        assert_eq!(phase, Phase::Download);
        assert!(msg.contains("404"), "{msg}");
        check("download failed");

        let (a, _) = release(&sk, "9.9.9");
        let (_, other_sk) = test_key();
        serve(a.clone(), sign_bytes(&other_sk, &a));
        let (phase, msg) = run_pipeline(&plan, fetch_served, &mut |_| {}).unwrap_err();
        assert_eq!((phase, msg.as_str()), (Phase::Verify, "signature did not verify"));
        check("signature");

        let empty = tar_xz(&[Member::File(&format!("{T}/README.md"), b"x")]);
        serve(empty.clone(), sign_bytes(&sk, &empty));
        let (phase, msg) = run_pipeline(&plan, fetch_served, &mut |_| {}).unwrap_err();
        assert_eq!((phase, msg.as_str()), (Phase::Unpack, "the archive has no roost member"));
        check("no member");

        let (a, sig) = release(&sk, "9.9.8");
        serve(a, sig);
        let (phase, msg) = run_pipeline(&plan, fetch_served, &mut |_| {}).unwrap_err();
        assert_eq!(phase, Phase::Probe);
        assert!(msg.contains("9.9.8"), "{msg}");
        check("wrong version");

        // `exec` so the probe's kill lands on the sleeper itself rather than
        // on a shell that would leave a `sleep 30` orphaned behind the test.
        let hang_script = "#!/bin/sh\nexec sleep 30\n";
        let a = tar_xz(&[Member::File(&format!("{T}/roost"), hang_script.as_bytes())]);
        serve(a.clone(), sign_bytes(&sk, &a));
        let (phase, _) = run_pipeline(&plan, fetch_served, &mut |_| {}).unwrap_err();
        assert_eq!(phase, Phase::Probe);
        check("hang");
    }

    /// Revert-checked: moving unpack ahead of verify fails here with
    /// `left: (Unpack, "the archive could not be decompressed: XzError(...)")`.
    ///
    /// A panic between staging and the rename — here in `report`, which in
    /// the server takes the hub lock — still removes the staged file, or the
    /// pid-fixed name would refuse every later update in the process.
    /// Revert-checked: a `Drop` that disarms without removing fails here at
    /// "a panic left the staged file behind" with
    /// `left: [".roost-update.<pid>"], right: []`.
    #[test]
    fn a_panic_after_staging_still_removes_the_staged_file() {
        let d = tempfile::tempdir().unwrap();
        let (pk, sk) = test_key();
        let (a, sig) = release(&sk, "9.9.9");
        serve(a, sig);
        let plan = plan_in(d.path(), &pk, "9.9.9");
        let before = std::fs::read(&plan.exe).unwrap();
        let mut staged_seen = false;
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_pipeline(&plan, fetch_served, &mut |p| {
                if p == Phase::Probe {
                    staged_seen = !leftovers(d.path()).is_empty();
                    panic!("report panicked on probe");
                }
            })
        }));
        assert!(r.is_err(), "the panic propagated");
        assert!(staged_seen, "the file was staged before the panic, so its absence below means removal");
        assert_eq!(leftovers(d.path()), Vec::<String>::new(), "a panic left the staged file behind");
        assert_eq!(std::fs::read(&plan.exe).unwrap(), before);
    }

    /// The rename itself failing — the executable's path is a non-empty
    /// directory, which `rename` refuses — reports `Swap` and still removes
    /// the staged file. Revert-checked: disarming the guard on that branch
    /// and returning the bare error fails the leftovers assertion with
    /// `left: [".roost-update.<pid>"], right: []`.
    #[test]
    fn a_refused_rename_reports_swap_and_removes_the_staged_file() {
        let d = tempfile::tempdir().unwrap();
        let (pk, sk) = test_key();
        let (a, sig) = release(&sk, "9.9.9");
        serve(a, sig);
        let mut plan = plan_in(d.path(), &pk, "9.9.9");
        plan.exe = d.path().join("in-the-way");
        std::fs::create_dir(&plan.exe).unwrap();
        std::fs::write(plan.exe.join("keep"), b"k").unwrap();
        let (phase, msg) = run_pipeline(&plan, fetch_served, &mut |_| {}).unwrap_err();
        assert_eq!(phase, Phase::Swap, "{msg}");
        assert!(msg.starts_with("rename refused"), "{msg}");
        assert_eq!(leftovers(d.path()), Vec::<String>::new());
        assert_eq!(std::fs::read(plan.exe.join("keep")).unwrap(), b"k");
    }

    /// Order: the signature is checked over the raw downloaded bytes, and a
    /// tarball that fails it never reaches the decompressor. The fixture is
    /// bytes `extract_binary` refuses loudly ("could not be decompressed"),
    /// signed by the wrong key — so the pipeline running unpack first would
    /// report `Unpack`, and running it at all would show in `phases`.
    #[test]
    fn a_bad_signature_is_refused_before_decompression_sees_the_bytes() {
        let d = tempfile::tempdir().unwrap();
        let (pk, _) = test_key();
        let (_, other_sk) = test_key();
        let garbage = b"definitely not xz".to_vec();
        assert!(extract_binary(&garbage).unwrap_err().starts_with("the archive could not be decompressed"));
        serve(garbage.clone(), sign_bytes(&other_sk, &garbage));
        let plan = plan_in(d.path(), &pk, "9.9.9");
        let before = std::fs::read(&plan.exe).unwrap();
        let mut phases = Vec::new();
        let err = run_pipeline(&plan, fetch_served, &mut |p| phases.push(p)).unwrap_err();
        assert_eq!(err, (Phase::Verify, "signature did not verify".to_string()));
        assert_eq!(phases, [Phase::Download, Phase::Verify], "unpack must not start");
        assert_eq!(std::fs::read(&plan.exe).unwrap(), before);
        assert_eq!(leftovers(d.path()), Vec::<String>::new());
    }

    /// The staged file lands in the directory it is given — the pipeline
    /// gives it the executable's, which is what keeps the rename on one
    /// filesystem — under the pid-unique name, executable, holding exactly
    /// the bytes. `stage` creates it 0600 and marks it 0755 only once it is
    /// written, so dropping the `set_permissions` fails the mode assertion
    /// whatever the umask is. Revert-checked: leaving the file at its
    /// creation mode fails here with `left: 384` (0o600).
    #[test]
    fn the_staged_file_is_beside_the_executable_pid_named_and_executable() {
        let d = tempfile::tempdir().unwrap();
        let p = stage(d.path(), b"new bytes").unwrap();
        assert_eq!(p, d.path().join(format!(".roost-update.{}", std::process::id())));
        assert_eq!(std::fs::read(&p).unwrap(), b"new bytes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o755);
        }
    }

    /// Something already at the staged name is not ours, so it is neither
    /// written through nor removed. A symlink planted there pointing at
    /// another file is the case that matters: `fs::write` would follow it
    /// and overwrite the target. Revert-checked: `create(true).truncate(true)`
    /// in place of `create_new` writes through the link, `stage` returns `Ok`
    /// and the `unwrap_err()` panics; adding a cleanup that removes
    /// the path on any error fails the "still there" assertion.
    #[cfg(unix)]
    #[test]
    fn a_file_already_at_the_staged_name_is_neither_followed_nor_removed() {
        let d = tempfile::tempdir().unwrap();
        let victim = d.path().join("victim");
        std::fs::write(&victim, b"precious").unwrap();
        let name = d.path().join(format!(".roost-update.{}", std::process::id()));
        std::os::unix::fs::symlink(&victim, &name).unwrap();
        let e = stage(d.path(), b"new bytes").unwrap_err();
        assert!(e.contains("could not create"), "{e}");
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
        assert!(std::fs::symlink_metadata(&name).unwrap().file_type().is_symlink(), "not ours, so still there");
    }

    /// `swap` is a rename: the executable's path names a new inode, and a
    /// handle open on the old one — the running process, in production —
    /// still reads the old bytes. A copy-over-the-top would rewrite the
    /// same inode in place, so the open handle would read the new bytes.
    /// Revert-checked: `fs::copy` + `remove_file` in place of the rename
    /// fails at "the old inode was rewritten, not replaced", the handle
    /// reading `b"new binary!"`.
    #[test]
    fn swap_replaces_the_executable_by_rename_not_by_rewriting_it() {
        let d = tempfile::tempdir().unwrap();
        let exe = d.path().join("roost");
        std::fs::write(&exe, b"old binary").unwrap();
        let mut running = std::fs::File::open(&exe).unwrap();
        let staged = stage(d.path(), b"new binary!").unwrap();
        swap(&staged, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new binary!");
        let mut old = Vec::new();
        running.read_to_end(&mut old).unwrap();
        assert_eq!(old, b"old binary", "the old inode was rewritten, not replaced");
        assert!(matches!(std::fs::symlink_metadata(&staged), Err(e) if e.kind() == std::io::ErrorKind::NotFound));

        // A rename that cannot happen leaves the target alone.
        let e = swap(&d.path().join(".roost-update.missing"), &exe).unwrap_err();
        assert!(e.starts_with("rename refused"), "{e}");
        assert_eq!(std::fs::read(&exe).unwrap(), b"new binary!");
    }

    /// The cap, enforced on the body as well as the header — a server that
    /// lies about `Content-Length` is a server that sends more than it said.
    #[test]
    fn a_download_past_the_cap_is_refused_while_reading() {
        let big = std::io::repeat(b'x').take(100);
        assert_eq!(read_capped(big, 99).unwrap_err(), "the download exceeded the 99 byte cap");
        let ok = std::io::repeat(b'x').take(99);
        assert_eq!(read_capped(ok, 99).unwrap().len(), 99);
    }

    /// A one-shot HTTP server on loopback answering every request with
    /// `head` then `body`, then closing.
    fn one_shot(head: String, body: Vec<u8>) -> String {
        use std::io::Write;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                let mut req = [0u8; 4096];
                let _ = s.read(&mut req);
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(&body);
            }
        });
        format!("http://{addr}/roost.tar.xz")
    }

    /// Through a real socket and the real agent: an announced length above
    /// the cap is refused from the header, before any body is read — the
    /// body here is short and the connection closes, so without the header
    /// check the read would succeed; and a server that announces nothing
    /// and sends too much is stopped by the read-side cap. Revert-checked:
    /// disabling the header check fails the first assertion with "the
    /// download stopped: response body closed before all bytes were read"
    /// — the body was read, which is what the header check exists to
    /// prevent. Replacing `read_capped` with a plain `read_to_end` fails
    /// the second assertion with `Ok`.
    #[test]
    fn the_cap_is_checked_against_content_length_and_again_while_reading() {
        let url = one_shot("HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n".into(), b"tiny".to_vec());
        assert_eq!(
            http_get_capped(&url, 100).unwrap_err(),
            "the server announced 1000 bytes, above the 100 byte cap"
        );

        let url = one_shot("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".into(), vec![b'x'; 1000]);
        assert_eq!(http_get_capped(&url, 100).unwrap_err(), "the download exceeded the 100 byte cap");

        let url = one_shot("HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n".into(), b"tiny".to_vec());
        assert_eq!(http_get_capped(&url, 100).unwrap(), b"tiny");

        let url = one_shot("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(), vec![]);
        let e = http_get_capped(&url, 100).unwrap_err();
        assert!(e.contains("404"), "{e}");
    }

    #[test]
    fn every_phase_has_the_name_the_wire_uses() {
        let all = [Phase::Download, Phase::Verify, Phase::Unpack, Phase::Probe, Phase::Swap, Phase::Exec, Phase::Internal];
        let names: Vec<&str> = all.iter().map(|p| p.as_str()).collect();
        assert_eq!(names, ["download", "verify", "unpack", "probe", "swap", "exec", "internal"]);
    }

    fn ch(skipped: Option<&str>, deferred_until: Option<u64>) -> Choices {
        Choices { skipped: skipped.map(str::to_string), deferred_until }
    }

    #[test]
    fn the_choices_file_round_trips_beside_the_check_file_not_in_it() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("update").join("choices.json");
        let c = ch(Some("0.5.3"), Some(1_789_320_967));
        write_choices_to(&p, &c).unwrap();
        assert_eq!(read_choices_from(&p), c);
        let text = std::fs::read_to_string(&p).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let keys: Vec<&str> = json.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["skipped", "deferred_until"], "{text}");
        // Its own file: the check thread writes check.json, the intent
        // handler writes this, and one file with two writers is a rename
        // race in which a completed check silently discards a click.
        //
        // `choices_path()` reads the process-global `ROOST_STATE_DIR` (via
        // `version::state_dir_for_update`) even though this test never sets
        // it itself — a concurrently-running test's `set_var` is still a
        // data race on that read without the lock, per `STATE_ENV_LOCK`'s
        // doc comment.
        let _envg = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(choices_path().file_name().unwrap(), "choices.json");
        assert_eq!(choices_path().parent().unwrap().file_name().unwrap(), "update");
    }

    /// Missing, truncated or the wrong shape is *no choice*, deliberately —
    /// that errs toward showing the dialog, the recoverable direction.
    #[test]
    fn an_unreadable_choices_file_is_no_choice() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("choices.json");
        assert_eq!(read_choices_from(&p), Choices::default());
        std::fs::write(&p, "{\"skipped\":\"0.5").unwrap();
        assert_eq!(read_choices_from(&p), Choices::default());
        std::fs::write(&p, "[]").unwrap();
        assert_eq!(read_choices_from(&p), Choices::default());
    }

    #[test]
    fn a_skip_is_per_version_and_a_newer_one_unskips_by_itself() {
        let c = ch(Some("0.5.3"), None);
        assert!(skipped(&c, "0.5.3"));
        assert!(!skipped(&c, "0.5.4"), "the stored value is the version, not a flag");
        assert!(!skipped(&Choices::default(), "0.5.3"));
    }

    #[test]
    fn a_deferral_expires_at_its_timestamp() {
        let now = 1_000_000u64;
        assert!(deferred(&ch(None, Some(now + 3600)), now), "an hour left");
        assert!(!deferred(&ch(None, Some(now)), now), "expired exactly now");
        assert!(!deferred(&ch(None, Some(now - 1)), now));
        assert!(!deferred(&Choices::default(), now));
    }

    /// A stepped clock, or a file from another host: a deferral further
    /// ahead than `Later` can ever write reads as expired, not as a mark that
    /// stays silent for a year.
    #[test]
    fn a_deferral_too_far_ahead_reads_as_expired() {
        let now = 1_000_000u64;
        assert!(deferred(&ch(None, Some(now + DEFER_SECS)), now), "exactly what Later writes");
        assert!(!deferred(&ch(None, Some(now + DEFER_SECS + 1)), now), "more than Later can write");
    }

    #[test]
    fn later_writes_now_plus_a_day_and_keeps_the_skip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("choices.json");
        write_choices_to(&p, &ch(Some("0.5.2"), None)).unwrap();
        defer_in(&p, 1_000_000).unwrap();
        assert_eq!(read_choices_from(&p), ch(Some("0.5.2"), Some(1_000_000 + DEFER_SECS)));
    }

    /// `SkipUpdate` carries the version so a click from a stale page cannot
    /// skip a version it never saw.
    #[test]
    fn a_skip_from_a_stale_page_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("choices.json");
        write_choices_to(&p, &ch(None, Some(5))).unwrap();
        let e = skip_in(&p, Some("0.5.4"), "0.5.3").unwrap_err();
        assert_eq!(e, "0.5.3 is not the version the last check saw (0.5.4)");
        assert_eq!(read_choices_from(&p), ch(None, Some(5)), "nothing written");
        assert!(skip_in(&p, None, "0.5.3").is_err(), "no check yet, nothing to skip");

        skip_in(&p, Some("0.5.3"), "0.5.3").unwrap();
        assert_eq!(read_choices_from(&p), ch(Some("0.5.3"), None), "a skip clears the deferral");
    }

    /// Points both process-global env vars at this test's own fixture.
    /// `STATE_ENV_LOCK` first, then `config::ENV_LOCK`, the order documented
    /// on both; an inversion deadlocks rather than fails.
    ///
    /// The returned `EnvFixture` unsets both vars on drop, before its locks
    /// are released — the fields drop in declaration order after `drop` —
    /// so no later test inherits a path into a deleted tempdir.
    fn env_fixture() -> EnvFixture {
        let g1 = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let g2 = crate::config::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path());
        let cfg = d.path().join("config.toml");
        std::fs::write(&cfg, "version_check = true\n").unwrap();
        std::env::set_var("ROOST_CONFIG", &cfg);
        crate::version::reset_for_test();
        reset_for_test();
        EnvFixture { _dir: d, _g2: g2, _g1: g1 }
    }

    struct EnvFixture {
        _dir: tempfile::TempDir,
        _g2: std::sync::MutexGuard<'static, ()>,
        _g1: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for EnvFixture {
        fn drop(&mut self) {
            std::env::remove_var("ROOST_STATE_DIR");
            std::env::remove_var("ROOST_CONFIG");
            crate::version::reset_for_test();
            reset_for_test();
        }
    }

    fn checked(latest: &str) -> crate::version::State {
        crate::version::State {
            latest: Some(latest.to_string()),
            checked_at: Some(crate::errlog::now_secs()),
            failed_at: None,
        }
    }

    /// The button's condition, from the same four strings `upgradesLabel`
    /// reads, so the server and the label cannot disagree.
    ///
    /// Deleting any one of `replaceable_by`'s three refusals turns the
    /// matching `unwrap_err` rows into a panic on `Ok(())`; the two `Ok` rows
    /// fail if the owner check is widened to refuse `cargo-bin` or `other`.
    #[test]
    fn replaceable_means_release_not_package_managed_and_writable() {
        let b = |channel: &str, owner: &str, replaceable: &str| crate::proto::BuildInfo {
            channel: channel.into(), owner: owner.into(), replaceable: replaceable.into(),
            ..Default::default()
        };
        assert_eq!(replaceable_by(&b("release", "cargo-bin", "yes")), Ok(()), "the shell installer");
        assert_eq!(replaceable_by(&b("release", "other", "yes")), Ok(()), "the tarball");
        assert_eq!(replaceable_by(&b("release", "homebrew", "yes")).unwrap_err(), "owned by homebrew");
        assert_eq!(replaceable_by(&b("release", "system-package", "yes")).unwrap_err(), "owned by system-package");
        assert_eq!(replaceable_by(&b("release", "other", "no")).unwrap_err(), "not writable by roost");
        assert_eq!(replaceable_by(&b("release", "other", "unknown")).unwrap_err(), "not writable by roost");
        assert_eq!(replaceable_by(&b("checkout", "other", "yes")).unwrap_err(), "channel checkout");
        assert_eq!(replaceable_by(&b("cargo", "cargo-bin", "yes")).unwrap_err(), "channel cargo");
    }

    /// The key clause, which `view()` cannot exercise in a checkout (no key,
    /// and not channel `release` either). Deleting `&& key.is_some()` fails
    /// the "no key, no button" row.
    #[test]
    fn the_button_needs_a_replaceable_copy_and_a_key() {
        let b = |owner: &str, replaceable: &str| crate::proto::BuildInfo {
            channel: "release".into(), owner: owner.into(), replaceable: replaceable.into(),
            ..Default::default()
        };
        assert!(!offer_for(&b("cargo-bin", "yes"), None), "no key, no button");
        assert!(offer_for(&b("cargo-bin", "yes"), Some("RWQkey")), "a replaceable release with a key");
        assert!(offer_for(&b("other", "yes"), Some("RWQkey")), "the tarball too");
        assert!(!offer_for(&b("homebrew", "yes"), Some("RWQkey")), "homebrew owns its copy");
        assert!(!offer_for(&b("system-package", "yes"), Some("RWQkey")), "so does the package manager");
        assert!(!offer_for(&b("other", "no"), Some("RWQkey")), "not writable");
        assert!(!offer_for(&b("other", "unknown"), Some("RWQkey")), "could not tell is not yes");
    }

    /// The five fields beside step 2's two, each from the place it lives:
    /// `offer` from the build and the key, `skipped`/`deferred_until` from
    /// the choices file, `failure`/`installed` from this process.
    ///
    /// Deleting the key clause cannot fail here — this checkout has no key
    /// *and* is not channel `release` — which is why `replaceable_by` and
    /// `offer_for` have their own tests above. Each of the other four
    /// assignments in `view()`, deleted, leaves its field at the default and
    /// fails its assertion; dropping the `for_version == latest` guard fails
    /// the "dropped when latest changes" assertion; dropping the `deferred()`
    /// gate fails "an expired deferral is not carried".
    #[test]
    fn the_view_carries_the_choices_and_the_last_outcome() {
        let _env = env_fixture();
        crate::version::write_state_to(&crate::version::state_path(), &checked("999.0.0")).unwrap();
        crate::version::reset_for_test();
        let v = view();
        assert_eq!((v.status.as_str(), v.latest.as_str()), ("newer", "999.0.0"), "step 2's fields are untouched");
        assert!(!v.offer, "this test binary is a checkout without a key: no button");
        assert_eq!((v.skipped.as_str(), v.deferred_until, v.failure.as_str(), v.installed.as_str()), ("", 0, "", ""));

        write_choices_to(&choices_path(), &Choices { skipped: Some("999.0.0".into()), deferred_until: None }).unwrap();
        assert_eq!(view().skipped, "999.0.0");

        let soon = crate::errlog::now_secs() + 100;
        write_choices_to(&choices_path(), &Choices { skipped: None, deferred_until: Some(soon) }).unwrap();
        assert_eq!(view().deferred_until, soon);
        write_choices_to(&choices_path(), &Choices { skipped: None, deferred_until: Some(1) }).unwrap();
        assert_eq!(view().deferred_until, 0, "an expired deferral is not carried");

        record_failure("999.0.0", Phase::Verify, "signature did not verify");
        assert_eq!(view().failure, "verify: signature did not verify");
        // A failure belongs to the version it was for. A new check that names
        // another version drops it: the row must not say "update failed" about
        // a release nobody has tried to install.
        crate::version::write_state_to(&crate::version::state_path(), &checked("999.1.0")).unwrap();
        crate::version::reset_for_test();
        assert_eq!(view().failure, "");

        record_installed("999.1.0");
        assert_eq!(view().installed, "999.1.0");
    }

    use std::sync::atomic::{AtomicBool as TestFlag, AtomicUsize, Ordering::SeqCst};
    static FETCHES: AtomicUsize = AtomicUsize::new(0);
    static RELEASE: TestFlag = TestFlag::new(false);

    fn blocking_404(url: &str) -> Result<Vec<u8>, String> {
        FETCHES.fetch_add(1, SeqCst);
        while !RELEASE.load(SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Err(format!("{url}: status code 404"))
    }

    fn panicking_fetch(_url: &str) -> Result<Vec<u8>, String> {
        panic!("the socket thread must not carry this");
    }

    fn no_exec(_exe: &Path) -> String {
        "test: exec not attempted".to_string()
    }

    fn events(rx: &std::sync::mpsc::Receiver<String>) -> Vec<String> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn wait_until(mut f: impl FnMut() -> bool) {
        for _ in 0..400 {
            if f() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("condition not reached in 4s");
    }

    /// Two subscribers, because with one `send_to` and `broadcast` are
    /// indistinguishable. This test binary is a checkout, so `start` refuses
    /// — and the refusal is the requester's alone. Replacing the `send_to`
    /// with a `broadcast` fails the second subscriber's assertion; deleting
    /// it fails `got.len() == 1`.
    #[test]
    fn a_refusal_reaches_the_requester_and_nobody_else() {
        let _env = env_fixture();
        let d = tempfile::tempdir().unwrap();
        let mut h = crate::hub::Hub::new("upd_refuse", d.path().to_path_buf());
        let (a, rx_a) = h.subscribe();
        let (_b, rx_b) = h.subscribe();
        let hub = std::sync::Arc::new(std::sync::Mutex::new(h));
        start(hub, a, fetch_404);
        let got = events(&rx_a);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains(r#""t":"UpdateProgress""#) && got[0].contains(r#""phase":"refused""#), "{}", got[0]);
        assert!(got[0].contains("channel checkout"), "the reason is named: {}", got[0]);
        assert!(events(&rx_b).is_empty(), "one connection's refusal is not everyone's");
        assert!(!IN_FLIGHT.load(SeqCst), "a refusal before `drive` never takes the guard");
    }

    /// A second click anywhere while one runs is answered *already updating*
    /// rather than starting a second download.
    #[test]
    fn two_intents_during_one_run_fetch_once_and_refuse_the_second() {
        let _env = env_fixture();
        let d = tempfile::tempdir().unwrap();
        // A check naming the version being installed, so `view()` keeps the
        // failure: it drops one that is for another version.
        crate::version::write_state_to(&crate::version::state_path(), &checked("9.9.9")).unwrap();
        crate::version::reset_for_test();
        FETCHES.store(0, SeqCst);
        RELEASE.store(false, SeqCst);
        let (pk, _) = test_key();
        let mut h = crate::hub::Hub::new("upd_flight", d.path().to_path_buf());
        let (a, rx_a) = h.subscribe();
        let (b, rx_b) = h.subscribe();
        let hub = std::sync::Arc::new(std::sync::Mutex::new(h));
        drive(hub.clone(), a, plan_in(d.path(), &pk, "9.9.9"), blocking_404, no_exec);
        wait_until(|| FETCHES.load(SeqCst) >= 1);
        drive(hub.clone(), b, plan_in(d.path(), &pk, "9.9.9"), blocking_404, no_exec);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(FETCHES.load(SeqCst), 1, "the guard is taken before the thread starts");
        let got = events(&rx_b);
        assert!(got.iter().any(|e| e.contains(r#""phase":"refused""#) && e.contains("already updating")), "{got:?}");
        RELEASE.store(true, SeqCst);
        wait_until(|| !IN_FLIGHT.load(SeqCst));
        let got = events(&rx_a);
        assert!(got.iter().any(|e| e.contains(r#""phase":"download""#)), "{got:?}");
        assert!(got.iter().any(|e| e.contains(r#""phase":"failed""#) && e.contains("404")), "{got:?}");
        assert_eq!(view().failure.split(':').next(), Some("download"), "the failure is kept for About");
        RELEASE.store(false, SeqCst);
    }

    /// CLAUDE.md: no panic may escape a socket or watcher thread — and this
    /// one would also leave the guard taken forever. Deleting the
    /// `catch_unwind` around the pipeline leaves no `failed` event (the
    /// thread dies with the panic); deleting the `Flight` guard as well
    /// leaves `IN_FLIGHT` set and `wait_until` panics after 4s.
    #[test]
    fn a_panicking_fetch_clears_the_guard_and_is_reported_as_a_failure() {
        let _env = env_fixture();
        let d = tempfile::tempdir().unwrap();
        let (pk, _) = test_key();
        let mut h = crate::hub::Hub::new("upd_panic", d.path().to_path_buf());
        let (a, rx_a) = h.subscribe();
        let hub = std::sync::Arc::new(std::sync::Mutex::new(h));
        drive(hub, a, plan_in(d.path(), &pk, "9.9.9"), panicking_fetch, no_exec);
        wait_until(|| !IN_FLIGHT.load(SeqCst));
        let got = events(&rx_a);
        assert!(got.iter().any(|e| e.contains(r#""phase":"failed""#) && e.contains("internal")), "{got:?}");
    }

    /// The swap succeeded and the exec did not: the file is the new version,
    /// the process is the old one, and About must say so rather than hide it.
    /// Deleting `record_installed` fails `installed == "9.9.9"`; routing the
    /// exec failure through `record_failure` instead fails `failure == ""`.
    #[test]
    fn a_swapped_file_whose_exec_fails_is_reported_as_installed() {
        let _env = env_fixture();
        let d = tempfile::tempdir().unwrap();
        let (pk, sk) = test_key();
        let (a, sig) = release(&sk, "9.9.9");
        serve(a, sig);
        let mut h = crate::hub::Hub::new("upd_exec", d.path().to_path_buf());
        let (id, rx) = h.subscribe();
        let hub = std::sync::Arc::new(std::sync::Mutex::new(h));
        let plan = plan_in(d.path(), &pk, "9.9.9");
        let exe = plan.exe.clone();
        drive(hub, id, plan, fetch_served, no_exec);
        wait_until(|| !IN_FLIGHT.load(SeqCst));
        assert!(String::from_utf8_lossy(&std::fs::read(&exe).unwrap()).contains("roost 9.9.9"), "the file was swapped");
        let got = events(&rx);
        assert!(got.iter().any(|e| e.contains(r#""phase":"restarting""#)), "{got:?}");
        assert!(got.iter().any(|e| e.contains(r#""phase":"failed""#) && e.contains("exec: test: exec not attempted")), "{got:?}");
        assert_eq!(view().installed, "9.9.9");
        assert_eq!(view().failure, "", "an exec failure is `installed`, not `failure`: the file is right");
    }

    /// A recorded failure names what it was working on — the URL for the
    /// network phases, the executable for the rest — without doubling a URL
    /// the HTTP error already carries. Deleting the append in `located`
    /// fails the verify and probe rows; dropping the `contains` check fails
    /// the download row with the URL twice.
    #[test]
    fn a_failure_names_the_url_or_path_it_was_working_on() {
        let d = tempfile::tempdir().unwrap();
        let (pk, _) = test_key();
        let plan = plan_in(d.path(), &pk, "9.9.9");
        let url = plan.tarball_url.clone();
        let exe = plan.exe.display().to_string();
        let dl = located(&plan, Phase::Download, &format!("{}: status code 404", plan.sig_url));
        assert_eq!(dl, format!("{}: status code 404", plan.sig_url), "the sig URL already names the tarball URL");
        let dl = located(&plan, Phase::Download, "the download exceeded the 9 byte cap");
        assert_eq!(dl, format!("the download exceeded the 9 byte cap ({url})"));
        assert_eq!(located(&plan, Phase::Verify, "signature did not verify"), format!("signature did not verify ({url})"));
        assert_eq!(located(&plan, Phase::Probe, "the new binary exited 1"), format!("the new binary exited 1 ({exe})"));
    }

    /// Revert-checked: without the one retry, this fails with
    /// "the new binary could not be started: Text file busy". The writer
    /// is released 50 ms in and the retry waits 100 ms; if the first spawn
    /// were scheduled after the release this run would pass without
    /// exercising the retry, which is why the release is not immediate.
    #[test]
    fn a_probe_that_meets_text_file_busy_retries_once() {
        let d = tempfile::tempdir().unwrap();
        let exe = fake_exe(d.path(), "busy", "echo 'roost 9.9.9'");
        let writer = std::fs::OpenOptions::new().write(true).open(&exe).unwrap();
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(writer);
        });
        let r = probe(&exe, "9.9.9", std::time::Duration::from_secs(2));
        t.join().unwrap();
        assert_eq!(r, Ok(()));
    }

    /// Only once: a file that stays busy is a probe failure, named.
    #[test]
    fn a_probe_that_stays_busy_fails_with_the_reason() {
        let d = tempfile::tempdir().unwrap();
        let exe = fake_exe(d.path(), "busy", "echo 'roost 9.9.9'");
        let _writer = std::fs::OpenOptions::new().write(true).open(&exe).unwrap();
        let e = probe(&exe, "9.9.9", std::time::Duration::from_secs(2)).unwrap_err();
        assert!(e.starts_with("the new binary could not be started:") && e.to_lowercase().contains("busy"), "{e}");
    }
}
