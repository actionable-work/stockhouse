//! Server sign-in for the Stockhouse client: the sign-in configuration, the
//! OAuth 2.0 token and revocation endpoint client, the refreshing
//! [`ServerCredential`], logout, status and the credential source factory.

mod client;
mod config;
mod credential;
mod outcome;
mod session;

pub use client::{MAX_BODY, Reply, block_on, form, post_form};
pub use config::{AuthConfig, AuthConfigError, Endpoints, StoreKind, secure_url};
pub use credential::{BACKOFF_SECS, INVALIDATE_GUARD_SECS, SKEW_SECS, ServerCredential};
pub use outcome::{Failure, OAuthCode, Transient, classify_error, token_outcome};
pub use session::{LogoutOutcome, Replaced, Status, logout, revoke, status, store_login};

use std::sync::Arc;
use stockhouse_credentials::{CredentialSource, CredentialSpec};

/// Unix time in seconds.
pub trait Clock: Send + Sync {
    /// Now.
    fn now(&self) -> u64;
}

/// The system clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
}

/// A built credential source.
pub enum Source {
    /// A static file or systemd credential.
    Static(Box<dyn CredentialSource>),
    /// The signed-in server credential.
    Server(Arc<ServerCredential>),
}

impl Source {
    /// The source as a trait object.
    pub fn shared(self) -> Arc<dyn CredentialSource> {
        match self {
            Self::Static(source) => Arc::from(source),
            Self::Server(source) => source,
        }
    }
}

/// Builds the source for any credential spec; `Server` loads its sign-in
/// configuration.
pub fn source(spec: Option<&CredentialSpec>) -> Result<Source, AuthConfigError> {
    match spec {
        Some(CredentialSpec::Server(path)) => {
            let config = Arc::new(AuthConfig::load(path)?);
            Ok(Source::Server(Arc::new(ServerCredential::new(
                config,
                Arc::new(SystemClock),
            ))))
        }
        other => Ok(Source::Static(stockhouse_credentials::source(other))),
    }
}
