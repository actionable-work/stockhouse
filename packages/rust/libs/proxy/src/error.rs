use crate::config::TierName;
use crate::upstream::UpstreamError;
use http::StatusCode;
use std::fmt;
use std::net::SocketAddr;
use stockhouse_credentials::CredentialError;
use stockhouse_proto::{CacheInfoError, NarInfoError, SignatureError};

/// A lookup or NAR failure. None of these is ever reported as a miss.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProxyError {
    /// The tier credential could not be resolved; nothing was sent.
    Credential {
        /// The tier.
        tier: TierName,
        /// Why.
        error: CredentialError,
    },
    /// The tier answered 401/403 or a file permission error.
    Denied {
        /// The tier.
        tier: TierName,
    },
    /// The tier failed with another status, a redirect, a transport error,
    /// a timeout or an oversized body.
    Unavailable {
        /// The tier.
        tier: TierName,
        /// Why.
        reason: UpstreamError,
    },
    /// The tier has no nix-cache-info.
    CacheInfoMissing {
        /// The tier.
        tier: TierName,
    },
    /// The tier's nix-cache-info is invalid.
    CacheInfo {
        /// The tier.
        tier: TierName,
        /// Why.
        error: CacheInfoError,
    },
    /// The tier's narinfo is invalid.
    NarInfo {
        /// The tier.
        tier: TierName,
        /// Why.
        error: NarInfoError,
    },
    /// The narinfo names a different store path hash.
    StorePathMismatch {
        /// The tier.
        tier: TierName,
    },
    /// Signature verification failed.
    Signature {
        /// The tier.
        tier: TierName,
        /// Why.
        error: SignatureError,
    },
    /// The tier no longer holds a path or NAR it was routed to.
    NarMissing {
        /// The tier.
        tier: TierName,
    },
    /// The narinfo URL changed since it was served.
    NarInfoChanged {
        /// The tier.
        tier: TierName,
    },
}

impl ProxyError {
    /// The tier the error belongs to.
    pub fn tier(&self) -> TierName {
        match self {
            Self::Credential { tier, .. }
            | Self::Denied { tier }
            | Self::Unavailable { tier, .. }
            | Self::CacheInfoMissing { tier }
            | Self::CacheInfo { tier, .. }
            | Self::NarInfo { tier, .. }
            | Self::StorePathMismatch { tier }
            | Self::Signature { tier, .. }
            | Self::NarMissing { tier }
            | Self::NarInfoChanged { tier } => *tier,
        }
    }

    /// A short stable kind for logs and error bodies.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Credential { .. } => "credential",
            Self::Denied { .. } => "denied",
            Self::Unavailable { .. } => "unavailable",
            Self::CacheInfoMissing { .. } => "cache-info-missing",
            Self::CacheInfo { .. } => "cache-info-invalid",
            Self::NarInfo { .. } => "narinfo-invalid",
            Self::StorePathMismatch { .. } => "store-path-mismatch",
            Self::Signature { .. } => "signature",
            Self::NarMissing { .. } => "nar-missing",
            Self::NarInfoChanged { .. } => "narinfo-changed",
        }
    }

    /// 503 for credential, denial and availability; 502 otherwise.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Credential { .. } | Self::Denied { .. } | Self::Unavailable { .. } => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            _ => StatusCode::BAD_GATEWAY,
        }
    }
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} tier: {}", self.tier(), self.kind())
    }
}

impl std::error::Error for ProxyError {}

/// The proxy could not start serving.
#[derive(Debug)]
#[non_exhaustive]
pub enum ServeError {
    /// The configured address is in use; no other port is tried.
    AddressInUse {
        /// The address.
        addr: SocketAddr,
    },
    /// Binding failed for another reason.
    Bind {
        /// The address.
        addr: SocketAddr,
        /// The OS error.
        error: std::io::Error,
    },
    /// The HTTP client for a tier could not be built.
    Client(String),
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AddressInUse { addr } => write!(f, "listen address {addr} is already in use"),
            Self::Bind { addr, error } => write!(f, "cannot listen on {addr}: {error}"),
            Self::Client(message) => write!(f, "cannot build upstream client: {message}"),
        }
    }
}

impl std::error::Error for ServeError {}
