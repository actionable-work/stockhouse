//! The loopback-only Stockhouse substitution proxy.
//!
//! The proxy serves a Nix binary-cache interface on loopback and looks paths
//! up in the private, shared, then public tier. It verifies narinfo
//! signatures and served bytes, keeps each tier's credential on that tier,
//! and lets only a genuine miss fall through to the next tier.

mod body;
mod config;
mod error;
mod lookup;
mod nar;
mod routes;
mod server;
mod upstream;

pub use config::{ConfigError, ProxyConfig, TierConfig, TierName, TierUrl};
pub use error::{ProxyError, ServeError};
pub use lookup::{Lookup, Tier};
pub use server::{BoundServer, LogSink, bind, stderr_log};
pub use upstream::{Fetched, UpstreamError};
