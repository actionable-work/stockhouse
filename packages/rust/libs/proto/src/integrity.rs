use crate::hash::Sha256;
use crate::narinfo::{Compression, NarInfo};
use sha2::Digest;
use std::fmt;

/// A served-representation consistency failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum IntegrityError {
    /// More bytes than expected arrived.
    SizeExceeded {
        /// Expected size.
        expected: u64,
    },
    /// Fewer bytes than expected arrived.
    SizeMismatch {
        /// Expected size.
        expected: u64,
        /// Observed size.
        actual: u64,
    },
    /// The SHA-256 differs.
    HashMismatch,
}

impl fmt::Display for IntegrityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SizeExceeded { expected } => write!(f, "content exceeds expected {expected} bytes"),
            Self::SizeMismatch { expected, actual } => write!(f, "content is {actual} bytes, expected {expected}"),
            Self::HashMismatch => f.write_str("content hash mismatch"),
        }
    }
}

impl std::error::Error for IntegrityError {}

/// What a proxy can check over the served representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServedIntegrity {
    /// Expected byte count.
    pub size: u64,
    /// Expected SHA-256.
    pub hash: Sha256,
}

impl NarInfo {
    /// `FileHash`/`FileSize`, else `NarHash`/`NarSize` when uncompressed, else none.
    pub fn served_integrity(&self) -> Option<ServedIntegrity> {
        match (&self.file, self.compression) {
            (Some(file), _) => Some(ServedIntegrity { size: file.size, hash: file.hash }),
            (None, Compression::None) => Some(ServedIntegrity { size: self.nar_size, hash: self.nar_hash }),
            (None, _) => None,
        }
    }
}

/// Incremental size and SHA-256 check.
#[derive(Clone, Debug)]
pub struct ContentVerifier {
    expected: ServedIntegrity,
    seen: u64,
    hasher: sha2::Sha256,
}

impl ContentVerifier {
    /// Starts verifying against an expected size and hash.
    pub fn new(expected: ServedIntegrity) -> Self {
        Self { expected, seen: 0, hasher: sha2::Sha256::new() }
    }

    /// Consumes a chunk, failing as soon as the size is exceeded.
    pub fn update(&mut self, chunk: &[u8]) -> Result<(), IntegrityError> {
        self.seen = self.seen.saturating_add(chunk.len() as u64);
        if self.seen > self.expected.size {
            return Err(IntegrityError::SizeExceeded { expected: self.expected.size });
        }
        self.hasher.update(chunk);
        Ok(())
    }

    /// Bytes consumed so far.
    pub fn seen(&self) -> u64 {
        self.seen
    }

    /// Finishes, checking size and hash.
    pub fn finish(self) -> Result<(), IntegrityError> {
        if self.seen != self.expected.size {
            return Err(IntegrityError::SizeMismatch { expected: self.expected.size, actual: self.seen });
        }
        let digest: [u8; 32] = self.hasher.finalize().into();
        if Sha256::from_bytes(digest) == self.expected.hash { Ok(()) } else { Err(IntegrityError::HashMismatch) }
    }
}
