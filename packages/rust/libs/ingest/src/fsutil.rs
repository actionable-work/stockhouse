//! Durable file placement: stage in `tmp/`, fsync, move, fsync the directory.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

fn stage(tmp: &Path, bytes: &[u8]) -> io::Result<std::path::PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = tmp.join(format!("{}-{n}.tmp", std::process::id()));
    let mut file = OpenOptions::new().write(true).create_new(true).custom_flags(libc::O_NOFOLLOW).open(&path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(path)
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

fn parent(path: &Path) -> &Path {
    path.parent().unwrap_or(Path::new("."))
}

/// Atomically replaces `target` with `bytes`.
pub(crate) fn replace(tmp: &Path, target: &Path, bytes: &[u8]) -> io::Result<()> {
    let staged = stage(tmp, bytes)?;
    fs::rename(&staged, target)?;
    sync_dir(parent(target))
}

/// Creates `target` only if it does not exist; `Ok(false)` if it existed.
pub(crate) fn create_new(tmp: &Path, target: &Path, bytes: &[u8]) -> io::Result<bool> {
    let staged = stage(tmp, bytes)?;
    let linked = fs::hard_link(&staged, target);
    fs::remove_file(&staged)?;
    match linked {
        Ok(()) => sync_dir(parent(target)).map(|()| true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// Removes a file and syncs its directory; a missing file is fine.
pub(crate) fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => sync_dir(parent(path)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Appends one byte unless the file has reached `cap` bytes.
pub(crate) fn append_marker(path: &Path, cap: u64) -> io::Result<()> {
    let mut file = OpenOptions::new().append(true).create(true).mode(0o644).custom_flags(libc::O_NOFOLLOW).open(path)?;
    if file.metadata()?.len() < cap {
        file.write_all(b"o")?;
    }
    Ok(())
}

/// Size of a file, or `None` when absent.
pub(crate) fn size(path: &Path) -> io::Result<Option<u64>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta.len())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Names of entries in `dir` ending in `suffix`, with the suffix removed.
pub(crate) fn stems(dir: &Path, suffix: &str) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        if let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(suffix)) {
            out.push(stem.to_owned());
        }
    }
    Ok(out)
}
