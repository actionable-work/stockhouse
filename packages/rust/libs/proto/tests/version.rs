use stockhouse_proto::{PROTOCOL_VERSION, UnsupportedVersion, UploadId, UploadIdError, check_version};

#[cfg_attr(any(), bloomery("CLIENT-PROTO-VERSION-001"))]
#[test]
fn unknown_versions_are_rejected() {
    assert_eq!(PROTOCOL_VERSION, 1);
    assert_eq!(check_version(1), Ok(()));
    assert_eq!(check_version(2), Err(UnsupportedVersion { found: 2, supported: 1 }));
    assert_eq!(check_version(0), Err(UnsupportedVersion { found: 0, supported: 1 }));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-VERSION-002"))]
#[test]
fn upload_ids_are_canonical_uuidv7() {
    let text = "01890a5d-ac96-774b-bcce-b302099a8057";
    assert_eq!(UploadId::parse(text).unwrap().to_string(), text);
    for bad in [
        "01890A5D-AC96-774B-BCCE-B302099A8057",
        "01890a5dac96774bbcceb302099a8057",
        "01890a5d-ac96-474b-bcce-b302099a8057",
        "not-a-uuid",
    ] {
        assert_eq!(UploadId::parse(bad), Err(UploadIdError::Invalid), "{bad}");
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-VERSION-003"))]
#[test]
fn generated_ids_are_distinct_uuidv7() {
    let (a, b) = (UploadId::generate(), UploadId::generate());
    assert_ne!(a, b);
    for id in [a, b] {
        assert_eq!(UploadId::parse(&id.to_string()), Ok(id));
    }
}
