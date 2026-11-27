mod common;

use common::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use stockhouse_ingest::{RetryKind, UploadOutcome, Uploader};
use stockhouse_proto::UploadId;
use stockhouse_wire_v2::ProtocolMode;

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-032"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-033"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-020"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-025"))]
#[test]
fn unknown_upload_and_open_rework_at_most_once_per_path() {
    for (failure, repeats) in [
        ("nar", false),
        ("publish", false),
        ("open", false),
        ("nar", true),
        ("open", true),
    ] {
        let mut store = Store::new();
        let path = store.add("bounded", &[]);
        let q = store.queue();
        let job = job(&q, &path);
        let bad = AtomicUsize::new(0);
        let node = path.clone();
        let queue = q.clone();
        let attempts = AtomicUsize::new(0);
        let server = fake(move |req, rows| {
            if req.rel() == "v2/declare" {
                let second = attempts.fetch_add(1, Ordering::SeqCst) == 1;
                if second {
                    assert!(!queue.upload_state(&node).unwrap().nar_sent);
                    // The identity's staged bytes were lost on the server.
                    return Some(reply(vec![rows[0].state("open")]));
                }
            }
            if req.rel().starts_with("v2/nar/") {
                assert!(queue.upload_state(&node).unwrap().nar_sent);
                if failure == "nar" && (bad.fetch_add(1, Ordering::SeqCst) == 0 || repeats) {
                    return Some(server::error(404, "unknown_upload"));
                }
            }
            if req.rel() == "v2/publish"
                && failure != "nar"
                && (bad.fetch_add(1, Ordering::SeqCst) == 0 || repeats)
            {
                return Some(reply(vec![if failure == "open" {
                    rows[0].state("open")
                } else {
                    rows[0].error("unknown_upload")
                }]));
            }
            None
        });
        let expected = if repeats {
            UploadOutcome::Retry(RetryKind::Transient, format!("{path}: upload round limit"))
        } else {
            UploadOutcome::Uploaded
        };
        assert_eq!(
            backend(&store, &server, ProtocolMode::V2, 5, 1).upload(&job),
            expected
        );
        assert_eq!(route_count(&server, "v2/status"), 0);
        assert_eq!(
            route_count(&server, "v2/declare"),
            if failure == "open" { 1 } else { 2 }
        );
        assert_eq!(route_count(&server, &format!("v2/nar/{}", job.id)), 2);
        assert_eq!(
            route_count(&server, "v2/publish"),
            if failure == "nar" {
                usize::from(!repeats)
            } else {
                2
            }
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-034"))]
#[test]
fn an_invalid_missing_name_cannot_remove_any_eligible_ledger_entries() {
    for contradiction in [false, true] {
        let mut store = Store::new();
        let stale = store.add("cached", &[]);
        let same = store.add("same-request", &[]);
        let root = store.add("broken-missing-list", &[&stale, &same]);
        let q = store.queue();
        let stale_id = UploadId::generate();
        q.complete_path(&stale, stale_id, 4).unwrap();
        let job = job(&q, &root);
        let id = job.id;
        let unrelated = store_path("not-a-reference");
        let present = same.clone();
        let ledger = stale.clone();
        let server = fake(move |req, rows| {
            (req.rel() == "v2/publish").then(|| {
                reply(
                    rows.iter()
                        .map(|row| {
                            if row.id == id {
                                let invalid = if contradiction { &present } else { &unrelated };
                                row.missing(&[&ledger, invalid])
                            } else {
                                row.state("published")
                            }
                        })
                        .collect(),
                )
            })
        });
        assert_eq!(
            backend(&store, &server, ProtocolMode::V2, 8, 2).upload(&job),
            UploadOutcome::Retry(RetryKind::Transient, "protocol violation".into())
        );
        assert_eq!(q.done_record(&stale).unwrap().id, stale_id.to_string());
        assert!(q.is_uploaded(&same));
        assert!(!q.is_uploaded(&root));
        assert_eq!(route_count(&server, "v2/declare"), 1);
        assert_eq!(route_count(&server, "v2/publish"), 1);
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-019"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-027"))]
#[test]
fn a_mixed_missing_list_reworks_the_ledgered_path_and_holds_for_the_refused_path() {
    let mut store = Store::new();
    let stale = store.add("ledgered-dependency", &[]);
    let dep = store.add("incomplete-dependency", &[]);
    let root = store.add("mixed-referrer", &[&stale, &dep]);
    let q = store.queue();
    q.complete_path(&stale, UploadId::generate(), 9).unwrap();
    let dep_id = job(&q, &dep).id;
    let job = job(&q, &root);
    let paths = [stale.clone(), dep.clone()];
    let root_id = job.id;
    let server = fake(move |req, rows| {
        (req.rel() == "v2/publish").then(|| {
            reply(
                rows.iter()
                    .map(|row| {
                        if row.id == root_id {
                            row.missing(&[&paths[0], &paths[1]])
                        } else if row.id == dep_id {
                            row.error("quota_exceeded")
                        } else {
                            row.state("published")
                        }
                    })
                    .collect(),
            )
        })
    });
    let result = backend(&store, &server, ProtocolMode::V2, 8, 2).upload(&job);
    assert_eq!(
        result,
        UploadOutcome::Retry(
            RetryKind::QuotaExceeded,
            format!("{dep}: POST v2 publish: quota_exceeded")
        )
    );
    assert!(q.is_uploaded(&stale));
    assert_eq!(q.done_record(&stale).unwrap().uploaded_at, 0);
    assert!(q.upload_state(&dep).unwrap().nar_sent && q.upload_state(&root).unwrap().nar_sent);
    assert_eq!(route_count(&server, "v2/declare"), 2);
    assert_eq!(route_count(&server, "v2/publish"), 2);
    assert_eq!(route_count(&server, &format!("v2/nar/{root_id}")), 1);
    assert_eq!(route_count(&server, &format!("v2/nar/{dep_id}")), 1);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-019"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-025"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-028"))]
#[test]
fn a_chain_of_verified_referrers_is_carried_without_status_or_transfer() {
    let mut store = Store::new();
    let stale = store.add("stale", &[]);
    let bottom = store.add("bottom", &[&stale]);
    let middle = store.add("middle", &[&bottom]);
    let root = store.add("chain-top", &[&middle]);
    let q = store.queue();
    let stale_job = job(&q, &stale);
    let root_job = job(&q, &root);
    let bottom_id = job(&q, &bottom).id;
    let middle_id = job(&q, &middle).id;
    assert_eq!(mark(&store, &stale), stale_job.id);
    let status = AtomicUsize::new(0);
    let original_stale = stale_job.id;
    let lost = stale.clone();
    let queue = q.clone();
    let pub_calls = AtomicUsize::new(0);
    let server = fake_with(
        HashMap::from([(original_stale, stale.clone())]),
        move |req, rows| {
            match req.rel() {
                "v2/status" => {
                    assert_eq!(
                        rows.iter().map(|row| row.id).collect::<Vec<_>>(),
                        [original_stale]
                    );
                    let count = status.fetch_add(1, Ordering::SeqCst);
                    assert!(count < 2, "third status in the same group");
                    Some(reply(vec![rows[0].state(if count == 0 {
                        "present"
                    } else {
                        "verified"
                    })]))
                }
                "v2/declare" => Some(reply(
                    rows.iter().map(|row| row.state("verified")).collect(),
                )),
                "v2/publish" if pub_calls.fetch_add(1, Ordering::SeqCst) == 0 => {
                    assert_eq!(rows[0].id, bottom_id);
                    assert_eq!(rows[1].id, middle_id);
                    // A mark from an earlier observed send may still be on disk
                    // after the ledger is invalidated. That state needs status.
                    let wire = queue.upload_state(&lost).unwrap();
                    queue.mark_nar_sent(&lost, wire).unwrap();
                    Some(reply(vec![
                        rows[0].missing(&[&lost]),
                        rows[1].missing(&[rows[0].path()]),
                        rows[2].missing(&[rows[1].path()]),
                    ]))
                }
                _ => None,
            }
        },
    );
    assert_eq!(
        backend(&store, &server, ProtocolMode::V2, 8, 2).upload(&root_job),
        UploadOutcome::Uploaded
    );
    assert_eq!(
        server.lines(),
        [
            "GET protocol",
            "POST v2/status",
            "POST v2/declare",
            "POST v2/publish",
            "POST v2/status",
            "POST v2/publish"
        ]
    );
    assert!(
        q.is_uploaded(&stale)
            && q.is_uploaded(&bottom)
            && q.is_uploaded(&middle)
            && q.is_uploaded(&root)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-037"))]
#[test]
fn missing_dependencies_revoke_known_and_newly_discovered_referrers() {
    for (split, recover) in [(false, false), (false, true), (true, false)] {
        let mut store = Store::new();
        let stale = store.add("stale-bottom", &[]);
        let first = store.add("ledgered-referrer", &[&stale]);
        let extra = store.add("extra-leaf", &[]);
        let refs = if split {
            vec![&stale, &extra]
        } else {
            vec![&stale]
        };
        let trigger = store.add("sees-missing", &refs);
        let newly_known = store.add("new-referrer", &[&stale, &extra]);
        let q = store.queue();
        let first_job = job(&q, &first);
        let trigger_job = job(&q, &trigger);
        q.complete_path(&newly_known, UploadId::generate(), 2)
            .unwrap();
        let new_job = stockhouse_ingest::UploadJob {
            id: UploadId::generate(),
            store_path: newly_known.clone(),
            attempts: 0,
        };
        let [original, referrer, report, new] = [
            stale.clone(),
            first.clone(),
            trigger.clone(),
            newly_known.clone(),
        ];
        let queue = q.clone();
        let repair = AtomicUsize::new(0);
        let server = fake(move |req, rows| match req.rel() {
            "v2/declare" => Some(reply(
                rows.iter()
                    .map(|row| {
                        if row.path() == &report {
                            row.state("verified")
                        } else if row.path() == &original
                            && repair.fetch_add(1, Ordering::SeqCst) > 0
                        {
                            assert!(!queue.is_uploaded(&original) && !queue.is_uploaded(&referrer));
                            if recover {
                                row.state("present")
                            } else {
                                row.error("quota_exceeded")
                            }
                        } else if row.path() == &new {
                            assert!(!queue.is_uploaded(&new));
                            row.state("present")
                        } else {
                            row.state("present")
                        }
                    })
                    .collect(),
            )),
            "v2/publish" if queue.is_uploaded(&original) && repair.load(Ordering::SeqCst) == 1 => {
                assert!(queue.is_uploaded(&referrer));
                Some(reply(vec![rows[0].missing(&[&original])]))
            }
            _ => None,
        });
        let jobs = if split {
            vec![first_job, trigger_job, new_job]
        } else {
            vec![first_job, trigger_job]
        };
        let outcomes = backend(&store, &server, ProtocolMode::V2, 3, 2).upload_batch(&jobs);
        if recover {
            assert_eq!(outcomes, vec![UploadOutcome::Uploaded; 2]);
            assert!(q.is_uploaded(&stale) && q.is_uploaded(&first) && q.is_uploaded(&trigger));
        } else {
            let quota = UploadOutcome::Retry(
                RetryKind::QuotaExceeded,
                format!("{stale}: POST v2 declare: quota_exceeded"),
            );
            let waiting = UploadOutcome::Retry(
                RetryKind::Transient,
                format!("{first}: waiting for a reference"),
            );
            if split {
                assert_eq!(outcomes, vec![waiting, quota.clone(), quota]);
            } else {
                assert_eq!(outcomes, vec![quota.clone(), quota]);
            }
            assert!(!q.is_uploaded(&stale) && !q.is_uploaded(&first) && !q.is_uploaded(&trigger));
            if split {
                assert!(!q.is_uploaded(&newly_known));
            }
        }
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-020"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-025"))]
#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-028"))]
#[test]
fn the_third_round_is_the_last_even_for_a_path_carried_twice() {
    let mut store = Store::new();
    let lost = store.add("already-ledgered", &[]);
    let forgotten = store.add("forgotten-id", &[]);
    let lower = store.add("reworked-referrer", &[&lost, &forgotten]);
    let upper = store.add("carried-referrer", &[&lower]);
    let q = store.queue();
    q.complete_path(&lost, UploadId::generate(), 4).unwrap();
    let old = job(&q, &forgotten).id;
    let job = job(&q, &upper);
    let upper_id = job.id;
    let lost_in_fixture = lost.clone();
    let lower_in_fixture = lower.clone();
    let publish = AtomicUsize::new(0);
    let declare = AtomicUsize::new(0);
    let server = fake(move |req, rows| match req.rel() {
        "v2/declare" => {
            let round = declare.fetch_add(1, Ordering::SeqCst);
            assert!(round < 3, "fourth declaration round");
            Some(reply(
                rows.iter()
                    .map(|row| {
                        if row.id == old {
                            row.error("unknown_upload")
                        } else if round == 0 {
                            row.state("verified")
                        } else {
                            row.state("present")
                        }
                    })
                    .collect(),
            ))
        }
        "v2/publish" => {
            assert_eq!(rows[0].path(), &lower_in_fixture);
            assert_eq!(rows[1].id, upper_id);
            let turn = publish.fetch_add(1, Ordering::SeqCst);
            assert!(turn < 2, "third publish would imply a fourth round");
            let results = if turn == 0 {
                vec![
                    rows[0].missing(&[&lost_in_fixture]),
                    rows[1].missing(&[&lower_in_fixture]),
                ]
            } else {
                vec![rows[0].state("published"), rows[1].state("open")]
            };
            Some(reply(results))
        }
        _ => None,
    });
    let expected =
        UploadOutcome::Retry(RetryKind::Transient, format!("{upper}: upload round limit"));
    assert_eq!(
        backend(&store, &server, ProtocolMode::V2, 8, 2).upload(&job),
        expected
    );
    assert_eq!(
        server.lines(),
        [
            "GET protocol",
            "POST v2/declare",
            "POST v2/declare",
            "POST v2/publish",
            "POST v2/declare",
            "POST v2/publish"
        ]
    );
    assert!(q.is_uploaded(&lost) && q.is_uploaded(&forgotten) && q.is_uploaded(&lower));
    assert!(q.upload_state(&upper).unwrap().nar_sent);
}
