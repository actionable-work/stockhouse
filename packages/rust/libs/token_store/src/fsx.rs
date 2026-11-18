//! Descriptor-relative filesystem steps with owner and mode checks.

use crate::StoreError;
use std::ffi::CString;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

/// The largest store file read.
pub(crate) const MAX_FILE: u64 = 65_536;

fn cname(name: &str) -> CString {
    CString::new(name).expect("store file names have no NUL")
}

fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Opens and checks the store directory.
pub(crate) fn open_dir(path: &Path, euid: u32) -> Result<File, StoreError> {
    let flags = libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_RDONLY;
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
        .map_err(|e| match e.kind() {
            ErrorKind::NotFound => StoreError::DirectoryMissing,
            _ => StoreError::DirectoryUnsafe,
        })?;
    let meta = dir.metadata().map_err(|_| StoreError::DirectoryUnsafe)?;
    if !meta.is_dir() || meta.uid() != euid || meta.mode() & 0o077 != 0 {
        return Err(StoreError::DirectoryUnsafe);
    }
    Ok(dir)
}

/// `openat` relative to the directory.
pub(crate) fn openat(dir: &File, name: &str, flags: i32, mode: u32) -> std::io::Result<File> {
    let name = cname(name);
    // SAFETY: a valid directory descriptor and NUL-terminated name.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            mode,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh descriptor we own.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Maps an open failure of an existing-file open.
pub(crate) fn open_error(error: &std::io::Error) -> StoreError {
    match error.raw_os_error() {
        Some(libc::ELOOP) => StoreError::Symlink,
        _ => StoreError::Io { op: "open" },
    }
}

/// Checks an open store or lock file.
pub(crate) fn check_file(file: &File, euid: u32) -> Result<(), StoreError> {
    let meta = file.metadata().map_err(|_| StoreError::Io { op: "stat" })?;
    check_meta(meta.is_file(), meta.uid(), meta.mode(), euid)
}

fn check_meta(regular: bool, uid: u32, mode: u32, euid: u32) -> Result<(), StoreError> {
    if !regular {
        return Err(StoreError::NotRegularFile);
    }
    if uid != euid {
        return Err(StoreError::WrongOwner);
    }
    if mode & 0o077 != 0 {
        return Err(StoreError::PermissionsTooOpen {
            mode: mode & 0o7777,
        });
    }
    Ok(())
}

/// Reads a checked file; `Ok(None)` when it does not exist.
pub(crate) fn read_file(dir: &File, name: &str, euid: u32) -> Result<Option<Vec<u8>>, StoreError> {
    let file = match openat(dir, name, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(open_error(&e)),
    };
    check_file(&file, euid)?;
    let mut bytes = Vec::with_capacity(4096);
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| StoreError::Io { op: "read" })?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(StoreError::TooLarge);
    }
    Ok(Some(bytes))
}

/// Writes `bytes` to a fresh `O_EXCL` temporary file and `fsync`s it.
pub(crate) fn write_temp(dir: &File, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL;
    let mut file = openat(dir, name, flags, 0o600).map_err(|_| StoreError::Io { op: "create" })?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if written.is_err() {
        let _ = unlink(dir, name);
        return Err(StoreError::Io { op: "write" });
    }
    Ok(())
}

/// `renameat` within the directory.
pub(crate) fn rename(dir: &File, from: &str, to: &str) -> Result<(), StoreError> {
    let (from, to) = (cname(from), cname(to));
    let fd = dir.as_raw_fd();
    // SAFETY: a valid directory descriptor and NUL-terminated names.
    let rc = unsafe { libc::renameat(fd, from.as_ptr(), fd, to.as_ptr()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(StoreError::Io { op: "rename" })
    }
}

/// `unlinkat` within the directory; a missing file is fine.
pub(crate) fn unlink(dir: &File, name: &str) -> Result<(), StoreError> {
    let name = cname(name);
    // SAFETY: a valid directory descriptor and NUL-terminated name.
    let rc = unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) };
    if rc == 0 || last_errno() == libc::ENOENT {
        Ok(())
    } else {
        Err(StoreError::Io { op: "unlink" })
    }
}

/// Names of directory entries.
pub(crate) fn entries(path: &Path) -> Vec<String> {
    std::fs::read_dir(path)
        .map(|dir| {
            dir.filter_map(Result::ok)
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// `fsync`s the directory.
pub(crate) fn sync_dir(dir: &File) -> Result<(), StoreError> {
    dir.sync_all().map_err(|_| StoreError::Io { op: "sync" })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A file owned by another UID cannot be created without root, so the
    // owner rule is checked on the metadata decision directly.
    #[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-009"))]
    #[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-017"))]
    #[test]
    fn foreign_owned_files_are_refused() {
        assert_eq!(
            check_meta(true, 1000, 0o100600, 1001),
            Err(StoreError::WrongOwner)
        );
        assert_eq!(check_meta(true, 1000, 0o100600, 1000), Ok(()));
    }
}
