mod common;
use common::*;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;
use stockhouse_ingest::{RetryKind, UploadOutcome, Uploader};
use stockhouse_proto::{ServedIntegrity, Sha256};
use stockhouse_upload::{CHUNK, Dump, DumpFailure, ServerUploader};

fn put(server: &FakeServer) -> Request {
    server
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap()
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-018"))]
#[test]
fn nars_stream_with_the_declared_length() {
    let mut store = Store::new();
    let root = store.add_sized("root", &[], 300_000);
    let server = Protocol::start(Arc::default(), |_| None);
    let queue = store.queue();
    assert_eq!(
        uploader(&store, &server).upload(&job(&queue, &root)),
        UploadOutcome::Uploaded
    );
    let req = put(&server);
    assert_eq!(req.header("content-type"), Some("application/x-nix-nar"));
    assert_eq!(req.header("content-length"), Some("300000"));
    assert_eq!(req.body.unwrap(), store.nar(&root));
    assert!(store.args().contains(&format!(
        "--extra-experimental-features nix-command store dump-path {root}"
    )));
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-019"))]
#[test]
fn nar_streaming_uses_bounded_memory() {
    let mut store = Store::new();
    let root = store.add_sized("big", &[], 16 * 1024 * 1024);
    let expected = ServedIntegrity {
        size: 16 * 1024 * 1024,
        hash: Sha256::digest(store.nar(&root)),
    };
    let (dump, mut reader) =
        Dump::start(&store.nix, &root, expected, Duration::from_secs(10)).unwrap();
    let mut buf = vec![0u8; 8192];
    let mut total = 0;
    loop {
        let n = reader.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        total += n;
    }
    assert_eq!(total, 16 * 1024 * 1024);
    assert!(dump.fully_sent());
    assert!(dump.high_water() <= 2 * CHUNK, "{}", dump.high_water());
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-020"))]
#[test]
fn a_nar_that_does_not_match_is_never_sent_complete() {
    for change in ["altered", "short", "long"] {
        let mut store = Store::new();
        let root = store.add_sized("root", &[], 200_000);
        let mut nar = store.nar(&root).to_vec();
        match change {
            "altered" => nar[100] ^= 1,
            "short" => nar.truncate(150_000),
            _ => nar.extend_from_slice(&[0u8; 10]),
        }
        std::fs::write(store.dir.join("nar").join(root.hash().as_str()), nar).unwrap();
        let server = Protocol::start(Arc::default(), |_| None);
        let queue = store.queue();
        let outcome = uploader(&store, &server).upload(&job(&queue, &root));
        assert_eq!(
            outcome,
            UploadOutcome::Rejected(format!("{root}: local NAR does not match its NarHash")),
            "{change}"
        );
        assert!(
            server
                .requests()
                .iter()
                .filter(|r| r.method == "PUT")
                .all(|r| !r.complete)
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-021"))]
#[test]
fn a_failing_dump_retries_and_its_child_is_reaped() {
    let mut store = Store::new();
    let root = store.add("root", &[]);
    store.set("dump.code", "1");
    let server = Protocol::start(Arc::default(), |_| None);
    let queue = store.queue();
    assert_eq!(
        uploader(&store, &server).upload(&job(&queue, &root)),
        UploadOutcome::Retry(RetryKind::Transient, format!("{root}: NAR dump failed"))
    );
    assert!(
        server
            .requests()
            .iter()
            .filter(|r| r.method == "PUT")
            .all(|r| !r.complete)
    );
    let expected = ServedIntegrity {
        size: 1,
        hash: Sha256::digest(b""),
    };
    let missing = store.dir.join("no-such-nix");
    assert_eq!(
        Dump::start(&missing, &root, expected, Duration::from_secs(1)).err(),
        Some(DumpFailure::Failed)
    );
    // A dump that never finishes is killed and reaped when the idle bound passes.
    let _ = std::fs::remove_file(store.dir.join("dump.code"));
    store.set("dump.sleep", "1");
    let hung = store.add("hung", &[]);
    let up = ServerUploader::new(
        endpoint(&server.base(), 1),
        store.nix.clone(),
        10,
        queue.clone(),
    );
    let outcome = up.upload(&job(&queue, &hung));
    assert_eq!(
        outcome,
        UploadOutcome::Retry(RetryKind::Transient, format!("{hung}: NAR dump failed"))
    );
    wait_gone(&store.dir.join("dump.pid"));
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-027"))]
#[test]
fn a_lost_reply_after_the_nar_is_uncertain() {
    for after in ["close", "off-table"] {
        let mut store = Store::new();
        let root = store.add("root", &[]);
        let proto = Arc::new(Protocol::default());
        let p3 = proto.clone();
        let server = Protocol::start(proto, move |req| {
            if req.method != "PUT" {
                return None;
            }
            let _ = p3.handle(req);
            Some(match after {
                "close" => Action::Close,
                _ => Action::Reply(200, "{\"state\":\"open\"}".into()),
            })
        });
        let queue = store.queue();
        let outcome = uploader(&store, &server).upload(&job(&queue, &root));
        assert!(
            matches!(outcome, UploadOutcome::Retry(RetryKind::Uncertain, _)),
            "{after}: {outcome:?}"
        );
        let entry = std::fs::read_to_string(entry_file(&queue, &root)).unwrap();
        assert!(entry.contains("\"nar_sent\":true"), "{entry}");
    }
}
