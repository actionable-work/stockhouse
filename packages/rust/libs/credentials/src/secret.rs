use std::fmt;
use zeroize::Zeroize;

/// A credential value. Formatting is redacted; the buffer is zeroed on drop.
/// Copies made by an HTTP client for a header are not tracked.
pub struct Secret(Vec<u8>);

impl Secret {
    /// Wraps credential bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Explicitly exposes the value, for building an `Authorization` header.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}
