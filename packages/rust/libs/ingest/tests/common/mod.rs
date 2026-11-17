#![allow(dead_code)]
//! Ingestion test fixtures.

use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use stockhouse_ingest::{Queue, ScanError, Scanner, UploadJob, UploadOutcome, Uploader, WorkerLimits};
use stockhouse_proto::{Sha256, StorePath, nix32};

pub fn temp_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("stockhouse-ingest-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn path(label: &str) -> StorePath {
    let hash = nix32::encode(&Sha256::digest(label.as_bytes()).bytes()[..20]);
    StorePath::parse(&format!("/nix/store/{hash}-{label}")).unwrap()
}

pub fn queue(max: usize) -> Queue {
    Queue::create(&temp_dir().join("queue"), max).unwrap()
}

pub fn limits() -> WorkerLimits {
    WorkerLimits { batch_size: 64, max_attempts: 3, backoff_base_secs: 5, backoff_max_secs: 100, rescan_interval_secs: 1000 }
}

/// Answers each job with queued outcomes and records every job.
#[derive(Default)]
pub struct FakeUploader {
    pub outcomes: RefCell<Vec<UploadOutcome>>,
    pub jobs: RefCell<Vec<UploadJob>>,
}

impl FakeUploader {
    pub fn answering(outcomes: Vec<UploadOutcome>) -> Self {
        Self { outcomes: RefCell::new(outcomes), jobs: RefCell::default() }
    }
}

impl Uploader for FakeUploader {
    fn upload(&self, job: &UploadJob) -> UploadOutcome {
        self.jobs.borrow_mut().push(job.clone());
        let mut outcomes = self.outcomes.borrow_mut();
        if outcomes.is_empty() { UploadOutcome::Uploaded } else { outcomes.remove(0) }
    }
}

pub struct FakeScanner(pub Result<Vec<StorePath>, ScanError>);

impl Scanner for FakeScanner {
    fn candidates(&self) -> Result<Vec<StorePath>, ScanError> {
        self.0.clone()
    }
}

/// A fake `nix` that records its arguments and replays configured output.
pub fn fake_nix(stdout: &[u8], stderr: &[u8], code: i32) -> PathBuf {
    let dir = temp_dir();
    std::fs::write(dir.join("stdout"), stdout).unwrap();
    std::fs::write(dir.join("stderr"), stderr).unwrap();
    let script = format!(
        "#!/bin/sh\nd=\"{}\"\nprintf '%s\\n' \"$@\" > \"$d/args\"\ncat \"$d/stdout\"\ncat \"$d/stderr\" >&2\nexit {code}\n",
        dir.display()
    );
    let nix = dir.join("nix");
    std::fs::write(&nix, script).unwrap();
    std::fs::set_permissions(&nix, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A parallel test may fork while this file is still open for writing
    // (ETXTBSY); wait until it can be executed.
    while let Err(e) = std::process::Command::new(&nix).output() {
        assert_eq!(e.raw_os_error(), Some(26), "{e}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let _ = std::fs::remove_file(dir.join("args"));
    nix
}

pub fn recorded_args(nix: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(nix.parent().unwrap().join("args")).unwrap();
    text.lines().map(str::to_owned).collect()
}
