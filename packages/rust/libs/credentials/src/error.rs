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
    /// Not signed in: no tokens, the grant ended, or expired without a
    /// refresh token.
    SignedOut,
    /// The access token expired and a refresh failed transiently.
    RefreshUnavailable,
    /// The token endpoint refused the refresh (not `invalid_grant`).
    RefreshRejected,
    /// The token store failed an ownership, mode, type or size check.
    StoreUnsafe,
    /// The token store could not be decrypted or parsed.
    StoreCorrupt,
    /// The token store was written for another server configuration.
    StoreMismatch,
    /// The sealing key credential is not loaded.
    KeyUnavailable,
    /// The sealing key credential is malformed.
    KeyMalformed,
    /// The sign-in configuration could not be loaded.
    AuthConfig,
    /// This resolver cannot build the requested kind of source.
    Unsupported,
}

impl CredentialError {
    /// A short stable kind for log lines.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotConfigured => "not-configured",
            Self::RelativePath => "relative-path",
            Self::InStore => "in-store",
            Self::Missing => "missing",
            Self::Symlink => "symlink",
            Self::Unreadable => "unreadable",
            Self::NotRegularFile => "not-regular-file",
            Self::WrongOwner => "wrong-owner",
            Self::PermissionsTooOpen { .. } => "permissions-too-open",
            Self::TooLarge => "too-large",
            Self::Empty => "empty",
            Self::Malformed => "malformed",
            Self::InvalidName => "invalid-name",
            Self::SignedOut => "signed-out",
            Self::RefreshUnavailable => "refresh-unavailable",
            Self::RefreshRejected => "refresh-rejected",
            Self::StoreUnsafe => "store-unsafe",
            Self::StoreCorrupt => "store-corrupt",
            Self::StoreMismatch => "store-mismatch",
            Self::KeyUnavailable => "key-unavailable",
            Self::KeyMalformed => "key-malformed",
            Self::AuthConfig => "auth-config",
            Self::Unsupported => "unsupported",
        }
    }
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
            Self::SignedOut => f.write_str("server credential: not signed in"),
            Self::RefreshUnavailable => {
                f.write_str("server credential: refresh temporarily unavailable")
            }
            Self::RefreshRejected => f.write_str("server credential: refresh refused"),
            Self::StoreUnsafe => f.write_str("token store: unsafe ownership or permissions"),
            Self::StoreCorrupt => f.write_str("token store: unreadable or corrupt"),
            Self::StoreMismatch => f.write_str("token store: written for another server"),
            Self::KeyUnavailable => f.write_str("token store: sealing key not loaded"),
            Self::KeyMalformed => f.write_str("token store: sealing key malformed"),
            Self::AuthConfig => f.write_str("server credential: sign-in configuration invalid"),
            Self::Unsupported => f.write_str("credential: source kind not supported here"),
        }
    }
}

impl std::error::Error for CredentialError {}
