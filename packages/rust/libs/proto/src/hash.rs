use crate::nix32;
use std::fmt;

/// Why a hash string was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum HashError {
    /// The prefix is not `sha256:`.
    UnsupportedAlgorithm,
    /// Wrong length, bad characters or a non-canonical base-32 value.
    InvalidEncoding,
}

impl fmt::Display for HashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnsupportedAlgorithm => "unsupported hash algorithm",
            Self::InvalidEncoding => "invalid hash encoding",
        })
    }
}

impl std::error::Error for HashError {}

/// A SHA-256 digest, rendered canonically as `sha256:<nix32>`.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct Sha256([u8; 32]);

impl Sha256 {
    /// Wraps raw digest bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw digest bytes.
    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Hashes `data`.
    pub fn digest(data: &[u8]) -> Self {
        use sha2::Digest;
        Self(sha2::Sha256::digest(data).into())
    }

    /// Parses `sha256:<52 nix32>` or `sha256:<64 lowercase hex>`.
    pub fn parse(text: &str) -> Result<Self, HashError> {
        let body = text
            .strip_prefix("sha256:")
            .ok_or(HashError::UnsupportedAlgorithm)?;
        let bytes = match body.len() {
            64 => decode_hex(body),
            _ => nix32::decode::<32>(body),
        };
        bytes.map(Self).ok_or(HashError::InvalidEncoding)
    }

    /// Parses the SRI form `sha256-<44 padded base64 chars>`.
    pub fn parse_sri(text: &str) -> Result<Self, HashError> {
        use base64::Engine;
        let body = text
            .strip_prefix("sha256-")
            .ok_or(HashError::UnsupportedAlgorithm)?;
        if body.len() != 44 {
            return Err(HashError::InvalidEncoding);
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(body)
            .map_err(|_| HashError::InvalidEncoding)?;
        let digest: [u8; 32] = bytes.try_into().map_err(|_| HashError::InvalidEncoding)?;
        Ok(Self(digest))
    }
}

fn decode_hex(text: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        *slot = hex_digit(pair[0])? << 4 | hex_digit(pair[1])?;
    }
    Some(out)
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

impl fmt::Display for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", nix32::encode(&self.0))
    }
}

impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
