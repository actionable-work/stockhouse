//! Authenticated server upload for the Stockhouse client.
//!
//! Implements the protocol v1 `server` backend behind
//! `stockhouse_ingest::Uploader`, which uploads each queued path's
//! closure references first. Credentials come from a `CredentialSource`
//! and are sent only under the configured base URL.

mod backend;
mod closure;
mod endpoint;
mod nar;
mod negotiate;
mod resend;
mod transport;

pub use backend::{PATH_BUDGET, ServerUploader};
pub use closure::{ClosureEntry, ClosureError, parse_closure, query, upload_order};
pub use endpoint::{
    EndpointError, Exchange, InvalidCredential, ServerEndpoint, check_base_url, nar_timeout,
};
pub use nar::{CHUNK, Dump, DumpFailure, NarReader};

pub use negotiate::{NegotiationFailure, Negotiator, PROBE_TTL_SECS};
pub use resend::{BatchFailure, BatchRequest};
pub use transport::RequestFailure;
