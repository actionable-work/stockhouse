//! Durable, bounded post-build ingestion for the Stockhouse client.
//!
//! The post-build hook records built store paths in a filesystem queue and
//! returns at once. A separate uploader drains the queue with retries and
//! backoff, and rescans the local store to recover paths the hook missed.
//! Uploader failures never fail or block a build.

mod fsutil;
pub mod hook;
mod identity;
mod queue;
mod record;
mod rescan;
mod scanner;
mod status;
mod uploader;
mod worker;

pub use identity::UploadEntry;
pub use queue::{EnqueueOutcome, FORMAT, Producer, Queue, QueueError};
pub use record::{DoneRecord, Record};
pub use rescan::{RescanError, RescanReport, rescan};
pub use scanner::{NixPathInfoScanner, NoScanner, ScanError, Scanner, parse_path_info};
pub use status::{OverflowStatus, QueueStatus};
pub use stockhouse_uploader_config::{
    BackendConfig, ScannerConfig, ServerBackendConfig, UploaderConfig, UploaderConfigError,
    WorkerLimits,
};
pub use uploader::{
    DeferredUploader, NixCopyUploader, RetryKind, UploadJob, UploadOutcome, Uploader, backoff,
};
pub use worker::{PassReport, Worker};
