use std::fmt;
use uuid::Uuid;

/// The text is not a canonical lowercase UUIDv7.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum UploadIdError {
    /// Not a lowercase hyphenated version 7 UUID.
    Invalid,
}

impl fmt::Display for UploadIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("upload identity is not a canonical UUIDv7")
    }
}

impl std::error::Error for UploadIdError {}

/// An opaque upload identity (UUIDv7). It carries no authorization.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UploadId(Uuid);

impl UploadId {
    /// Generates a new identity.
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }

    /// Parses the canonical lowercase hyphenated form of a version 7 UUID.
    pub fn parse(text: &str) -> Result<Self, UploadIdError> {
        let uuid = Uuid::try_parse(text).map_err(|_| UploadIdError::Invalid)?;
        let canonical = uuid.hyphenated().to_string() == text;
        if canonical && uuid.get_version_num() == 7 {
            Ok(Self(uuid))
        } else {
            Err(UploadIdError::Invalid)
        }
    }
}

impl fmt::Display for UploadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.hyphenated())
    }
}
