mod common;
use common::*;
use stockhouse_ingest::{EnqueueOutcome, NoScanner, Queue, Record, RetryKind, UploadOutcome, Worker, backoff};

fn record(q: &Queue, label: &str) -> Record {
    let dir = q.root().join("pending");
    Record::from_json(&std::fs::read(dir.join(format!("{}.json", path(label).hash()))).unwrap()).unwrap()
}

fn queued(q: &Queue, label: &str) -> stockhouse_proto::UploadId {
    match q.producer().unwrap().enqueue(&path(label), 100, true).unwrap() {
        EnqueueOutcome::Queued(id) => id,
        other => panic!("{other:?}"),
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RETRY-001"))]
#[test]
fn uploaded_records_move_to_the_ledger() {
    let q = queue(10);
    let id = queued(&q, "a");
    let uploader = FakeUploader::default();
    let report = Worker::new(q.clone(), &uploader, &NoScanner, limits()).pass(150, &mut Vec::new()).unwrap();
    assert_eq!(report.uploaded, 1);
    let done = q.done_record(&path("a")).unwrap();
    assert_eq!((done.id, done.uploaded_at, done.version), (id.to_string(), 150, 1));
    assert_eq!(q.status().unwrap().pending, 0);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RETRY-002"))]
#[test]
fn transient_and_uncertain_failures_back_off() {
    let q = queue(10);
    let id = queued(&q, "a");
    let worker = Worker::new(q.clone(), &stockhouse_ingest::DeferredUploader, &NoScanner, stockhouse_ingest::WorkerLimits { max_attempts: 10, ..limits() });
    let mut now = 100;
    for (attempt, kind, delay) in [(1, RetryKind::Transient, 5), (2, RetryKind::Uncertain, 10), (3, RetryKind::Transient, 20), (4, RetryKind::Transient, 40), (5, RetryKind::Transient, 80), (6, RetryKind::Uncertain, 100)] {
        worker.apply(record(&q, "a"), UploadOutcome::Retry(kind, format!("try {attempt}")), now).unwrap();
        let r = record(&q, "a");
        assert_eq!((r.attempts, r.next_attempt_at, r.id), (attempt, now + delay, id));
        assert_eq!(r.last_error.as_deref(), Some(format!("try {attempt}").as_str()));
        now += 1;
    }
    assert_eq!(backoff(1, 5, 100), 5);
    assert_eq!(backoff(64, 5, 100), 100);
    assert_eq!(backoff(200, u64::MAX / 2, u64::MAX), u64::MAX);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RETRY-003"))]
#[test]
fn quota_exhaustion_waits_the_maximum() {
    let q = queue(10);
    let id = queued(&q, "a");
    let worker = Worker::new(q.clone(), &stockhouse_ingest::DeferredUploader, &NoScanner, limits());
    worker.apply(record(&q, "a"), UploadOutcome::Retry(RetryKind::QuotaExceeded, "quota".into()), 100).unwrap();
    let r = record(&q, "a");
    assert_eq!((r.attempts, r.next_attempt_at, r.id), (1, 200, id));
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RETRY-004"))]
#[test]
fn deferred_uploads_do_not_consume_attempts() {
    let q = queue(10);
    let id = queued(&q, "a");
    let mut worker = Worker::new(q.clone(), &stockhouse_ingest::DeferredUploader, &NoScanner, limits());
    for now in [100, 200, 300, 400, 500] {
        worker.pass(now, &mut Vec::new()).unwrap();
    }
    let r = record(&q, "a");
    assert_eq!((r.attempts, r.next_attempt_at, r.id), (0, 500 + 100, id));
    assert_eq!(q.status().unwrap().pending, 1);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RETRY-005"))]
#[test]
fn exhausted_retries_fail_visibly() {
    let q = queue(10);
    let id = queued(&q, "a");
    let worker = Worker::new(q.clone(), &stockhouse_ingest::DeferredUploader, &NoScanner, limits());
    for _ in 0..3 {
        worker.apply(record(&q, "a"), UploadOutcome::Retry(RetryKind::Transient, "boom".into()), 100).unwrap();
    }
    let failed = q.root().join("failed").join(format!("{}.json", path("a").hash()));
    let r = Record::from_json(&std::fs::read(failed).unwrap()).unwrap();
    assert_eq!((r.id, r.attempts, r.last_error.as_deref()), (id, 3, Some("boom")));
    let status = q.status().unwrap();
    assert_eq!((status.pending, status.failed), (0, 1));
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RETRY-006"))]
#[test]
fn rejected_uploads_fail_visibly() {
    let q = queue(10);
    let id = queued(&q, "a");
    let uploader = FakeUploader::answering(vec![UploadOutcome::Rejected("not allowed".into())]);
    Worker::new(q.clone(), &uploader, &NoScanner, limits()).pass(100, &mut Vec::new()).unwrap();
    let failed = q.root().join("failed").join(format!("{}.json", path("a").hash()));
    let r = Record::from_json(&std::fs::read(failed).unwrap()).unwrap();
    assert_eq!((r.id, r.reason.as_deref()), (id, Some("not allowed")));
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RETRY-007"))]
#[test]
fn upload_identity_survives_restarts() {
    let q = queue(10);
    let id = queued(&q, "a");
    let first = FakeUploader::answering(vec![UploadOutcome::Retry(RetryKind::Uncertain, "lost reply".into())]);
    Worker::new(q.clone(), &first, &NoScanner, limits()).pass(100, &mut Vec::new()).unwrap();
    let reopened = Queue::open(q.root()).unwrap();
    let second = FakeUploader::answering(vec![UploadOutcome::Retry(RetryKind::QuotaExceeded, "quota".into())]);
    Worker::new(reopened.clone(), &second, &NoScanner, limits()).pass(200, &mut Vec::new()).unwrap();
    let third = FakeUploader::default();
    Worker::new(Queue::open(q.root()).unwrap(), &third, &NoScanner, limits()).pass(1000, &mut Vec::new()).unwrap();
    let ids: Vec<_> = [&first, &second, &third].iter().flat_map(|u| u.jobs.borrow().clone()).map(|j| j.id).collect();
    assert_eq!(ids, [id, id, id]);
    assert_eq!(q.done_record(&path("a")).unwrap().id, id.to_string());
}
