mod common;
use RequestKind::{Declare, Nar, Status};
use common::*;
use stockhouse_proto::StorePath;
use stockhouse_wire::{
    MAX_BODY, MAX_REFERENCES, ReplyError, RequestKind, UploadRefusal, UploadReply, UploadState,
    parse_reply,
};

fn state(s: UploadState) -> Result<UploadReply, ReplyError> {
    Ok(UploadReply::State(s))
}

fn refused(r: UploadRefusal) -> Result<UploadReply, ReplyError> {
    Ok(UploadReply::Refused(r))
}

fn unexpected(status: u16) -> Result<UploadReply, ReplyError> {
    Err(ReplyError::Unexpected { status })
}

fn reply(kind: RequestKind, status: u16, body: &str) -> Result<UploadReply, ReplyError> {
    parse_reply(kind, status, body.as_bytes())
}

fn err(code: &str) -> String {
    format!("{{\"error\":\"{code}\"}}")
}

fn missing_body() -> String {
    format!("{{\"error\":\"missing_references\",\"missing\":[\"{HASH_C}-glibc\"]}}")
}

fn glibc() -> Vec<StorePath> {
    vec![StorePath::from_basename(&format!("{HASH_C}-glibc")).unwrap()]
}

fn common_states(kind: RequestKind) {
    assert_eq!(
        reply(kind, 200, r#"{"state":"present"}"#),
        state(UploadState::Present)
    );
    assert_eq!(
        reply(kind, 200, r#"{"state":"published"}"#),
        state(UploadState::Published)
    );
    assert_eq!(reply(kind, 401, ""), refused(UploadRefusal::Unauthorized));
    assert_eq!(reply(kind, 503, ""), refused(UploadRefusal::Unavailable));
    assert_eq!(
        reply(kind, 400, &err("verification_failed")),
        refused(UploadRefusal::VerificationFailed)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-UPLOAD-008"))]
#[test]
fn declaration_replies_classify() {
    assert_eq!(
        reply(Declare, 404, &err("unknown_upload")),
        refused(UploadRefusal::UnknownUpload)
    );
    common_states(Declare);
    assert_eq!(
        reply(Declare, 200, r#"{"state":"open"}"#),
        state(UploadState::Open)
    );
    assert_eq!(
        reply(Declare, 400, &err("bad_request")),
        refused(UploadRefusal::BadRequest)
    );
    assert_eq!(
        reply(Declare, 409, &err("conflict")),
        refused(UploadRefusal::Conflict)
    );
    assert_eq!(
        reply(Declare, 413, &err("too_large")),
        refused(UploadRefusal::TooLarge)
    );
    assert_eq!(
        reply(Declare, 422, &missing_body()),
        refused(UploadRefusal::MissingReferences(glibc()))
    );
    assert_eq!(
        reply(Declare, 507, &err("quota_exceeded")),
        refused(UploadRefusal::QuotaExceeded)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-UPLOAD-009"))]
#[test]
fn nar_replies_classify() {
    common_states(Nar);
    assert_eq!(
        reply(Nar, 400, &err("bad_request")),
        refused(UploadRefusal::BadRequest)
    );
    assert_eq!(
        reply(Nar, 404, &err("unknown_upload")),
        refused(UploadRefusal::UnknownUpload)
    );
    assert_eq!(
        reply(Nar, 409, &err("conflict")),
        refused(UploadRefusal::Conflict)
    );
    assert_eq!(
        reply(Nar, 413, &err("too_large")),
        refused(UploadRefusal::TooLarge)
    );
    assert_eq!(
        reply(Nar, 422, &missing_body()),
        refused(UploadRefusal::MissingReferences(glibc()))
    );
    assert_eq!(
        reply(Nar, 507, &err("quota_exceeded")),
        refused(UploadRefusal::QuotaExceeded)
    );
    assert_eq!(reply(Nar, 200, r#"{"state":"open"}"#), unexpected(200));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-UPLOAD-010"))]
#[test]
fn status_replies_classify() {
    common_states(Status);
    assert_eq!(
        reply(Status, 200, r#"{"state":"open"}"#),
        state(UploadState::Open)
    );
    assert_eq!(
        reply(Status, 404, &err("unknown_upload")),
        refused(UploadRefusal::UnknownUpload)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-UPLOAD-011"))]
#[test]
fn replies_outside_the_protocol_are_unexpected() {
    assert_eq!(reply(Status, 409, &err("conflict")), unexpected(409));
    assert_eq!(reply(Status, 422, &missing_body()), unexpected(422));
    assert_eq!(reply(Declare, 500, &err("conflict")), unexpected(500));
    assert_eq!(reply(Declare, 302, ""), unexpected(302));
    assert_eq!(
        reply(Declare, 200, r#"{"state":"present","extra":1}"#),
        unexpected(200)
    );
    assert_eq!(
        reply(Declare, 200, r#"{"version":1,"state":"present"}"#),
        unexpected(200)
    );
    assert_eq!(reply(Declare, 200, r#"["present"]"#), unexpected(200));
    assert_eq!(reply(Declare, 200, r#"{"state":"gone"}"#), unexpected(200));
    assert_eq!(
        reply(Declare, 409, r#"{"error":"conflict","missing":[]}"#),
        unexpected(409)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-UPLOAD-012"))]
#[test]
fn unauthorized_and_unavailable_ignore_the_body() {
    for kind in [Declare, Nar, Status] {
        assert_eq!(
            reply(kind, 401, "<html>no</html>"),
            refused(UploadRefusal::Unauthorized)
        );
        assert_eq!(
            reply(kind, 503, "\u{0}"),
            refused(UploadRefusal::Unavailable)
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-UPLOAD-013"))]
#[test]
fn missing_reference_lists_are_bounded_and_valid() {
    let with = |items: &str| format!("{{\"error\":\"missing_references\",\"missing\":[{items}]}}");
    let one = format!("\"{HASH_C}-glibc\"");
    let too_many = vec![one.as_str(); MAX_REFERENCES + 1].join(",");
    for items in [
        String::new(),
        too_many,
        format!("{one},{one}"),
        format!("\"/nix/store/{HASH_C}-glibc\""),
        "1".to_owned(),
    ] {
        assert_eq!(reply(Declare, 422, &with(&items)), unexpected(422));
    }
    assert_eq!(
        reply(Declare, 422, &err("missing_references")),
        unexpected(422)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-UPLOAD-014"))]
#[test]
fn oversized_replies_are_refused() {
    let big = vec![b' '; MAX_BODY + 1];
    assert_eq!(parse_reply(Declare, 200, &big), Err(ReplyError::TooLarge));
    let names: Vec<String> = (0..MAX_REFERENCES)
        .map(|i| {
            let hash = stockhouse_proto::nix32::encode(
                &stockhouse_proto::Sha256::digest(&i.to_le_bytes()).bytes()[..20],
            );
            format!("\"{hash}-{}\"", "n".repeat(211))
        })
        .collect();
    let body = format!(
        "{{\"error\":\"missing_references\",\"missing\":[{}]}}",
        names.join(",")
    );
    assert!(body.len() <= MAX_BODY);
    assert!(matches!(
        reply(Declare, 422, &body),
        Ok(UploadReply::Refused(UploadRefusal::MissingReferences(m))) if m.len() == MAX_REFERENCES
    ));
}
