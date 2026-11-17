mod common;
use common::*;
use stockhouse_proto::{HashError, Sha256, nix32};

const EMPTY_HEX: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[cfg_attr(any(), bloomery("CLIENT-PROTO-HASH-001"))]
#[test]
fn nix32_round_trips_and_matches_nix() {
    let parsed = Sha256::parse(EMPTY_SHA256).unwrap();
    assert_eq!(parsed, Sha256::digest(b""));
    assert_eq!(parsed.to_string(), EMPTY_SHA256);
    assert_eq!(
        nix32::encode(Sha256::digest(b"").bytes()),
        &EMPTY_SHA256[7..]
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-HASH-002"))]
#[test]
fn hex_parses_and_renders_as_nix32() {
    let parsed = Sha256::parse(EMPTY_HEX).unwrap();
    assert_eq!(parsed, Sha256::parse(EMPTY_SHA256).unwrap());
    assert_eq!(parsed.to_string(), EMPTY_SHA256);
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-HASH-003"))]
#[test]
fn other_algorithms_are_rejected() {
    for text in [
        "sha512:abc",
        "md5:abc",
        "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=",
        "",
    ] {
        assert_eq!(
            Sha256::parse(text),
            Err(HashError::UnsupportedAlgorithm),
            "{text}"
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-HASH-004"))]
#[test]
fn bad_encodings_are_rejected() {
    let upper_hex = EMPTY_HEX.to_uppercase().replace("SHA256", "sha256");
    let bad_char = format!("sha256:e{}", &EMPTY_SHA256[8..]);
    // The leading character carries only 1 significant bit; 'z' sets unused bits.
    let non_canonical = format!("sha256:z{}", &EMPTY_SHA256[8..]);
    for text in ["sha256:abc".to_owned(), upper_hex, bad_char, non_canonical] {
        assert_eq!(
            Sha256::parse(&text),
            Err(HashError::InvalidEncoding),
            "{text}"
        );
    }
}
