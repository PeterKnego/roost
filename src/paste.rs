//! Scratch storage for images pasted onto a terminal.
//!
//! These files live deliberately *outside* the project. roost already keeps its
//! own state out of the working tree so that using it never shows up in `git
//! status`; a paste directory inside the repo would undo that on the first
//! screenshot.
//!
//! The extension is not cosmetic. The program receiving the paste decides
//! whether a pasted path is an image by looking at the *filename*, not at the
//! bytes, so a correct PNG written as `.dat` silently arrives as text instead of
//! as an image. That is why this sniffs the content and refuses anything it
//! cannot name correctly, rather than trusting a MIME type from the browser.
use std::path::{Path, PathBuf};

/// The formats the receiver recognises from a path. BMP is deliberately absent:
/// the clipboard route accepts it, the path route does not, so writing one would
/// produce a file that arrives as text.
pub fn extension_of(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("png");
    }
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("jpg");
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some("gif");
    }
    if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        return Some("webp");
    }
    None
}

/// Keyed by `storage_key`, like `wsstate::path_for`, so a nested project's `/`
/// cannot land in a filename or be read as a directory separator.
pub fn scratch_dir(project: &str) -> PathBuf {
    crate::wsstate::state_dir().join("pasted").join(crate::projects::storage_key(project))
}

/// A name nothing already holds. A counter rather than a bare timestamp: two
/// pastes inside the same second would collide, and `UploadTemp` refuses a
/// collision rather than resolving it, so the second paste would appear to do
/// nothing at all.
pub fn free_name(dir: &Path, ext: &str) -> Result<String, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for n in 0..1000 {
        let name = if n == 0 { format!("{stamp}.{ext}") } else { format!("{stamp}-{n}.{ext}") };
        // symlink_metadata, not exists(): "cannot tell" must not be read as
        // "free", or the paste writes through whatever is really there.
        match dir.join(&name).symlink_metadata() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(name),
            Err(e) => return Err(format!("cannot check {name}: {e}")),
            Ok(_) => continue,
        }
    }
    Err("too many pasted images in the same second".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];

    #[test]
    fn sniffs_each_accepted_format() {
        assert_eq!(extension_of(PNG), Some("png"));
        assert_eq!(extension_of(&[0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0]), Some("jpg"));
        assert_eq!(extension_of(b"GIF89a__"), Some("gif"));
        assert_eq!(extension_of(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
    }

    /// A BMP is a real image that the *clipboard* route accepts, and it is still
    /// refused here: the receiver's filename test does not cover `.bmp`, so
    /// writing one would produce a file that silently arrives as text. Refusing
    /// is the honest outcome.
    #[test]
    fn refuses_formats_the_receiver_cannot_read_from_a_path() {
        assert_eq!(extension_of(b"BM\0\0\0\0\0\0"), None);
        assert_eq!(extension_of(b"not an image at all"), None);
        assert_eq!(extension_of(&[]), None);
        // Truncated magic must not match on a prefix.
        assert_eq!(extension_of(&[0x89, b'P']), None);
        assert_eq!(extension_of(b"RIFF\0\0\0\0AVI "), None);
    }

    /// A nested project's `/` must not become a separator — the same reason
    /// wsstate keys by storage_key rather than the raw project string.
    #[test]
    fn a_nested_project_key_is_encoded_not_split() {
        // `ROOST_STATE_DIR` is process-global and read on every `state_dir()`
        // call, so setting it without this lock clobbers it under a
        // concurrently-running test — whose writes then land in this test's
        // `TempDir` while it is being removed. `TempDir::drop` ignores the
        // failed removal, leaking the directory silently.
        let _envg = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("ROOST_STATE_DIR", d.path());
        let p = scratch_dir("karpie/src");
        let leaf = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(!leaf.contains('/'), "project key leaked a separator: {leaf}");
        assert_eq!(leaf, crate::projects::storage_key("karpie/src"));
        assert!(p.starts_with(d.path()), "scratch must live under the state dir, not the project");
    }

    #[test]
    fn free_name_steps_past_a_name_already_taken() {
        let d = tempfile::tempdir().unwrap();
        let first = free_name(d.path(), "png").unwrap();
        std::fs::write(d.path().join(&first), b"x").unwrap();
        let second = free_name(d.path(), "png").unwrap();
        assert_ne!(first, second, "a second paste in the same second must not reuse the name");
        assert!(second.ends_with(".png"));
    }
}
