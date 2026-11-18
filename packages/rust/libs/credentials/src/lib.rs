//! Runtime credential sources for the Stockhouse client.
//!
//! Credentials come only from an explicitly configured source: a systemd
//! credential, an owner-only file or (built by `stockhouse-auth`) the
//! signed-in server token store. Configuration names where a credential
//! lives, never its value.

mod error;
mod file;
mod secret;

pub use error::CredentialError;
pub use file::{OwnerOnlyFile, SystemdCredential};
pub use secret::Secret;

/// A source that yields the current credential on every call.
///
/// `credential` may block (file reads, a token refresh); async callers run it
/// on a blocking thread.
pub trait CredentialSource: Send + Sync {
    /// Reads the credential now.
    fn credential(&self) -> Result<Secret, CredentialError>;

    /// Reports that `rejected` was refused with 401. Returns whether a
    /// different credential may now be available, so one retry is
    /// worthwhile. Static sources cannot refresh and return `false`.
    fn invalidate(&self, rejected: &Secret) -> bool {
        let _ = rejected;
        false
    }
}

/// Where a configured credential lives: `{ file = … }`, `{ systemd = … }`
/// or `{ server = … }`, shared by every configuration that names one.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum CredentialSpec {
    /// An owner-only file at an absolute path.
    File(std::path::PathBuf),
    /// A systemd credential name under `$CREDENTIALS_DIRECTORY`.
    Systemd(String),
    /// The signed-in server token store, configured by this sign-in file.
    Server(std::path::PathBuf),
}

/// Builds the source for an optional spec; absence is `NotConfigured`.
///
/// A `Server` spec needs `stockhouse_auth::source`; here it yields a source
/// that fails with `Unsupported`, so miswiring is visible.
pub fn source(spec: Option<&CredentialSpec>) -> Box<dyn CredentialSource> {
    match spec {
        Some(CredentialSpec::File(path)) => Box::new(OwnerOnlyFile::new(path.clone())),
        Some(CredentialSpec::Systemd(name)) => Box::new(SystemdCredential::new(name.clone())),
        Some(CredentialSpec::Server(_)) => Box::new(Failing(CredentialError::Unsupported)),
        None => Box::new(Failing(CredentialError::NotConfigured)),
    }
}

struct Failing(CredentialError);

impl CredentialSource for Failing {
    fn credential(&self) -> Result<Secret, CredentialError> {
        Err(self.0)
    }
}

/// A fixed credential for tests. Configuration cannot produce one.
#[derive(Clone, Debug)]
pub struct StaticCredential(Vec<u8>);

impl StaticCredential {
    /// Wraps a test value.
    pub fn new(value: &[u8]) -> Self {
        Self(value.to_vec())
    }
}

impl CredentialSource for StaticCredential {
    fn credential(&self) -> Result<Secret, CredentialError> {
        Ok(Secret::new(self.0.clone()))
    }
}
