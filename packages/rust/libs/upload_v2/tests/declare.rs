mod common;

use common::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use stockhouse_ingest::{RetryKind, UploadOutcome, Uploader};
use stockhouse_upload::parse_closure;
use stockhouse_wire_v2::{DeclareBatch, ProtocolMode};

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-010"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-011"))]
#[test]
fn declaration_chunks_keep_v1_facts_and_mark_open_and_verified_before_use() {
    let mut store = Store::new();
    let paths: Vec<_> = (0..1000)
        .map(|i| store.add(&format!("leaf-{i}"), &[]))
        .collect();
    let refs: Vec<_> = paths.iter().collect();
    let root = store.add("batch-root", &refs);
    let q = store.queue();
    let facts = parse_closure(store.closure_json(&root).as_bytes()).unwrap();
    let held = paths[0].clone();
    let open = paths[1].clone();
    let initially_verified = paths[2].clone();
    let verified_file = entry_file(&q, &initially_verified);
    let open_file = entry_file(&q, &open);
    let open_in_fixture = open.clone();
    let count = AtomicUsize::new(0);
    let server = fake(move |req, rows| match req.rel() {
        "v2/declare" => {
            let batch = DeclareBatch::parse(req.body.as_ref().unwrap()).unwrap();
            let call = count.fetch_add(1, Ordering::SeqCst);
            assert_eq!(batch.entries().len(), if call == 0 { 1000 } else { 1 });
            for entry in batch.entries() {
                let fact = &facts[entry.declaration.store_path()];
                assert_eq!(entry.declaration, fact.declaration().unwrap());
            }
            Some(reply(
                rows.iter()
                    .map(|row| {
                        let state = if row.path() == &held {
                            "present"
                        } else if row.path() == &open_in_fixture {
                            "open"
                        } else if row.path() == &initially_verified {
                            "verified"
                        } else {
                            "published"
                        };
                        row.state(state)
                    })
                    .collect(),
            ))
        }
        "v2/publish" => {
            assert!(
                std::fs::read_to_string(&verified_file)
                    .unwrap()
                    .contains("\"nar_sent\":true")
            );
            None
        }
        rel if rel.starts_with("v2/nar/") => {
            assert_eq!(rows[0].path(), &open_in_fixture);
            assert!(
                std::fs::read_to_string(&open_file)
                    .unwrap()
                    .contains("\"nar_sent\":true")
            );
            None
        }
        _ => None,
    });
    assert_eq!(
        backend(&store, &server, ProtocolMode::V2, 1001, 2).upload(&job(&q, &root)),
        UploadOutcome::Uploaded
    );
    assert_eq!(route_count(&server, "v2/declare"), 2);
    assert_eq!(nar_count(&server), 1);
    assert_eq!(route_count(&server, "v2/publish"), 1);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-035"))]
#[test]
fn forgotten_declaration_renews_once_and_transfers_only_the_new_id() {
    for always_forgotten in [false, true] {
        let mut store = Store::new();
        let root = store.add("fading-id", &[]);
        let q = store.queue();
        let job = job(&q, &root);
        let old_id = job.id;
        let server = fake(move |req, rows| {
            (req.rel() == "v2/declare" && (always_forgotten || rows[0].id == old_id))
                .then(|| reply(vec![rows[0].error("unknown_upload")]))
        });
        let result = backend(&store, &server, ProtocolMode::V2, 2, 1).upload(&job);
        let declared: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|req| req.rel() == "v2/declare")
            .map(|req| {
                DeclareBatch::parse(req.body.as_ref().unwrap())
                    .unwrap()
                    .entries()[0]
                    .id
            })
            .collect();
        assert_eq!(declared.len(), 2);
        assert_eq!(declared[0], old_id);
        assert_ne!(declared[1], old_id);
        if always_forgotten {
            assert_eq!(
                result,
                UploadOutcome::Retry(RetryKind::Transient, format!("{root}: upload round limit"))
            );
            assert_eq!(nar_count(&server), 0);
            assert_eq!(q.upload_state(&root).unwrap().id, declared[1]);
        } else {
            assert_eq!(result, UploadOutcome::Uploaded);
            assert_eq!(q.done_record(&root).unwrap().id, declared[1].to_string());
            assert_eq!(nar_count(&server), 1);
            assert!(
                server
                    .lines()
                    .contains(&format!("PUT v2/nar/{}", declared[1]))
            );
        }
    }
}
