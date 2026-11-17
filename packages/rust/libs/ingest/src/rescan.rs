use crate::queue::{EnqueueOutcome, Queue, QueueError, io_err};
use crate::scanner::{ScanError, Scanner};
use std::fmt;
use std::fs;

/// What a rescan did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RescanReport {
    /// Newly queued paths.
    pub enqueued: usize,
    /// Every candidate was handled.
    pub complete: bool,
}

/// A rescan failed; overflow stays visible.
#[derive(Debug)]
pub enum RescanError {
    /// The scanner failed.
    Scan(ScanError),
    /// The queue failed.
    Queue(QueueError),
}

impl fmt::Display for RescanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scan(e) => write!(f, "rescan: {e}"),
            Self::Queue(e) => write!(f, "rescan: {e}"),
        }
    }
}

impl std::error::Error for RescanError {}

impl From<QueueError> for RescanError {
    fn from(e: QueueError) -> Self {
        Self::Queue(e)
    }
}

/// Reconciles the queue with the store's locally built paths.
pub fn rescan(queue: &Queue, scanner: &dyn Scanner, now: u64) -> Result<RescanReport, RescanError> {
    let (live, held) = (queue.root().join("overflow"), queue.root().join("overflow.rescan"));
    if live.exists() && !held.exists() {
        fs::rename(&live, &held).map_err(io_err("take overflow"))?;
    }
    let candidates = scanner.candidates().map_err(RescanError::Scan)?;
    let mut producer = queue.producer()?;
    let mut report = RescanReport { enqueued: 0, complete: true };
    for path in &candidates {
        match producer.enqueue(path, now, false)? {
            EnqueueOutcome::Queued(_) => report.enqueued += 1,
            EnqueueOutcome::Overflow => {
                report.complete = false;
                break;
            }
            _ => {}
        }
    }
    if !report.complete && !held.exists() && !live.exists() {
        producer.overflow()?;
    }
    if report.complete && scanner.recovers() && held.exists() {
        fs::remove_file(&held).map_err(io_err("clear overflow"))?;
    }
    Ok(report)
}
