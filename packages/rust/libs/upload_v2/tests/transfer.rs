mod common;

use common::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use stockhouse_ingest::{RetryKind, UploadOutcome, Uploader};
use stockhouse_wire_v2::ProtocolMode;

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-012"))]
#[test]
fn concurrency_is_bounded_and_a_refusal_does_not_stop_other_transfers() {
    let mut store = Store::new();
    let paths: Vec<_> = (0..7)
        .map(|i| store.add(&format!("concurrent-{i}"), &[]))
        .collect();
    let rejected = paths[0].clone();
    let q = store.queue();
    let jobs: Vec<_> = paths.iter().map(|path| job(&q, path)).collect();
    let active = AtomicUsize::new(0);
    let peak = Arc::new(AtomicUsize::new(0));
    let observed = peak.clone();
    let failed = rejected.clone();
    let server = fake(move |req, rows| {
        if !req.rel().starts_with("v2/nar/") {
            return None;
        }
        let count = active.fetch_add(1, Ordering::SeqCst) + 1;
        observed.fetch_max(count, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        active.fetch_sub(1, Ordering::SeqCst);
        (rows[0].path() == &failed).then(|| server::error(400, "verification_failed"))
    });
    let uploader = backend(&store, &server, ProtocolMode::V2, 12, 3);
    let results = uploader.upload_batch(&jobs);
    assert_eq!(
        results[0],
        UploadOutcome::Rejected(format!("{rejected}: PUT v2 nar: verification_failed"))
    );
    assert_eq!(&results[1..], vec![UploadOutcome::Uploaded; 6]);
    assert_eq!(peak.load(Ordering::SeqCst), 3);
    assert_eq!(nar_count(&server), 7);
    assert_eq!(route_count(&server, "v2/publish"), 1);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-013"))]
#[test]
fn nar_replies_distinguish_confirmation_rejection_and_uncertainty() {
    for (action, should_publish, rejection, retry) in [
        (server::state("present"), false, None, None),
        (server::state("published"), false, None, None),
        (server::state("verified"), true, None, None),
        (
            server::error(400, "verification_failed"),
            false,
            Some("verification_failed"),
            None,
        ),
        (
            server::error(400, "bad_request"),
            false,
            Some("bad_request"),
            None,
        ),
        (
            server::error(409, "conflict"),
            false,
            Some("conflict"),
            None,
        ),
        (
            server::error(413, "too_large"),
            false,
            Some("too_large"),
            None,
        ),
        (
            Action::Close,
            false,
            None,
            Some((RetryKind::Uncertain, "request failed")),
        ),
        (
            Action::Reply(200, "bad".into()),
            false,
            None,
            Some((RetryKind::Uncertain, "unexpected reply (status 200)")),
        ),
        (
            Action::Truncated(429, 9),
            false,
            None,
            Some((RetryKind::Transient, "busy")),
        ),
        (
            Action::Truncated(503, 9),
            false,
            None,
            Some((RetryKind::Transient, "unavailable")),
        ),
    ] {
        let mut store = Store::new();
        let root = store.add("nar-result", &[]);
        let q = store.queue();
        let server = fake(move |req, _| req.rel().starts_with("v2/nar/").then(|| action.clone()));
        let outcome = backend(&store, &server, ProtocolMode::V2, 2, 1).upload(&job(&q, &root));
        let expected = match (rejection, retry) {
            (Some(code), _) => UploadOutcome::Rejected(format!("{root}: PUT v2 nar: {code}")),
            (_, Some((kind, why))) => {
                UploadOutcome::Retry(kind, format!("{root}: PUT v2 nar: {why}"))
            }
            _ => UploadOutcome::Uploaded,
        };
        assert_eq!(outcome, expected);
        assert_eq!(
            route_count(&server, "v2/publish"),
            usize::from(should_publish)
        );
        assert_eq!(nar_count(&server), 1);
        if retry.is_some() {
            assert!(q.upload_state(&root).unwrap().nar_sent);
        }
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-012"))]
#[test]
fn nar_dump_failure_reaps_the_child_and_mismatches_hold_back_the_final_chunk() {
    for slow in [false, true] {
        let mut store = Store::new();
        let root = store.add_sized("bad-nar", &[], 128 * 1024);
        let q = store.queue();
        if slow {
            store.set("dump.sleep", "");
        } else {
            let mut nar = store.nar(&root).to_vec();
            *nar.last_mut().unwrap() ^= 1;
            std::fs::write(store.dir.join("nar").join(root.hash().as_str()), nar).unwrap();
        }
        let server = fake(|req, _| req.rel().starts_with("v2/nar/").then_some(Action::Close));
        let result = backend(&store, &server, ProtocolMode::V2, 2, 1).upload(&job(&q, &root));
        assert_eq!(route_count(&server, "v2/publish"), 0);
        assert!(
            server
                .requests()
                .iter()
                .filter(|req| req.rel().starts_with("v2/nar/"))
                .all(|req| !req.complete)
        );
        if slow {
            assert!(matches!(
                result,
                UploadOutcome::Retry(RetryKind::Transient, _)
            ));
            wait_gone(&store.dir.join("dump.pid"));
        } else {
            assert_eq!(
                result,
                UploadOutcome::Rejected(format!("{root}: local NAR does not match its NarHash"))
            );
        }
    }
}
