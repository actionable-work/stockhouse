mod common;

use common::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use stockhouse_ingest::{RetryKind, UploadOutcome, Uploader};
use stockhouse_wire_v2::ProtocolMode;

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-017"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-030"))]
#[test]
fn retries_are_bounded_sequential_and_byte_identical() {
    for (route, kind) in [
        ("v2/declare", RetryKind::Transient),
        ("v2/status", RetryKind::Transient),
        ("v2/publish", RetryKind::Uncertain),
    ] {
        let mut store = Store::new();
        let path = store.add("resend", &[]);
        let q = store.queue();
        let job = job(&q, &path);
        if route == "v2/status" {
            mark(&store, &path);
        }
        let calls = AtomicUsize::new(0);
        let active = AtomicUsize::new(0);
        let server = fake(move |req, _| {
            if req.rel() != route {
                return None;
            }
            assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
            std::thread::sleep(std::time::Duration::from_millis(25));
            active.fetch_sub(1, Ordering::SeqCst);
            Some(match calls.fetch_add(1, Ordering::SeqCst) {
                0 => Action::Truncated(429, 120),
                1 => Action::Reply(503, "outage".into()),
                _ => Action::Close,
            })
        });
        let delays = Arc::new(Mutex::new(Vec::new()));
        let record = delays.clone();
        let uploader = backend(&store, &server, ProtocolMode::V2, 5, 2).with_time(
            Arc::new(Tick::default()),
            Arc::new(move |delay| record.lock().unwrap().push(delay.as_secs())),
        );
        assert_eq!(
            uploader.upload(&job),
            UploadOutcome::Retry(
                kind,
                format!("{path}: POST {}: request failed", route.replace('/', " "))
            )
        );
        assert_eq!(*delays.lock().unwrap(), [30, 4]);
        let attempts: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|req| req.rel() == route)
            .collect();
        assert_eq!(attempts.len(), 3);
        assert!(attempts.windows(2).all(|pair| pair[0].body == pair[1].body));
        assert_eq!(route_count(&server, route), 3);
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-018"))]
#[test]
fn batch_level_bad_requests_reject_and_bad_replies_retry() {
    for (action, retry) in [
        (server::error(400, "bad_request"), None),
        (server::error(413, "too_large"), Some("protocol violation")),
        (
            Action::Reply(418, "unexpected".into()),
            Some("unexpected reply (status 418)"),
        ),
    ] {
        let mut store = Store::new();
        let path = store.add("batch-refusal", &[]);
        let q = store.queue();
        let server = fake(move |req, _| (req.rel() == "v2/declare").then(|| action.clone()));
        let expected = match retry {
            Some(why) => UploadOutcome::Retry(
                RetryKind::Transient,
                format!("{path}: POST v2 declare: {why}"),
            ),
            None => UploadOutcome::Rejected(format!("{path}: POST v2 declare: bad_request")),
        };
        assert_eq!(
            backend(&store, &server, ProtocolMode::V2, 5, 1).upload(&job(&q, &path)),
            expected
        );
        assert_eq!(server.lines(), ["GET protocol", "POST v2/declare"]);
    }
}
