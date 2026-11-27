//! Wire codecs for batched uploads and lease renewal.

mod common;
mod declare;
mod ids;
mod lease;
mod paths;
mod reply;
mod version;

use serde::Deserialize;
use std::fmt;

pub use declare::{DeclareBatch, DeclareEntry, DeclarePushError, MAX_DECLARE_BODY};
pub use ids::IdBatch;
pub use lease::{LeaseBatch, LeaseBatchError, LeaseEntry, MAX_LEASE_ENTRIES};
pub use paths::{declare_path, leases_path, nar_path, probe_path, publish_path, status_path};
pub use reply::{
    BatchKind, BatchRefusal, BatchReply, EntryKey, EntryResult, LeaseResult, MAX_BATCH_REPLY,
    MAX_NAR_REPLY, NarReply, UploadResult, UploadState, parse_batch_reply, parse_nar_reply,
    parse_retry_after,
};
pub use version::{
    MAX_PROBE_REPLY, ProbeReply, ProtocolMode, VersionError, WIRE_VERSIONS, parse_probe_reply,
    select_version,
};

const VERSION: u64 = 2;
/// The maximum number of entries in an upload batch.
pub const MAX_BATCH_ENTRIES: usize = 1000;
/// The maximum length of an ID or lease batch body.
pub const MAX_SMALL_BATCH_BODY: usize = 64 * 1024;

/// Why a batch body or typed batch failed validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchError {
    TooLarge,
    Malformed,
    UnsupportedVersion,
    Count,
    InvalidField { index: usize, field: &'static str },
    Duplicate { index: usize },
}

impl fmt::Display for BatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str("batch body too large"),
            Self::Malformed => f.write_str("malformed batch"),
            Self::UnsupportedVersion => f.write_str("unsupported batch version"),
            Self::Count => f.write_str("batch entry count out of range"),
            Self::InvalidField { index, field } => {
                write!(f, "invalid {field} at batch entry {index}")
            }
            Self::Duplicate { index } => write!(f, "duplicate batch entry {index}"),
        }
    }
}

impl std::error::Error for BatchError {}

fn parse_body<'de, T: Deserialize<'de>>(bytes: &'de [u8], max: usize) -> Result<T, BatchError> {
    if bytes.len() > max {
        return Err(BatchError::TooLarge);
    }
    let common::Object(raw) = serde_json::from_slice(bytes).map_err(|_| BatchError::Malformed)?;
    Ok(raw)
}

fn check_header(version: u64, count: usize, max: usize) -> Result<(), BatchError> {
    if version != VERSION {
        return Err(BatchError::UnsupportedVersion);
    }
    if !(1..=max).contains(&count) {
        return Err(BatchError::Count);
    }
    Ok(())
}
