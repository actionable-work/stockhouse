use crate::nix32;
use std::fmt;

/// The only supported Nix store directory.
pub const STORE_DIR: &str = "/nix/store";
const HASH_LEN: usize = 32;
const MAX_NAME_LEN: usize = 211;

/// Why a store path, basename or hash was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum StorePathError {
    /// The path is not directly under `/nix/store/`.
    NotInStore,
    /// The hash part is not 32 Nix base-32 characters.
    InvalidHash,
    /// The name part violates Nix's name rules.
    InvalidName,
}

impl fmt::Display for StorePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotInStore => "path is not in /nix/store",
            Self::InvalidHash => "invalid store path hash",
            Self::InvalidName => "invalid store path name",
        })
    }
}

impl std::error::Error for StorePathError {}

/// The 32-character hash part of a store path.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StorePathHash(String);

impl StorePathHash {
    /// Parses a bare store path hash.
    pub fn parse(text: &str) -> Result<Self, StorePathError> {
        let valid = text.len() == HASH_LEN && text.bytes().all(|c| nix32::digit(c).is_some());
        if valid {
            Ok(Self(text.to_owned()))
        } else {
            Err(StorePathError::InvalidHash)
        }
    }

    /// The hash text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StorePathHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A validated path `/nix/store/<hash>-<name>`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StorePath {
    full: String,
}

impl StorePath {
    /// Parses a full store path.
    pub fn parse(text: &str) -> Result<Self, StorePathError> {
        let base = text
            .strip_prefix(STORE_DIR)
            .and_then(|rest| rest.strip_prefix('/'))
            .ok_or(StorePathError::NotInStore)?;
        Self::from_basename(base)
    }

    /// Parses a `<hash>-<name>` basename.
    pub fn from_basename(base: &str) -> Result<Self, StorePathError> {
        let hash = base.get(..HASH_LEN).ok_or(StorePathError::InvalidHash)?;
        StorePathHash::parse(hash)?;
        let name = base[HASH_LEN..]
            .strip_prefix('-')
            .ok_or(StorePathError::InvalidHash)?;
        validate_name(name)?;
        Ok(Self {
            full: format!("{STORE_DIR}/{base}"),
        })
    }

    /// The full path text.
    pub fn as_str(&self) -> &str {
        &self.full
    }

    /// The `<hash>-<name>` basename.
    pub fn basename(&self) -> &str {
        &self.full[STORE_DIR.len() + 1..]
    }

    /// The hash part.
    pub fn hash(&self) -> StorePathHash {
        StorePathHash(self.basename()[..HASH_LEN].to_owned())
    }

    /// The name part.
    pub fn name(&self) -> &str {
        &self.basename()[HASH_LEN + 1..]
    }
}

fn validate_name(name: &str) -> Result<(), StorePathError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"+-._?=".contains(&c));
    if ok {
        Ok(())
    } else {
        Err(StorePathError::InvalidName)
    }
}

impl fmt::Display for StorePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.full)
    }
}
