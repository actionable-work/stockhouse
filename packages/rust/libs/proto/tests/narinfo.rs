mod common;
use common::*;
use stockhouse_proto::{Compression, Deriver, NarInfo, NarInfoError, Sha256};

fn parse(text: &str) -> Result<NarInfo, NarInfoError> {
    NarInfo::parse(text.as_bytes())
}

fn replace_line(key: &str, line: &str) -> String {
    let mut out: Vec<String> = unsigned_text().lines().filter(|l| !l.starts_with(&format!("{key}:"))).map(str::to_owned).collect();
    if !line.is_empty() {
        out.push(line.to_owned());
    }
    out.join("\n") + "\n"
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NARINFO-001"))]
#[test]
fn valid_narinfo_parses() {
    let info = signed("fixture-1", 1);
    let text = info.render() + "System: x86_64-linux\nCA: fixed:r:sha256:abc\n";
    let info = parse(&text).unwrap();
    assert_eq!(info.store_path.as_str(), format!("/nix/store/{HASH_A}-hello-1.0"));
    assert_eq!(info.url, format!("nar/{HASH_A}.nar.xz"));
    assert_eq!(info.compression, Compression::Xz);
    assert_eq!(info.file.unwrap().size, 120);
    assert_eq!(info.nar_hash, Sha256::digest(b""));
    assert_eq!(info.nar_size, 400);
    let refs: Vec<_> = info.references.iter().map(|p| p.basename().to_owned()).collect();
    assert_eq!(refs, [format!("{HASH_C}-glibc"), format!("{HASH_B}-ncurses")]);
    assert!(matches!(info.deriver, Some(Deriver::Path(_))));
    assert_eq!(info.signatures.len(), 1);
    assert_eq!(info.signatures[0].name(), "fixture-1");
    assert_eq!(info.system.as_deref(), Some("x86_64-linux"));
    assert_eq!(info.ca.as_deref(), Some("fixed:r:sha256:abc"));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NARINFO-002"))]
#[test]
fn missing_compression_defaults_to_bzip2() {
    assert_eq!(parse(&replace_line("Compression", "")).unwrap().compression, Compression::Bzip2);
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NARINFO-003"))]
#[test]
fn required_fields_must_be_present() {
    for key in ["StorePath", "URL", "NarHash", "NarSize"] {
        assert_eq!(parse(&replace_line(key, "")), Err(NarInfoError::MissingField(key)), "{key}");
    }
    assert_eq!(parse("Compression: xz\n"), Err(NarInfoError::MissingField("StorePath")));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NARINFO-004"))]
#[test]
fn unknown_fields_are_rejected() {
    let text = unsigned_text() + "Tenant: secret-org\n";
    assert_eq!(parse(&text), Err(NarInfoError::UnknownField("Tenant".to_owned())));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NARINFO-005"))]
#[test]
fn single_valued_fields_cannot_repeat() {
    let text = unsigned_text() + "NarSize: 400\n";
    assert_eq!(parse(&text), Err(NarInfoError::DuplicateField("NarSize")));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NARINFO-006"))]
#[test]
fn malformed_lines_are_rejected() {
    let lines = unsigned_text().lines().count();
    assert_eq!(parse(&(unsigned_text() + "garbage\n")), Err(NarInfoError::MalformedLine { line: lines + 1 }));
    assert_eq!(parse(&(unsigned_text() + ": value\n")), Err(NarInfoError::MalformedLine { line: lines + 1 }));
    assert_eq!(parse(&(unsigned_text() + "System:nospace\n")), Err(NarInfoError::MalformedLine { line: lines + 1 }));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NARINFO-007"))]
#[test]
fn invalid_values_are_rejected() {
    let dup_ref = format!("References: {HASH_B}-ncurses {HASH_B}-ncurses");
    let cases = [
        ("StorePath", "StorePath: /tmp/x".to_owned()),
        ("NarHash", "NarHash: md5:abc".to_owned()),
        ("NarSize", "NarSize: 0".to_owned()),
        ("NarSize", "NarSize: -1".to_owned()),
        ("Compression", "Compression: lzma".to_owned()),
        ("References", "References: not-a-path".to_owned()),
        ("References", dup_ref),
        ("Deriver", format!("Deriver: {HASH_B}-hello")),
    ];
    for (key, line) in cases {
        assert_eq!(parse(&replace_line(key, &line)), Err(NarInfoError::InvalidField(key)), "{line}");
    }
    assert_eq!(parse(&(unsigned_text() + "Sig: nocolon\n")), Err(NarInfoError::InvalidField("Sig")));
    assert_eq!(parse(&(unsigned_text() + "System: a\u{7}b\n")), Err(NarInfoError::InvalidField("System")));
    assert!(parse(&replace_line("Deriver", "Deriver: unknown-deriver")).is_ok());
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NARINFO-008"))]
#[test]
fn unsafe_urls_are_rejected() {
    for url in ["/abs/x.nar", "https://evil.example.invalid/x", "nar/../x", "nar//x", "nar/./x", "nar/x?y", "nar/x y", ""] {
        let line = format!("URL: {url}");
        assert_eq!(parse(&replace_line("URL", &line)), Err(NarInfoError::InvalidField("URL")), "{url:?}");
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NARINFO-009"))]
#[test]
fn file_hash_and_size_go_together() {
    assert_eq!(parse(&replace_line("FileSize", "")), Err(NarInfoError::InvalidField("FileHash")));
    assert_eq!(parse(&replace_line("FileHash", "")), Err(NarInfoError::InvalidField("FileSize")));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NARINFO-010"))]
#[test]
fn oversized_or_non_utf8_is_rejected() {
    let big = vec![b'a'; 1024 * 1024 + 1];
    assert_eq!(NarInfo::parse(&big), Err(NarInfoError::TooLarge));
    assert_eq!(NarInfo::parse(b"StorePath: \xff\n"), Err(NarInfoError::NotUtf8));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NARINFO-011"))]
#[test]
fn render_round_trips() {
    let mut info = signed("fixture-1", 1);
    info.signatures.push(sign(&info, "fixture-2", 2));
    info.system = Some("x86_64-linux".to_owned());
    assert_eq!(parse(&info.render()).unwrap(), info);
    let bare = parse(&replace_line("References", "")).unwrap();
    assert!(bare.render().contains("References: \n"));
    assert_eq!(parse(&bare.render()).unwrap(), bare);
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-NARINFO-012"))]
#[test]
fn url_replacement_changes_only_url() {
    let info = signed("fixture-1", 1);
    let moved = info.with_url(&format!("nar/private/{HASH_A}/nar/{HASH_A}.nar.xz")).unwrap();
    assert_eq!(moved.fingerprint(), info.fingerprint());
    assert_eq!(moved.signatures, info.signatures);
    assert_eq!(moved.url, format!("nar/private/{HASH_A}/nar/{HASH_A}.nar.xz"));
    assert_eq!(NarInfo { url: info.url.clone(), ..moved.clone() }, info);
    assert_eq!(info.with_url("../x"), Err(NarInfoError::InvalidField("URL")));
}
