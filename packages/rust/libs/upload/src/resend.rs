use crate::{InvalidCredential, RequestFailure, ServerEndpoint};
use reqwest::{Method, header::CONTENT_TYPE};
use std::{fmt, time::Duration};
use stockhouse_credentials::{CredentialError, Secret};
use stockhouse_wire_v2::{
    BatchKind, BatchReply, EntryKey, declare_path, leases_path, publish_path, status_path,
};

/// One immutable POST batch, including the keys needed to validate the reply.
pub struct BatchRequest {
    pub kind: BatchKind,
    pub keys: Vec<EntryKey>,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchFailure {
    Credential(CredentialError),
    InvalidCredential(InvalidCredential),
    Unauthorized,
    Busy,
    Unavailable,
    Request(RequestFailure),
}

impl BatchFailure {
    pub fn is_credential_error(self) -> bool {
        matches!(self, Self::Credential(_) | Self::InvalidCredential(_))
    }

    /// Whether the request may have been applied without a confirmed reply.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::Busy | Self::Unavailable | Self::Request(RequestFailure::Transport)
        )
    }
}

impl fmt::Display for BatchFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Credential(error) => write!(f, "credential unavailable: {error}"),
            Self::InvalidCredential(error) => write!(f, "credential unavailable: {error}"),
            Self::Unauthorized => f.write_str("unauthorized"),
            Self::Busy => f.write_str("busy"),
            Self::Unavailable => f.write_str("unavailable"),
            Self::Request(error) => error.fmt(f),
        }
    }
}

impl BatchRequest {
    /// Send once and perform the bounded, sequential 401 and availability resends.
    pub fn send(
        &self,
        endpoint: &ServerEndpoint,
        secret: &mut Secret,
        sleep: &dyn Fn(Duration),
    ) -> Result<BatchReply, BatchFailure> {
        self.send_with(endpoint, secret, sleep, &|sent| endpoint.invalidate(sent))
    }

    /// Use a shared invalidation policy for concurrent transfers and batches.
    pub fn send_with(
        &self,
        endpoint: &ServerEndpoint,
        secret: &mut Secret,
        sleep: &dyn Fn(Duration),
        invalidate: &dyn Fn(&Secret) -> bool,
    ) -> Result<BatchReply, BatchFailure> {
        let mut refreshed = false;
        let mut delays = [2, 4].into_iter();
        loop {
            let (failure, retry_after) = match self.once(endpoint, secret) {
                Ok(BatchReply::Unauthorized) => {
                    if refreshed || !invalidate(secret) {
                        return Err(BatchFailure::Unauthorized);
                    }
                    refreshed = true;
                    *secret = endpoint.credential().map_err(BatchFailure::Credential)?;
                    continue;
                }
                Ok(BatchReply::Busy { retry_after }) => (BatchFailure::Busy, retry_after),
                Ok(BatchReply::Unavailable { retry_after }) => {
                    (BatchFailure::Unavailable, retry_after)
                }
                Ok(reply) => return Ok(reply),
                Err(failure) if failure.retryable() => (failure, None),
                Err(failure) => return Err(failure),
            };
            let Some(default) = delays.next() else {
                return Err(failure);
            };
            sleep(Duration::from_secs(u64::from(
                retry_after.map_or(default, |seconds| seconds.min(30)),
            )));
        }
    }

    fn once(&self, endpoint: &ServerEndpoint, secret: &Secret) -> Result<BatchReply, BatchFailure> {
        let path = match self.kind {
            BatchKind::Declare => declare_path(),
            BatchKind::Publish => publish_path(),
            BatchKind::Status => status_path(),
            BatchKind::Leases => leases_path(),
        };
        let request = endpoint
            .request(Method::POST, path, secret, endpoint.timeout())
            .map_err(BatchFailure::InvalidCredential)?
            .header(CONTENT_TYPE, "application/json")
            .body(self.body.clone());
        endpoint
            .batch_reply(request, self.kind, &self.keys)
            .map_err(BatchFailure::Request)
    }
}
