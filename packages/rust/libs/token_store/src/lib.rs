//! The Stockhouse token store: one signed-in session, sealed with
//! XChaCha20-Poly1305 under a systemd credential key (or, only when
//! configured, an owner-only plaintext file), written atomically and
//! serialized across processes with an exclusive lock. No network code.

mod error;
mod fsx;
mod lock;
mod seal;
mod set;
mod store;

pub use error::StoreError;
pub use lock::Lock;
pub use seal::{Key, MAGIC};
pub use set::{Binding, MAX_TOKEN_BYTES, TokenSet, valid_token};
pub use store::{FILE_NAME, SEALED_NAME, Store, StoreMode};

/// Fills `buf` from the operating system's random source.
pub fn random(buf: &mut [u8]) -> Result<(), StoreError> {
    getrandom::fill(buf).map_err(|_| StoreError::Io { op: "random" })
}
