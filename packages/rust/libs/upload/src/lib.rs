//! Authenticated server upload and lease renewal for the Stockhouse client.
//!
//! Implements protocol v1 from `stockhouse-proto`: the `server` backend
//! behind `stockhouse_ingest::Uploader`, which uploads each queued path's
//! closure references first, and the lease client that renews this
//! machine's system roots. Credentials come only from an injected
//! `CredentialSource` and are sent only under the configured base URL.

mod backend;
mod closure;
mod endpoint;
pub mod lease;
mod nar;

pub use backend::{PATH_BUDGET, ServerUploader};
pub use closure::{ClosureEntry, ClosureError, parse_closure, query, upload_order};
pub use endpoint::{EndpointError, ServerEndpoint, check_base_url, nar_timeout};
pub use nar::{CHUNK, Dump, DumpFailure, NarReader};
