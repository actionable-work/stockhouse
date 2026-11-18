mod common;
use common::*;
use stockhouse_ingest::{RetryKind, UploadOutcome, Uploader};
use stockhouse_proto::Sha256;
use stockhouse_upload::{ClosureError, ServerUploader, parse_closure, upload_order};

const EMPTY_SRI: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-008"))]
#[test]
fn a_path_missing_locally_is_rejected_without_contact() {
    let store = Store::new();
    let server = FakeServer::start(|_| Some(state_open()));
    let missing = store_path("never-built");
    let queue = store.queue();
    let outcome = uploader(&store, &server).upload(&job(&queue, &missing));
    assert_eq!(
        outcome,
        UploadOutcome::Rejected("not in the local store".into())
    );
    assert!(store.args().is_empty());
    assert!(server.requests().is_empty());
}

fn state_open() -> Action {
    Action::Reply(200, "{\"state\":\"open\"}".into())
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-009"))]
#[test]
fn the_closure_query_has_fixed_arguments() {
    let mut store = Store::new();
    let root = store.add("hello", &[]);
    let server =
        FakeServer::start(|req| req.body.as_ref().map(|_| Action::Reply(503, String::new())));
    let queue = store.queue();
    uploader(&store, &server).upload(&job(&queue, &root));
    assert_eq!(
        store.args()[0],
        format!("--extra-experimental-features nix-command path-info --json --recursive {root}")
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-010"))]
#[test]
fn closures_parse_from_both_pinned_shapes() {
    let (a, b) = (store_path("a"), store_path("b"));
    let drv = store_path("b.drv");
    let plain = format!(
        "{{\"{a}\":{{\"narHash\":\"{EMPTY_SRI}\",\"narSize\":5,\"references\":[],\"deriver\":null,\"ca\":null,\"ultimate\":false}},\
\"{b}\":{{\"narHash\":\"{}\",\"narSize\":7,\"references\":[\"{a}\",\"{b}\"],\"deriver\":\"{drv}\",\"ca\":\"fixed:r:sha256:x\",\"signatures\":[]}}}}",
        Sha256::digest(b"b")
    );
    let v2 = format!(
        "{{\"info\":{{\"{}\":{{\"narHash\":\"{EMPTY_SRI}\",\"narSize\":5,\"references\":[],\"version\":2}},\
\"{}\":{{\"narHash\":\"{}\",\"narSize\":7,\"references\":[\"{}\",\"{}\"],\"deriver\":\"{}\",\"ca\":\"fixed:r:sha256:x\"}}}},\"storeDir\":\"/nix/store\",\"version\":2}}",
        a.basename(),
        b.basename(),
        Sha256::digest(b"b"),
        a.basename(),
        b.basename(),
        drv.basename()
    );
    for text in [plain, v2] {
        let closure = parse_closure(text.as_bytes()).unwrap();
        let (ea, eb) = (&closure[&a], &closure[&b]);
        assert_eq!((ea.nar_hash, ea.nar_size), (Sha256::digest(b""), 5));
        assert_eq!((ea.deriver.clone(), ea.ca.clone()), (None, None));
        assert_eq!(eb.references, vec![a.clone(), b.clone()]);
        assert_eq!(eb.deriver, Some(drv.clone()));
        assert_eq!(eb.ca.as_deref(), Some("fixed:r:sha256:x"));
        assert_eq!(eb.nar_hash, Sha256::digest(b"b"));
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-011"))]
#[test]
fn unusable_closure_output_retries() {
    let a = store_path("a");
    let entry = |extra: &str| {
        format!(
            "{{\"{a}\":{{\"narHash\":\"{EMPTY_SRI}\",\"narSize\":5,\"references\":[]{extra}}}}}"
        )
    };
    for bad in [
        "[]".to_owned(),
        "{\"/tmp/x\":{}}".to_owned(),
        entry(",\"narSize\":0").replace("\"narSize\":5,", ""),
        format!("{{\"{a}\":{{\"narHash\":\"md5-x\",\"narSize\":5,\"references\":[]}}}}"),
        format!(
            "{{\"{a}\":{{\"narHash\":\"{EMPTY_SRI}\",\"narSize\":5,\"references\":[\"{}\"]}}}}",
            store_path("gone")
        ),
        entry(",\"ca\":5"),
    ] {
        assert_eq!(
            parse_closure(bad.as_bytes()),
            Err(ClosureError::Format),
            "{bad}"
        );
    }
    let closure = parse_closure(entry("").as_bytes()).unwrap();
    assert_eq!(
        upload_order(&closure, &store_path("other")),
        Err(ClosureError::Format)
    );
    let b = store_path("b");
    let cycle = format!(
        "{{\"{a}\":{{\"narHash\":\"{EMPTY_SRI}\",\"narSize\":5,\"references\":[\"{b}\"]}},\"{b}\":{{\"narHash\":\"{EMPTY_SRI}\",\"narSize\":5,\"references\":[\"{a}\"]}}}}"
    );
    assert_eq!(
        upload_order(&parse_closure(cycle.as_bytes()).unwrap(), &a),
        Err(ClosureError::Format)
    );
    // Through the backend: a failing command, oversized and malformed output.
    let mut store = Store::new();
    let root = store.add("root", &[]);
    let server = FakeServer::start(|_| None);
    let queue = store.queue();
    let file = store
        .dir
        .join("closure")
        .join(format!("{}.json", root.hash()));
    for setup in ["code", "huge", "garbage"] {
        match setup {
            "code" => store.set("closure.code", "1"),
            "huge" => {
                let _ = std::fs::remove_file(store.dir.join("closure.code"));
                std::fs::write(&file, vec![b' '; 64 * 1024 * 1024 + 1]).unwrap();
            }
            _ => std::fs::write(&file, "garbage").unwrap(),
        }
        let outcome = uploader(&store, &server).upload(&job(&queue, &root));
        let UploadOutcome::Retry(RetryKind::Transient, message) = outcome else {
            panic!("{setup}: {outcome:?}");
        };
        assert!(message.len() <= 512, "{setup}");
    }
    assert!(server.requests().is_empty());
    // A closure query that hangs is bounded by the request timeout, then
    // killed and reaped.
    std::fs::write(&file, store.closure_json(&root)).unwrap();
    store.set("closure.sleep", "1");
    let up = ServerUploader::new(
        endpoint(&server.base(), 1),
        store.nix.clone(),
        10,
        queue.clone(),
    );
    let started = std::time::Instant::now();
    let outcome = up.upload(&job(&queue, &root));
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(
        matches!(outcome, UploadOutcome::Retry(RetryKind::Transient, _)),
        "{outcome:?}"
    );
    wait_gone(&store.dir.join("closure.pid"));
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-012"))]
#[test]
fn oversized_closures_are_rejected() {
    let mut store = Store::new();
    let a = store.add("a", &[]);
    let b = store.add("b", &[&a]);
    let root = store.add("root", &[&a, &b]);
    let server = FakeServer::start(|_| None);
    let queue = store.queue();
    let up = ServerUploader::new(
        endpoint(&server.base(), 5),
        store.nix.clone(),
        2,
        queue.clone(),
    );
    assert_eq!(
        up.upload(&job(&queue, &root)),
        UploadOutcome::Rejected("closure too large".into())
    );
    assert!(server.requests().is_empty());
}
