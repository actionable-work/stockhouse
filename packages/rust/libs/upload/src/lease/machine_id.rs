//! The owner-only random machine identity file.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use stockhouse_wire::MachineId;

/// Why the machine identity cannot be used. Never regenerated automatically.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MachineIdCheck {
    /// The file is a symbolic link.
    Symlink,
    /// Not a regular file.
    NotRegularFile,
    /// Owned by another user.
    WrongOwner,
    /// Group or other permission bits are set.
    PermissionsTooOpen,
    /// Not 32 lowercase hex characters and a newline.
    Malformed,
    /// It could not be created or read.
    Unreadable,
}

impl MachineIdCheck {
    /// The failed check, for messages.
    pub fn name(self) -> &'static str {
        match self {
            Self::Symlink => "symbolic link refused",
            Self::NotRegularFile => "not a regular file",
            Self::WrongOwner => "owned by another user",
            Self::PermissionsTooOpen => "grants group or other access",
            Self::Malformed => "malformed content",
            Self::Unreadable => "unreadable",
        }
    }
}

fn create(path: &Path) -> Result<(), MachineIdCheck> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| MachineIdCheck::Unreadable)?;
    let content = format!("{}\n", MachineId::from_bytes(bytes));
    let dir = path.parent().ok_or(MachineIdCheck::Unreadable)?;
    let staged = dir.join(format!(".machine-id.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&staged);
    let write = || -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&staged)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        match fs::hard_link(&staged, path) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
            _ => File::open(dir)?.sync_all(),
        }
    };
    let result = write();
    let _ = fs::remove_file(&staged);
    result.map_err(|_| MachineIdCheck::Unreadable)
}

fn read(path: &Path, euid: u32) -> Result<MachineId, MachineIdCheck> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) => MachineIdCheck::Symlink,
            _ => MachineIdCheck::Unreadable,
        })?;
    let meta = file.metadata().map_err(|_| MachineIdCheck::Unreadable)?;
    if !meta.is_file() {
        return Err(MachineIdCheck::NotRegularFile);
    }
    if meta.uid() != euid {
        return Err(MachineIdCheck::WrongOwner);
    }
    if meta.mode() & 0o077 != 0 {
        return Err(MachineIdCheck::PermissionsTooOpen);
    }
    let mut content = Vec::new();
    file.take(34)
        .read_to_end(&mut content)
        .map_err(|_| MachineIdCheck::Unreadable)?;
    let text = std::str::from_utf8(&content).map_err(|_| MachineIdCheck::Malformed)?;
    let id = text.strip_suffix('\n').ok_or(MachineIdCheck::Malformed)?;
    MachineId::parse(id).map_err(|_| MachineIdCheck::Malformed)
}

/// Reads the identity, creating it first if the file does not exist.
pub fn load_or_create(path: &Path, euid: u32) -> Result<MachineId, MachineIdCheck> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => create(path)?,
        _ => {}
    }
    read(path, euid)
}
