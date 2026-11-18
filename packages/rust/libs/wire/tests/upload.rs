mod common;
use common::*;
use stockhouse_proto::{Sha256, StorePath, UploadId};
use stockhouse_wire::{
    DeclarationError, MAX_BODY, MAX_REFERENCES, UploadDeclaration, nar_path, upload_path,
};

fn path(base: &str) -> StorePath {
    StorePath::from_basename(base).unwrap()
}

fn fixture(deriver: bool, ca: bool) -> UploadDeclaration {
    UploadDeclaration::new(
        path(&format!("{HASH_A}-hello")),
        Sha256::parse(EMPTY_SHA256).unwrap(),
        400,
        vec![
            path(&format!("{HASH_A}-hello")),
            path(&format!("{HASH_C}-glibc")),
        ],
        deriver.then(|| path(&format!("{HASH_B}-hello.drv"))),
        ca.then(|| "fixed:r:sha256:abc".to_owned()),
    )
    .unwrap()
}

fn json(text: &str) -> Result<UploadDeclaration, DeclarationError> {
    UploadDeclaration::parse(text.as_bytes())
}

fn valid_text() -> String {
    String::from_utf8(fixture(true, false).render()).unwrap()
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-UPLOAD-001"))]
#[test]
fn declarations_render_canonically() {
    let expected = format!(
        "{{\"version\":1,\"store_path\":\"/nix/store/{HASH_A}-hello\",\"nar_hash\":\"{EMPTY_SHA256}\",\
\"nar_size\":400,\"references\":[\"{HASH_A}-hello\",\"{HASH_C}-glibc\"],\
\"deriver\":\"{HASH_B}-hello.drv\",\"ca\":\"fixed:r:sha256:abc\"}}"
    );
    assert_eq!(
        String::from_utf8(fixture(true, true).render()).unwrap(),
        expected
    );
    let bare = String::from_utf8(fixture(false, false).render()).unwrap();
    assert!(bare.ends_with("\"deriver\":null,\"ca\":null}"), "{bare}");
}

fn long_name(i: usize) -> StorePath {
    let hash = stockhouse_proto::nix32::encode(&Sha256::digest(&i.to_le_bytes()).bytes()[..20]);
    path(&format!("{hash}-{}", "n".repeat(211)))
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-UPLOAD-002"))]
#[test]
fn valid_declarations_fit_the_cap_and_round_trip() {
    for declaration in [fixture(true, true), fixture(false, false)] {
        assert_eq!(
            json(std::str::from_utf8(&declaration.render()).unwrap()),
            Ok(declaration)
        );
    }
    let references = (0..MAX_REFERENCES).map(long_name).collect();
    let largest = UploadDeclaration::new(
        long_name(usize::MAX),
        Sha256::digest(b"x"),
        u64::MAX,
        references,
        None,
        Some("c".repeat(1024)),
    )
    .unwrap();
    let rendered = largest.render();
    assert!(rendered.len() <= MAX_BODY, "{}", rendered.len());
    assert_eq!(UploadDeclaration::parse(&rendered), Ok(largest));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-UPLOAD-003"))]
#[test]
fn declarations_refuse_unknown_missing_and_mistyped_keys() {
    let valid = valid_text();
    let cases = [
        "[]".to_owned(),
        valid.replace("{\"version\"", "{\"extra\":1,\"version\""),
        valid.replace(",\"ca\":null", ""),
        valid.replace(",\"deriver\":", ",\"deriver\":null,\"deriver\":"),
        valid.replace("\"nar_size\":400", "\"nar_size\":\"400\""),
    ];
    for case in cases {
        assert_eq!(json(&case), Err(DeclarationError::Malformed), "{case}");
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-UPLOAD-004"))]
#[test]
fn declarations_refuse_other_versions() {
    let text = valid_text().replace("\"version\":1", "\"version\":2");
    assert!(matches!(json(&text), Err(DeclarationError::UnsupportedVersion(v)) if v.found == 2));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-UPLOAD-005"))]
#[test]
fn declarations_refuse_invalid_values() {
    let valid = valid_text();
    let glibc = format!("\"{HASH_C}-glibc\"");
    let cases = [
        (valid.replace("/nix/store/", "/opt/store/"), "store_path"),
        (valid.replace(EMPTY_SHA256, "md5:00"), "nar_hash"),
        (
            valid.replace("\"nar_size\":400", "\"nar_size\":0"),
            "nar_size",
        ),
        (valid.replace(&glibc, "\"bad\""), "references"),
        (
            valid.replace(&glibc, &format!("\"{HASH_A}-hello\"")),
            "references",
        ),
        (valid.replace("hello.drv", "hello"), "deriver"),
        (
            valid.replace("\"ca\":null", &format!("\"ca\":\"{}\"", "c".repeat(1025))),
            "ca",
        ),
        (valid.replace("\"ca\":null", "\"ca\":\"a\\nb\""), "ca"),
    ];
    for (case, field) in cases {
        assert_eq!(
            json(&case),
            Err(DeclarationError::InvalidField { field }),
            "{case}"
        );
    }
    let many = format!(
        "\"references\":[{}]",
        vec![glibc.as_str(); MAX_REFERENCES + 1].join(",")
    );
    let refs = valid.replace(
        &format!("\"references\":[\"{HASH_A}-hello\",{glibc}]"),
        &many,
    );
    assert_eq!(
        json(&refs),
        Err(DeclarationError::InvalidField {
            field: "references"
        })
    );
    let built = UploadDeclaration::new(
        path(&format!("{HASH_A}-hello")),
        Sha256::digest(b""),
        0,
        vec![],
        None,
        None,
    );
    assert_eq!(
        built,
        Err(DeclarationError::InvalidField { field: "nar_size" })
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-UPLOAD-006"))]
#[test]
fn oversized_declarations_are_refused_before_parsing() {
    let padded = format!("{}{}", " ".repeat(MAX_BODY + 1 - 2), "{}");
    assert_eq!(json(&padded), Err(DeclarationError::TooLarge));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-UPLOAD-007"))]
#[test]
fn upload_paths_are_rendered_from_typed_values() {
    let id = UploadId::parse("01890a5d-ac96-774b-bcce-b302099a8057").unwrap();
    assert_eq!(
        upload_path(&id),
        "uploads/01890a5d-ac96-774b-bcce-b302099a8057"
    );
    assert_eq!(
        nar_path(&id),
        "uploads/01890a5d-ac96-774b-bcce-b302099a8057/nar"
    );
}
