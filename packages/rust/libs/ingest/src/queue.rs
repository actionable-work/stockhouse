use crate::fsutil;
use crate::record::{DoneRecord, Record};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use stockhouse_proto::{StorePath, UploadId};

/// The current queue format marker.
pub const FORMAT: &str = "stockhouse-queue-v1\n";
pub(crate) const OVERFLOW_CAP: u64 = 1024 * 1024;
const DIRS: [&str; 4] = ["pending", "failed", "done", "tmp"];

/// A queue could not be used.
#[derive(Debug)]
#[non_exhaustive]
pub enum QueueError {
    /// `FORMAT` or `LIMITS` is missing or `LIMITS` does not parse.
    NotInitialized,
    /// `FORMAT` names another format.
    UnsupportedFormat,
    /// A filesystem operation failed.
    Io {
        /// What was being done.
        op: &'static str,
        /// The OS error.
        error: io::Error,
    },
}

impl fmt::Display for QueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInitialized => f.write_str("queue is not initialized"),
            Self::UnsupportedFormat => f.write_str("queue has an unsupported format"),
            Self::Io { op, error } => write!(f, "queue {op} failed: {error}"),
        }
    }
}

impl std::error::Error for QueueError {}

pub(crate) fn io_err(op: &'static str) -> impl FnOnce(io::Error) -> QueueError {
    move |error| QueueError::Io { op, error }
}

/// The result of enqueueing one store path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnqueueOutcome {
    /// A new pending record with this identity.
    Queued(UploadId),
    /// Already pending with this identity.
    AlreadyQueued(UploadId),
    /// Already in the upload ledger.
    AlreadyUploaded,
    /// Already failed with this identity.
    AlreadyFailed(UploadId),
    /// The queue is full.
    Overflow,
}

/// An opened, versioned queue directory.
#[derive(Clone, Debug)]
pub struct Queue {
    root: PathBuf,
    max_pending: usize,
}

impl Queue {
    /// Creates the layout (keeping records) and writes `LIMITS` atomically.
    pub fn create(root: &Path, max_pending: usize) -> Result<Self, QueueError> {
        for dir in DIRS {
            fs::create_dir_all(root.join(dir)).map_err(io_err("create layout"))?;
        }
        match fs::read_to_string(root.join("FORMAT")) {
            Ok(text) if text != FORMAT => return Err(QueueError::UnsupportedFormat),
            Ok(_) => {}
            Err(_) => fsutil::replace(&root.join("tmp"), &root.join("FORMAT"), FORMAT.as_bytes())
                .map_err(io_err("write FORMAT"))?,
        }
        let limits = format!("max_pending={max_pending}\n");
        fsutil::replace(&root.join("tmp"), &root.join("LIMITS"), limits.as_bytes())
            .map_err(io_err("write LIMITS"))?;
        Ok(Self {
            root: root.to_path_buf(),
            max_pending,
        })
    }

    /// Opens an existing queue without creating anything.
    pub fn open(root: &Path) -> Result<Self, QueueError> {
        let format =
            fs::read_to_string(root.join("FORMAT")).map_err(|_| QueueError::NotInitialized)?;
        if format != FORMAT {
            return Err(QueueError::UnsupportedFormat);
        }
        let limits =
            fs::read_to_string(root.join("LIMITS")).map_err(|_| QueueError::NotInitialized)?;
        let max_pending = parse_limits(&limits).ok_or(QueueError::NotInitialized)?;
        Ok(Self {
            root: root.to_path_buf(),
            max_pending,
        })
    }

    /// The configured capacity.
    pub fn max_pending(&self) -> usize {
        self.max_pending
    }

    /// The queue directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn path(&self, dir: &str, hash: &str, ext: &str) -> PathBuf {
        self.root.join(dir).join(format!("{hash}{ext}"))
    }

    pub(crate) fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// Number of pending records.
    pub fn pending_count(&self) -> Result<usize, QueueError> {
        Ok(fsutil::stems(&self.root.join("pending"), ".json")
            .map_err(io_err("list pending"))?
            .len())
    }

    /// Starts a producer batch, counting `pending/` once.
    pub fn producer(&self) -> Result<Producer<'_>, QueueError> {
        Ok(Producer {
            queue: self,
            pending: self.pending_count()?,
        })
    }

    /// Reads a record file; `Ok(None)` when absent, `Err(())` when corrupt.
    pub(crate) fn read_record(
        &self,
        path: &Path,
    ) -> Result<Option<Result<Record, ()>>, QueueError> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(Record::from_json(&bytes).ok_or(()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(QueueError::Io {
                op: "read record",
                error,
            }),
        }
    }

    pub(crate) fn is_uploaded(&self, hash: &str) -> bool {
        self.path("done", hash, ".json").exists()
    }

    pub(crate) fn write_done(&self, record: &Record, now: u64) -> Result<(), QueueError> {
        let done = DoneRecord {
            version: 1,
            id: record.id.to_string(),
            store_path: record.store_path.to_string(),
            uploaded_at: now,
        };
        let bytes = serde_json::to_vec(&done).expect("done record serializes");
        let target = self.path("done", record.store_path.hash().as_str(), ".json");
        fsutil::replace(&self.tmp(), &target, &bytes).map_err(io_err("write done"))
    }

    /// The done record for a path, if uploaded.
    pub fn done_record(&self, path: &StorePath) -> Option<DoneRecord> {
        let bytes = fs::read(self.path("done", path.hash().as_str(), ".json")).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

fn parse_limits(text: &str) -> Option<usize> {
    let value = text.strip_prefix("max_pending=")?.strip_suffix('\n')?;
    value.parse().ok().filter(|&n| n > 0)
}

/// A batch of enqueues sharing one pending count.
pub struct Producer<'q> {
    queue: &'q Queue,
    pending: usize,
}

impl Producer<'_> {
    /// Enqueues one path. `record_overflow` appends to the overflow marker.
    pub fn enqueue(
        &mut self,
        path: &StorePath,
        now: u64,
        record_overflow: bool,
    ) -> Result<EnqueueOutcome, QueueError> {
        let q = self.queue;
        let hash = path.hash();
        if q.is_uploaded(hash.as_str()) {
            return Ok(EnqueueOutcome::AlreadyUploaded);
        }
        if let Some(Ok(r)) = q.read_record(&q.path("pending", hash.as_str(), ".json"))? {
            return Ok(EnqueueOutcome::AlreadyQueued(r.id));
        }
        if let Some(Ok(r)) = q.read_record(&q.path("failed", hash.as_str(), ".json"))? {
            return Ok(EnqueueOutcome::AlreadyFailed(r.id));
        }
        if self.pending >= q.max_pending {
            if record_overflow {
                fsutil::append_marker(&q.root.join("overflow"), OVERFLOW_CAP)
                    .map_err(io_err("record overflow"))?;
            }
            return Ok(EnqueueOutcome::Overflow);
        }
        self.place(path, now)
    }

    fn place(&mut self, path: &StorePath, now: u64) -> Result<EnqueueOutcome, QueueError> {
        let q = self.queue;
        let record = Record::new(path.clone(), now);
        let target = q.path("pending", path.hash().as_str(), ".json");
        if fsutil::create_new(&q.tmp(), &target, &record.to_json())
            .map_err(io_err("write pending"))?
        {
            self.pending += 1;
            return Ok(EnqueueOutcome::Queued(record.id));
        }
        match q.read_record(&target)? {
            Some(Ok(existing)) => Ok(EnqueueOutcome::AlreadyQueued(existing.id)),
            // An unreadable record holds the name until it is quarantined;
            // the marker makes a later rescan enqueue the path again.
            _ => self.overflow().map(|()| EnqueueOutcome::Overflow),
        }
    }

    /// Records an overflow without attempting to enqueue.
    pub fn overflow(&self) -> Result<(), QueueError> {
        fsutil::append_marker(&self.queue.root.join("overflow"), OVERFLOW_CAP)
            .map_err(io_err("record overflow"))
    }
}
