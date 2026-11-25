mod common;
use common::*;
use stockhouse_ingest::{FORMAT, NoScanner, Queue, Record, UploadEntry, UploadOutcome, Worker};
use stockhouse_proto::UploadId;

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-017"))]
#[test]
fn uploads_directory_is_additive_to_format_v1() {
    let dir = temp_dir().join("queue");
    for sub in ["pending", "failed", "done", "tmp"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    std::fs::write(dir.join("FORMAT"), FORMAT).unwrap();
    std::fs::write(dir.join("LIMITS"), "max_pending=4\n").unwrap();
    Queue::open(&dir).unwrap();
    Queue::create(&dir, 4).unwrap();
    assert!(dir.join("uploads").is_dir());
    assert_eq!(std::fs::read_to_string(dir.join("FORMAT")).unwrap(), FORMAT);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-018"))]
#[test]
fn dependency_identity_is_created_once() {
    let q = queue(4);
    let dep = path("dep");
    let first = q.upload_state(&dep).unwrap();
    assert!(!first.nar_sent);
    assert_eq!(q.upload_state(&dep).unwrap(), first);
    let reopened = Queue::open(q.root()).unwrap();
    assert_eq!(reopened.upload_state(&dep).unwrap(), first);
    let queued = path("queued");
    let stockhouse_ingest::EnqueueOutcome::Queued(id) =
        q.producer().unwrap().enqueue(&queued, 0, false).unwrap()
    else {
        panic!("not queued");
    };
    assert_eq!(q.upload_state(&queued).unwrap().id, id);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-019"))]
#[test]
fn queued_path_identity_is_the_record_identity() {
    let q = queue(4);
    let p = path("root");
    let dep = q.mark_nar_sent(&p, q.upload_state(&p).unwrap()).unwrap();
    let stockhouse_ingest::EnqueueOutcome::Queued(root) =
        q.producer().unwrap().enqueue(&p, 0, false).unwrap()
    else {
        panic!("not queued");
    };
    assert_ne!(dep.id, root);
    let entry = q.upload_state(&p).unwrap();
    assert_eq!(
        entry,
        UploadEntry {
            id: root,
            nar_sent: false
        }
    );
    let marked = q.mark_nar_sent(&p, entry).unwrap();
    assert_eq!(q.upload_state(&p).unwrap(), marked);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-020"))]
#[test]
fn corrupt_upload_entries_are_quarantined() {
    let q = queue(4);
    let p = path("corrupt");
    let file = q.root().join(format!("uploads/{}.json", p.hash()));
    let other = path("other");
    let id = UploadId::generate();
    for bytes in [
        b"garbage".to_vec(),
        format!("{{\"version\":2,\"id\":\"{id}\",\"store_path\":\"{p}\",\"nar_sent\":false}}")
            .into_bytes(),
        format!("{{\"version\":1,\"id\":\"{id}\",\"store_path\":\"{other}\",\"nar_sent\":false}}")
            .into_bytes(),
    ] {
        std::fs::write(&file, &bytes).unwrap();
        let entry = q.upload_state(&p).unwrap();
        assert!(!entry.nar_sent);
        let quarantined = q.root().join(format!("failed/{}.upload.corrupt", p.hash()));
        assert_eq!(std::fs::read(&quarantined).unwrap(), bytes);
        assert_eq!(q.status().unwrap().failed, 1);
        std::fs::remove_file(&quarantined).unwrap();
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-021"))]
#[test]
fn nar_sent_mark_is_durable() {
    let q = queue(4);
    let p = path("mark");
    let entry = q.mark_nar_sent(&p, q.upload_state(&p).unwrap()).unwrap();
    let reopened = Queue::open(q.root()).unwrap();
    assert!(reopened.upload_state(&p).unwrap().nar_sent);
    reopened.clear_nar_sent(&p, entry).unwrap();
    assert!(!q.upload_state(&p).unwrap().nar_sent);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-022"))]
#[test]
fn completing_a_dependency_writes_the_ledger_first() {
    let q = queue(4);
    let p = path("complete");
    let entry = q.upload_state(&p).unwrap();
    q.complete_path(&p, entry.id, 9).unwrap();
    let done = q.done_record(&p).unwrap();
    assert_eq!((done.id, done.uploaded_at), (entry.id.to_string(), 9));
    assert!(!q.root().join(format!("uploads/{}.json", p.hash())).exists());
    // A crash after the ledger write leaves an entry that is then completed again.
    let again = q.upload_state(&p).unwrap();
    q.complete_path(&p, again.id, 10).unwrap();
    assert_eq!(q.done_record(&p).unwrap().uploaded_at, 9);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-023"))]
#[test]
fn stale_upload_entries_are_reconciled() {
    let store = temp_dir();
    let q = queue(4).with_store_root(&store);
    let (done, gone, live) = (path("done"), path("gone"), path("live"));
    std::fs::create_dir_all(store.join(live.as_str().trim_start_matches('/'))).unwrap();
    std::fs::create_dir_all(store.join(done.as_str().trim_start_matches('/'))).unwrap();
    let done_entry = q.upload_state(&done).unwrap();
    q.upload_state(&gone).unwrap();
    let live_entry = q.upload_state(&live).unwrap();
    std::fs::write(
        q.root().join(format!("done/{}.json", done.hash())),
        format!(
            "{{\"version\":1,\"id\":\"{}\",\"store_path\":\"{done}\",\"uploaded_at\":1}}",
            done_entry.id
        ),
    )
    .unwrap();
    let uploader = FakeUploader::answering(vec![UploadOutcome::Uploaded]);
    Worker::new(q.clone(), &uploader, &NoScanner, limits())
        .pass(0, &mut Vec::new())
        .unwrap();
    let exists = |p: &stockhouse_proto::StorePath| {
        q.root().join(format!("uploads/{}.json", p.hash())).exists()
    };
    assert!(!exists(&done) && !exists(&gone) && exists(&live));
    assert_eq!(q.upload_state(&live).unwrap(), live_entry);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-024"))]
#[test]
fn a_ledger_entry_can_be_forgotten() {
    let q = queue(4);
    let p = path("forget");
    q.complete_path(&p, UploadId::generate(), 1).unwrap();
    assert!(q.is_uploaded(&p));
    q.forget_done(&p).unwrap();
    assert!(!q.is_uploaded(&p));
    q.forget_done(&p).unwrap();
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-025"))]
#[test]
fn renewals_replace_the_identity_and_pending_recovers_a_partial_write() {
    let q = queue(4);
    let dep = path("dependency");
    let queued = path("queued");
    q.producer().unwrap().enqueue(&queued, 0, false).unwrap();
    let pending = q.root().join(format!("pending/{}.json", queued.hash()));
    let mut before = Record::from_json(&std::fs::read(&pending).unwrap()).unwrap();
    before.attempts = 2;
    before.next_attempt_at = 9;
    before.last_error = Some("retrying".into());
    std::fs::write(&pending, before.to_json()).unwrap();
    for p in [&dep, &queued] {
        let old = q.mark_nar_sent(p, q.upload_state(p).unwrap()).unwrap();
        let fresh = q.renew_identity(p).unwrap();
        assert_ne!(fresh.id, old.id);
        assert!(!fresh.nar_sent);
        let upload = q.root().join(format!("uploads/{}.json", p.hash()));
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(upload).unwrap()).unwrap();
        assert_eq!(json["id"], fresh.id.to_string());
        assert_eq!(json["nar_sent"], false);
        if p == &queued {
            let persisted = Record::from_json(&std::fs::read(&pending).unwrap()).unwrap();
            assert_eq!(
                persisted,
                Record {
                    id: fresh.id,
                    ..before.clone()
                }
            );
        }
        assert_eq!(
            Queue::open(q.root()).unwrap().upload_state(p).unwrap(),
            fresh
        );
    }

    let old = q
        .mark_nar_sent(&queued, q.upload_state(&queued).unwrap())
        .unwrap();
    let mut record = Record::from_json(&std::fs::read(&pending).unwrap()).unwrap();
    assert_eq!(record.id, old.id);
    record.id = UploadId::generate();
    std::fs::write(pending, record.to_json()).unwrap();
    let reopened = Queue::open(q.root()).unwrap();
    assert_eq!(
        reopened.upload_state(&queued).unwrap(),
        UploadEntry {
            id: record.id,
            nar_sent: false
        }
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-026"))]
#[test]
fn a_stale_job_snapshot_applies_the_outcome_to_the_current_identity() {
    use stockhouse_ingest::RetryKind;

    for (outcome, target) in [
        (
            UploadOutcome::Retry(RetryKind::Transient, "later".into()),
            "pending",
        ),
        (UploadOutcome::Rejected("conflict".into()), "failed"),
        (UploadOutcome::Uploaded, "done"),
    ] {
        let q = queue(4);
        let p = path("renewed");
        q.producer().unwrap().enqueue(&p, 0, false).unwrap();
        let upload = FakeUploader::default();
        let worker = Worker::new(q.clone(), &upload, &NoScanner, limits());
        let stale = worker.due(0).unwrap().remove(0);
        let fresh = q.renew_identity(&p).unwrap();
        assert_ne!(stale.id, fresh.id);
        assert_eq!(worker.apply(stale, outcome, 9).unwrap(), target == "done");
        if target == "done" {
            assert_eq!(q.done_record(&p).unwrap().id, fresh.id.to_string());
        } else {
            let file = q.root().join(format!("{target}/{}.json", p.hash()));
            let record = Record::from_json(&std::fs::read(file).unwrap()).unwrap();
            assert_eq!(record.id, fresh.id);
        }
    }
}
