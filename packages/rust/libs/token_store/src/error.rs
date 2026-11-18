use std::fmt;
use stockhouse_credentials::CredentialError;

/// Why the token store could not be used. Never contains content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum StoreError {
    /// The store directory does not exist.
    DirectoryMissing,
    /// The store directory is a link, not a directory, foreign or too open.
    DirectoryUnsafe,
    /// A store or lock file is a symbolic link.
    Symlink,
    /// A store or lock file is not a regular file.
    NotRegularFile,
    /// A store or lock file is owned by another UID.
    WrongOwner,
    /// A store or lock file has group or other permission bits.
    PermissionsTooOpen {
        /// The observed permission bits.
        mode: u32,
    },
    /// The store file exceeds 65 536 bytes.
    TooLarge,
    /// The store could not be decrypted or parsed.
    Corrupt,
    /// The store was written for another configuration.
    Mismatch,
    /// The sealing key credential is not loaded.
    KeyUnavailable,
    /// The sealing key credential is malformed.
    KeyMalformed,
    /// The lock was not acquired within the wait.
    LockTimeout,
    /// A filesystem step failed.
    Io {
        /// The step.
        op: &'static str,
    },
}

impl StoreError {
    /// A short stable kind for status output and logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::DirectoryMissing => "directory-missing",
            Self::DirectoryUnsafe => "directory-unsafe",
            Self::Symlink => "symlink",
            Self::NotRegularFile => "not-regular-file",
            Self::WrongOwner => "wrong-owner",
            Self::PermissionsTooOpen { .. } => "permissions-too-open",
            Self::TooLarge => "too-large",
            Self::Corrupt => "corrupt",
            Self::Mismatch => "mismatch",
            Self::KeyUnavailable => "key-unavailable",
            Self::KeyMalformed => "key-malformed",
            Self::LockTimeout => "lock-timeout",
            Self::Io { .. } => "io",
        }
    }

    /// The credential error a consumer sees.
    pub fn credential_error(&self) -> CredentialError {
        match self {
            Self::KeyUnavailable => CredentialError::KeyUnavailable,
            Self::KeyMalformed => CredentialError::KeyMalformed,
            Self::Corrupt => CredentialError::StoreCorrupt,
            Self::Mismatch => CredentialError::StoreMismatch,
            Self::LockTimeout | Self::Io { .. } => CredentialError::RefreshUnavailable,
            _ => CredentialError::StoreUnsafe,
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PermissionsTooOpen { mode } => {
                write!(
                    f,
                    "token store: mode {mode:04o} grants group or other access"
                )
            }
            Self::Io { op } => write!(f, "token store: {op} failed"),
            other => write!(f, "token store: {}", other.kind()),
        }
    }
}

impl std::error::Error for StoreError {}
