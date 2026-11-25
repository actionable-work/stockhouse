mod common;

use common::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use stockhouse_ingest::{RetryKind, UploadOutcome, Uploader};
use stockhouse_wire_v2::{IdBatch, ProtocolMode};

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-014"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-015"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-016"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-028"))]
#[test]
fn a_refused_dependency_holds_its_verified_referrer_until_the_next_job() {
    let mut store = Store::new();
    let dep = store.add("lower", &[]);
    let root = store.add("upper", &[&dep]);
    let q = store.queue();
    let dep_id = job(&q, &dep).id;
    let root_job = job(&q, &root);
    let root_id = root_job.id;
    let calls = AtomicUsize::new(0);
    let missing = dep.clone();
    let server = fake(move |req, rows| {
        if req.rel() != "v2/publish" {
            return None;
        }
        assert_eq!(
            rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            [dep_id, root_id]
        );
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Some(reply(vec![
                rows[0].error("quota_exceeded"),
                rows[1].missing(&[&missing]),
            ]))
        } else {
            Some(reply(vec![
                rows[0].state("present"),
                rows[1].state("published"),
            ]))
        }
    });
    let uploader = backend(&store, &server, ProtocolMode::V2, 8, 2);
    assert_eq!(
        uploader.upload(&root_job),
        UploadOutcome::Retry(
            RetryKind::QuotaExceeded,
            format!("{dep}: POST v2 publish: quota_exceeded")
        )
    );
    assert!(!q.is_uploaded(&root));
    assert!(q.upload_state(&root).unwrap().nar_sent);
    assert_eq!(nar_count(&server), 2);
    let before = server.lines().len();
    assert_eq!(uploader.upload(&root_job), UploadOutcome::Uploaded);
    assert_eq!(
        &server.lines()[before..],
        ["POST v2/status", "POST v2/publish"]
    );
    assert_eq!(nar_count(&server), 2);
    assert!(q.is_uploaded(&dep));
    assert!(q.is_uploaded(&root));
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-028"))]
#[test]
fn reconciliation_after_lost_publish_never_resends_a_verified_body() {
    for committed in [false, true] {
        let mut store = Store::new();
        let path = store.add("lost-publish", &[]);
        let q = store.queue();
        let job = job(&q, &path);
        let prior = AtomicUsize::new(0);
        let server = fake(move |req, rows| {
            if req.rel() == "v2/publish" && prior.fetch_add(1, Ordering::SeqCst) < 3 {
                Some(Action::Close)
            } else if req.rel() == "v2/status" && committed {
                Some(reply(vec![rows[0].state("published")]))
            } else {
                None
            }
        });
        let uploader = backend(&store, &server, ProtocolMode::V2, 8, 4);
        assert_eq!(
            uploader.upload(&job),
            UploadOutcome::Retry(
                RetryKind::Uncertain,
                format!("{path}: POST v2 publish: request failed")
            )
        );
        assert!(q.upload_state(&path).unwrap().nar_sent);
        assert_eq!(route_count(&server, "v2/publish"), 3);
        let before = server.lines().len();
        assert_eq!(uploader.upload(&job), UploadOutcome::Uploaded);
        let lines = server.lines();
        assert_eq!(lines[before], "POST v2/status");
        assert_eq!(
            route_count(&server, "v2/publish"),
            if committed { 3 } else { 4 }
        );
        assert_eq!(nar_count(&server), 1);
        assert_eq!(q.done_record(&path).unwrap().id, job.id.to_string());
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-014"))]
#[test]
fn publish_rechecks_eligibility_after_each_full_batch() {
    let mut store = Store::new();
    let mut leaves: Vec<_> = (0..1001)
        .map(|i| store.add(&format!("prior-{i}"), &[]))
        .collect();
    leaves.sort();
    let root = store.add("batch-referrer", &leaves.iter().collect::<Vec<_>>());
    let q = store.queue();
    let job = job(&q, &root);
    let pub_calls = AtomicUsize::new(0);
    let root_id = job.id;
    let server = fake(move |req, rows| match req.rel() {
        "v2/declare" => Some(reply(
            rows.iter().map(|row| row.state("verified")).collect(),
        )),
        "v2/publish" => {
            let publish = pub_calls.fetch_add(1, Ordering::SeqCst);
            let batch = IdBatch::parse(req.body.as_ref().unwrap()).unwrap();
            assert_eq!(batch.entries().len(), if publish == 0 { 1000 } else { 1 });
            assert!(!batch.entries().contains(&root_id));
            Some(reply(
                rows.iter()
                    .enumerate()
                    .map(|(i, row)| {
                        if publish == 0 && i == 0 {
                            row.error("verification_failed")
                        } else {
                            row.state("published")
                        }
                    })
                    .collect(),
            ))
        }
        _ => None,
    });
    assert_eq!(
        backend(&store, &server, ProtocolMode::V2, 1002, 4).upload(&job),
        UploadOutcome::Rejected(format!(
            "{}: POST v2 publish: verification_failed",
            leaves[0]
        ))
    );
    assert_eq!(route_count(&server, "v2/publish"), 2);
    assert!(q.is_uploaded(&leaves[1000]));
    assert!(!q.is_uploaded(&root));
    assert!(q.upload_state(&root).unwrap().nar_sent);
}
