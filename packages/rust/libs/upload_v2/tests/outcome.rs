mod common;

use common::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use stockhouse_ingest::{RetryKind, UploadOutcome, Uploader};
use stockhouse_wire_v2::ProtocolMode;

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-024"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-026"))]
#[test]
fn a_confirmed_referrer_only_becomes_complete_after_its_references() {
    for dep_recovers in [false, true] {
        let mut store = Store::new();
        let dep = store.add("incomplete", &[]);
        let top = store.add("confirmed", &[&dep]);
        let later = store.add("independent-between-groups", &[]);
        let q = store.queue();
        let top_job = job(&q, &top);
        let dep_job = job(&q, &dep);
        let extra = job(&q, &later);
        let [verified, unavailable] = [top_job.id, dep_job.id];
        let calls = AtomicUsize::new(0);
        let queue = q.clone();
        let above = top.clone();
        let below = dep.clone();
        let server = fake(move |req, rows| {
            if req.rel() != "v2/declare" {
                return None;
            }
            let contains_dep = rows.iter().any(|row| row.id == unavailable);
            if contains_dep {
                assert!(!queue.is_uploaded(&above));
                assert!(!queue.is_uploaded(&below));
            }
            let call = calls.fetch_add(1, Ordering::SeqCst);
            Some(reply(
                rows.iter()
                    .map(|row| {
                        if row.id == verified || row.id != unavailable || dep_recovers && call > 0 {
                            row.state("present")
                        } else {
                            row.error("conflict")
                        }
                    })
                    .collect(),
            ))
        });
        let uploader = backend(&store, &server, ProtocolMode::V2, 2, 1);
        let outcomes = uploader.upload_batch(&[top_job.clone(), extra, top_job]);
        let conflict = UploadOutcome::Rejected(format!("{dep}: POST v2 declare: conflict"));
        assert_eq!(outcomes[0], conflict);
        assert_eq!(outcomes[1], UploadOutcome::Uploaded);
        if dep_recovers {
            assert_eq!(outcomes[2], UploadOutcome::Uploaded);
            assert!(q.is_uploaded(&dep) && q.is_uploaded(&top));
        } else {
            assert_eq!(outcomes[2], conflict);
            assert!(!q.is_uploaded(&dep) && !q.is_uploaded(&top));
            assert!(entry_file(&q, &top).is_file());
        }
        assert_eq!(nar_count(&server), 0);
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-024"))]
#[test]
fn outcomes_prioritize_rejection_then_quota_then_uncertain_then_transient() {
    let mut store = Store::new();
    let names = ["rejected", "quota", "uncertain", "transient"];
    let paths: Vec<_> = names.iter().map(|name| store.add(name, &[])).collect();
    let roots: Vec<_> = (0..paths.len())
        .map(|i| store.add(&format!("root-{i}"), &paths[i..].iter().collect::<Vec<_>>()))
        .collect();
    let q = store.queue();
    let jobs: Vec<_> = roots.iter().map(|p| job(&q, p)).collect();
    let [rejected, quota, uncertain, transient]: [_; 4] = paths.try_into().unwrap();
    let server_names = [
        rejected.clone(),
        quota.clone(),
        uncertain.clone(),
        transient.clone(),
    ];
    let server = fake(move |req, rows| match req.rel() {
        "v2/declare" => Some(reply(
            rows.iter()
                .map(|row| {
                    if row.path() == &server_names[0] {
                        row.error("too_large")
                    } else if row.path() == &server_names[1] {
                        row.error("quota_exceeded")
                    } else {
                        row.state("open")
                    }
                })
                .collect(),
        )),
        rel if rel.starts_with("v2/nar/") && rows[0].path() == &server_names[2] => {
            Some(Action::Close)
        }
        rel if rel.starts_with("v2/nar/") && rows[0].path() == &server_names[3] => {
            Some(Action::Truncated(429, 0))
        }
        _ => None,
    });
    assert_eq!(
        backend(&store, &server, ProtocolMode::V2, 12, 3).upload_batch(&jobs),
        [
            UploadOutcome::Rejected(format!("{rejected}: POST v2 declare: too_large")),
            UploadOutcome::Retry(
                RetryKind::QuotaExceeded,
                format!("{quota}: POST v2 declare: quota_exceeded")
            ),
            UploadOutcome::Retry(
                RetryKind::Uncertain,
                format!("{uncertain}: PUT v2 nar: request failed")
            ),
            UploadOutcome::Retry(
                RetryKind::Transient,
                format!("{transient}: PUT v2 nar: busy")
            ),
        ]
    );
}
