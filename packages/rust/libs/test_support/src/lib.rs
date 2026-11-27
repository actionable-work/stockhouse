//! Test fixtures for the loopback upload and lease component tests.

pub mod server;
mod store;

pub use store::{Store, entry_file, job, wait_gone};

pub use server::{Action, FakeServer, Request};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use stockhouse_credentials::{CredentialError, CredentialSource, Secret};
use stockhouse_proto::{Sha256, StorePath, nix32};

/// The synthetic bearer accepted by the fake tier.
pub const TOKEN: &str = "synthetic-test-token";

/// A fresh test directory unique within the process.
pub fn temp_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("stockhouse-upload-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A deterministic store path for a test label.
pub fn store_path(label: &str) -> StorePath {
    let hash = nix32::encode(&Sha256::digest(label.as_bytes()).bytes()[..20]);
    StorePath::parse(&format!("/nix/store/{hash}-{label}")).unwrap()
}

/// A source that counts reads and fails from read `fail_at` (1-based) on;
/// `invalidate` returns `refreshes` and is counted.
pub struct CountingSource {
    reads: Arc<AtomicUsize>,
    /// How many times the source was asked to invalidate a sent credential.
    pub invalidations: AtomicUsize,
    fail_at: Option<usize>,
    error: CredentialError,
    refreshes: bool,
}

impl CredentialSource for CountingSource {
    fn credential(&self) -> Result<Secret, CredentialError> {
        let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_at.is_some_and(|at| n >= at) {
            return Err(self.error);
        }
        Ok(Secret::new(TOKEN.as_bytes().to_vec()))
    }

    fn invalidate(&self, rejected: &Secret) -> bool {
        assert_eq!(rejected.expose(), TOKEN.as_bytes());
        self.invalidations.fetch_add(1, Ordering::SeqCst);
        self.refreshes
    }
}

/// An injected credential source with scripted read and invalidation behavior.
pub fn counting_source(
    fail_at: Option<usize>,
    error: CredentialError,
    refreshes: bool,
) -> Arc<CountingSource> {
    Arc::new(CountingSource {
        reads: Arc::new(AtomicUsize::new(0)),
        invalidations: AtomicUsize::new(0),
        fail_at,
        error,
        refreshes,
    })
}

/// A source that can fail a read and a handle to its read counter.
pub fn counting(
    fail_at: Option<usize>,
    error: CredentialError,
) -> (Arc<CountingSource>, Arc<AtomicUsize>) {
    let source = counting_source(fail_at, error, false);
    let reads = source.reads.clone();
    (source, reads)
}
