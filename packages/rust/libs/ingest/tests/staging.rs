//! Runs in its own process so the staging-name counter starts at zero.
mod common;
use common::*;
use stockhouse_ingest::EnqueueOutcome;

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-014"))]
#[test]
fn stale_staging_names_are_skipped() {
    let q = queue(5);
    let tmp = q.root().join("tmp");
    let pid = std::process::id();
    let stale: Vec<_> = (0..4).map(|n| tmp.join(format!("{pid}-{n}.tmp"))).collect();
    for file in &stale {
        std::fs::write(file, b"left by a crash").unwrap();
    }
    let outcome = q
        .producer()
        .unwrap()
        .enqueue(&path("a"), 100, true)
        .unwrap();
    assert!(matches!(outcome, EnqueueOutcome::Queued(_)), "{outcome:?}");
    let pending = q
        .root()
        .join("pending")
        .join(format!("{}.json", path("a").hash()));
    assert!(pending.is_file());
    let mut left: Vec<_> = std::fs::read_dir(&tmp)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    left.sort();
    assert_eq!(left, stale);
    for file in &stale {
        assert_eq!(std::fs::read(file).unwrap(), b"left by a crash");
    }
}
