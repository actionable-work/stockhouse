mod common;
use common::*;
use stockhouse_proto::{KeyError, NarInfo, PublicKey, Signature, SignatureError, TrustedKeys};

const KEY_B64: &str = "UpT9w0BGYiXPBfpdNiWwBf1b3LAwvtjoUHLlbFaOM3E=";

#[cfg_attr(any(), bloomery("CLIENT-PROTO-SIGNATURE-001"))]
#[test]
fn well_formed_keys_and_signatures_parse() {
    let key = PublicKey::parse(&format!("stockhouse-vm-fixture-1:{KEY_B64}")).unwrap();
    assert_eq!(key.name(), "stockhouse-vm-fixture-1");
    assert_eq!(key.to_string(), format!("stockhouse-vm-fixture-1:{KEY_B64}"));
    let sig = sign(&signed("a", 1), "fixture-1", 1);
    let parsed = Signature::parse(&sig.to_string()).unwrap();
    assert_eq!(parsed, sig);
    assert_eq!(parsed.bytes().len(), 64);
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-SIGNATURE-008"))]
#[test]
fn separator_and_name_are_required() {
    assert_eq!(PublicKey::parse(KEY_B64), Err(KeyError::Malformed));
    assert_eq!(PublicKey::parse(&format!(":{KEY_B64}")), Err(KeyError::InvalidName));
    assert_eq!(Signature::parse("a b:AAAA"), Err(KeyError::InvalidName));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-SIGNATURE-009"))]
#[test]
fn payloads_must_be_valid_base64_of_the_right_length() {
    assert_eq!(PublicKey::parse("k:!!!!"), Err(KeyError::InvalidBase64));
    assert_eq!(PublicKey::parse("k:UpT9w0BGYiXPBfpdNiWwBf1b3LAwvtjoUHLlbFaOM3E"), Err(KeyError::InvalidBase64));
    assert_eq!(PublicKey::parse("k:AAAA"), Err(KeyError::InvalidLength));
    assert_eq!(Signature::parse(&format!("k:{KEY_B64}")), Err(KeyError::InvalidLength));
    // y = 2 is not on the curve.
    let not_a_point = "k:AgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    assert_eq!(PublicKey::parse(not_a_point), Err(KeyError::InvalidKey));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-SIGNATURE-002"))]
#[test]
fn fingerprint_sorts_references() {
    let info = signed("fixture-1", 1);
    let expected = format!(
        "1;/nix/store/{HASH_A}-hello-1.0;{EMPTY_SHA256};400;/nix/store/{HASH_B}-ncurses,/nix/store/{HASH_C}-glibc"
    );
    assert_eq!(info.fingerprint(), expected);
    let mut reversed = info.clone();
    reversed.references.reverse();
    assert_eq!(reversed.fingerprint(), expected);
}

fn trusted() -> TrustedKeys {
    TrustedKeys::new(vec![public_key("fixture-1", 1), public_key("fixture-2", 2)]).unwrap()
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-SIGNATURE-003"))]
#[test]
fn valid_trusted_signature_verifies() {
    let mut info = signed("other", 9);
    info.signatures.push(sign(&info, "fixture-2", 2));
    assert_eq!(trusted().verify(&info), Ok("fixture-2".to_owned()));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-SIGNATURE-004"))]
#[test]
fn failing_trusted_signature_is_a_hard_error() {
    let mut info = signed("fixture-1", 1);
    info.signatures.push(sign(&info, "fixture-2", 7));
    assert_eq!(trusted().verify(&info), Err(SignatureError::InvalidSignature { key: "fixture-2".to_owned() }));
    let mut tampered = signed("fixture-1", 1);
    tampered.nar_size += 1;
    assert_eq!(trusted().verify(&tampered), Err(SignatureError::InvalidSignature { key: "fixture-1".to_owned() }));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-SIGNATURE-005"))]
#[test]
fn unsigned_narinfo_is_rejected() {
    let info = NarInfo::parse(unsigned_text().as_bytes()).unwrap();
    assert_eq!(trusted().verify(&info), Err(SignatureError::Unsigned));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-SIGNATURE-006"))]
#[test]
fn only_untrusted_signers_are_rejected() {
    assert_eq!(trusted().verify(&signed("stranger", 5)), Err(SignatureError::UntrustedSigner));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-SIGNATURE-007"))]
#[test]
fn duplicate_key_names_are_rejected() {
    let keys = vec![public_key("same", 1), public_key("same", 2)];
    assert_eq!(TrustedKeys::new(keys), Err(KeyError::DuplicateName("same".to_owned())));
}
