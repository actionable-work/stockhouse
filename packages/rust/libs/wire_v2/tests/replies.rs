mod common;

use common::{HASH, ID, MACHINE, path, upload_id};
use stockhouse_wire::{LeaseRefusal, LeaseSlot, MachineId, ReplyError, UploadRefusal};
use stockhouse_wire_v2::*;

fn key(n: usize) -> EntryKey {
    EntryKey::Upload(upload_id(n))
}

fn batch(kind: BatchKind, row: &str) -> Result<BatchReply, ReplyError> {
    let reply = format!(r#"{{"results":[{{"id":"{ID}",{row}}}]}}"#);
    parse_batch_reply(kind, &[key(0)], 200, None, reply.as_bytes())
}

fn unexpected<T: std::fmt::Debug>(result: Result<T, ReplyError>, expected: u16) {
    assert!(
        matches!(&result, Err(ReplyError::Unexpected { status }) if *status == expected),
        "{result:?}"
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-012"))]
#[test]
fn request_level_statuses_have_exact_typed_replies() {
    let ignored = vec![b'x'; MAX_BATCH_REPLY + 1];
    let kind = BatchKind::Declare;
    for (status, expected) in [
        (401, BatchReply::Unauthorized),
        (404, BatchReply::NotSupported),
        (
            429,
            BatchReply::Busy {
                retry_after: Some(7),
            },
        ),
        (
            503,
            BatchReply::Unavailable {
                retry_after: Some(7),
            },
        ),
    ] {
        assert_eq!(
            parse_batch_reply(kind, &[], status, Some("7"), &ignored),
            Ok(expected)
        );
    }
    for (status, body, expected) in [
        (
            400,
            br#"{"error":"bad_request"}"#.as_slice(),
            BatchRefusal::BadRequest,
        ),
        (
            413,
            br#"{"error":"too_large"}"#.as_slice(),
            BatchRefusal::TooLarge,
        ),
    ] {
        assert_eq!(
            parse_batch_reply(kind, &[], status, None, body),
            Ok(BatchReply::Refused(expected))
        );
        unexpected(
            parse_batch_reply(kind, &[], status, None, b"wrong body"),
            status,
        );
    }
    unexpected(parse_batch_reply(kind, &[], 418, None, b""), 418);
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-013"))]
#[test]
fn retry_after_only_accepts_bounded_decimal_seconds() {
    for (header, retry_after) in [
        (Some("0"), Some(0)),
        (Some("0009"), Some(9)),
        (Some("86400"), Some(86_400)),
        (Some("86401"), None),
        (Some("-1"), None),
        (Some("+1"), None),
        (Some(""), None),
        (Some("Sun, 06 Nov 1994 08:49:37 GMT"), None),
        (None, None),
    ] {
        assert_eq!(parse_retry_after(header), retry_after);
        assert_eq!(
            parse_batch_reply(BatchKind::Status, &[], 429, header, b""),
            Ok(BatchReply::Busy { retry_after })
        );
        assert_eq!(
            parse_nar_reply(503, header, b"x"),
            Ok(NarReply::Unavailable { retry_after })
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-014"))]
#[test]
fn result_identity_count_order_and_keys_are_strict() {
    let other = upload_id(1);
    let row = format!(r#"{{"id":"{ID}","state":"open"}}"#);
    let swapped = format!(r#"{{"results":[{{"id":"{other}","state":"verified"}},{row}]}}"#);
    unexpected(
        parse_batch_reply(
            BatchKind::Status,
            &[key(0), key(1)],
            200,
            None,
            swapped.as_bytes(),
        ),
        200,
    );
    for body in [
        r#"{"results":[]}"#.into(),
        format!(r#"{{"version":2,"results":[{row}]}}"#),
        format!(r#"{{"results":[{row},{row}]}}"#),
        format!(r#"{{"results":[{row}],"results":[{row}]}}"#),
        format!(r#"{{"results":[{{"id":"{ID}","state":"open","unknown":null}}]}}"#),
        format!(r#"{{"results":[{{"id":"{ID}","id":"{ID}","state":"open"}}]}}"#),
        format!(r#"{{"results":[{{"id":"{ID}","state":"open","error":null}}]}}"#),
        format!(r#"{{"results":[["{ID}","open"]]}}"#),
    ] {
        unexpected(
            parse_batch_reply(BatchKind::Declare, &[key(0)], 200, None, body.as_bytes()),
            200,
        );
    }
}

fn upload_state(kind: BatchKind, name: &str, state: UploadState) {
    assert_eq!(
        batch(kind, &format!(r#""state":"{name}""#)),
        Ok(BatchReply::Results(vec![EntryResult::Upload(
            UploadResult::State(state)
        )]))
    );
}

fn upload_refusal(kind: BatchKind, name: &str, refusal: UploadRefusal) {
    assert_eq!(
        batch(kind, &format!(r#""error":"{name}""#)),
        Ok(BatchReply::Results(vec![EntryResult::Upload(
            UploadResult::Refused(refusal)
        )]))
    );
}

fn common_upload_results(kind: BatchKind) {
    upload_state(kind, "open", UploadState::Open);
    upload_state(kind, "present", UploadState::Present);
    upload_state(kind, "published", UploadState::Published);
    upload_refusal(
        kind,
        "verification_failed",
        UploadRefusal::VerificationFailed,
    );
    upload_refusal(kind, "unknown_upload", UploadRefusal::UnknownUpload);
    unexpected(batch(kind, r#""error":"bad_request""#), 200);
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-015"))]
#[test]
fn declaration_entry_results() {
    let kind = BatchKind::Declare;
    common_upload_results(kind);
    upload_state(kind, "verified", UploadState::Verified);
    upload_refusal(kind, "conflict", UploadRefusal::Conflict);
    upload_refusal(kind, "quota_exceeded", UploadRefusal::QuotaExceeded);
    upload_refusal(kind, "too_large", UploadRefusal::TooLarge);
    unexpected(
        batch(
            kind,
            &format!(r#""error":"missing_references","missing":["{HASH}-ref"]"#),
        ),
        200,
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-016"))]
#[test]
fn publish_entry_results() {
    let kind = BatchKind::Publish;
    common_upload_results(kind);
    upload_refusal(kind, "conflict", UploadRefusal::Conflict);
    upload_refusal(kind, "quota_exceeded", UploadRefusal::QuotaExceeded);
    unexpected(batch(kind, r#""state":"verified""#), 200);
    unexpected(batch(kind, r#""error":"too_large""#), 200);
    assert_eq!(
        batch(
            kind,
            &format!(r#""error":"missing_references","missing":["{HASH}-b","{HASH}-a"]"#)
        ),
        Ok(BatchReply::Results(vec![EntryResult::Upload(
            UploadResult::Refused(UploadRefusal::MissingReferences(vec![path("b"), path("a")]),)
        )]))
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-017"))]
#[test]
fn status_entry_results() {
    let kind = BatchKind::Status;
    common_upload_results(kind);
    upload_state(kind, "verified", UploadState::Verified);
    for code in [
        "conflict",
        "quota_exceeded",
        "too_large",
        "missing_references",
    ] {
        unexpected(batch(kind, &format!(r#""error":"{code}""#)), 200);
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-018"))]
#[test]
fn missing_references_are_distinct_valid_and_bounded_across_the_reply() {
    let kind = BatchKind::Publish;
    for row in [
        r#""error":"missing_references","missing":[]"#.into(),
        format!(r#""error":"missing_references","missing":["{HASH}-a","{HASH}-a"]"#),
        format!(r#""error":"missing_references","missing":["/nix/store/{HASH}-a"]"#),
        format!(r#""error":"quota_exceeded","missing":["{HASH}-a"]"#),
        r#""error":"missing_references""#.into(),
    ] {
        unexpected(batch(kind, &row), 200);
    }

    let list = (0..10_000)
        .map(|n| format!("\"{HASH}-path-{n}\""))
        .collect::<Vec<_>>()
        .join(",");
    assert!(
        batch(
            kind,
            &format!(r#""error":"missing_references","missing":[{list}]"#)
        )
        .is_ok()
    );
    let second = upload_id(1);
    let response = format!(
        r#"{{"results":[{{"id":"{ID}","error":"missing_references","missing":[{list}]}},{{"id":"{second}","error":"missing_references","missing":["{HASH}-last"]}}]}}"#
    );
    unexpected(
        parse_batch_reply(kind, &[key(0), key(1)], 200, None, response.as_bytes()),
        200,
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-019"))]
#[test]
fn batch_replies_which_carry_a_body_are_bounded() {
    assert_eq!(
        parse_batch_reply(
            BatchKind::Publish,
            &[key(0)],
            200,
            None,
            &vec![b'x'; MAX_BATCH_REPLY + 1]
        ),
        Err(ReplyError::TooLarge)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-020"))]
#[test]
fn nar_reply_matrix() {
    for (name, state) in [
        ("verified", UploadState::Verified),
        ("present", UploadState::Present),
        ("published", UploadState::Published),
    ] {
        assert_eq!(
            parse_nar_reply(200, None, format!(r#"{{"state":"{name}"}}"#).as_bytes()),
            Ok(NarReply::State(state))
        );
    }
    for (status, code, reason) in [
        (400, "bad_request", UploadRefusal::BadRequest),
        (
            400,
            "verification_failed",
            UploadRefusal::VerificationFailed,
        ),
        (404, "unknown_upload", UploadRefusal::UnknownUpload),
        (409, "conflict", UploadRefusal::Conflict),
        (413, "too_large", UploadRefusal::TooLarge),
        (507, "quota_exceeded", UploadRefusal::QuotaExceeded),
    ] {
        assert_eq!(
            parse_nar_reply(status, None, format!(r#"{{"error":"{code}"}}"#).as_bytes()),
            Ok(NarReply::Refused(reason))
        );
    }
    assert_eq!(
        parse_nar_reply(404, None, &vec![b'x'; MAX_NAR_REPLY + 1]),
        Ok(NarReply::NotSupported)
    );
    assert_eq!(
        parse_nar_reply(404, None, b"missing route"),
        Ok(NarReply::NotSupported)
    );
    assert_eq!(
        parse_nar_reply(401, None, b"invalid"),
        Ok(NarReply::Unauthorized)
    );
    assert_eq!(
        parse_nar_reply(429, Some("2"), b"invalid"),
        Ok(NarReply::Busy {
            retry_after: Some(2)
        })
    );
    assert_eq!(
        parse_nar_reply(503, None, b"invalid"),
        Ok(NarReply::Unavailable { retry_after: None })
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-021"))]
#[test]
fn nar_rejects_open_missing_references_and_overlong_replies() {
    for (status, body) in [
        (200, br#"{"state":"open"}"#.as_slice()),
        (422, br#"{"error":"missing_references","missing":["z"]}"#),
        (200, br#"{"state":"verified","extra":null}"#),
        (400, br#"{"error":"bad_request","error":"bad_request"}"#),
    ] {
        unexpected(parse_nar_reply(status, None, body), status);
    }
    assert_eq!(
        parse_nar_reply(200, None, &vec![b'x'; MAX_NAR_REPLY + 1]),
        Err(ReplyError::TooLarge)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-LEASE-009"))]
#[test]
fn lease_results_match_machine_slot_and_allowed_codes() {
    let key = EntryKey::Lease(
        MachineId::parse(MACHINE).unwrap(),
        LeaseSlot::new(1).unwrap(),
    );
    let classify = |row: &str, slot| {
        let response = format!(r#"{{"results":[{{"machine":"{MACHINE}","slot":{slot},{row}}}]}}"#);
        parse_batch_reply(BatchKind::Leases, &[key], 200, None, response.as_bytes())
    };
    for (row, result) in [
        (r#""state":"renewed""#, LeaseResult::Renewed),
        (
            r#""error":"unknown_path""#,
            LeaseResult::Refused(LeaseRefusal::UnknownPath),
        ),
        (
            r#""error":"conflict""#,
            LeaseResult::Refused(LeaseRefusal::Conflict),
        ),
        (r#""error":"machines_full""#, LeaseResult::MachinesFull),
    ] {
        assert_eq!(
            classify(row, 1),
            Ok(BatchReply::Results(vec![EntryResult::Lease(result)]))
        );
        unexpected(classify(row, 2), 200);
    }
    unexpected(classify(r#""error":"bad_request""#, 1), 200);
    unexpected(classify(r#""state":"published""#, 1), 200);
    let other = MachineId::from_bytes([0; 16]);
    let body = format!(r#"{{"results":[{{"machine":"{other}","slot":1,"state":"renewed"}}]}}"#);
    unexpected(
        parse_batch_reply(BatchKind::Leases, &[key], 200, None, body.as_bytes()),
        200,
    );
}
