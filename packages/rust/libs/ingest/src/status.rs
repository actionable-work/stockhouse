use crate::fsutil;
use crate::queue::{OVERFLOW_CAP, Queue, QueueError, io_err};
use crate::record::Record;
use serde::Serialize;

/// Overflow visibility across both markers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct OverflowStatus {
    /// Either marker exists.
    pub active: bool,
    /// Paths dropped (bytes across both markers).
    pub dropped: u64,
    /// A marker reached its cap, so `dropped` is a lower bound.
    pub truncated: bool,
}

/// A queue snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct QueueStatus {
    /// Status format version.
    pub version: u32,
    /// Pending records.
    pub pending: usize,
    /// Failed records, including quarantined corrupt files.
    pub failed: usize,
    /// Completed uploads.
    pub uploaded: usize,
    /// Earliest `next_attempt_at` among readable pending records.
    pub next_attempt_at: Option<u64>,
    /// Overflow state.
    pub overflow: OverflowStatus,
}

impl Queue {
    /// Overflow state from `overflow` and `overflow.rescan`.
    pub fn overflow_status(&self) -> Result<OverflowStatus, QueueError> {
        let mut status = OverflowStatus::default();
        for name in ["overflow", "overflow.rescan"] {
            if let Some(len) =
                fsutil::size(&self.root().join(name)).map_err(io_err("read overflow"))?
            {
                status.active = true;
                status.dropped += len;
                status.truncated |= len >= OVERFLOW_CAP;
            }
        }
        Ok(status)
    }

    /// Counts records and reports overflow.
    pub fn status(&self) -> Result<QueueStatus, QueueError> {
        let pending =
            fsutil::stems(&self.root().join("pending"), ".json").map_err(io_err("list pending"))?;
        let failed = fsutil::stems(&self.root().join("failed"), "")
            .map_err(io_err("list failed"))?
            .len();
        let uploaded = fsutil::stems(&self.root().join("done"), ".json")
            .map_err(io_err("list done"))?
            .len();
        let next_attempt_at = pending
            .iter()
            .filter_map(|stem| std::fs::read(self.path("pending", stem, ".json")).ok())
            .filter_map(|bytes| Record::from_json(&bytes))
            .map(|r| r.next_attempt_at)
            .min();
        Ok(QueueStatus {
            version: 1,
            pending: pending.len(),
            failed,
            uploaded,
            next_attempt_at,
            overflow: self.overflow_status()?,
        })
    }
}
