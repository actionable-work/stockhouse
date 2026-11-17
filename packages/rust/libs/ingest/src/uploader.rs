use std::path::PathBuf;
use std::process::{Command, Stdio};
use stockhouse_proto::{StorePath, UploadId};

const STDERR_LIMIT: usize = 512;

/// One upload attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UploadJob {
    /// The stable upload identity (an idempotency key for a server).
    pub id: UploadId,
    /// The path to upload.
    pub store_path: StorePath,
    /// Attempts already consumed.
    pub attempts: u32,
}

/// Why a retry is needed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryKind {
    /// A transient failure.
    Transient,
    /// The outcome is unknown (for example a lost reply).
    Uncertain,
    /// The tenant quota is exhausted.
    QuotaExceeded,
}

/// The result of one upload attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UploadOutcome {
    /// Uploaded.
    Uploaded,
    /// Retry later.
    Retry(RetryKind, String),
    /// Not possible yet; retry later without consuming an attempt.
    Deferred(String),
    /// Permanently refused.
    Rejected(String),
}

/// An upload backend.
pub trait Uploader {
    /// Attempts one upload.
    fn upload(&self, job: &UploadJob) -> UploadOutcome;
}

/// The default backend until authenticated server upload exists.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeferredUploader;

impl Uploader for DeferredUploader {
    fn upload(&self, _job: &UploadJob) -> UploadOutcome {
        UploadOutcome::Deferred("server upload requires P2.08".to_owned())
    }
}

/// A local development backend: `nix copy --to file://…`.
#[derive(Clone, Debug)]
pub struct NixCopyUploader {
    nix: PathBuf,
    destination: String,
}

impl NixCopyUploader {
    /// Uses `nix` to copy into an absolute `file://` destination.
    pub fn new(nix: PathBuf, destination: String) -> Self {
        Self { nix, destination }
    }
}

impl Uploader for NixCopyUploader {
    fn upload(&self, job: &UploadJob) -> UploadOutcome {
        let output = Command::new(&self.nix)
            .args(["--extra-experimental-features", "nix-command", "copy", "--to"])
            .arg(&self.destination)
            .arg(job.store_path.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();
        match output {
            Ok(out) if out.status.success() => UploadOutcome::Uploaded,
            Ok(out) => UploadOutcome::Retry(RetryKind::Transient, bounded(&out.stderr)),
            Err(error) => UploadOutcome::Retry(RetryKind::Transient, bounded(format!("cannot start nix: {error}").as_bytes())),
        }
    }
}

fn bounded(bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(&bytes[..bytes.len().min(STDERR_LIMIT)]).into_owned();
    while text.len() > STDERR_LIMIT {
        text.pop();
    }
    text
}

/// `min(base * 2^(attempts-1), max)` for `attempts >= 1`.
pub fn backoff(attempts: u32, base: u64, max: u64) -> u64 {
    let exponent = attempts.saturating_sub(1).min(63);
    base.checked_mul(1u64 << exponent).map_or(max, |delay| delay.min(max))
}
