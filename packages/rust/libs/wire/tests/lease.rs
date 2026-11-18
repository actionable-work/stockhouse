mod common;
use common::*;
use stockhouse_proto::StorePath;
use stockhouse_wire::{
    LeaseRefusal, LeaseReply, LeaseRequest, LeaseRequestError, LeaseSlot, LeaseSlotError,
    MachineId, MachineIdError, ReplyError, lease_path, parse_lease_reply,
};

const ID: &str = "0123456789abcdef0123456789abcdef";

#[cfg_attr(any(), bloomery("CLIENT-PROTO-LEASE-001"))]
#[test]
fn machine_identities_are_32_lowercase_hex() {
    assert_eq!(MachineId::parse(ID).unwrap().to_string(), ID);
    for bad in [
        &ID[..31],
        &format!("{ID}0"),
        &ID.to_uppercase(),
        &format!("{}g", &ID[..31]),
        "",
    ] {
        assert_eq!(MachineId::parse(bad), Err(MachineIdError::Invalid), "{bad}");
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-LEASE-002"))]
#[test]
fn machine_identities_render_from_bytes() {
    let id = MachineId::from_bytes([0xab; 16]);
    assert_eq!(id.to_string(), "ab".repeat(16));
    assert_eq!(MachineId::parse(&id.to_string()), Ok(id));
    assert_eq!(format!("{id:?}"), "MachineId(..)");
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-LEASE-003"))]
#[test]
fn lease_slots_are_one_to_ten() {
    assert_eq!(LeaseSlot::new(1).unwrap().get(), 1);
    assert_eq!(LeaseSlot::new(10).unwrap().get(), 10);
    for bad in [0, 11, 255] {
        assert_eq!(LeaseSlot::new(bad), Err(LeaseSlotError::OutOfRange));
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-LEASE-004"))]
#[test]
fn lease_paths_are_rendered_from_typed_values() {
    let id = MachineId::parse(ID).unwrap();
    assert_eq!(
        lease_path(&id, LeaseSlot::new(2).unwrap()),
        format!("leases/{ID}/2")
    );
    assert_eq!(
        lease_path(&id, LeaseSlot::new(10).unwrap()),
        format!("leases/{ID}/10")
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-LEASE-005"))]
#[test]
fn lease_requests_render_and_parse_strictly() {
    let store_path = StorePath::parse(&format!("/nix/store/{HASH_A}-system")).unwrap();
    let request = LeaseRequest { store_path };
    let text = String::from_utf8(request.render()).unwrap();
    assert_eq!(
        text,
        format!("{{\"version\":1,\"store_path\":\"/nix/store/{HASH_A}-system\"}}")
    );
    assert_eq!(LeaseRequest::parse(text.as_bytes()), Ok(request));
    let parse = |t: &str| LeaseRequest::parse(t.as_bytes());
    assert_eq!(parse(&" ".repeat(4097)), Err(LeaseRequestError::TooLarge));
    for bad in [
        text.replace("{\"version\"", "{\"x\":1,\"version\""),
        "{\"version\":1}".to_owned(),
        text.replace("\"version\":1", "\"version\":\"1\""),
    ] {
        assert_eq!(parse(&bad), Err(LeaseRequestError::Malformed), "{bad}");
    }
    let v2 = parse(&text.replace("\"version\":1", "\"version\":2"));
    assert!(matches!(v2, Err(LeaseRequestError::UnsupportedVersion(v)) if v.found == 2));
    assert_eq!(
        parse(&text.replace("/nix/store/", "/tmp/")),
        Err(LeaseRequestError::InvalidField {
            field: "store_path"
        })
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-LEASE-006"))]
#[test]
fn lease_replies_classify() {
    let r = |s, b: &str| parse_lease_reply(s, b.as_bytes());
    assert_eq!(r(200, r#"{"state":"renewed"}"#), Ok(LeaseReply::Renewed));
    assert_eq!(
        r(422, r#"{"error":"unknown_path"}"#),
        Ok(LeaseReply::Refused(LeaseRefusal::UnknownPath))
    );
    assert_eq!(
        r(409, r#"{"error":"conflict"}"#),
        Ok(LeaseReply::Refused(LeaseRefusal::Conflict))
    );
    assert_eq!(
        r(400, r#"{"error":"bad_request"}"#),
        Ok(LeaseReply::Refused(LeaseRefusal::BadRequest))
    );
    assert_eq!(r(401, "<html>"), Ok(LeaseReply::Unauthorized));
    assert_eq!(r(503, "full"), Ok(LeaseReply::Unavailable));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-LEASE-007"))]
#[test]
fn lease_replies_outside_the_protocol_are_unexpected() {
    let r = |s, b: &str| parse_lease_reply(s, b.as_bytes());
    assert_eq!(
        r(200, r#"{"state":"present"}"#),
        Err(ReplyError::Unexpected { status: 200 })
    );
    assert_eq!(
        r(200, r#"{"state":"renewed","x":1}"#),
        Err(ReplyError::Unexpected { status: 200 })
    );
    assert_eq!(
        r(404, r#"{"error":"conflict"}"#),
        Err(ReplyError::Unexpected { status: 404 })
    );
    assert_eq!(
        r(422, r#"{"error":"conflict"}"#),
        Err(ReplyError::Unexpected { status: 422 })
    );
    assert_eq!(
        r(200, &" ".repeat(64 * 1024 + 1)),
        Err(ReplyError::TooLarge)
    );
}
