mod common;

use common::{HASH, ID, declaration, path, upload_id};
use stockhouse_wire::MAX_REFERENCES;
use stockhouse_wire_v2::*;

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-002"))]
#[test]
fn declare_uses_the_canonical_entry_and_field_order() {
    let decl = declaration(
        "one",
        vec![path("second"), path("first")],
        Some("quote\"".into()),
    );
    let expected = format!(
        "{{\"version\":2,\"uploads\":[{{\"id\":\"{ID}\",\"store_path\":\"/nix/store/{HASH}-one\",\"nar_hash\":\"{}\",\"nar_size\":3,\"references\":[\"{HASH}-second\",\"{HASH}-first\"],\"deriver\":null,\"ca\":\"quote\\\"\"}}]}}",
        decl.nar_hash(),
    );
    let mut builder = DeclareBatch::new();
    builder.push(upload_id(0), decl).unwrap();
    assert_eq!(builder.render().unwrap(), expected.as_bytes());
    assert_eq!(DeclareBatch::parse(expected.as_bytes()).unwrap(), builder);
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-004"))]
#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-023"))]
#[test]
fn a_full_batch_still_rejects_duplicate_entries_first() {
    let mut builder = DeclareBatch::new();
    for n in 0..MAX_BATCH_ENTRIES {
        builder
            .push(
                upload_id(n),
                declaration(&format!("path-{n}"), vec![], None),
            )
            .unwrap();
    }
    let original = builder.render().unwrap();
    assert_eq!(DeclareBatch::parse(&original).unwrap(), builder);
    assert_eq!(
        builder.push(upload_id(0), declaration("new-path", vec![], None)),
        Err(DeclarePushError::Duplicate {
            index: MAX_BATCH_ENTRIES
        })
    );
    assert_eq!(
        builder.push(upload_id(1001), declaration("path-0", vec![], None)),
        Err(DeclarePushError::Duplicate {
            index: MAX_BATCH_ENTRIES
        })
    );
    assert_eq!(
        builder.push(upload_id(1001), declaration("new-path", vec![], None)),
        Err(DeclarePushError::BatchFull)
    );
    assert_eq!(builder.render().unwrap(), original);
}

fn refs(count: usize) -> Vec<stockhouse_proto::StorePath> {
    let prefix = "a".repeat(190);
    (0..count)
        .map(|n| path(&format!("{prefix}{n:04}")))
        .collect()
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-003"))]
#[test]
fn byte_limit_allows_an_exact_fit_and_rejects_one_extra_byte() {
    let mut builder = DeclareBatch::new();
    for n in 0..3 {
        builder
            .push(
                upload_id(n),
                declaration(&format!("large-{n}"), refs(MAX_REFERENCES), None),
            )
            .unwrap();
    }

    let mut base = DeclareBatch::new();
    base.push(
        upload_id(3),
        declaration("final", vec![], Some(String::new())),
    )
    .unwrap();
    let envelope = b"{\"version\":2,\"uploads\":[]}".len();
    let used = builder.render().unwrap().len() + base.render().unwrap().len() - envelope + 1;
    let item_width = refs(1)[0].basename().len() + 3;
    let (count, ca_len) = (
        (MAX_DECLARE_BODY - used + 1) / item_width,
        (MAX_DECLARE_BODY - used + 1) % item_width,
    );

    let original_len = builder.render().unwrap().len();
    let oversized = declaration("final", refs(count), Some("x".repeat(ca_len + 1)));
    assert_eq!(
        builder.push(upload_id(3), oversized),
        Err(DeclarePushError::BatchFull)
    );
    assert_eq!(builder.render().unwrap().len(), original_len);

    let exact = declaration("final", refs(count), Some("x".repeat(ca_len)));
    builder.push(upload_id(3), exact).unwrap();
    let rendered = builder.render().unwrap();
    assert_eq!(rendered.len(), MAX_DECLARE_BODY);
    assert_eq!(DeclareBatch::parse(&rendered).unwrap(), builder);
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-024"))]
#[test]
fn an_empty_declare_batch_cannot_render() {
    assert_eq!(DeclareBatch::new().render(), Err(BatchError::Count));
}
