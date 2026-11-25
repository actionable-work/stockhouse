use crate::fsutil;
use crate::queue::{Queue, QueueError, io_err};
use crate::record::Record;
use crate::rescan::{RescanError, rescan};
use crate::scanner::Scanner;
use crate::uploader::{RetryKind, UploadJob, UploadOutcome, Uploader, backoff};
use std::io::{self, Write};
use stockhouse_uploader_config::WorkerLimits;

/// What one pass did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PassReport {
    /// Pending records removed because their upload had completed.
    pub reconciled: usize,
    /// Corrupt records quarantined.
    pub quarantined: usize,
    /// A rescan ran.
    pub rescanned: bool,
    /// Upload attempts made.
    pub attempted: usize,
    /// Uploads completed.
    pub uploaded: usize,
}

/// Drains a queue through an uploader.
pub struct Worker<'a> {
    queue: Queue,
    uploader: &'a dyn Uploader,
    scanner: &'a dyn Scanner,
    limits: WorkerLimits,
    last_rescan: Option<u64>,
    overflow_seen: bool,
}

impl<'a> Worker<'a> {
    /// A worker over an opened queue.
    pub fn new(
        queue: Queue,
        uploader: &'a dyn Uploader,
        scanner: &'a dyn Scanner,
        limits: WorkerLimits,
    ) -> Self {
        Self {
            queue,
            uploader,
            scanner,
            limits,
            last_rescan: None,
            overflow_seen: false,
        }
    }

    /// Reconciles, rescans when needed, then uploads due records.
    pub fn pass(&mut self, now: u64, log: &mut dyn Write) -> Result<PassReport, QueueError> {
        let mut report = PassReport::default();
        (report.reconciled, report.quarantined) = self.reconcile()?;
        report.rescanned = self.maybe_rescan(now, log)?;
        let records = self.due(now)?;
        if records.is_empty() {
            return Ok(report);
        }
        let jobs: Vec<_> = records
            .iter()
            .map(|record| UploadJob {
                id: record.id,
                store_path: record.store_path.clone(),
                attempts: record.attempts,
            })
            .collect();
        let outcomes = self.uploader.upload_batch(&jobs);
        if outcomes.len() != jobs.len() {
            return Err(io_err("upload batch")(io::Error::new(
                io::ErrorKind::InvalidData,
                "uploader returned a different number of outcomes than jobs",
            )));
        }
        report.attempted = jobs.len();
        for (record, outcome) in records.into_iter().zip(outcomes) {
            report.uploaded += usize::from(self.apply(record, outcome, now)?);
        }
        Ok(report)
    }

    fn reconcile(&self) -> Result<(usize, usize), QueueError> {
        self.queue.reconcile_uploads()?;
        let (mut reconciled, mut quarantined) = (0, 0);
        for stem in fsutil::stems(&self.queue.root().join("pending"), ".json")
            .map_err(io_err("list pending"))?
        {
            let path = self.queue.path("pending", &stem, ".json");
            if self.queue.is_uploaded_hash(&stem) {
                fsutil::remove(&path).map_err(io_err("remove reconciled"))?;
                reconciled += 1;
            } else if let Some(Err(())) = self.queue.read_pending_hash(&stem)? {
                fsutil::rename(&path, &self.queue.path("failed", &stem, ".corrupt"))
                    .map_err(io_err("quarantine"))?;
                quarantined += 1;
            }
        }
        Ok((reconciled, quarantined))
    }

    fn maybe_rescan(&mut self, now: u64, log: &mut dyn Write) -> Result<bool, QueueError> {
        let overflow = self.queue.overflow_status()?.active;
        if overflow && !self.overflow_seen {
            let _ = writeln!(
                log,
                "stockhouse-uploader: queue overflowed; builds were not all recorded"
            );
        }
        self.overflow_seen = overflow;
        let has_room = self.queue.pending_count()? < self.queue.max_pending();
        let interval_due = self
            .last_rescan
            .is_none_or(|t| now.saturating_sub(t) >= self.limits.rescan_interval_secs);
        if !((overflow && has_room) || interval_due) {
            return Ok(false);
        }
        self.last_rescan = Some(now);
        match rescan(&self.queue, self.scanner, now) {
            Ok(_) | Err(RescanError::Scan(_)) if !self.queue.overflow_status()?.active => {}
            Ok(_) => {
                let _ = writeln!(log, "stockhouse-uploader: overflow remains after rescan");
            }
            Err(RescanError::Scan(e)) => {
                let _ = writeln!(log, "stockhouse-uploader: {e}; overflow remains");
            }
            Err(RescanError::Queue(e)) => return Err(e),
        }
        Ok(true)
    }

    /// Due records in `(next_attempt_at, id)` order, at most `batch_size`.
    pub fn due(&self, now: u64) -> Result<Vec<Record>, QueueError> {
        let mut due = Vec::new();
        for stem in fsutil::stems(&self.queue.root().join("pending"), ".json")
            .map_err(io_err("list pending"))?
        {
            if let Some(Ok(record)) = self.queue.read_pending_hash(&stem)?
                && record.next_attempt_at <= now
            {
                due.push(record);
            }
        }
        due.sort_by_key(|r| (r.next_attempt_at, r.id));
        due.truncate(self.limits.batch_size);
        Ok(due)
    }

    /// Applies an outcome; `Ok(true)` when the upload completed.
    pub fn apply(
        &self,
        record: Record,
        outcome: UploadOutcome,
        now: u64,
    ) -> Result<bool, QueueError> {
        let path = &record.store_path;
        if self.queue.is_uploaded(path) {
            fsutil::remove(&self.queue.pending_path(path)).map_err(io_err("remove pending"))?;
            return Ok(true);
        }
        let mut record = match self.queue.read_pending(path)? {
            Some(Ok(record)) => record,
            None => return Ok(false),
            Some(Err(())) => {
                return Err(io_err("read pending")(io::Error::from(
                    io::ErrorKind::InvalidData,
                )));
            }
        };
        let (base, max) = (self.limits.backoff_base_secs, self.limits.backoff_max_secs);
        match outcome {
            UploadOutcome::Uploaded => return self.complete(&record, now).map(|()| true),
            UploadOutcome::Rejected(reason) => {
                record.reason = Some(reason);
                return self.fail(&record).map(|()| false);
            }
            UploadOutcome::Deferred(message) => {
                record.next_attempt_at = now.saturating_add(max);
                record.last_error = Some(message);
            }
            UploadOutcome::Retry(kind, message) => {
                record.attempts = record.attempts.saturating_add(1);
                let delay = if kind == RetryKind::QuotaExceeded {
                    max
                } else {
                    backoff(record.attempts, base, max)
                };
                record.next_attempt_at = now.saturating_add(delay);
                record.last_error = Some(message);
                if record.attempts >= self.limits.max_attempts {
                    record.reason = Some("retries exhausted".to_owned());
                    return self.fail(&record).map(|()| false);
                }
            }
        }
        self.rewrite(&record).map(|()| false)
    }

    fn complete(&self, record: &Record, now: u64) -> Result<(), QueueError> {
        self.queue.write_done(&record.store_path, record.id, now)?;
        fsutil::remove(&self.queue.pending_path(&record.store_path))
            .map_err(io_err("remove pending"))
    }

    fn fail(&self, record: &Record) -> Result<(), QueueError> {
        let target = self
            .queue
            .path("failed", record.store_path.hash().as_str(), ".json");
        fsutil::replace(&self.queue.tmp(), &target, &record.to_json())
            .map_err(io_err("write failed"))?;
        fsutil::remove(&self.queue.pending_path(&record.store_path))
            .map_err(io_err("remove pending"))
    }

    fn rewrite(&self, record: &Record) -> Result<(), QueueError> {
        fsutil::replace(
            &self.queue.tmp(),
            &self.queue.pending_path(&record.store_path),
            &record.to_json(),
        )
        .map_err(io_err("rewrite pending"))
    }
}
