mod common;

use common::{HASH, ID, MACHINE, declaration, path, upload_id};
use stockhouse_wire::{LeaseSlot, MachineId};
use stockhouse_wire_v2::*;

fn valid_declare() -> Vec<u8> {
    let mut batch = DeclareBatch::new();
    batch
        .push(upload_id(0), declaration("one", vec![], None))
        .unwrap();
    batch.render().unwrap()
}

fn valid_lease() -> Vec<u8> {
    LeaseBatch::new(vec![LeaseEntry {
        machine: MachineId::parse(MACHINE).unwrap(),
        slot: LeaseSlot::new(1).unwrap(),
        store_path: path("system"),
    }])
    .unwrap()
    .render()
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-005"))]
#[test]
fn size_is_checked_before_json() {
    assert_eq!(
        DeclareBatch::parse(&vec![0; MAX_DECLARE_BODY + 1]),
        Err(BatchError::TooLarge)
    );
    let oversized = vec![0; MAX_SMALL_BATCH_BODY + 1];
    assert_eq!(IdBatch::parse(&oversized), Err(BatchError::TooLarge));
    assert_eq!(
        LeaseBatch::parse(&oversized),
        Err(LeaseBatchError::TooLarge)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-006"))]
#[test]
fn objects_have_closed_keys_and_require_even_nullable_fields() {
    let declare = String::from_utf8(valid_declare()).unwrap();
    for body in [
        declare.replace(",\"ca\":null", ""),
        declare.replace("\"deriver\":null", "\"deriver\":null,\"deriver\":null"),
        declare.replace("\"nar_size\":3", "\"nar_size\":\"3\""),
        declare.replace("\"ca\":null", "\"other\":null"),
        "[2,[]]".into(),
        r#"{"version":2,"uploads":[["something"]]}"#.into(),
    ] {
        assert_eq!(
            DeclareBatch::parse(body.as_bytes()),
            Err(BatchError::Malformed)
        );
    }

    for body in [
        br#"{"version":2,"uploads":[7]}"#.as_slice(),
        br#"{"version":2,"version":2,"uploads":[]}"#,
        br#"{"version":2,"other":[]}"#,
        br#"[2,[]]"#,
    ] {
        assert_eq!(IdBatch::parse(body), Err(BatchError::Malformed));
    }
    for body in [
        br#"{"version":2,"leases":[["x",1,"y"]]}"#.as_slice(),
        br#"{"version":2,"leases":[{"machine":"x","slot":1,"store_path":"y","slot":1}]}"#,
        br#"{"version":2,"leases":[{"machine":"x","slot":"1","store_path":"y"}]}"#,
        br#"{"version":2,"leases":[{"machine":"x","slot":1}]}"#,
    ] {
        assert_eq!(LeaseBatch::parse(body), Err(LeaseBatchError::Malformed));
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-007"))]
#[test]
fn version_precedes_count() {
    let uploads = br#"{"version":1,"uploads":[]}"#;
    assert_eq!(
        DeclareBatch::parse(uploads),
        Err(BatchError::UnsupportedVersion)
    );
    assert_eq!(IdBatch::parse(uploads), Err(BatchError::UnsupportedVersion));
    assert_eq!(
        LeaseBatch::parse(br#"{"version":1,"leases":[]}"#),
        Err(LeaseBatchError::UnsupportedVersion)
    );
}

fn repeat_body(key: &str, entry: &str, count: usize) -> String {
    let entries = std::iter::repeat_n(entry, count)
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"version\":2,\"{key}\":[{entries}]}}")
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-008"))]
#[test]
fn each_batch_checks_count_before_values_and_duplicates() {
    for count in [0, 1001] {
        let ids = repeat_body("uploads", r#""invalid-id""#, count);
        assert_eq!(IdBatch::parse(ids.as_bytes()), Err(BatchError::Count));
        let valid = String::from_utf8(valid_declare()).unwrap();
        let raw = valid.split_once("[").unwrap().1.strip_suffix("]}").unwrap();
        assert_eq!(
            DeclareBatch::parse(repeat_body("uploads", raw, count).as_bytes()),
            Err(BatchError::Count)
        );
    }
    for count in [0, 65] {
        let entry = r#"{"machine":"invalid","slot":0,"store_path":"invalid"}"#;
        assert_eq!(
            LeaseBatch::parse(repeat_body("leases", entry, count).as_bytes()),
            Err(LeaseBatchError::Count)
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-009"))]
#[test]
fn invalid_values_name_the_field_and_entry() {
    let valid = String::from_utf8(valid_declare()).unwrap();
    let bad_cases = [
        (valid.replace(ID, "invalid"), "id"),
        (
            valid.replace(&format!("/nix/store/{HASH}-one"), "one"),
            "store_path",
        ),
        (
            valid.replace("\"nar_size\":3", "\"nar_size\":0"),
            "nar_size",
        ),
        (
            valid.replace("\"references\":[]", "\"references\":[\"invalid\"]"),
            "references",
        ),
        (
            valid.replace(
                "\"deriver\":null",
                &format!("\"deriver\":\"{HASH}-not-a-drv\""),
            ),
            "deriver",
        ),
        (valid.replace("\"ca\":null", "\"ca\":\"\\n\""), "ca"),
    ];
    for (body, field) in bad_cases {
        assert_eq!(
            DeclareBatch::parse(body.as_bytes()),
            Err(BatchError::InvalidField { index: 0, field })
        );
    }
    let ids = format!(r#"{{"version":2,"uploads":["{ID}","invalid"]}}"#);
    assert_eq!(
        IdBatch::parse(ids.as_bytes()),
        Err(BatchError::InvalidField {
            index: 1,
            field: "uploads"
        })
    );
    let valid = String::from_utf8(valid_lease()).unwrap();
    for (body, field) in [
        (valid.replace(MACHINE, "invalid"), "machine"),
        (valid.replace("\"slot\":1", "\"slot\":256"), "slot"),
        (
            valid.replace(&format!("/nix/store/{HASH}-system"), "system"),
            "store_path",
        ),
    ] {
        assert_eq!(
            LeaseBatch::parse(body.as_bytes()),
            Err(LeaseBatchError::InvalidField { index: 0, field })
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-010"))]
#[test]
fn duplicate_keys_are_refused_after_value_validation() {
    let body = String::from_utf8(valid_declare()).unwrap();
    let first = body.split_once("[").unwrap().1.strip_suffix("]}").unwrap();
    for second in [
        first.replace(&format!("{HASH}-one"), &format!("{HASH}-other")),
        first.replace(ID, &upload_id(1).to_string()),
    ] {
        let body = format!(r#"{{"version":2,"uploads":[{first},{second}]}}"#);
        assert_eq!(
            DeclareBatch::parse(body.as_bytes()),
            Err(BatchError::Duplicate { index: 1 })
        );
    }
    let ids = format!(r#"{{"version":2,"uploads":["{ID}","{ID}"]}}"#);
    assert_eq!(
        IdBatch::parse(ids.as_bytes()),
        Err(BatchError::Duplicate { index: 1 })
    );
    let bad = ids.trim_end_matches("]}").to_owned() + ",\"invalid\"]}";
    assert_eq!(
        IdBatch::parse(bad.as_bytes()),
        Err(BatchError::InvalidField {
            index: 2,
            field: "uploads"
        })
    );
    let lease = String::from_utf8(valid_lease()).unwrap();
    let entry = lease.split_once("[").unwrap().1.strip_suffix("]}").unwrap();
    let second = entry.replace(&format!("{HASH}-system"), &format!("{HASH}-other"));
    let twice = format!(r#"{{"version":2,"leases":[{entry},{second}]}}"#);
    assert_eq!(
        LeaseBatch::parse(twice.as_bytes()),
        Err(LeaseBatchError::Duplicate { index: 1 })
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-BATCH-011"))]
#[test]
fn ids_preserve_order_and_fit_the_maximum() {
    let batch = IdBatch::new(vec![upload_id(1), upload_id(0)]).unwrap();
    assert_eq!(
        batch.render(),
        format!(r#"{{"version":2,"uploads":["{}","{ID}"]}}"#, upload_id(1)).as_bytes()
    );
    let full = IdBatch::new((0..1000).rev().map(upload_id).collect()).unwrap();
    let body = full.render();
    assert!(body.len() <= MAX_SMALL_BATCH_BODY);
    assert_eq!(IdBatch::parse(&body).unwrap(), full);
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-LEASE-008"))]
#[test]
fn leases_render_in_order_and_fit_the_maximum() {
    let expected = format!(
        r#"{{"version":2,"leases":[{{"machine":"{MACHINE}","slot":1,"store_path":"/nix/store/{HASH}-system"}}]}}"#
    );
    assert_eq!(valid_lease(), expected.as_bytes());
    let full = LeaseBatch::new(
        (0..64)
            .rev()
            .map(|n| LeaseEntry {
                machine: MachineId::from_bytes([n; 16]),
                slot: LeaseSlot::new(1).unwrap(),
                store_path: path("system"),
            })
            .collect(),
    )
    .unwrap();
    let body = full.render();
    assert!(body.len() <= MAX_SMALL_BATCH_BODY);
    assert_eq!(LeaseBatch::parse(&body).unwrap(), full);
}
