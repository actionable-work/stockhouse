use std::fmt;

/// The public client protocol version for client-owned formats.
pub const PROTOCOL_VERSION: u32 = 1;

/// A client-owned format declared a version this build does not read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnsupportedVersion {
    /// The version found in the input.
    pub found: u32,
    /// The only version this build supports.
    pub supported: u32,
}

impl fmt::Display for UnsupportedVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unsupported version {} (supported: {})",
            self.found, self.supported
        )
    }
}

impl std::error::Error for UnsupportedVersion {}

/// Accepts exactly [`PROTOCOL_VERSION`].
pub fn check_version(found: u32) -> Result<(), UnsupportedVersion> {
    if found == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(UnsupportedVersion {
            found,
            supported: PROTOCOL_VERSION,
        })
    }
}
