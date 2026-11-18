//! Runtime credential sources for the Stockhouse client.
//!
//! Credentials come only from an explicitly configured source: a systemd
//! credential or an owner-only file. Configuration names where a credential
//! lives, never its value.

mod error;
mod file;
mod grant;
mod secret;

pub use error::CredentialError;
pub use file::{OwnerOnlyFile, SystemdCredential};
pub use grant::{DeferredGrantFlow, GrantError, GrantFlow};
pub use secret::Secret;

/// A source that yields the current credential on every call.
pub trait CredentialSource: Send + Sync {
    /// Reads the credential now.
    fn credential(&self) -> Result<Secret, CredentialError>;
}

/// Where a configured credential lives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CredentialSpec {
    /// An owner-only file at an absolute path.
    File(std::path::PathBuf),
    /// A systemd credential name under `$CREDENTIALS_DIRECTORY`.
    Systemd(String),
}

/// Builds the source for an optional spec; absence is `NotConfigured`.
pub fn source(spec: Option<&CredentialSpec>) -> Box<dyn CredentialSource> {
    match spec {
        Some(CredentialSpec::File(path)) => Box::new(OwnerOnlyFile::new(path.clone())),
        Some(CredentialSpec::Systemd(name)) => Box::new(SystemdCredential::new(name.clone())),
        None => Box::new(Unconfigured),
    }
}

struct Unconfigured;

impl CredentialSource for Unconfigured {
    fn credential(&self) -> Result<Secret, CredentialError> {
        Err(CredentialError::NotConfigured)
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
