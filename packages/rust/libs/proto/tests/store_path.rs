mod common;
use common::*;
use stockhouse_proto::{StorePath, StorePathError, StorePathHash};

#[cfg_attr(any(), bloomery("CLIENT-PROTO-STOREPATH-001"))]
#[test]
fn valid_store_path_parses() {
    let text = format!("/nix/store/{HASH_A}-hello-1.0");
    let path = StorePath::parse(&text).unwrap();
    assert_eq!(path.hash().as_str(), HASH_A);
    assert_eq!(path.name(), "hello-1.0");
    assert_eq!(path.as_str(), text);
    assert_eq!(path.basename(), format!("{HASH_A}-hello-1.0"));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-STOREPATH-002"))]
#[test]
fn paths_outside_store_are_rejected() {
    for text in [format!("/gnu/store/{HASH_A}-x"), format!("/nix/storex/{HASH_A}-x"), format!("nix/store/{HASH_A}-x")] {
        assert_eq!(StorePath::parse(&text), Err(StorePathError::NotInStore), "{text}");
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-STOREPATH-003"))]
#[test]
fn bad_hashes_are_rejected() {
    let short = &HASH_A[..31];
    let bad_char = "e".repeat(32);
    for text in [format!("/nix/store/{short}-x"), format!("/nix/store/{bad_char}-x"), format!("/nix/store/{HASH_A}x-y")] {
        assert_eq!(StorePath::parse(&text), Err(StorePathError::InvalidHash), "{text}");
    }
    assert_eq!(StorePathHash::parse(short), Err(StorePathError::InvalidHash));
    assert_eq!(StorePathHash::parse(&bad_char), Err(StorePathError::InvalidHash));
    assert_eq!(StorePathHash::parse(HASH_A).unwrap().as_str(), HASH_A);
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-STOREPATH-004"))]
#[test]
fn bad_names_are_rejected() {
    let long = "a".repeat(212);
    for name in ["", ".hidden", "has space", "slash/x", "uni\u{e9}", long.as_str()] {
        let text = format!("/nix/store/{HASH_A}-{name}");
        assert_eq!(StorePath::parse(&text), Err(StorePathError::InvalidName), "{name:?}");
    }
    let max = "a".repeat(211);
    assert!(StorePath::parse(&format!("/nix/store/{HASH_A}-{max}")).is_ok());
    assert!(StorePath::parse(&format!("/nix/store/{HASH_A}-a+b-c._?=")).is_ok());
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-STOREPATH-005"))]
#[test]
fn basenames_parse_with_same_rules() {
    let path = StorePath::from_basename(&format!("{HASH_B}-ncurses")).unwrap();
    assert_eq!(path.as_str(), format!("/nix/store/{HASH_B}-ncurses"));
    assert_eq!(StorePath::from_basename("short-x"), Err(StorePathError::InvalidHash));
    assert_eq!(StorePath::from_basename(&format!("{HASH_B}-.x")), Err(StorePathError::InvalidName));
}
