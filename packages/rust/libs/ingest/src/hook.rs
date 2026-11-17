//! The Nix post-build hook: record built paths and return. It never fails.

use crate::queue::{EnqueueOutcome, Queue};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use stockhouse_proto::StorePath;

/// Paths enqueued per invocation; the rest count as overflow.
pub const MAX_PATHS_PER_HOOK: usize = 1024;

/// What one hook invocation did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HookReport {
    /// New pending records.
    pub queued: usize,
    /// Paths already pending, uploaded or failed.
    pub known: usize,
    /// Paths counted as overflow.
    pub overflow: usize,
    /// Invalid entries skipped.
    pub skipped: usize,
}

/// Why the hook did nothing.
#[derive(Debug)]
pub enum Refusal {
    /// The queue path is a symlink, not a directory, or root-owned.
    UnsafeQueue,
    /// Dropping to the queue owner failed.
    PrivilegeDrop,
    /// The queue cannot be used.
    Queue(crate::QueueError),
}

/// Checks the queue directory and, when root, becomes its owner.
/// Must run while the process is single-threaded.
pub fn drop_to_queue_owner(dir: &Path) -> Result<(), Refusal> {
    let meta = std::fs::symlink_metadata(dir).map_err(|_| Refusal::UnsafeQueue)?;
    if !meta.file_type().is_dir() || meta.uid() == 0 || meta.gid() == 0 {
        return Err(Refusal::UnsafeQueue);
    }
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    // SAFETY: plain syscalls with valid arguments; the caller is single-threaded.
    let ok = unsafe {
        libc::setgroups(0, std::ptr::null()) == 0
            && libc::setgid(meta.gid()) == 0
            && libc::setuid(meta.uid()) == 0
    };
    if ok {
        Ok(())
    } else {
        Err(Refusal::PrivilegeDrop)
    }
}

/// Runs the hook for `out_paths` (Nix's `OUT_PATHS`). Never panics outward
/// by design of its callers; every problem becomes one line on `log`.
pub fn run(dir: &Path, out_paths: &str, now: u64, log: &mut dyn Write) -> HookReport {
    let mut report = HookReport::default();
    let queue =
        match drop_to_queue_owner(dir).and_then(|()| Queue::open(dir).map_err(Refusal::Queue)) {
            Ok(queue) => queue,
            Err(refusal) => {
                let _ = writeln!(
                    log,
                    "stockhouse post-build-hook: queue unavailable: {refusal:?}"
                );
                return report;
            }
        };
    let mut producer = match queue.producer() {
        Ok(producer) => producer,
        Err(e) => {
            let _ = writeln!(log, "stockhouse post-build-hook: {e}");
            return report;
        }
    };
    for (index, entry) in out_paths.split_ascii_whitespace().enumerate() {
        let Ok(path) = StorePath::parse(entry) else {
            report.skipped += 1;
            let _ = writeln!(
                log,
                "stockhouse post-build-hook: skipping invalid store path"
            );
            continue;
        };
        let outcome = if index < MAX_PATHS_PER_HOOK {
            producer.enqueue(&path, now, true)
        } else {
            producer.overflow().map(|()| EnqueueOutcome::Overflow)
        };
        match outcome {
            Ok(EnqueueOutcome::Queued(_)) => report.queued += 1,
            Ok(EnqueueOutcome::Overflow) => report.overflow += 1,
            Ok(_) => report.known += 1,
            Err(e) => {
                let _ = writeln!(log, "stockhouse post-build-hook: {e}");
            }
        }
    }
    report
}
