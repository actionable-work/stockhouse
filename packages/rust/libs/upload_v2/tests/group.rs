mod common;

use common::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use stockhouse_ingest::{RetryKind, UploadOutcome, Uploader};
use stockhouse_wire_v2::ProtocolMode;

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-007"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-008"))]
#[test]
fn overlapping_closures_share_one_declaration_and_split_at_the_union_bound() {
    let mut store = Store::new();
    let shared = store.add("shared", &[]);
    let a = store.add("first", &[&shared]);
    let b = store.add("second", &[&shared]);
    let c = store.add("third-group", &[&shared]);
    let malformed = store.add("malformed", &[]);
    let big = store.add("oversized-closure", &[&a, &b, &c]);
    store.set(&format!("closure/{}.json", malformed.hash()), "invalid");
    let absent = store_path("absent");
    let q = store.queue();
    let jobs: Vec<_> = [&absent, &a, &malformed, &b, &big, &c]
        .into_iter()
        .map(|path| job(&q, path))
        .collect();
    let batches = Arc::new(Mutex::new(Vec::new()));
    let recorded = batches.clone();
    let server = fake(move |req, rows| {
        if req.rel() == "v2/declare" {
            recorded
                .lock()
                .unwrap()
                .push(rows.iter().map(|r| r.path().clone()).collect::<Vec<_>>());
        }
        None
    });
    assert_eq!(
        backend(&store, &server, ProtocolMode::V2, 3, 2).upload_batch(&jobs),
        [
            UploadOutcome::Rejected("not in the local store".into()),
            UploadOutcome::Uploaded,
            UploadOutcome::Retry(
                RetryKind::Transient,
                "unrecognized nix path-info closure".into()
            ),
            UploadOutcome::Uploaded,
            UploadOutcome::Rejected("closure too large".into()),
            UploadOutcome::Uploaded,
        ]
    );
    let batches = batches.lock().unwrap();
    let mut referrers = vec![a, b];
    referrers.sort();
    let mut first = vec![shared];
    first.extend(referrers);
    assert_eq!(*batches, [first, vec![c]]);
    assert_eq!(route_count(&server, "v2/publish"), 2);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-036"))]
#[test]
fn later_groups_observe_a_renewed_dependency_or_its_completion() {
    for completed in [false, true] {
        let mut store = Store::new();
        let dep = store.add("later-root", &[]);
        let root = store.add("first-root", &[&dep]);
        let unrelated = store.add("next-group", &[]);
        let q = store.queue();
        let jobs = [job(&q, &root), job(&q, &unrelated), job(&q, &dep)];
        let stale = jobs[2].id;
        let counts = AtomicUsize::new(0);
        let watched = dep.clone();
        let server = fake(move |req, rows| match req.rel() {
            "v2/declare" if rows.iter().any(|row| row.id == stale) => Some(reply(
                rows.iter()
                    .map(|row| {
                        if row.id == stale {
                            row.error("unknown_upload")
                        } else {
                            row.state("verified")
                        }
                    })
                    .collect(),
            )),
            rel if rel.starts_with("v2/nar/")
                && rows[0].path() == &watched
                && !completed
                && counts.fetch_add(1, Ordering::SeqCst) == 0 =>
            {
                Some(Action::Truncated(429, 0))
            }
            _ => None,
        });
        let results = backend(&store, &server, ProtocolMode::V2, 2, 2).upload_batch(&jobs);
        let current = q.done_record(&dep).unwrap().id;
        assert_ne!(current, stale.to_string());
        assert_eq!(results[2], UploadOutcome::Uploaded);
        let lines = server.lines();
        if completed {
            assert_eq!(results[0], UploadOutcome::Uploaded);
            assert!(!lines.iter().any(|line| line == "POST v2/status"));
        } else {
            assert!(matches!(
                results[0],
                UploadOutcome::Retry(RetryKind::Transient, _)
            ));
            let status = server
                .requests()
                .into_iter()
                .find(|req| req.rel() == "v2/status")
                .unwrap();
            assert!(
                String::from_utf8(status.body.unwrap())
                    .unwrap()
                    .contains(&current)
            );
            assert_eq!(
                lines
                    .iter()
                    .filter(|line| **line == format!("PUT v2/nar/{current}"))
                    .count(),
                2
            );
        }
    }
}
