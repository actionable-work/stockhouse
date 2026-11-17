use crate::{CredentialError, CredentialSource, Secret};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 4096;

/// The explicitly configured owner-only file fallback.
#[derive(Clone, Debug)]
pub struct OwnerOnlyFile {
    path: PathBuf,
    euid: u32,
}

impl OwnerOnlyFile {
    /// A source for `path`, checked against the process's effective UID.
    pub fn new(path: PathBuf) -> Self {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        Self { path, euid }
    }

    /// A source checked against an explicit UID.
    pub fn with_euid(path: PathBuf, euid: u32) -> Self {
        Self { path, euid }
    }
}

impl CredentialSource for OwnerOnlyFile {
    fn credential(&self) -> Result<Secret, CredentialError> {
        check_path(&self.path)?;
        let file = open(&self.path)?;
        let meta = file.metadata().map_err(|_| CredentialError::Unreadable)?;
        if !meta.is_file() {
            return Err(CredentialError::NotRegularFile);
        }
        check_owner(meta.uid(), meta.mode(), self.euid)?;
        read_content(file)
    }
}

/// A systemd credential under `$CREDENTIALS_DIRECTORY`.
#[derive(Clone, Debug)]
pub struct SystemdCredential {
    name: String,
    directory: Option<Option<PathBuf>>,
}

impl SystemdCredential {
    /// Reads `CREDENTIALS_DIRECTORY` from the environment on every use.
    pub fn new(name: String) -> Self {
        Self {
            name,
            directory: None,
        }
    }

    /// Uses an explicit directory (`None` behaves as an unset variable).
    pub fn with_directory(name: String, directory: Option<PathBuf>) -> Self {
        Self {
            name,
            directory: Some(directory),
        }
    }

    fn directory(&self) -> Option<PathBuf> {
        match &self.directory {
            Some(dir) => dir.clone(),
            None => std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from),
        }
    }
}

impl CredentialSource for SystemdCredential {
    fn credential(&self) -> Result<Secret, CredentialError> {
        let dir = self.directory().ok_or(CredentialError::NotConfigured)?;
        let plain = !self.name.is_empty()
            && self.name != "."
            && self.name != ".."
            && !self.name.contains('/');
        if !plain {
            return Err(CredentialError::InvalidName);
        }
        let file = open(&dir.join(&self.name))?;
        let meta = file.metadata().map_err(|_| CredentialError::Unreadable)?;
        if !meta.is_file() {
            return Err(CredentialError::NotRegularFile);
        }
        read_content(file)
    }
}

fn check_path(path: &Path) -> Result<(), CredentialError> {
    let bytes = path.as_os_str().as_encoded_bytes();
    let normal = !bytes.split(|&b| b == b'/').any(|s| s == b"." || s == b"..");
    if !path.is_absolute() || !normal {
        return Err(CredentialError::RelativePath);
    }
    if path.starts_with("/nix/store") {
        Err(CredentialError::InStore)
    } else {
        Ok(())
    }
}

fn open(path: &Path) -> Result<File, CredentialError> {
    let flags = libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
        .map_err(|e| match e.kind() {
            ErrorKind::NotFound => CredentialError::Missing,
            _ if e.raw_os_error() == Some(libc::ELOOP) => CredentialError::Symlink,
            _ => CredentialError::Unreadable,
        })
}

fn check_owner(uid: u32, mode: u32, euid: u32) -> Result<(), CredentialError> {
    if uid != euid {
        return Err(CredentialError::WrongOwner);
    }
    if mode & 0o077 != 0 {
        return Err(CredentialError::PermissionsTooOpen {
            mode: mode & 0o7777,
        });
    }
    Ok(())
}

fn read_content(file: File) -> Result<Secret, CredentialError> {
    // Sized up front so growth never leaves an unzeroed copy behind.
    let mut bytes = Vec::with_capacity(MAX_BYTES as usize + 1);
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CredentialError::Unreadable)?;
    let secret = Secret::new(bytes);
    let mut content = secret.expose();
    if content.len() as u64 > MAX_BYTES {
        return Err(CredentialError::TooLarge);
    }
    if let Some(stripped) = content.strip_suffix(b"\n") {
        content = stripped;
    }
    if content.is_empty() {
        return Err(CredentialError::Empty);
    }
    if !content.iter().all(|b| (0x21..=0x7e).contains(b)) {
        return Err(CredentialError::Malformed);
    }
    Ok(Secret::new(content.to_vec()))
}
