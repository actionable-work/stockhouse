use crate::lock::{self, Lock};
use crate::seal::{self, Key};
use crate::{Binding, StoreError, TokenSet, fsx};
use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;
use stockhouse_credentials::SystemdCredential;

/// The sealed store file.
pub const SEALED_NAME: &str = "tokens.sealed";
/// The plaintext store file (`file` mode only).
pub const FILE_NAME: &str = "tokens.json";
const TEMP_PREFIX: &str = ".tokens.";

/// How the token set is protected.
#[derive(Clone, Debug)]
pub enum StoreMode {
    /// Sealed under a key read from this systemd credential.
    Sealed(SystemdCredential),
    /// Plaintext, owner-only; used only when explicitly configured.
    File,
}

/// One token store directory.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
    mode: StoreMode,
    binding: Binding,
    euid: u32,
}

impl Store {
    /// A store checked against the process's effective UID.
    pub fn new(dir: PathBuf, mode: StoreMode, binding: Binding) -> Self {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        Self::with_euid(dir, mode, binding, euid)
    }

    /// A store checked against an explicit UID.
    pub fn with_euid(dir: PathBuf, mode: StoreMode, binding: Binding, euid: u32) -> Self {
        Self {
            dir,
            mode,
            binding,
            euid,
        }
    }

    /// `sealed` or `file`.
    pub fn mode_name(&self) -> &'static str {
        match self.mode {
            StoreMode::Sealed(_) => "sealed",
            StoreMode::File => "file",
        }
    }

    fn names(&self) -> (&'static str, &'static str) {
        match self.mode {
            StoreMode::Sealed(_) => (SEALED_NAME, FILE_NAME),
            StoreMode::File => (FILE_NAME, SEALED_NAME),
        }
    }

    fn key(&self) -> Result<Option<Key>, StoreError> {
        match &self.mode {
            StoreMode::Sealed(source) => Key::load(source).map(Some),
            StoreMode::File => Ok(None),
        }
    }

    fn open_dir(&self) -> Result<File, StoreError> {
        fsx::open_dir(&self.dir, self.euid)
    }

    /// Reads the token set; `Ok(None)` when none is stored. Takes no lock.
    pub fn read(&self) -> Result<Option<TokenSet>, StoreError> {
        let dir = self.open_dir()?;
        let key = self.key()?;
        let Some(bytes) = fsx::read_file(&dir, self.names().0, self.euid)? else {
            return Ok(None);
        };
        let bytes = zeroize::Zeroizing::new(bytes);
        match key {
            Some(key) => seal::unseal(&bytes, &key, &self.binding).map(Some),
            None => TokenSet::decode(&bytes, &self.binding).map(Some),
        }
    }

    /// Acquires the exclusive store lock within `wait`.
    pub fn lock(&self, wait: Duration) -> Result<Lock, StoreError> {
        lock::acquire(&self.open_dir()?, self.euid, wait)
    }

    /// Checks the directory, the key and the lock without changing anything.
    pub fn preflight(&self, wait: Duration) -> Result<(), StoreError> {
        self.key()?;
        self.lock(wait).map(drop)
    }

    /// Writes the token set atomically; the lock must be held.
    pub fn write(&self, _lock: &Lock, set: &TokenSet) -> Result<(), StoreError> {
        let dir = self.open_dir()?;
        let bytes = match self.key()? {
            Some(key) => zeroize::Zeroizing::new(seal::seal(set, &key, &self.binding)?),
            None => set.encode(&self.binding),
        };
        self.remove_stale(&dir);
        let temp = self.temp_name()?;
        fsx::write_temp(&dir, &temp, &bytes)?;
        let (name, other) = self.names();
        if let Err(error) = fsx::rename(&dir, &temp, name) {
            let _ = fsx::unlink(&dir, &temp);
            return Err(error);
        }
        fsx::unlink(&dir, other)?;
        fsx::sync_dir(&dir)
    }

    /// Removes every copy of the token set; the lock must be held.
    pub fn delete(&self, _lock: &Lock) -> Result<(), StoreError> {
        let dir = self.open_dir()?;
        fsx::unlink(&dir, SEALED_NAME)?;
        fsx::unlink(&dir, FILE_NAME)?;
        fsx::sync_dir(&dir)
    }

    fn temp_name(&self) -> Result<String, StoreError> {
        let mut random = [0u8; 8];
        crate::random(&mut random)?;
        let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
        Ok(format!("{TEMP_PREFIX}{}.{hex}.tmp", std::process::id()))
    }

    fn remove_stale(&self, dir: &File) {
        for name in fsx::entries(&self.dir) {
            if name.starts_with(TEMP_PREFIX) && name.ends_with(".tmp") {
                let _ = fsx::unlink(dir, &name);
            }
        }
    }
}
