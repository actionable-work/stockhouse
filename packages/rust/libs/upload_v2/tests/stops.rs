mod common;

use common::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::time::Duration;
use stockhouse_credentials::{CredentialError, CredentialSource, Secret};
use stockhouse_ingest::{RetryKind, UploadOutcome, Uploader};
use stockhouse_test_support::counting_source;
use stockhouse_wire_v2::ProtocolMode;

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-021"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-031"))]
#[test]
fn quota_at_each_phase_is_local_and_independent_rework_can_progress() {
    for (phase, label) in [
        ("v2/declare", "POST v2 declare"),
        ("v2/nar", "PUT v2 nar"),
        ("v2/publish", "POST v2 publish"),
    ] {
        let mut store = Store::new();
        let dep = store.add_sized("large-dep", &[], 128 * 1024);
        let root = store.add("small-with-dep", &[&dep]);
        let independent = store.add("independent", &[]);
        let q = store.queue();
        let jobs = [job(&q, &dep), job(&q, &root), job(&q, &independent)];
        let [dep_id, root_id, free_id] = jobs.each_ref().map(|job| job.id);
        let first = AtomicUsize::new(0);
        let missing = dep.clone();
        let server = fake(move |req, rows| match req.rel() {
            "v2/declare" if phase == "v2/declare" => Some(reply(
                rows.iter()
                    .map(|row| {
                        if row.id == dep_id {
                            row.error("quota_exceeded")
                        } else {
                            row.state("open")
                        }
                    })
                    .collect(),
            )),
            rel if rel.starts_with("v2/nar/") => {
                if rows[0].id == dep_id && phase == "v2/nar" {
                    Some(server::error(507, "quota_exceeded"))
                } else if rows[0].id == free_id && first.fetch_add(1, Ordering::SeqCst) == 0 {
                    Some(server::error(404, "unknown_upload"))
                } else {
                    None
                }
            }
            "v2/publish" if phase == "v2/publish" => Some(reply(
                rows.iter()
                    .map(|row| {
                        if row.id == dep_id {
                            row.error("quota_exceeded")
                        } else if row.id == root_id {
                            row.missing(&[&missing])
                        } else {
                            row.state("published")
                        }
                    })
                    .collect(),
            )),
            "v2/publish" => {
                assert!(rows.iter().all(|row| row.id != root_id && row.id != dep_id));
                None
            }
            _ => None,
        });
        let uploader = backend(&store, &server, ProtocolMode::V2, 8, 2);
        let quota = UploadOutcome::Retry(
            RetryKind::QuotaExceeded,
            format!("{dep}: {label}: quota_exceeded"),
        );
        assert_eq!(
            uploader.upload_batch(&jobs),
            [quota.clone(), quota, UploadOutcome::Uploaded]
        );
        assert!(q.upload_state(&root).unwrap().nar_sent);
        assert!(!q.is_uploaded(&root) && !q.is_uploaded(&dep) && q.is_uploaded(&independent));
        let lines = server.lines();
        assert_eq!(
            lines
                .iter()
                .filter(|line| **line == format!("PUT v2/nar/{free_id}"))
                .count(),
            2
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| **line == format!("PUT v2/nar/{dep_id}"))
                .count(),
            usize::from(phase != "v2/declare")
        );
        assert_eq!(route_count(&server, "v2/declare"), 2);
        assert_eq!(route_count(&server, "v2/status"), 0);
    }
}

struct FailsDuringTransfer {
    reads: AtomicUsize,
    started: Arc<(Mutex<bool>, Condvar)>,
}

impl CredentialSource for FailsDuringTransfer {
    fn credential(&self) -> Result<Secret, CredentialError> {
        if self.reads.fetch_add(1, Ordering::SeqCst) < 3 {
            return Ok(Secret::new(TOKEN.as_bytes().to_vec()));
        }
        let (lock, condvar) = &*self.started;
        let (_guard, wait) = condvar
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(4), |started| {
                !*started
            })
            .unwrap();
        assert!(
            !wait.timed_out(),
            "the other transfer never reached the server"
        );
        Err(CredentialError::Missing)
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-022"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-024"))]
#[test]
fn a_credential_stop_keeps_rejections_completions_and_incomplete_marks() {
    for withdrawn in [false, true] {
        let mut store = Store::new();
        let conflict = store.add("conflict", &[]);
        let present = store.add("present", &[]);
        let opens = [
            store.add("open-a", &[]),
            store.add("open-b", &[]),
            store.add("open-c", &[]),
        ];
        let q = store.queue();
        let jobs: Vec<_> = [&present, &conflict]
            .into_iter()
            .chain(opens.iter())
            .map(|path| job(&q, path))
            .collect();
        let [kept, rejected] = [jobs[0].id, jobs[1].id];
        let ready = Arc::new((Mutex::new(false), Condvar::new()));
        let started = ready.clone();
        let server = fake(move |req, rows| match req.rel() {
            "v2/declare" => Some(reply(
                rows.iter()
                    .map(|r| {
                        if r.id == kept {
                            r.state("present")
                        } else if r.id == rejected {
                            r.error("conflict")
                        } else {
                            r.state("open")
                        }
                    })
                    .collect(),
            )),
            rel if rel.starts_with("v2/nar/") => {
                let (lock, signal) = &*started;
                *lock.lock().unwrap() = true;
                signal.notify_all();
                std::thread::sleep(Duration::from_millis(50));
                Some(if withdrawn {
                    Action::Reply(404, String::new())
                } else {
                    server::error(404, "unknown_upload")
                })
            }
            _ => None,
        });
        let credential = Arc::new(FailsDuringTransfer {
            reads: AtomicUsize::new(0),
            started: ready,
        });
        let uploader = setup(&store, &server, ProtocolMode::V2, 8, 2, credential)
            .with_time(Arc::new(Tick::default()), Arc::new(no_wait));
        let outcomes = uploader.upload_batch(&jobs);
        assert_eq!(outcomes[0], UploadOutcome::Uploaded);
        assert_eq!(
            outcomes[1],
            UploadOutcome::Rejected(format!("{conflict}: POST v2 declare: conflict"))
        );
        let deferred =
            UploadOutcome::Deferred("credential unavailable: credential file: missing".into());
        assert_eq!(
            outcomes[2..],
            [deferred.clone(), deferred.clone(), deferred.clone()]
        );
        assert!(
            opens
                .iter()
                .all(|path| q.upload_state(path).unwrap().nar_sent)
        );
        assert_eq!(nar_count(&server), 1);
        assert_eq!(route_count(&server, "v2/declare"), 1);
        assert_eq!(route_count(&server, "v2/publish"), 0);
        let previous = store.args().len();
        assert_eq!(uploader.upload(&jobs[2]), deferred);
        assert_eq!(
            store.args().len(),
            previous + usize::from(!withdrawn),
            "a withdrawn selection must probe before loading the next closure",
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-023"))]
#[test]
fn concurrent_unauthorized_transfers_share_one_invalidation_and_each_retry_once() {
    use std::collections::HashMap;
    let mut store = Store::new();
    let paths = [store.add("token-a", &[]), store.add("token-b", &[])];
    let q = store.queue();
    let jobs = paths.iter().map(|path| job(&q, path)).collect::<Vec<_>>();
    let both = Barrier::new(2);
    let counts = Mutex::new(HashMap::new());
    let declare = AtomicBool::new(true);
    let server = fake(move |req, rows| {
        if req.rel() == "v2/declare" && declare.swap(false, Ordering::SeqCst) {
            return Some(Action::Truncated(401, 0));
        }
        if !req.rel().starts_with("v2/nar/") {
            return None;
        }
        let first = {
            let mut calls = counts.lock().unwrap();
            let n = calls.entry(rows[0].id).or_insert(0);
            *n += 1;
            *n == 1
        };
        if first {
            both.wait();
            Some(Action::Truncated(401, 0))
        } else {
            None
        }
    });
    let credentials = counting_source(None, CredentialError::Missing, true);
    let uploader = setup(&store, &server, ProtocolMode::V2, 5, 2, credentials.clone());
    assert_eq!(
        uploader.upload_batch(&jobs),
        [UploadOutcome::Uploaded, UploadOutcome::Uploaded]
    );
    assert_eq!(credentials.invalidations.load(Ordering::SeqCst), 1);
    assert_eq!(route_count(&server, "v2/declare"), 2);
    for job in jobs {
        assert_eq!(route_count(&server, &format!("v2/nar/{}", job.id)), 2);
    }
}
