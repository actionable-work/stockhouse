mod common;
use common::server::{error, state};
use common::*;
use std::sync::{Arc, Mutex};
use stockhouse_ingest::{Queue, RetryKind, UploadOutcome, Uploader};
use stockhouse_proto::StorePath;
use stockhouse_upload::ServerUploader;

fn marked(store: &Store, root: &StorePath) -> (Queue, stockhouse_ingest::UploadJob) {
    let queue = store.queue();
    let job = job(&queue, root);
    let entry = queue.upload_state(root, Some(job.id)).unwrap();
    queue.mark_nar_sent(root, entry).unwrap();
    (queue, job)
}

fn methods(server: &FakeServer) -> Vec<String> {
    server.requests().iter().map(|r| r.method.clone()).collect()
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-028"))]
#[test]
fn uncertain_uploads_are_reconciled_before_resending() {
    let path = store_path("root");
    let cases: [(&str, Option<Action>, UploadOutcome, &[&str]); 5] = [
        (
            "published",
            Some(state("published")),
            UploadOutcome::Uploaded,
            &["GET"],
        ),
        (
            "present",
            Some(state("present")),
            UploadOutcome::Uploaded,
            &["GET"],
        ),
        (
            "unknown",
            None,
            UploadOutcome::Uploaded,
            &["GET", "POST", "PUT"],
        ),
        (
            "verification",
            Some(error(400, "verification_failed")),
            UploadOutcome::Rejected(format!("{path}: GET status: verification_failed")),
            &["GET"],
        ),
        (
            "unavailable",
            Some(Action::Reply(503, String::new())),
            UploadOutcome::Retry(
                RetryKind::Uncertain,
                format!("{path}: GET status: unavailable"),
            ),
            &["GET"],
        ),
    ];
    for (name, get, expected, sequence) in cases {
        let mut store = Store::new();
        let root = store.add("root", &[]);
        let (_queue, job) = marked(&store, &root);
        let server = FakeServer::protocol(Arc::default(), move |req| {
            if req.method == "GET" {
                get.clone()
            } else {
                None
            }
        });
        assert_eq!(root, path);
        let outcome = uploader(&store, &server).upload(&job);
        assert_eq!(outcome, expected, "{name}");
        assert_eq!(methods(&server), sequence, "{name}");
    }
    // `open` sends the NAR without declaring again.
    let mut store = Store::new();
    let root = store.add("root", &[]);
    let proto = Arc::new(Protocol::default());
    let (_queue, job) = marked(&store, &root);
    let seeded = Arc::new(Mutex::new(false));
    let s2 = seeded.clone();
    let server = FakeServer::protocol(proto, move |req| {
        let first_get = req.method == "GET" && !std::mem::replace(&mut *s2.lock().unwrap(), true);
        first_get.then(|| state("open"))
    });
    let outcome = uploader(&store, &server).upload(&job);
    // The fake never saw the declaration, so it reports the PUT unknown and
    // the backend declares again: GET, PUT, POST, PUT.
    assert_eq!(outcome, UploadOutcome::Uploaded);
    assert_eq!(methods(&server), ["GET", "PUT", "POST", "PUT"]);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-029"))]
#[test]
fn an_unknown_upload_is_declared_again() {
    let mut store = Store::new();
    let root = store.add("root", &[]);
    let queue = store.queue();
    let first = Arc::new(Mutex::new(true));
    let f2 = first.clone();
    let server = FakeServer::protocol(Arc::default(), move |req| {
        if req.method == "PUT" && std::mem::replace(&mut *f2.lock().unwrap(), false) {
            return Some(error(404, "unknown_upload"));
        }
        None
    });
    assert_eq!(
        uploader(&store, &server).upload(&job(&queue, &root)),
        UploadOutcome::Uploaded
    );
    assert_eq!(methods(&server), ["POST", "PUT", "POST", "PUT"]);
    let other = store.add("other", &[]);
    let (_q, job) = marked(&store, &other);
    let server = FakeServer::protocol(Arc::default(), |_| None);
    assert_eq!(
        uploader(&store, &server).upload(&job),
        UploadOutcome::Uploaded
    );
    assert_eq!(methods(&server), ["GET", "POST", "PUT"]);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-030"))]
#[test]
fn upload_identities_are_stable() {
    let mut store = Store::new();
    let a = store.add("a", &[]);
    let root = store.add("root", &[&a]);
    let queue = store.queue();
    let job = job(&queue, &root);
    let proto = Arc::new(Protocol::default());
    let fail_put = Arc::new(Mutex::new(true));
    let (f2, r2) = (fail_put.clone(), root.clone());
    let server = FakeServer::protocol(proto, move |req| {
        (req.method == "POST"
            && declared(req).as_deref() == Some(r2.as_str())
            && *f2.lock().unwrap())
        .then(|| Action::Reply(503, String::new()))
    });
    let first = uploader(&store, &server).upload(&job);
    assert!(
        matches!(first, UploadOutcome::Retry(RetryKind::Transient, _)),
        "{first:?}"
    );
    *fail_put.lock().unwrap() = false;
    let reopened = Queue::open(queue.root())
        .unwrap()
        .with_store_root(&store.root);
    let up = ServerUploader::new(endpoint(&server.base(), 5), store.nix.clone(), 10, reopened);
    assert_eq!(up.upload(&job), UploadOutcome::Uploaded);
    let posts: Vec<(String, String)> = server
        .requests()
        .iter()
        .filter(|r| r.method == "POST")
        .map(|r| (declared(r).unwrap(), r.rel().to_owned()))
        .collect();
    let root_ids: Vec<&String> = posts
        .iter()
        .filter(|(p, _)| p == root.as_str())
        .map(|(_, u)| u)
        .collect();
    assert_eq!(root_ids.len(), 2);
    assert!(
        root_ids
            .iter()
            .all(|u| **u == format!("uploads/{}", job.id))
    );
    assert_eq!(posts.iter().filter(|(p, _)| p == a.as_str()).count(), 1);
    // A dependency's identity survives a restart before it completes.
    let b = store.add("b", &[]);
    let top = store.add("top", &[&b]);
    let top_job = stockhouse_ingest::UploadJob {
        id: stockhouse_proto::UploadId::generate(),
        store_path: top.clone(),
        attempts: 0,
    };
    let down =
        FakeServer::start(|req| req.body.as_ref().map(|_| Action::Reply(503, String::new())));
    uploader(&store, &down).upload(&top_job);
    uploader(&store, &down).upload(&top_job);
    let ids: Vec<String> = down.requests().iter().map(|r| r.rel().to_owned()).collect();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], ids[1]);
}
