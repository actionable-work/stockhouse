use crate::{InvalidCredential, RequestFailure, ServerEndpoint};
use reqwest::Method;
use std::{fmt, sync::Mutex};
use stockhouse_credentials::{CredentialError, Secret};
use stockhouse_wire_v2::{ProbeReply, ProtocolMode, VersionError, probe_path, select_version};

/// Lifetime of a successful version selection in the uploader process.
pub const PROBE_TTL_SECS: u64 = 3600;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NegotiationFailure {
    Credential(CredentialError),
    InvalidCredential(InvalidCredential),
    Unauthorized,
    Busy(Option<u32>),
    Unavailable(Option<u32>),
    Request(RequestFailure),
    Version(VersionError),
}

impl NegotiationFailure {
    pub fn is_credential_error(self) -> bool {
        matches!(self, Self::Credential(_) | Self::InvalidCredential(_))
    }
}

impl fmt::Display for NegotiationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Credential(error) => write!(f, "credential unavailable: {error}"),
            Self::InvalidCredential(error) => write!(f, "credential unavailable: {error}"),
            Self::Unauthorized => f.write_str("unauthorized"),
            Self::Busy(_) => f.write_str("protocol probe: busy"),
            Self::Unavailable(_) => f.write_str("protocol probe: unavailable"),
            Self::Request(error) => write!(f, "protocol probe: {error}"),
            Self::Version(error) => error.fmt(f),
        }
    }
}

/// Remembers the version selected for one server endpoint.
pub struct Negotiator {
    mode: ProtocolMode,
    cached: Mutex<Option<(u16, u64)>>,
}

impl Negotiator {
    pub fn new(mode: ProtocolMode) -> Self {
        Self {
            mode,
            cached: Mutex::new(None),
        }
    }

    /// Returns a fresh cached selection or probes using the current credential.
    pub fn version(&self, endpoint: &ServerEndpoint, now: u64) -> Result<u16, NegotiationFailure> {
        self.select(now, || {
            let mut secret = endpoint
                .credential()
                .map_err(NegotiationFailure::Credential)?;
            offered(endpoint, &mut secret)
        })
    }

    /// Like `version`, using the credential already read at the start of a lease run.
    pub fn version_with(
        &self,
        endpoint: &ServerEndpoint,
        now: u64,
        secret: &mut Secret,
    ) -> Result<u16, NegotiationFailure> {
        self.select(now, || offered(endpoint, secret))
    }

    /// Drops a selection after a v2 route is withdrawn.
    pub fn forget(&self) {
        *self.cached.lock().expect("version cache lock") = None;
    }

    fn select(
        &self,
        now: u64,
        probe: impl FnOnce() -> Result<Vec<u16>, NegotiationFailure>,
    ) -> Result<u16, NegotiationFailure> {
        if self.mode == ProtocolMode::V1 {
            return Ok(1);
        }
        let mut cached = self.cached.lock().expect("version cache lock");
        if let Some((version, since)) = *cached
            && now.saturating_sub(since) < PROBE_TTL_SECS
        {
            return Ok(version);
        }
        *cached = None;
        let version = select_version(self.mode, &probe()?).map_err(NegotiationFailure::Version)?;
        *cached = Some((version, now));
        Ok(version)
    }
}

fn probe(endpoint: &ServerEndpoint, secret: &Secret) -> Result<ProbeReply, NegotiationFailure> {
    let request = endpoint
        .request(Method::GET, probe_path(), secret, endpoint.timeout())
        .map_err(NegotiationFailure::InvalidCredential)?;
    endpoint
        .probe_reply(request)
        .map_err(NegotiationFailure::Request)
}

fn offered(endpoint: &ServerEndpoint, secret: &mut Secret) -> Result<Vec<u16>, NegotiationFailure> {
    let mut reply = probe(endpoint, secret)?;
    if reply == ProbeReply::Unauthorized && endpoint.invalidate(secret) {
        *secret = endpoint
            .credential()
            .map_err(NegotiationFailure::Credential)?;
        reply = probe(endpoint, secret)?;
    }
    match reply {
        ProbeReply::Offered(versions) => Ok(versions),
        ProbeReply::Unauthorized => Err(NegotiationFailure::Unauthorized),
        ProbeReply::Busy { retry_after } => Err(NegotiationFailure::Busy(retry_after)),
        ProbeReply::Unavailable { retry_after } => {
            Err(NegotiationFailure::Unavailable(retry_after))
        }
    }
}
