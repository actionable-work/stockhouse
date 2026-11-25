mod common;

use common::{declaration, path, upload_id};
use stockhouse_proto::PROTOCOL_VERSION;
use stockhouse_wire::{LeaseSlot, MachineId, ReplyError};
use stockhouse_wire_v2::*;

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NEGOTIATE-001"))]
#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-001"))]
#[test]
fn paths_are_relative_and_canonical() {
    assert_eq!(probe_path(), "protocol");
    assert_eq!(declare_path(), "v2/declare");
    assert_eq!(nar_path(&upload_id(0)), format!("v2/nar/{}", common::ID));
    assert_eq!(publish_path(), "v2/publish");
    assert_eq!(status_path(), "v2/status");
    assert_eq!(leases_path(), "v2/leases");
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NEGOTIATE-002"))]
#[test]
fn a_probe_requires_a_strict_ordered_version_list() {
    assert_eq!(
        parse_probe_reply(
            200,
            None,
            br#"{"features":{"unknown":true},"versions":[1,2,65535]}"#
        ),
        Ok(ProbeReply::Offered(vec![1, 2, 65535]))
    );
    let bad: &[&[u8]] = &[
        br#"{"version":1}"#,
        br#"{"versions":[]}"#,
        br#"{"versions":[2,1]}"#,
        br#"{"versions":[1,1]}"#,
        br#"{"versions":[0]}"#,
        br#"{"versions":[65536]}"#,
        br#"{"versions":[1.0]}"#,
        br#"{"versions":["1"]}"#,
        br#"{"versions":[1],"versions":[2]}"#,
        br#"[[1,2]]"#,
        br#"{"versions":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17]}"#,
    ];
    for body in bad {
        assert!(matches!(
            parse_probe_reply(200, None, body),
            Err(ReplyError::Unexpected { status: 200 })
        ));
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NEGOTIATE-003"))]
#[test]
fn no_probe_means_v1() {
    assert_eq!(
        parse_probe_reply(404, None, b"not JSON"),
        Ok(ProbeReply::Offered(vec![1]))
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NEGOTIATE-004"))]
#[test]
fn probe_retry_responses_ignore_the_body() {
    let body = vec![b'x'; MAX_PROBE_REPLY + 1];
    assert_eq!(
        parse_probe_reply(401, Some("1"), &body),
        Ok(ProbeReply::Unauthorized)
    );
    assert_eq!(
        parse_probe_reply(429, Some("12"), &body),
        Ok(ProbeReply::Busy {
            retry_after: Some(12)
        })
    );
    assert_eq!(
        parse_probe_reply(503, Some("never"), &body),
        Ok(ProbeReply::Unavailable { retry_after: None })
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NEGOTIATE-005"))]
#[test]
fn unexpected_and_oversized_probes_are_distinct() {
    assert!(matches!(
        parse_probe_reply(418, None, b""),
        Err(ReplyError::Unexpected { status: 418 })
    ));
    assert_eq!(
        parse_probe_reply(200, None, &vec![0; MAX_PROBE_REPLY + 1]),
        Err(ReplyError::TooLarge)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NEGOTIATE-006"))]
#[test]
fn selection_obeys_the_mode() {
    use ProtocolMode::{Auto, V1, V2};
    for (offered, automatic, required) in [
        (
            &[][..],
            Err(VersionError::NoCommonVersion),
            Err(VersionError::NotOffered),
        ),
        (&[1][..], Ok(1), Err(VersionError::NotOffered)),
        (&[2][..], Ok(2), Ok(2)),
        (&[1, 2, 3][..], Ok(2), Ok(2)),
        (
            &[3][..],
            Err(VersionError::NoCommonVersion),
            Err(VersionError::NotOffered),
        ),
    ] {
        assert_eq!(select_version(V1, offered), Ok(1));
        assert_eq!(select_version(Auto, offered), automatic);
        assert_eq!(select_version(V2, offered), required);
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NEGOTIATE-007"))]
#[test]
fn wire_versions_do_not_version_local_formats() {
    assert_eq!(WIRE_VERSIONS, [1, 2]);
    assert_eq!(PROTOCOL_VERSION, 1);
    let mut declare = DeclareBatch::new();
    declare
        .push(upload_id(0), declaration("one", vec![], None))
        .unwrap();
    let ids = IdBatch::new(vec![upload_id(0)]).unwrap();
    let leases = LeaseBatch::new(vec![LeaseEntry {
        machine: MachineId::from_bytes([0; 16]),
        slot: LeaseSlot::new(1).unwrap(),
        store_path: path("system"),
    }])
    .unwrap();
    for rendered in [declare.render().unwrap(), ids.render(), leases.render()] {
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&rendered).unwrap()["version"],
            2
        );
    }
}
