use std::fmt;

/// Why a credential could not be produced. Never contains credential content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CredentialError {
    /// No source is configured.
    NotConfigured,
    /// The path is relative or has a `.` or `..` component.
    RelativePath,
    /// The path is under `/nix/store`.
    InStore,
    /// The file does not exist.
    Missing,
    /// The path is a symbolic link.
    Symlink,
    /// The file could not be opened or read.
    Unreadable,
    /// Not a regular file.
    NotRegularFile,
    /// Owned by a UID other than the effective UID.
    WrongOwner,
    /// Group or other permission bits are set.
    PermissionsTooOpen {
        /// The observed permission bits.
        mode: u32,
    },
    /// More than 4096 bytes.
    TooLarge,
    /// Empty after removing one trailing newline.
    Empty,
    /// A byte outside visible ASCII.
    Malformed,
    /// An invalid systemd credential name.
    InvalidName,
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured => f.write_str("credential: no source configured"),
            Self::RelativePath => {
                f.write_str("credential file: path must be absolute and normalized")
            }
            Self::InStore => f.write_str("credential file: path is in the Nix store"),
            Self::Missing => f.write_str("credential file: missing"),
            Self::Symlink => f.write_str("credential file: symbolic link refused"),
            Self::Unreadable => f.write_str("credential file: unreadable"),
            Self::NotRegularFile => f.write_str("credential file: not a regular file"),
            Self::WrongOwner => f.write_str("credential file: owned by another user"),
            Self::PermissionsTooOpen { mode } => write!(
                f,
                "credential file: mode {mode:04o} grants group or other access"
            ),
            Self::TooLarge => f.write_str("credential file: larger than 4096 bytes"),
            Self::Empty => f.write_str("credential file: empty"),
            Self::Malformed => f.write_str("credential file: content is not visible ASCII"),
            Self::InvalidName => f.write_str("systemd credential: invalid name"),
        }
    }
}

impl std::error::Error for CredentialError {}
