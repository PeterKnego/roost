//! A folder from the tree, as a zip (#162).
//!
//! Two halves, kept apart on purpose: [`plan`] walks and decides, [`write`]
//! streams. Once the first byte of a download is out its status is spent, so
//! everything that can refuse — the caps, an unreadable folder — has to be
//! known before [`write`] starts. The walk only collects paths and sizes,
//! which is the cheap half; reading and compressing is the expensive one.
//!
//! The walk is search's walk, not "everything on disk": the tree's filter,
//! plus `.git`, nested checkouts and gitignored directories. A zip of a
//! project root that shipped `.git`, `target/` and `node_modules/` would be
//! gigabytes nobody asked for. And like search, an archive that left
//! something out says so — `ROOST-SKIPPED.txt` — because a silent partial
//! archive is the same defect as an empty result list (CLAUDE.md, "a search
//! that skipped something says so").
//!
//! The format is written by hand over `flate2` and `crc32fast`, both already
//! in the dependency tree through ureq. Every entry uses a data descriptor,
//! so a size never has to be known before the bytes are, and the caps keep
//! every offset and count inside classic zip — no Zip64.
use crate::gitignore::Ignore;
use crate::projects::TreeFilter;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Files per archive. Matches search's walk budget.
pub const MAX_FILES: usize = 20_000;
/// Uncompressed bytes per archive. Also what keeps every offset under the
/// 4 GiB classic zip can address, deflate's worst-case growth included.
pub const MAX_BYTES: u64 = 1 << 30;
/// Lines per group in `ROOST-SKIPPED.txt`; the rest is a count.
const MAX_LISTED: usize = 200;
const SKIPPED_NAME: &str = "ROOST-SKIPPED.txt";

/// Why something is not in the archive. Six groups, because each has a
/// different answer to "how do I get it": a gap is not a decision, and a
/// gitignored build directory is not a submodule.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Skipped {
    pub unreadable: Vec<String>,
    pub nested: Vec<String>,
    pub ignored: Vec<String>,
    pub hidden: Vec<String>,
    pub symlinks: Vec<String>,
    /// Filled by [`write`]: files that failed or vanished after planning.
    pub changed: Vec<String>,
}

impl Skipped {
    pub fn is_empty(&self) -> bool {
        self.unreadable.is_empty()
            && self.nested.is_empty()
            && self.ignored.is_empty()
            && self.hidden.is_empty()
            && self.symlinks.is_empty()
            && self.changed.is_empty()
    }

    fn render(&self) -> String {
        let groups: [(&str, &str, &Vec<String>); 6] = [
            ("Could not be read", "These are gaps, not decisions: check their permissions.", &self.unreadable),
            ("Changed or vanished while zipping", "Each entry holds what could be read; download again for a consistent copy.", &self.changed),
            ("Nested checkouts", "A directory with its own .git is another repository; zip it on its own.", &self.nested),
            ("Ignored by git", "Turn on hidden files in roost's tree to include these.", &self.ignored),
            ("Hidden by the tree's filter", "Dotfiles, the configured `hide` list and build directories; turn on hidden files to include dotfiles.", &self.hidden),
            ("Symlinks", "Only file symlinks that stay inside the project are included; directory symlinks are never followed.", &self.symlinks),
        ];
        let mut s = String::from("roost left these out of the archive.\n");
        for (title, why, list) in groups {
            if list.is_empty() {
                continue;
            }
            s.push_str(&format!("\n{title} ({}) — {why}\n", list.len()));
            for p in list.iter().take(MAX_LISTED) {
                s.push_str(&format!("  {p}\n"));
            }
            if list.len() > MAX_LISTED {
                s.push_str(&format!("  … and {} more\n", list.len() - MAX_LISTED));
            }
        }
        s
    }
}

/// What a download of one folder will contain, decided before any byte goes
/// out.
#[derive(Debug)]
pub struct Plan {
    /// The top-level directory inside the archive, and the download's name.
    pub name: String,
    /// `(name in the archive, file on disk)`, in walk order; a directory has
    /// no file and its name ends in `/`.
    pub entries: Vec<(String, Option<PathBuf>)>,
    pub files: usize,
    pub bytes: u64,
    pub skipped: Skipped,
}

/// Walks `rel` (a directory under `root`, `""` for the root itself).
///
/// `Err` is a refusal the user is shown: the folder cannot be read, is not a
/// folder, or is over a cap. Nothing else fails the plan — an unreadable
/// subdirectory is a line in `ROOST-SKIPPED.txt`, never a reason to hand
/// back nothing.
pub fn plan(root: &Path, rel: &str, filter: &TreeFilter) -> Result<Plan, String> {
    let rel = rel.trim_matches('/');
    let dir = if rel.is_empty() {
        root.canonicalize().map_err(|e| format!("cannot read the project directory: {e}"))?
    } else {
        crate::projects::safe_resolve(root, rel)?
    };
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(format!("{rel} is not a folder")),
        Err(e) => return Err(format!("cannot read {rel}: {e}")),
    }
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "project".into());

    // Ignore rules are inherited from the project root down to the chosen
    // folder, so zipping `app/` honours the root's `.gitignore` as well as
    // `app/.gitignore`. The folder itself is never refused: choosing it is
    // the override, the way opening an ignored folder in the tree is.
    let ignoring = !filter.show_hidden;
    let mut ign = if ignoring { Ignore::for_root(root) } else { Ignore::none() };
    if ignoring && !rel.is_empty() {
        let mut acc = String::new();
        for seg in rel.split('/') {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(seg);
            ign = ign.enter(&root.join(&acc), &acc);
        }
    }

    let mut p = Plan { name: name.clone(), entries: Vec::new(), files: 0, bytes: 0, skipped: Skipped::default() };
    p.entries.push((format!("{name}/"), None));
    // An explicit stack, depth first; each pending directory carries its
    // own ignore chain, the same shape as search's walk.
    let mut stack: Vec<(PathBuf, String, Rc<Ignore>)> = vec![(dir, rel.to_string(), ign)];
    let mut first = true;
    while let Some((d, drel, chain)) = stack.pop() {
        let mut entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(&d) {
            Ok(it) => it.filter_map(|e| e.ok()).collect(),
            Err(e) => {
                if first {
                    return Err(format!("cannot read {}: {e}", if rel.is_empty() { "the project directory" } else { rel }));
                }
                p.skipped.unreadable.push(format!("{drel}/"));
                continue;
            }
        };
        first = false;
        // Sorted so an archive of the same tree is the same archive, and the
        // skipped list reads in order.
        entries.sort_by_key(|e| e.file_name());
        let mut subdirs = Vec::new();
        for entry in entries {
            let fname = entry.file_name().to_string_lossy().into_owned();
            let erel = if drel.is_empty() { fname.clone() } else { format!("{drel}/{fname}") };
            let arch = archive_name(&name, rel, &erel);
            if filter.skips(&fname) {
                p.skipped.hidden.push(erel);
                continue;
            }
            let path = entry.path();
            // symlink_metadata, never metadata: a symlink is how a walk
            // leaves the project.
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => {
                    p.skipped.unreadable.push(erel);
                    continue;
                }
            };
            if meta.file_type().is_symlink() {
                // Confined, and a file: included as its target's content.
                match crate::projects::safe_resolve(root, &erel) {
                    Ok(real) if std::fs::metadata(&real).map(|m| m.is_file()).unwrap_or(false) => {
                        let len = std::fs::metadata(&real).map(|m| m.len()).unwrap_or(0);
                        add_file(&mut p, arch, real, len)?;
                    }
                    _ => p.skipped.symlinks.push(erel),
                }
                continue;
            }
            if meta.is_dir() {
                if fname == ".git" {
                    p.skipped.hidden.push(erel);
                    continue;
                }
                match std::fs::symlink_metadata(path.join(".git")) {
                    Ok(_) => {
                        p.skipped.nested.push(format!("{erel}/"));
                        continue;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    // Could not probe: descend, and let `read_dir` file it
                    // as unreadable — the true answer — rather than calling
                    // a gap a decision.
                    Err(_) => {}
                }
                if ignoring && chain.skips_dir(&erel) {
                    p.skipped.ignored.push(format!("{erel}/"));
                    continue;
                }
                let child = if ignoring { chain.enter(&path, &erel) } else { Rc::clone(&chain) };
                subdirs.push((path, erel, child, arch));
                continue;
            }
            if meta.is_file() {
                add_file(&mut p, arch, path, meta.len())?;
            }
            // Sockets, fifos, devices: not content, and reading a fifo
            // blocks forever. Silently absent, like in the tree.
        }
        // Pushed in reverse so the walk pops them in name order.
        for (path, erel, child, arch) in subdirs.into_iter().rev() {
            p.entries.push((format!("{arch}/"), None));
            stack.push((path, erel, child));
        }
    }
    Ok(p)
}

fn archive_name(top: &str, base: &str, erel: &str) -> String {
    let inner = if base.is_empty() { erel } else { erel.strip_prefix(base).map(|s| s.trim_start_matches('/')).unwrap_or(erel) };
    format!("{top}/{inner}")
}

fn add_file(p: &mut Plan, arch: String, path: PathBuf, len: u64) -> Result<(), String> {
    p.files += 1;
    p.bytes += len;
    if p.files > MAX_FILES {
        return Err(format!("that folder has more than {MAX_FILES} files to zip — download a smaller folder, or use git or scp"));
    }
    if p.bytes > MAX_BYTES {
        return Err(format!(
            "that folder has more than {} MiB to zip — download a smaller folder, or use git or scp",
            MAX_BYTES >> 20
        ));
    }
    p.entries.push((arch, Some(path)));
    Ok(())
}

/// Counts what passes through, because a zip records offsets.
struct Counting<'a, W: Write> {
    w: &'a mut W,
    n: u64,
}

impl<W: Write> Write for Counting<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let k = self.w.write(buf)?;
        self.n += k as u64;
        Ok(k)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.w.flush()
    }
}

/// Hashes and counts the uncompressed bytes on their way into the encoder.
struct Crc<W: Write> {
    inner: W,
    crc: crc32fast::Hasher,
    n: u64,
}

impl<W: Write> Write for Crc<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let k = self.inner.write(buf)?;
        self.crc.update(&buf[..k]);
        self.n += k as u64;
        Ok(k)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

struct Central {
    name: Vec<u8>,
    crc: u32,
    csize: u32,
    usize: u32,
    offset: u32,
    method: u16,
    time: u16,
    date: u16,
    dir: bool,
}

/// Streams `plan` as a zip into `w`. `Err` only when `w` fails — the peer
/// went away — and then the caller must not finish the response. A file
/// that cannot be read any more is not an error: its entry holds what was
/// read, and it is listed under "changed" in `ROOST-SKIPPED.txt`.
pub fn write(plan: Plan, w: &mut impl Write) -> std::io::Result<()> {
    let Plan { name, entries, mut skipped, .. } = plan;
    let mut out = Counting { w, n: 0 };
    let mut central: Vec<Central> = Vec::with_capacity(entries.len() + 1);
    for (arch, path) in entries {
        let (time, date) = path.as_deref().map(dos_time).unwrap_or((0, 0x21));
        match path {
            None => central.push(entry::<_, std::fs::File>(&mut out, arch.into_bytes(), time, date, None)?),
            Some(p) => match std::fs::File::open(&p) {
                Ok(f) => {
                    let mut failed = false;
                    let c = entry(&mut out, arch.clone().into_bytes(), time, date, Some((f, &mut failed)))?;
                    if failed {
                        skipped.changed.push(arch);
                    }
                    central.push(c);
                }
                Err(_) => skipped.changed.push(arch),
            },
        }
    }
    if !skipped.is_empty() {
        let text = skipped.render();
        let (time, date) = dos_now();
        let c = entry(&mut out, format!("{name}/{SKIPPED_NAME}").into_bytes(), time, date, Some((text.as_bytes(), &mut false)))?;
        central.push(c);
    }
    let cd_start = out.n;
    for c in &central {
        let ver_made: u16 = 0x0300 | 20; // Unix, so the attributes below are read
        let ext_attr: u32 = if c.dir { (0o40755 << 16) | 0x10 } else { 0o100644 << 16 };
        let mut h = Vec::with_capacity(46 + c.name.len());
        h.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        h.extend_from_slice(&ver_made.to_le_bytes());
        h.extend_from_slice(&20u16.to_le_bytes());
        h.extend_from_slice(&FLAGS.to_le_bytes());
        h.extend_from_slice(&c.method.to_le_bytes());
        h.extend_from_slice(&c.time.to_le_bytes());
        h.extend_from_slice(&c.date.to_le_bytes());
        h.extend_from_slice(&c.crc.to_le_bytes());
        h.extend_from_slice(&c.csize.to_le_bytes());
        h.extend_from_slice(&c.usize.to_le_bytes());
        h.extend_from_slice(&(c.name.len() as u16).to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // extra
        h.extend_from_slice(&0u16.to_le_bytes()); // comment
        h.extend_from_slice(&0u16.to_le_bytes()); // disk
        h.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        h.extend_from_slice(&ext_attr.to_le_bytes());
        h.extend_from_slice(&c.offset.to_le_bytes());
        h.extend_from_slice(&c.name);
        out.write_all(&h)?;
    }
    let cd_size = out.n - cd_start;
    let mut e = Vec::with_capacity(22);
    e.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    e.extend_from_slice(&0u16.to_le_bytes());
    e.extend_from_slice(&0u16.to_le_bytes());
    e.extend_from_slice(&(central.len() as u16).to_le_bytes());
    e.extend_from_slice(&(central.len() as u16).to_le_bytes());
    e.extend_from_slice(&(cd_size as u32).to_le_bytes());
    e.extend_from_slice(&(cd_start as u32).to_le_bytes());
    e.extend_from_slice(&0u16.to_le_bytes());
    out.write_all(&e)?;
    out.flush()
}

/// Bit 3: sizes and CRC follow the data. Bit 11: names are UTF-8.
const FLAGS: u16 = 0x0808;

/// One local header, its data, and its data descriptor. `src` is `None` for
/// a directory. A read error ends the entry with what was read and sets the
/// flag; only a write error is returned.
fn entry<W: Write, R: Read>(
    out: &mut Counting<W>,
    name: Vec<u8>,
    time: u16,
    date: u16,
    src: Option<(R, &mut bool)>,
) -> std::io::Result<Central> {
    let offset = out.n as u32;
    let dir = src.is_none();
    let method: u16 = if dir { 0 } else { 8 };
    let mut h = Vec::with_capacity(30 + name.len());
    h.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    h.extend_from_slice(&20u16.to_le_bytes());
    h.extend_from_slice(&FLAGS.to_le_bytes());
    h.extend_from_slice(&method.to_le_bytes());
    h.extend_from_slice(&time.to_le_bytes());
    h.extend_from_slice(&date.to_le_bytes());
    h.extend_from_slice(&[0u8; 12]); // crc, sizes: in the descriptor
    h.extend_from_slice(&(name.len() as u16).to_le_bytes());
    h.extend_from_slice(&0u16.to_le_bytes());
    h.extend_from_slice(&name);
    out.write_all(&h)?;
    let (crc, csize, usize) = match src {
        None => (0, 0, 0),
        Some((mut r, failed)) => {
            let start = out.n;
            let enc = flate2::write::DeflateEncoder::new(&mut *out, flate2::Compression::default());
            let mut sink = Crc { inner: enc, crc: crc32fast::Hasher::new(), n: 0 };
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(k) => {
                        // A file that grew past the cap after planning is
                        // cut where the cap would have put it.
                        if sink.n + k as u64 > MAX_BYTES {
                            *failed = true;
                            break;
                        }
                        sink.write_all(&buf[..k])?
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        *failed = true;
                        break;
                    }
                }
            }
            let Crc { inner, crc, n } = sink;
            // `finish` hands the borrow of `out` back; only then can its
            // count say how many compressed bytes went out.
            let back = inner.finish()?;
            (crc.finalize(), (back.n - start) as u32, n as u32)
        }
    };
    let mut d = Vec::with_capacity(16);
    d.extend_from_slice(&0x0807_4b50u32.to_le_bytes());
    d.extend_from_slice(&crc.to_le_bytes());
    d.extend_from_slice(&csize.to_le_bytes());
    d.extend_from_slice(&usize.to_le_bytes());
    out.write_all(&d)?;
    Ok(Central { name, crc, csize, usize, offset, method, time, date, dir })
}

fn dos_time(p: &Path) -> (u16, u16) {
    let secs = std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    dos_from_unix(secs)
}

fn dos_now() -> (u16, u16) {
    dos_from_unix(crate::errlog::now_secs())
}

/// Unix seconds to DOS (time, date), as UTC — a zip's DOS time has no zone.
/// Clamped to 1980, the earliest DOS date.
fn dos_from_unix(secs: u64) -> (u16, u16) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    if y < 1980 {
        return (0, 0x21);
    }
    let time = (((rem / 3600) << 11) | (((rem % 3600) / 60) << 5) | ((rem % 60) / 2)) as u16;
    let date = (((y - 1980) as u16) << 9) | ((m as u16) << 5) | d as u16;
    (time, date)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn filter(show_hidden: bool) -> TreeFilter<'static> {
        TreeFilter { hide: &[], show_hidden }
    }

    /// Everything the walk has a rule for, in one tree.
    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        let w = |p: &str, s: &str| {
            let f = r.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, s).unwrap();
        };
        w("src/main.rs", "fn main() {}\n");
        w("src/deep/a.txt", "deep\n");
        w(".env", "SECRET=1\n");
        w(".gitignore", "dist/\n");
        w("dist/bundle.js", "x\n");
        w("node_modules/pkg/index.js", "x\n");
        w("vendor/lib/.git/HEAD", "ref\n");
        w("vendor/lib/code.c", "x\n");
        w(".git/HEAD", "ref\n");
        std::fs::create_dir_all(r.join("empty")).unwrap();
        std::os::unix::fs::symlink("src/main.rs", r.join("inside-link")).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", r.join("outside-link")).unwrap();
        std::os::unix::fs::symlink("src", r.join("dir-link")).unwrap();
        d
    }

    fn names(p: &Plan) -> Vec<&str> {
        p.entries.iter().map(|(n, _)| n.as_str()).collect()
    }

    #[test]
    fn the_walk_takes_what_search_would_and_lists_everything_else() {
        let d = fixture();
        let root = d.path().canonicalize().unwrap();
        let top = root.file_name().unwrap().to_string_lossy().into_owned();
        let p = plan(&root, "", &filter(false)).unwrap();
        let n = names(&p);
        for want in ["src/main.rs", "src/deep/a.txt", "empty/", "inside-link"] {
            assert!(n.contains(&format!("{top}/{want}").as_str()), "missing {want}: {n:?}");
        }
        for never in [".env", "dist/bundle.js", "node_modules", "vendor/lib/code.c", ".git/HEAD", "outside-link", "dir-link"] {
            assert!(!n.iter().any(|x| x.contains(never)), "{never} must not be in the archive: {n:?}");
        }
        let s = &p.skipped;
        assert_eq!(s.ignored, ["dist/"]);
        assert_eq!(s.nested, ["vendor/lib/"]);
        assert!(s.hidden.contains(&".env".to_string()) && s.hidden.contains(&"node_modules".to_string()), "{:?}", s.hidden);
        assert!(s.symlinks.contains(&"outside-link".to_string()) && s.symlinks.contains(&"dir-link".to_string()), "{:?}", s.symlinks);
        assert_eq!(p.files, 3);
    }

    /// `show_hidden` lifts what it lifts in search — dotfiles and gitignored
    /// directories — and nothing else: `.git` and nested checkouts stay out.
    #[test]
    fn show_hidden_lifts_dotfiles_and_gitignore_but_not_git_or_checkouts() {
        let d = fixture();
        let root = d.path().canonicalize().unwrap();
        let p = plan(&root, "", &filter(true)).unwrap();
        let n = names(&p).join(" ");
        assert!(n.contains("/.env") && n.contains("/dist/bundle.js"), "{n}");
        assert!(!n.contains("/.git/") && !n.contains("vendor/lib/code.c"), "{n}");
    }

    /// Zipping a subfolder still honours the root's `.gitignore`.
    /// Revert-checked: starting the chain at the subfolder (no ancestor
    /// `enter`s, `for_root(dir)`) puts `app/dist/out.js` in the archive.
    #[test]
    fn a_subfolder_inherits_the_roots_gitignore() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        std::fs::write(r.join(".gitignore"), "dist/\n").unwrap();
        std::fs::create_dir_all(r.join("app/dist")).unwrap();
        std::fs::write(r.join("app/dist/out.js"), "x").unwrap();
        std::fs::write(r.join("app/keep.js"), "x").unwrap();
        let p = plan(&r.canonicalize().unwrap(), "app", &filter(false)).unwrap();
        assert_eq!(names(&p), ["app/", "app/keep.js"]);
        assert_eq!(p.skipped.ignored, ["app/dist/"]);
    }

    #[test]
    fn an_unreadable_subfolder_is_a_listed_gap_and_an_unreadable_target_a_refusal() {
        if unsafe { libc::geteuid() } == 0 {
            return; // root reads everything; the permission cannot bite
        }
        let d = tempfile::tempdir().unwrap();
        let r = d.path().canonicalize().unwrap();
        std::fs::create_dir_all(r.join("a/locked")).unwrap();
        std::fs::write(r.join("a/ok.txt"), "x").unwrap();
        std::fs::set_permissions(r.join("a/locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let p = plan(&r, "a", &filter(false));
        let refused = plan(&r, "a/locked", &filter(false));
        std::fs::set_permissions(r.join("a/locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let p = p.unwrap();
        assert_eq!(p.skipped.unreadable, ["a/locked/"]);
        assert!(names(&p).contains(&"a/ok.txt"));
        assert!(refused.unwrap_err().contains("cannot read"), "the folder asked for cannot be read: refuse");
    }

    #[test]
    fn a_file_or_an_escape_is_refused() {
        let d = fixture();
        let root = d.path().canonicalize().unwrap();
        assert!(plan(&root, "src/main.rs", &filter(false)).unwrap_err().contains("not a folder"));
        assert!(plan(&root, "../", &filter(false)).is_err());
        assert!(plan(&root, "dir-link/../..", &filter(false)).is_err());
    }

    #[test]
    fn the_file_cap_refuses_before_anything_is_written() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        for i in 0..=MAX_FILES {
            std::fs::write(r.join(format!("f{i}")), "").unwrap();
        }
        let e = plan(&r.canonicalize().unwrap(), "", &filter(false)).unwrap_err();
        assert!(e.contains(&MAX_FILES.to_string()), "{e}");
    }

    /// Unpacked by Python's `zipfile`, an independent reader: the structure,
    /// the CRCs (`testzip`), the contents, the directory entries and the
    /// skipped list. Revert-checked: a central-directory offset one byte off
    /// makes the open fail. `zipfile` reads sizes from the central directory,
    /// so it cannot see the local headers — `local_headers_defer_to_their_descriptors`
    /// covers those.
    #[test]
    fn the_archive_unpacks_with_an_independent_reader() {
        let d = fixture();
        let root = d.path().canonicalize().unwrap();
        let top = root.file_name().unwrap().to_string_lossy().into_owned();
        // Big enough to span several reads and actually compress.
        std::fs::write(root.join("src/big.txt"), "roost ".repeat(100_000)).unwrap();
        let p = plan(&root, "", &filter(false)).unwrap();
        let mut zip = Vec::new();
        write(p, &mut zip).unwrap();
        assert!(zip.len() < 100_000, "deflate is doing its job: {} bytes", zip.len());
        let out = d.path().join("out.zip");
        std::fs::write(&out, &zip).unwrap();
        let py = format!(
            r#"
import zipfile, json
z = zipfile.ZipFile({out:?})
assert z.testzip() is None, "bad crc"
print(json.dumps({{
  "names": z.namelist(),
  "main": z.read("{top}/src/main.rs").decode(),
  "big": len(z.read("{top}/src/big.txt")),
  "link": z.read("{top}/inside-link").decode(),
  "skipped": z.read("{top}/ROOST-SKIPPED.txt").decode(),
  "utf8": [i.flag_bits & 0x800 != 0 for i in z.infolist()][0],
}}))
"#,
            out = out.to_string_lossy()
        );
        let r = std::process::Command::new("python3").arg("-c").arg(&py).output().expect("python3");
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        let v: serde_json::Value = serde_json::from_slice(&r.stdout).unwrap();
        assert_eq!(v["main"], "fn main() {}\n");
        assert_eq!(v["big"], 600_000);
        assert_eq!(v["link"], "fn main() {}\n", "a confined file symlink carries its target");
        assert!(v["names"].as_array().unwrap().iter().any(|n| n == &format!("{top}/empty/")), "{}", v["names"]);
        let sk = v["skipped"].as_str().unwrap();
        assert!(sk.contains("Ignored by git (1)") && sk.contains("dist/") && sk.contains("Nested checkouts (1)"), "{sk}");
        assert!(sk.contains("outside-link"), "{sk}");
        assert_eq!(v["utf8"], true);
    }

    /// A streaming reader (one that reads local headers in order and never
    /// seeks to the central directory) finds the sizes only in the data
    /// descriptor, so every local header must say so with bit 3 and must not
    /// claim sizes it cannot know. Revert-checked: FLAGS without bit 3 fails
    /// the flag assertion.
    #[test]
    fn local_headers_defer_to_their_descriptors() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path().canonicalize().unwrap();
        std::fs::write(r.join("a.txt"), "hello").unwrap();
        let p = plan(&r, "", &filter(false)).unwrap();
        let mut zip = Vec::new();
        write(p, &mut zip).unwrap();
        let mut found = 0;
        let mut i = 0;
        while i + 30 <= zip.len() {
            if zip[i..i + 4] == 0x0403_4b50u32.to_le_bytes() {
                let flags = u16::from_le_bytes([zip[i + 6], zip[i + 7]]);
                assert_eq!(flags & 0x0008, 0x0008, "bit 3 at local header {i}");
                assert_eq!(flags & 0x0800, 0x0800, "UTF-8 names at local header {i}");
                assert_eq!(&zip[i + 14..i + 26], &[0u8; 12], "no crc or sizes in local header {i}");
                found += 1;
            }
            i += 1;
        }
        assert_eq!(found, 2, "the folder entry and a.txt");
    }

    /// A file that is gone by the time it is read is listed, not fatal.
    #[test]
    fn a_file_that_vanishes_after_planning_is_listed_as_changed() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path().canonicalize().unwrap();
        std::fs::write(r.join("a.txt"), "a").unwrap();
        std::fs::write(r.join("b.txt"), "b").unwrap();
        let p = plan(&r, "", &filter(false)).unwrap();
        std::fs::remove_file(r.join("b.txt")).unwrap();
        let mut zip = Vec::new();
        write(p, &mut zip).unwrap();
        let text = String::from_utf8_lossy(&zip);
        assert!(text.contains("ROOST-SKIPPED.txt"), "a skipped file is written");
    }

    #[test]
    fn nothing_skipped_means_no_skipped_file() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path().canonicalize().unwrap();
        std::fs::write(r.join("a.txt"), "a").unwrap();
        let p = plan(&r, "", &filter(false)).unwrap();
        let mut zip = Vec::new();
        write(p, &mut zip).unwrap();
        assert!(!String::from_utf8_lossy(&zip).contains(SKIPPED_NAME));
    }

    #[test]
    fn dos_time_is_utc_civil_time() {
        // 2024-02-29 13:45:30 UTC
        let (t, d) = dos_from_unix(1_709_214_330);
        assert_eq!((d >> 9) + 1980, 2024);
        assert_eq!((d >> 5) & 0xf, 2);
        assert_eq!(d & 0x1f, 29);
        assert_eq!((t >> 11, (t >> 5) & 0x3f, (t & 0x1f) * 2), (13, 45, 30));
        assert_eq!(dos_from_unix(0), (0, 0x21), "before 1980 clamps");
    }
}
