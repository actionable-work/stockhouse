mod common;
use common::server::error;
use common::*;
use std::sync::{Arc, Mutex};
use stockhouse_credentials::CredentialError;
use stockhouse_ingest::{NoScanner, RetryKind, UploadOutcome, Uploader, Worker, WorkerLimits};
use stockhouse_wire::UploadDeclaration;

fn posts(server: &FakeServer) -> Vec<String> {
    server
        .requests()
        .iter()
        .filter(|r| r.method == "POST")
        .filter_map(declared)
        .collect()
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-003"))]
#[test]
fn the_bearer_goes_only_under_the_base_url() {
    let mut store = Store::new();
    let a = store.add("a", &[]);
    let root = store.add("root", &[&a]);
    let server = FakeServer::protocol(Arc::default(), |_| None);
    let queue = store.queue();
    assert_eq!(
        uploader(&store, &server).upload(&job(&queue, &root)),
        UploadOutcome::Uploaded
    );
    let bearer = format!("Bearer {TOKEN}");
    for req in server.requests() {
        assert!(req.path.starts_with("/t/1/uploads/"), "{}", req.path);
        assert_eq!(req.header("authorization"), Some(bearer.as_str()));
    }
    let redirect = FakeServer::start(|req| {
        req.body.as_ref()?;
        Some(Action::Reply(302, String::new()))
    });
    let other = store.add("other", &[]);
    let outcome = uploader(&store, &redirect).upload(&job(&queue, &other));
    assert!(
        matches!(outcome, UploadOutcome::Retry(RetryKind::Transient, _)),
        "{outcome:?}"
    );
    assert_eq!(redirect.requests().len(), 1);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-004"))]
#[test]
fn credentials_never_appear_in_outcomes() {
    let marker = "body-marker-7f3a";
    let mut store = Store::new();
    let queue = store.queue();
    let cases: [(u16, String, RetryKind, &str); 5] = [
        (
            400,
            format!("{{\"error\":\"bad_request\",\"why\":\"{marker}\"}}"),
            RetryKind::Transient,
            "POST declare: unexpected reply (status 400)",
        ),
        (
            500,
            format!("{marker} {TOKEN}"),
            RetryKind::Transient,
            "POST declare: unexpected reply (status 500)",
        ),
        (
            503,
            marker.to_owned(),
            RetryKind::Transient,
            "POST declare: unavailable",
        ),
        (
            401,
            TOKEN.to_owned(),
            RetryKind::Transient,
            "POST declare: unauthorized",
        ),
        (
            302,
            marker.to_owned(),
            RetryKind::Transient,
            "POST declare: unexpected reply (status 302)",
        ),
    ];
    for (i, (status, body, kind, why)) in cases.into_iter().enumerate() {
        let root = store.add(&format!("p{i}"), &[]);
        let server = FakeServer::start(move |req| {
            req.body.as_ref()?;
            Some(Action::Reply(status, body.clone()))
        });
        let outcome = uploader(&store, &server).upload(&job(&queue, &root));
        assert_eq!(
            outcome,
            UploadOutcome::Retry(kind, format!("{root}: {why}"))
        );
    }
    // A permanent code, and an off-table reply after the NAR was sent.
    let root = store.add("conflict", &[]);
    let server = FakeServer::start(|req| req.body.as_ref().map(|_| error(409, "conflict")));
    assert_eq!(
        uploader(&store, &server).upload(&job(&queue, &root)),
        UploadOutcome::Rejected(format!("{root}: POST declare: conflict"))
    );
    let root = store.add("redirected", &[]);
    let server = FakeServer::protocol(Arc::default(), move |req| {
        (req.method == "PUT").then(|| Action::Reply(302, marker.to_owned()))
    });
    let outcome = uploader(&store, &server).upload(&job(&queue, &root));
    assert_eq!(
        outcome,
        UploadOutcome::Retry(
            RetryKind::Uncertain,
            format!("{root}: PUT nar: unexpected reply (status 302)")
        )
    );
    let text = format!("{outcome:?}");
    assert!(!text.contains(TOKEN) && !text.contains(marker), "{text}");
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-005"))]
#[test]
fn credential_errors_defer_without_a_request() {
    let mut store = Store::new();
    let root = store.add("root", &[]);
    let server = FakeServer::protocol(Arc::default(), |_| None);
    let queue = store.queue();
    for error in [
        CredentialError::NotConfigured,
        CredentialError::Missing,
        CredentialError::Symlink,
        CredentialError::PermissionsTooOpen { mode: 0o644 },
    ] {
        let (source, _) = counting(Some(1), error);
        let outcome = uploader_with(&store, &server, source).upload(&job(&queue, &root));
        assert_eq!(
            outcome,
            UploadOutcome::Deferred(format!("credential unavailable: {error}"))
        );
    }
    assert!(server.requests().is_empty());
    // A credential that is not a valid header value is never sent as an
    // empty bearer.
    let bad = std::sync::Arc::new(stockhouse_credentials::StaticCredential::new(b"tok\nen"));
    let outcome = uploader_with(&store, &server, bad).upload(&job(&queue, &root));
    assert_eq!(
        outcome,
        UploadOutcome::Deferred(
            "credential unavailable: credential is not a valid header value".into()
        )
    );
    assert!(server.requests().is_empty());
    // Through the worker, a deferral consumes no attempt.
    let (source, _) = counting(Some(1), CredentialError::NotConfigured);
    let up = uploader_with(&store, &server, source);
    let limits = WorkerLimits {
        batch_size: 8,
        max_attempts: 3,
        backoff_base_secs: 1,
        backoff_max_secs: 10,
        rescan_interval_secs: 1000,
    };
    Worker::new(queue.clone(), &up, &NoScanner, limits)
        .pass(0, &mut Vec::new())
        .unwrap();
    let record =
        std::fs::read_to_string(queue.root().join(format!("pending/{}.json", root.hash())))
            .unwrap();
    assert!(record.contains("\"attempts\":0"), "{record}");
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-006"))]
#[test]
fn credential_errors_later_in_a_job_also_defer() {
    let mut store = Store::new();
    let root = store.add("root", &[]);
    let server = FakeServer::protocol(Arc::default(), |_| None);
    let queue = store.queue();
    let (source, reads) = counting(Some(2), CredentialError::Unreadable);
    let outcome = uploader_with(&store, &server, source).upload(&job(&queue, &root));
    assert!(matches!(outcome, UploadOutcome::Deferred(_)), "{outcome:?}");
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        server
            .lines()
            .iter()
            .filter(|l| l.starts_with("PUT"))
            .count(),
        0
    );
    let entry = std::fs::read_to_string(entry_file(&queue, &root)).unwrap();
    assert!(entry.contains("\"nar_sent\":true"), "{entry}");
    // A source that fails when read again after a successful invalidate.
    let other = store.add("other", &[]);
    let rejecting =
        FakeServer::start(|req| req.body.as_ref().map(|_| Action::Reply(401, String::new())));
    let source = counting_source(Some(2), CredentialError::Unreadable, true);
    let outcome = uploader_with(&store, &rejecting, source.clone()).upload(&job(&queue, &other));
    assert!(matches!(outcome, UploadOutcome::Deferred(_)), "{outcome:?}");
    assert_eq!(rejecting.requests().len(), 1);
    assert_eq!(
        source
            .invalidations
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-007"))]
#[test]
fn a_401_retries_once_only_after_a_successful_invalidate() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut store = Store::new();
    let queue = store.queue();
    // invalidate returns true: the request is sent again and succeeds.
    let root = store.add("refreshed", &[]);
    let first = Arc::new(AtomicBool::new(true));
    let f2 = first.clone();
    let server = FakeServer::protocol(Arc::default(), move |_| {
        f2.swap(false, Ordering::SeqCst)
            .then(|| Action::Reply(401, String::new()))
    });
    let source = counting_source(None, CredentialError::Missing, true);
    let outcome = uploader_with(&store, &server, source.clone()).upload(&job(&queue, &root));
    assert_eq!(outcome, UploadOutcome::Uploaded);
    assert_eq!(source.invalidations.load(Ordering::SeqCst), 1);
    let lines = server.lines();
    assert_eq!(lines[0], lines[1]);
    assert!(lines[0].starts_with("POST"));
    // invalidate returns false: no retry.
    for (refreshes, requests) in [(false, 1), (true, 2)] {
        let path = store.add(&format!("rejected-{refreshes}"), &[]);
        let server =
            FakeServer::start(|req| req.body.as_ref().map(|_| Action::Reply(401, String::new())));
        let source = counting_source(None, CredentialError::Missing, refreshes);
        let outcome = uploader_with(&store, &server, source.clone()).upload(&job(&queue, &path));
        assert_eq!(
            outcome,
            UploadOutcome::Retry(
                RetryKind::Transient,
                format!("{path}: POST declare: unauthorized")
            )
        );
        assert_eq!(server.requests().len(), requests, "{refreshes}");
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-013"))]
#[test]
fn references_upload_before_referrers() {
    let mut store = Store::new();
    let a = store.add("a", &[]);
    let b = store.add("b", &[&a]);
    let c = store.add("c", &[&a]);
    let root = store.add("root", &[&b, &c]);
    let server = FakeServer::protocol(Arc::default(), |_| None);
    let queue = store.queue();
    assert_eq!(
        uploader(&store, &server).upload(&job(&queue, &root)),
        UploadOutcome::Uploaded
    );
    let (first, second) = if b < c { (&b, &c) } else { (&c, &b) };
    let expected: Vec<String> = [&a, first, second, &root]
        .iter()
        .map(|p| p.to_string())
        .collect();
    // Exactly one declaration per path: no missing-reference round was needed.
    assert_eq!(posts(&server), expected);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-014"))]
#[test]
fn uploaded_dependencies_are_skipped() {
    let mut store = Store::new();
    let a = store.add("a", &[]);
    let root = store.add("root", &[&a]);
    let proto = Arc::new(Protocol::default());
    proto.hold(a.basename());
    let server = FakeServer::protocol(proto, |_| None);
    let queue = store.queue();
    queue
        .complete_path(&a, stockhouse_proto::UploadId::generate(), 1)
        .unwrap();
    assert_eq!(
        uploader(&store, &server).upload(&job(&queue, &root)),
        UploadOutcome::Uploaded
    );
    assert_eq!(posts(&server), vec![root.to_string()]);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-015"))]
#[test]
fn declarations_carry_the_path_info_facts() {
    let mut store = Store::new();
    let a = store.add("a", &[]);
    let root = store.add("root", &[&root_self(), &a]);
    let server = FakeServer::protocol(Arc::default(), |_| None);
    let queue = store.queue();
    uploader(&store, &server).upload(&job(&queue, &root));
    let req = server
        .requests()
        .into_iter()
        .find(|r| r.method == "POST" && declared(r).as_deref() == Some(root.as_str()))
        .unwrap();
    assert_eq!(req.header("content-type"), Some("application/json"));
    let expected = UploadDeclaration::new(
        root.clone(),
        stockhouse_proto::Sha256::digest(store.nar(&root)),
        store.nar(&root).len() as u64,
        vec![root.clone(), a.clone()],
        None,
        None,
    )
    .unwrap();
    assert_eq!(req.body.unwrap(), expected.render());
}

fn root_self() -> stockhouse_proto::StorePath {
    store_path("root")
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-016"))]
#[test]
fn held_paths_complete_without_a_nar() {
    let mut store = Store::new();
    let a = store.add("a", &[]);
    let root = store.add("root", &[&a]);
    let proto = Arc::new(Protocol::default());
    proto.hold(a.basename());
    proto.hold(root.basename());
    let server = FakeServer::protocol(proto, |_| None);
    let queue = store.queue();
    assert_eq!(
        uploader(&store, &server).upload(&job(&queue, &root)),
        UploadOutcome::Uploaded
    );
    assert!(server.lines().iter().all(|l| l.starts_with("POST")));
    assert!(queue.is_uploaded(&a));
    assert!(!entry_file(&queue, &a).exists() && !entry_file(&queue, &root).exists());
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-017"))]
#[test]
fn an_open_upload_is_marked_before_its_nar_is_sent() {
    let mut store = Store::new();
    let root = store.add("root", &[]);
    let queue = store.queue();
    let file = entry_file(&queue, &root);
    let seen = Arc::new(Mutex::new(None));
    let record = seen.clone();
    let server = FakeServer::protocol(Arc::default(), move |req| {
        if req.method == "PUT" {
            *record.lock().unwrap() = std::fs::read_to_string(&file).ok();
        }
        None
    });
    assert_eq!(
        uploader(&store, &server).upload(&job(&queue, &root)),
        UploadOutcome::Uploaded
    );
    let at_put = seen.lock().unwrap().clone().unwrap();
    assert!(at_put.contains("\"nar_sent\":true"), "{at_put}");
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-022"))]
#[test]
fn missing_references_are_reuploaded_through_the_worklist() {
    let mut store = Store::new();
    let b = store.add("b", &[]);
    let a = store.add("a", &[&b]);
    let root = store.add("root", &[&a]);
    let queue = store.queue();
    for p in [&a, &b] {
        queue
            .complete_path(p, stockhouse_proto::UploadId::generate(), 1)
            .unwrap();
    }
    let ledger_seen = Arc::new(Mutex::new(Vec::new()));
    let (q, seen, a2) = (queue.clone(), ledger_seen.clone(), a.clone());
    let server = FakeServer::protocol(Arc::default(), move |req| {
        if req.method == "POST" && declared(req).as_deref() == Some(a2.as_str()) {
            seen.lock().unwrap().push(q.is_uploaded(&a2));
        }
        None
    });
    assert_eq!(
        uploader(&store, &server).upload(&job(&queue, &root)),
        UploadOutcome::Uploaded
    );
    let expected: Vec<String> = [&root, &a, &b, &a, &root]
        .iter()
        .map(|p| p.to_string())
        .collect();
    assert_eq!(posts(&server), expected);
    assert!(ledger_seen.lock().unwrap().iter().all(|uploaded| !uploaded));
    assert!(queue.is_uploaded(&a) && queue.is_uploaded(&b));
}

fn always(status: u16, body: &'static str, method: &'static str) -> FakeServer {
    FakeServer::protocol(Arc::default(), move |req| {
        (req.method == method).then(|| Action::Reply(status, body.to_owned()))
    })
}

fn req(method: &str) -> &'static str {
    if method == "POST" {
        "POST declare"
    } else {
        "PUT nar"
    }
}

fn one_path() -> (Store, stockhouse_proto::StorePath) {
    let mut store = Store::new();
    let root = store.add("root", &[]);
    (store, root)
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-023"))]
#[test]
fn missing_reference_and_redeclare_transitions_are_bounded() {
    let mut store = Store::new();
    let a = store.add("a", &[]);
    let root = store.add("root", &[&a]);
    let queue = store.queue();
    let missing_a = format!(
        "{{\"error\":\"missing_references\",\"missing\":[\"{}\"]}}",
        a.basename()
    );
    let r2 = root.clone();
    let server = FakeServer::protocol(Arc::default(), move |req| {
        (declared(req).as_deref() == Some(r2.as_str()))
            .then(|| Action::Reply(422, missing_a.clone()))
    });
    let outcome = uploader(&store, &server).upload(&job(&queue, &root));
    assert_eq!(
        outcome,
        UploadOutcome::Retry(
            RetryKind::Transient,
            format!("{root}: POST declare: references still missing")
        )
    );
    let other = store_path("undeclared");
    let bad = format!(
        "{{\"error\":\"missing_references\",\"missing\":[\"{}\"]}}",
        other.basename()
    );
    let server = FakeServer::protocol(Arc::default(), move |req| {
        (req.method == "POST").then(|| Action::Reply(422, bad.clone()))
    });
    let lonely = store.add("lonely", &[]);
    assert_eq!(
        uploader(&store, &server).upload(&job(&queue, &lonely)),
        UploadOutcome::Retry(
            RetryKind::Transient,
            format!("{lonely}: POST declare: protocol violation")
        )
    );
    let server = always(404, "{\"error\":\"unknown_upload\"}", "PUT");
    let again = store.add("again", &[]);
    assert_eq!(
        uploader(&store, &server).upload(&job(&queue, &again)),
        UploadOutcome::Retry(
            RetryKind::Transient,
            format!("{again}: PUT nar: unknown_upload")
        )
    );
    assert_eq!(
        server
            .lines()
            .iter()
            .filter(|l| l.starts_with("PUT"))
            .count(),
        2
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-024"))]
#[test]
fn permanent_refusals_reject_the_record() {
    for (method, status, code) in [
        ("POST", 400, "bad_request"),
        ("POST", 400, "verification_failed"),
        ("POST", 409, "conflict"),
        ("POST", 413, "too_large"),
        ("PUT", 400, "verification_failed"),
        ("PUT", 400, "bad_request"),
        ("PUT", 409, "conflict"),
        ("PUT", 413, "too_large"),
    ] {
        let (store, root) = one_path();
        let server = FakeServer::protocol(Arc::default(), move |req| {
            (req.method == method).then(|| error(status, code))
        });
        let queue = store.queue();
        assert_eq!(
            uploader(&store, &server).upload(&job(&queue, &root)),
            UploadOutcome::Rejected(format!("{root}: {}: {code}", req(method))),
            "{method} {code}"
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-025"))]
#[test]
fn quota_exhaustion_is_a_quota_retry() {
    for method in ["POST", "PUT"] {
        let (store, root) = one_path();
        let server = always(507, "{\"error\":\"quota_exceeded\"}", method);
        let queue = store.queue();
        assert_eq!(
            uploader(&store, &server).upload(&job(&queue, &root)),
            UploadOutcome::Retry(
                RetryKind::QuotaExceeded,
                format!("{root}: {}: quota_exceeded", req(method))
            )
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-026"))]
#[test]
fn outages_before_the_nar_is_sent_are_transient() {
    let transient = |o: UploadOutcome| matches!(o, UploadOutcome::Retry(RetryKind::Transient, _));
    let (store, root) = one_path();
    let queue = store.queue();
    let unavailable = always(503, "", "POST");
    assert!(transient(
        uploader(&store, &unavailable).upload(&job(&queue, &root))
    ));
    let off_table = always(404, "{\"error\":\"unknown_upload\"}", "POST");
    assert!(transient(
        uploader(&store, &off_table).upload(&job(&queue, &root))
    ));
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/t/1/", closed.local_addr().unwrap());
    drop(closed);
    let up = stockhouse_upload::ServerUploader::new(
        endpoint(&base, 2),
        store.nix.clone(),
        10,
        queue.clone(),
    );
    assert!(transient(up.upload(&job(&queue, &root))));
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-031"))]
#[test]
fn a_job_uploads_only_when_its_whole_closure_did() {
    let mut store = Store::new();
    let a = store.add("a", &[]);
    let b = store.add("b", &[&a]);
    let root = store.add("root", &[&b]);
    let b2 = b.clone();
    let server = FakeServer::protocol(Arc::default(), move |req| {
        (declared(req).as_deref() == Some(b2.as_str())).then(|| Action::Reply(503, String::new()))
    });
    let queue = store.queue();
    let outcome = uploader(&store, &server).upload(&job(&queue, &root));
    assert_eq!(
        outcome,
        UploadOutcome::Retry(
            RetryKind::Transient,
            format!("{b}: POST declare: unavailable")
        )
    );
    assert_eq!(posts(&server), vec![a.to_string(), b.to_string()]);
    assert!(queue.is_uploaded(&a));
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-034"))]
#[test]
fn the_credential_is_fresh_for_each_nar_upload() {
    let mut store = Store::new();
    let a = store.add("a", &[]);
    let root = store.add("root", &[&a]);
    let server = FakeServer::protocol(Arc::default(), |_| None);
    let queue = store.queue();
    let (source, reads) = counting(None, CredentialError::Missing);
    let outcome = uploader_with(&store, &server, source).upload(&job(&queue, &root));
    assert_eq!(outcome, UploadOutcome::Uploaded);
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-035"))]
#[test]
fn each_path_has_a_request_budget() {
    let (store, root) = one_path();
    let server = always(404, "{\"error\":\"unknown_upload\"}", "PUT");
    let queue = store.queue();
    let up = uploader(&store, &server).with_path_budget(2);
    assert_eq!(
        up.upload(&job(&queue, &root)),
        UploadOutcome::Retry(
            RetryKind::Transient,
            format!("{root}: upload step budget exhausted")
        )
    );
    assert_eq!(server.lines().len(), 2);
    assert_eq!(stockhouse_upload::PATH_BUDGET, 8);
}
