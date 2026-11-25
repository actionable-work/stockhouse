mod common;

use common::*;
use std::collections::HashMap;
use stockhouse_ingest::{UploadOutcome, Uploader};
use stockhouse_wire_v2::ProtocolMode;

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-009"))]
#[test]
fn marked_paths_query_status_before_all_other_requests() {
    for (result, routes, rejected) in [
        ("present", vec!["v2/status"], false),
        ("published", vec!["v2/status"], false),
        ("verified", vec!["v2/status", "v2/publish"], false),
        ("open", vec!["v2/status", "v2/nar", "v2/publish"], false),
        (
            "unknown_upload",
            vec!["v2/status", "v2/declare", "v2/nar", "v2/publish"],
            false,
        ),
        ("verification_failed", vec!["v2/status"], true),
    ] {
        let mut store = Store::new();
        let path = store.add("resume", &[]);
        let q = store.queue();
        let job = job(&q, &path);
        let id = mark(&store, &path);
        let server = fake_with(HashMap::from([(id, path.clone())]), move |req, rows| {
            (req.rel() == "v2/status").then(|| {
                let row = &rows[0];
                assert_eq!(row.id, id);
                reply(vec![match result {
                    "unknown_upload" | "verification_failed" => row.error(result),
                    _ => row.state(result),
                }])
            })
        });
        let expected = if rejected {
            UploadOutcome::Rejected(format!("{path}: POST v2 status: verification_failed"))
        } else {
            UploadOutcome::Uploaded
        };
        assert_eq!(
            backend(&store, &server, ProtocolMode::V2, 2, 2).upload(&job),
            expected
        );
        assert_eq!(
            server
                .requests()
                .iter()
                .skip(1)
                .map(|req| {
                    if req.rel().starts_with("v2/nar/") {
                        "v2/nar"
                    } else {
                        req.rel()
                    }
                })
                .collect::<Vec<_>>(),
            routes
        );
    }
}
