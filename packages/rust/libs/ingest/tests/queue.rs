mod common;
use common::*;
use std::sync::{Arc, Barrier};
use stockhouse_ingest::{EnqueueOutcome, FORMAT, Queue, QueueError, Record, UploadOutcome, Worker};

fn enqueue(q: &Queue, label: &str) -> EnqueueOutcome {
    q.producer().unwrap().enqueue(&path(label), 100, true).unwrap()
}

fn pending_record(q: &Queue, label: &str) -> Record {
    let file = q.root().join("pending").join(format!("{}.json", path(label).hash()));
    Record::from_json(&std::fs::read(file).unwrap()).unwrap()
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-001"))]
#[test]
fn creation_writes_layout_and_limits() {
    let root = temp_dir().join("q");
    let q = Queue::create(&root, 7).unwrap();
    for dir in ["pending", "failed", "done", "tmp"] {
        assert!(root.join(dir).is_dir(), "{dir}");
    }
    assert_eq!(std::fs::read_to_string(root.join("FORMAT")).unwrap(), "stockhouse-queue-v1\n");
    assert_eq!(std::fs::read_to_string(root.join("LIMITS")).unwrap(), "max_pending=7\n");
    assert!(matches!(enqueue(&q, "kept"), EnqueueOutcome::Queued(_)));
    let reopened = Queue::create(&root, 9).unwrap();
    assert_eq!(reopened.max_pending(), 9);
    assert_eq!(reopened.status().unwrap().pending, 1);
    assert_eq!(FORMAT, "stockhouse-queue-v1\n");
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-002"))]
#[test]
fn unknown_or_missing_formats_are_rejected() {
    let root = temp_dir().join("q");
    assert!(matches!(Queue::open(&root), Err(QueueError::NotInitialized)));
    Queue::create(&root, 3).unwrap();
    std::fs::write(root.join("LIMITS"), "max_pending=oops\n").unwrap();
    assert!(matches!(Queue::open(&root), Err(QueueError::NotInitialized)));
    std::fs::remove_file(root.join("LIMITS")).unwrap();
    assert!(matches!(Queue::open(&root), Err(QueueError::NotInitialized)));
    std::fs::write(root.join("LIMITS"), "max_pending=3\n").unwrap();
    std::fs::write(root.join("FORMAT"), "stockhouse-queue-v2\n").unwrap();
    assert!(matches!(Queue::open(&root), Err(QueueError::UnsupportedFormat)));
    assert!(matches!(Queue::create(&root, 3), Err(QueueError::UnsupportedFormat)));
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-003"))]
#[test]
fn new_paths_get_a_fresh_identity() {
    let q = queue(10);
    let EnqueueOutcome::Queued(id) = enqueue(&q, "a") else { panic!("expected Queued") };
    let record = pending_record(&q, "a");
    assert_eq!((record.id, record.attempts, record.next_attempt_at, record.enqueued_at), (id, 0, 100, 100));
    assert_eq!(record.store_path, path("a"));
    let EnqueueOutcome::Queued(other) = enqueue(&q, "b") else { panic!("expected Queued") };
    assert_ne!(id, other);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-004"))]
#[test]
fn pending_paths_keep_their_identity() {
    let q = queue(10);
    let EnqueueOutcome::Queued(id) = enqueue(&q, "a") else { panic!() };
    let before = std::fs::read(q.root().join("pending").join(format!("{}.json", path("a").hash()))).unwrap();
    assert_eq!(enqueue(&q, "a"), EnqueueOutcome::AlreadyQueued(id));
    let after = std::fs::read(q.root().join("pending").join(format!("{}.json", path("a").hash()))).unwrap();
    assert_eq!(before, after);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-005"))]
#[test]
fn uploaded_paths_are_not_queued_again() {
    let q = queue(10);
    enqueue(&q, "a");
    let uploader = FakeUploader::default();
    Worker::new(q.clone(), &uploader, &stockhouse_ingest::NoScanner, limits()).pass(200, &mut Vec::new()).unwrap();
    assert_eq!(enqueue(&q, "a"), EnqueueOutcome::AlreadyUploaded);
    assert_eq!(q.status().unwrap().pending, 0);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-013"))]
#[test]
fn failed_paths_stay_failed() {
    let q = queue(10);
    let EnqueueOutcome::Queued(id) = enqueue(&q, "a") else { panic!() };
    let uploader = FakeUploader::answering(vec![UploadOutcome::Rejected("no".into())]);
    Worker::new(q.clone(), &uploader, &stockhouse_ingest::NoScanner, limits()).pass(200, &mut Vec::new()).unwrap();
    assert_eq!(enqueue(&q, "a"), EnqueueOutcome::AlreadyFailed(id));
    assert_eq!(q.status().unwrap().pending, 0);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-006"))]
#[test]
fn concurrent_enqueues_create_one_record() {
    let q = queue(100);
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (q, barrier) = (q.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                enqueue(&q, "same")
            })
        })
        .collect();
    let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let id = pending_record(&q, "same").id;
    assert_eq!(q.status().unwrap().pending, 1);
    assert_eq!(outcomes.iter().filter(|o| **o == EnqueueOutcome::Queued(id)).count(), 1);
    assert!(outcomes.iter().all(|o| *o == EnqueueOutcome::Queued(id) || *o == EnqueueOutcome::AlreadyQueued(id)));
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-007"))]
#[test]
fn full_queue_overflows_visibly() {
    let q = queue(2);
    enqueue(&q, "a");
    enqueue(&q, "b");
    assert_eq!(enqueue(&q, "c"), EnqueueOutcome::Overflow);
    assert_eq!(enqueue(&q, "d"), EnqueueOutcome::Overflow);
    let status = q.status().unwrap();
    assert_eq!(status.pending, 2);
    assert!(status.overflow.active);
    assert_eq!(status.overflow.dropped, 2);
    assert_eq!(std::fs::metadata(q.root().join("overflow")).unwrap().len(), 2);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-008"))]
#[test]
fn overflow_marker_is_bounded() {
    let q = queue(1);
    enqueue(&q, "a");
    std::fs::write(q.root().join("overflow"), vec![b'o'; 1024 * 1024]).unwrap();
    assert_eq!(enqueue(&q, "b"), EnqueueOutcome::Overflow);
    assert_eq!(std::fs::metadata(q.root().join("overflow")).unwrap().len(), 1024 * 1024);
    let overflow = q.status().unwrap().overflow;
    assert!(overflow.active && overflow.truncated);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-009"))]
#[test]
fn status_reports_counts_and_overflow() {
    let q = queue(3);
    for label in ["a", "b", "c", "d"] {
        enqueue(&q, label);
    }
    let uploader = FakeUploader::answering(vec![UploadOutcome::Uploaded, UploadOutcome::Rejected("x".into())]);
    let mut worker = Worker::new(q.clone(), &uploader, &stockhouse_ingest::NoScanner, stockhouse_ingest::WorkerLimits { batch_size: 2, ..limits() });
    worker.pass(100, &mut Vec::new()).unwrap();
    assert!(q.root().join("overflow.rescan").exists(), "the pass rescan took the marker over");
    std::fs::write(q.root().join("overflow"), b"oo").unwrap();
    let status = q.status().unwrap();
    assert_eq!((status.pending, status.failed, status.uploaded), (1, 1, 1));
    assert_eq!(status.next_attempt_at, Some(100));
    assert_eq!((status.overflow.active, status.overflow.dropped, status.overflow.truncated), (true, 3, false));
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-010"))]
#[test]
fn only_due_records_are_attempted_in_order() {
    let q = queue(10);
    for label in ["a", "b", "c", "d"] {
        enqueue(&q, label);
    }
    let worker = Worker::new(q.clone(), &stockhouse_ingest::DeferredUploader, &stockhouse_ingest::NoScanner, limits());
    let mut later = pending_record(&q, "a");
    later.next_attempt_at = 500;
    worker.apply(later, UploadOutcome::Deferred("x".into()), 400).unwrap();
    let small = Worker::new(q.clone(), &stockhouse_ingest::DeferredUploader, &stockhouse_ingest::NoScanner, stockhouse_ingest::WorkerLimits { batch_size: 2, ..limits() });
    let due = small.due(150).unwrap();
    assert_eq!(due.len(), 2);
    assert!(due.windows(2).all(|w| (w[0].next_attempt_at, w[0].id) <= (w[1].next_attempt_at, w[1].id)));
    assert!(due.iter().all(|r| r.store_path != path("a")));
    assert_eq!(worker.due(150).unwrap().len(), 3);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-011"))]
#[test]
fn corrupt_records_are_quarantined() {
    let q = queue(10);
    enqueue(&q, "good");
    let bad = path("bad").hash().to_string();
    std::fs::write(q.root().join("pending").join(format!("{bad}.json")), b"{not json").unwrap();
    let old = path("old").hash().to_string();
    let v2 = String::from_utf8(Record::new(path("old"), 1).to_json()).unwrap().replace("\"version\":1", "\"version\":2");
    std::fs::write(q.root().join("pending").join(format!("{old}.json")), v2).unwrap();
    let uploader = FakeUploader::default();
    let report = Worker::new(q.clone(), &uploader, &stockhouse_ingest::NoScanner, limits()).pass(200, &mut Vec::new()).unwrap();
    assert_eq!((report.quarantined, report.uploaded), (2, 1));
    assert_eq!(std::fs::read(q.root().join("failed").join(format!("{bad}.corrupt"))).unwrap(), b"{not json");
    assert!(q.root().join("failed").join(format!("{old}.corrupt")).exists());
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-012"))]
#[test]
fn interrupted_completions_are_reconciled() {
    let q = queue(10);
    enqueue(&q, "a");
    let record = pending_record(&q, "a");
    let done = format!("{{\"version\":1,\"id\":\"{}\",\"store_path\":\"{}\",\"uploaded_at\":5}}", record.id, record.store_path);
    std::fs::write(q.root().join("done").join(format!("{}.json", path("a").hash())), done).unwrap();
    let uploader = FakeUploader::default();
    let report = Worker::new(q.clone(), &uploader, &stockhouse_ingest::NoScanner, limits()).pass(200, &mut Vec::new()).unwrap();
    assert_eq!(report.reconciled, 1);
    assert!(uploader.jobs.borrow().is_empty());
    assert_eq!(q.status().unwrap().pending, 0);
}
