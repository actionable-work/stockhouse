//! Versioned public types for the Stockhouse cache client.
//!
//! This crate encodes the public Nix binary-cache facts the client relies on:
//! store paths, hashes, narinfo, `nix-cache-info`, Ed25519 narinfo signatures
//! and incremental content integrity. The upload and lease protocol v1
//! codecs, which build on these types, live in `stockhouse-wire`. It
//! performs no I/O and has no dependency on any server implementation.

mod cache_info;
mod hash;
mod integrity;
mod lines;
mod narinfo;
mod narinfo_fields;
pub mod nix32;
mod signature;
mod store_path;
mod upload_id;
mod version;

pub use cache_info::{CacheInfoError, NixCacheInfo};
pub use hash::{HashError, Sha256};
pub use integrity::{ContentVerifier, IntegrityError, ServedIntegrity};
pub use narinfo::{Compression, Deriver, FileInfo, NarInfo, NarInfoError};
pub use signature::{KeyError, PublicKey, Signature, SignatureError, TrustedKeys};
pub use store_path::{STORE_DIR, StorePath, StorePathError, StorePathHash};
pub use upload_id::{UploadId, UploadIdError};
pub use version::{PROTOCOL_VERSION, UnsupportedVersion, check_version};
