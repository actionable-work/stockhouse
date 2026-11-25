mod common;

use common::{FakeUploader, limits, path, queue};
use std::cell::RefCell;
use stockhouse_ingest::{NoScanner, Queue, RetryKind, UploadJob, UploadOutcome, Uploader, Worker};

struct BatchUploader<F>(F);

impl<F: Fn(&[UploadJob]) -> Vec<UploadOutcome>> Uploader for BatchUploader<F> {
    fn upload(&self, _: &UploadJob) -> UploadOutcome {
        panic!("worker called a single upload");
    }

    fn upload_batch(&self, jobs: &[UploadJob]) -> Vec<UploadOutcome> {
        (self.0)(jobs)
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-001"))]
#[test]
fn worker_passes_due_jobs_once_and_applies_the_ordered_results() {
    let queue = queue(4);
    let (first, second, over_limit, future) =
        (path("first"), path("second"), path("limit"), path("future"));
    {
        let mut producer = queue.producer().unwrap();
        for (path, due) in [(&second, 2), (&future, 20), (&over_limit, 3), (&first, 1)] {
            producer.enqueue(path, due, false).unwrap();
        }
    }

    let calls = RefCell::new(Vec::new());
    let batch = BatchUploader(|jobs: &[UploadJob]| {
        calls.borrow_mut().push(jobs.to_vec());
        vec![
            UploadOutcome::Rejected("rejected first".into()),
            UploadOutcome::Uploaded,
        ]
    });
    let mut limits = limits();
    limits.batch_size = 2;
    let report = Worker::new(queue.clone(), &batch, &NoScanner, limits)
        .pass(4, &mut Vec::new())
        .unwrap();

    let calls = calls.into_inner();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0]
            .iter()
            .map(|job| &job.store_path)
            .collect::<Vec<_>>(),
        [&first, &second]
    );
    assert_eq!((report.attempted, report.uploaded), (2, 1));
    assert!(queue.is_uploaded(&second));
    assert!(
        queue
            .root()
            .join(format!("failed/{}.json", first.hash()))
            .is_file()
    );
    assert_eq!(queue.pending_count().unwrap(), 2);

    let outcomes = vec![
        UploadOutcome::Uploaded,
        UploadOutcome::Deferred("later".into()),
    ];
    let sequential = FakeUploader::answering(outcomes.clone());
    assert_eq!(sequential.upload_batch(&calls[0]), outcomes);
    assert_eq!(*sequential.jobs.borrow(), calls[0]);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-027"))]
#[test]
fn a_completion_during_the_batch_wins_over_a_stale_retry() {
    let queue = queue(2);
    let root = path("completed");
    queue.producer().unwrap().enqueue(&root, 0, false).unwrap();
    let batch = BatchUploader(|jobs: &[UploadJob]| {
        let [job] = jobs else {
            panic!("expected one job")
        };
        complete_during_batch(&queue, job)
    });
    let mut limits = limits();
    limits.max_attempts = 1;
    let report = Worker::new(queue.clone(), &batch, &NoScanner, limits)
        .pass(20, &mut Vec::new())
        .unwrap();
    assert_eq!(report.uploaded, 1);
    assert_eq!(queue.pending_count().unwrap(), 0);
    assert_eq!(queue.status().unwrap().failed, 0);
    assert_eq!(queue.done_record(&root).unwrap().uploaded_at, 10);
}

fn complete_during_batch(queue: &Queue, job: &UploadJob) -> Vec<UploadOutcome> {
    queue.complete_path(&job.store_path, job.id, 10).unwrap();
    vec![UploadOutcome::Retry(
        RetryKind::Transient,
        "stale retry".into(),
    )]
}
