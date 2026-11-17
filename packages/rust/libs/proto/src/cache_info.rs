use crate::lines::{self, CollectError};
use crate::store_path::STORE_DIR;
use std::fmt;

const MAX_CACHE_INFO_BYTES: usize = 4096;
const KNOWN: &[&str] = &["StoreDir", "WantMassQuery", "Priority"];

/// Why a `nix-cache-info` was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CacheInfoError {
    /// Larger than 4 KiB.
    TooLarge,
    /// Not valid UTF-8.
    NotUtf8,
    /// A non-empty line without a separator or key.
    MalformedLine {
        /// One-based line number.
        line: usize,
    },
    /// An unknown key.
    UnknownField(String),
    /// A repeated key.
    DuplicateField(&'static str),
    /// `StoreDir` is absent.
    MissingField(&'static str),
    /// A value violates its rule.
    InvalidField(&'static str),
    /// `StoreDir` is not `/nix/store`.
    StoreDirMismatch {
        /// The declared directory.
        found: String,
    },
}

impl fmt::Display for CacheInfoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str("nix-cache-info is too large"),
            Self::NotUtf8 => f.write_str("nix-cache-info is not UTF-8"),
            Self::MalformedLine { line } => write!(f, "malformed nix-cache-info line {line}"),
            Self::UnknownField(key) => write!(f, "unknown nix-cache-info field {key:?}"),
            Self::DuplicateField(key) => write!(f, "duplicate nix-cache-info field {key}"),
            Self::MissingField(key) => write!(f, "missing nix-cache-info field {key}"),
            Self::InvalidField(key) => write!(f, "invalid nix-cache-info field {key}"),
            Self::StoreDirMismatch { found } => write!(f, "foreign store directory {found:?}"),
        }
    }
}

impl std::error::Error for CacheInfoError {}

impl From<CollectError> for CacheInfoError {
    fn from(error: CollectError) -> Self {
        match error {
            CollectError::Malformed(line) => Self::MalformedLine { line },
            CollectError::Unknown(key) => Self::UnknownField(key),
            CollectError::Duplicate(key) => Self::DuplicateField(key),
        }
    }
}

/// A parsed `nix-cache-info` for `/nix/store`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NixCacheInfo {
    /// `WantMassQuery`.
    pub want_mass_query: Option<bool>,
    /// `Priority`.
    pub priority: Option<u32>,
}

impl NixCacheInfo {
    /// Parses `nix-cache-info` bytes strictly.
    pub fn parse(input: &[u8]) -> Result<Self, CacheInfoError> {
        if input.len() > MAX_CACHE_INFO_BYTES {
            return Err(CacheInfoError::TooLarge);
        }
        let text = std::str::from_utf8(input).map_err(|_| CacheInfoError::NotUtf8)?;
        let fields = lines::collect(text, KNOWN, None)?;
        let store_dir = fields.get("StoreDir").ok_or(CacheInfoError::MissingField("StoreDir"))?;
        if store_dir != STORE_DIR {
            return Err(CacheInfoError::StoreDirMismatch { found: store_dir.to_owned() });
        }
        let want_mass_query = fields.get("WantMassQuery").map(parse_flag).transpose()?;
        let priority = fields.get("Priority").map(parse_priority).transpose()?;
        Ok(Self { want_mass_query, priority })
    }

    /// Renders `nix-cache-info` text.
    pub fn render(&self) -> String {
        let mut out = format!("StoreDir: {STORE_DIR}\n");
        if let Some(flag) = self.want_mass_query {
            out.push_str(&format!("WantMassQuery: {}\n", u8::from(flag)));
        }
        if let Some(priority) = self.priority {
            out.push_str(&format!("Priority: {priority}\n"));
        }
        out
    }
}

fn parse_flag(text: &str) -> Result<bool, CacheInfoError> {
    match text {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(CacheInfoError::InvalidField("WantMassQuery")),
    }
}

fn parse_priority(text: &str) -> Result<u32, CacheInfoError> {
    let value = lines::parse_decimal(text).and_then(|n| u32::try_from(n).ok());
    value.ok_or(CacheInfoError::InvalidField("Priority"))
}
