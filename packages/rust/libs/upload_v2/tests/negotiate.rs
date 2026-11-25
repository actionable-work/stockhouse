mod common;

use common::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use stockhouse_credentials::CredentialError;
use stockhouse_ingest::{RetryKind, UploadOutcome, Uploader};
use stockhouse_test_support::counting_source;
use stockhouse_upload::{NegotiationFailure, Negotiator, ServerEndpoint};
use stockhouse_wire_v2::ProtocolMode;

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-002"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-003"))]
#[test]
fn selects_v1_or_v2_and_caches_the_selection_for_one_hour() {
    for (mode, response, v2) in [
        (ProtocolMode::V1, offer("[1,2]"), false),
        (ProtocolMode::Auto, offer("[1]"), false),
        (ProtocolMode::Auto, Action::Truncated(404, 5), false),
        (ProtocolMode::Auto, offer("[1,2]"), true),
    ] {
        let mut store = Store::new();
        let paths: Vec<_> = (0..3)
            .map(|i| store.add(&format!("probe-{i}"), &[]))
            .collect();
        let server = fake(move |req, _| (req.rel() == "protocol").then(|| response.clone()));
        let clock = Arc::new(Tick::default());
        let uploader =
            backend(&store, &server, mode, 4, 2).with_time(clock.clone(), Arc::new(no_wait));
        let queue = store.queue();
        for (index, now) in [0, 3599, 3600].into_iter().enumerate() {
            clock.0.store(now, Ordering::SeqCst);
            assert_eq!(
                uploader.upload(&job(&queue, &paths[index])),
                UploadOutcome::Uploaded
            );
            let expected = usize::from(mode != ProtocolMode::V1) * if index == 2 { 2 } else { 1 };
            assert_eq!(route_count(&server, "protocol"), expected);
        }
        assert_eq!(route_count(&server, "v2/declare"), if v2 { 3 } else { 0 });
        assert_eq!(
            server
                .requests()
                .iter()
                .filter(|req| req.rel().starts_with("uploads/"))
                .count(),
            if v2 { 0 } else { 3 }
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-004"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-005"))]
#[test]
fn probe_errors_fail_all_jobs_without_uploading_or_downgrading() {
    use std::time::Duration;
    for (mode, response, expected) in [
        (
            ProtocolMode::Auto,
            Action::Truncated(429, 5),
            "protocol probe: busy",
        ),
        (
            ProtocolMode::Auto,
            Action::Reply(503, "unavailable".into()),
            "protocol probe: unavailable",
        ),
        (
            ProtocolMode::Auto,
            Action::Close,
            "protocol probe: request failed",
        ),
        (
            ProtocolMode::Auto,
            Action::Sleep(Duration::from_millis(2200), 200, String::new()),
            "protocol probe: request failed",
        ),
        (
            ProtocolMode::Auto,
            Action::Reply(200, "invalid".into()),
            "protocol probe: unexpected reply (status 200)",
        ),
        (
            ProtocolMode::Auto,
            offer("[3,4]"),
            "no common protocol version",
        ),
        (
            ProtocolMode::V2,
            offer("[1]"),
            "server does not offer protocol v2",
        ),
    ] {
        let mut store = Store::new();
        let jobs: Vec<_> = (0..2)
            .map(|i| job(&store.queue(), &store.add(&format!("retry-{i}"), &[])))
            .collect();
        let server = fake(move |req, _| {
            assert_eq!(req.rel(), "protocol");
            Some(response.clone())
        });
        let uploader = backend(&store, &server, mode, 4, 2);
        assert_eq!(
            uploader.upload_batch(&jobs),
            vec![UploadOutcome::Retry(RetryKind::Transient, expected.into()); 2]
        );
        assert_eq!(server.lines(), ["GET protocol"]);
    }
    let server = fake(|_, _| Some(Action::Truncated(429, 16)));
    let endpoint = ServerEndpoint::new(&server.base(), source(), 2).unwrap();
    assert_eq!(
        Negotiator::new(ProtocolMode::Auto).version(&endpoint, 0),
        Err(NegotiationFailure::Busy(Some(16)))
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-029"))]
#[test]
fn unauthorized_probes_are_retried_once_only_after_invalidation() {
    for (refreshes, reject_second, probes) in [(true, false, 2), (false, false, 1), (true, true, 2)]
    {
        let mut store = Store::new();
        let path = store.add("probe-401", &[]);
        let count = AtomicUsize::new(0);
        let server = fake(move |req, _| {
            (req.rel() == "protocol"
                && (count.fetch_add(1, Ordering::SeqCst) == 0 || reject_second))
                .then(|| Action::Truncated(401, 0))
        });
        let credentials = counting_source(None, CredentialError::Missing, refreshes);
        let uploader = setup(
            &store,
            &server,
            ProtocolMode::Auto,
            4,
            2,
            credentials.clone(),
        );
        let outcome = uploader.upload(&job(&store.queue(), &path));
        if refreshes && !reject_second {
            assert_eq!(outcome, UploadOutcome::Uploaded);
            assert_eq!(route_count(&server, "v2/declare"), 1);
        } else {
            assert_eq!(
                outcome,
                UploadOutcome::Retry(RetryKind::Transient, "unauthorized".into())
            );
            assert_eq!(server.lines(), vec!["GET protocol"; probes]);
        }
        assert_eq!(route_count(&server, "protocol"), probes);
        assert_eq!(credentials.invalidations.load(Ordering::SeqCst), 1);
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-006"))]
#[test]
fn a_withdrawn_route_stops_the_call_but_keeps_completed_transfers_and_marks() {
    let mut store = Store::new();
    let (a, b, later) = (
        store.add("running-a", &[]),
        store.add("running-b", &[]),
        store.add("later", &[]),
    );
    let q = store.queue();
    let jobs = [job(&q, &a), job(&q, &b), job(&q, &later)];
    let probe = AtomicUsize::new(0);
    let both_running = Barrier::new(2);
    let running_a = a.clone();
    let server = fake(move |req, rows| match req.rel() {
        "protocol" if probe.fetch_add(1, Ordering::SeqCst) != 0 => Some(Action::Truncated(404, 0)),
        rel if rel.starts_with("v2/nar/") => {
            both_running.wait();
            Some(if rows[0].path() == &running_a {
                server::state("present")
            } else {
                Action::Truncated(404, 0)
            })
        }
        _ => None,
    });
    let uploader = backend(&store, &server, ProtocolMode::Auto, 2, 2);
    let outcome = uploader.upload_batch(&jobs);
    assert_eq!(outcome[0], UploadOutcome::Uploaded);
    for (result, path) in outcome[1..].iter().zip([&b, &later]) {
        assert_eq!(
            result,
            &UploadOutcome::Retry(
                RetryKind::Transient,
                format!("{path}: protocol v2 not available")
            )
        );
    }
    assert!(q.is_uploaded(&a));
    assert!(q.upload_state(&b).unwrap().nar_sent);
    assert_eq!(route_count(&server, "v2/declare"), 1);
    assert_eq!(route_count(&server, "v2/publish"), 0);
    assert_eq!(nar_count(&server), 2);
    assert_eq!(uploader.upload(&jobs[2]), UploadOutcome::Uploaded);
    assert_eq!(route_count(&server, "protocol"), 2);
    assert!(server.lines().last().unwrap().starts_with("POST uploads/"));
}
