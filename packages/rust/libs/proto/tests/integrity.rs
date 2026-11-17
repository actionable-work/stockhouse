mod common;
use common::*;
use stockhouse_proto::{Compression, ContentVerifier, IntegrityError, NarInfo, ServedIntegrity, Sha256};

fn expect(data: &[u8]) -> ServedIntegrity {
    ServedIntegrity { size: data.len() as u64, hash: Sha256::digest(data) }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-INTEGRITY-001"))]
#[test]
fn matching_content_verifies_in_any_chunking() {
    let data = b"stockhouse nar bytes";
    for size in [1, 3, data.len()] {
        let mut v = ContentVerifier::new(expect(data));
        data.chunks(size).for_each(|c| v.update(c).unwrap());
        assert_eq!(v.finish(), Ok(()));
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-INTEGRITY-002"))]
#[test]
fn excess_content_fails_immediately() {
    let mut v = ContentVerifier::new(expect(b"abc"));
    v.update(b"ab").unwrap();
    assert_eq!(v.update(b"cd"), Err(IntegrityError::SizeExceeded { expected: 3 }));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-INTEGRITY-003"))]
#[test]
fn short_or_altered_content_fails() {
    let mut short = ContentVerifier::new(expect(b"abc"));
    short.update(b"ab").unwrap();
    assert_eq!(short.finish(), Err(IntegrityError::SizeMismatch { expected: 3, actual: 2 }));
    let mut altered = ContentVerifier::new(expect(b"abc"));
    altered.update(b"abd").unwrap();
    assert_eq!(altered.finish(), Err(IntegrityError::HashMismatch));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-INTEGRITY-004"))]
#[test]
fn served_integrity_prefers_file_hash() {
    let info = NarInfo::parse(unsigned_text().as_bytes()).unwrap();
    let file = info.file.unwrap();
    assert_eq!(info.served_integrity(), Some(ServedIntegrity { size: file.size, hash: file.hash }));
    let none = NarInfo { file: None, compression: Compression::None, ..info };
    assert_eq!(none.served_integrity(), Some(ServedIntegrity { size: none.nar_size, hash: none.nar_hash }));
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-INTEGRITY-005"))]
#[test]
fn compressed_without_file_hash_has_no_check() {
    let info = NarInfo::parse(unsigned_text().as_bytes()).unwrap();
    let unchecked = NarInfo { file: None, compression: Compression::Zstd, ..info };
    assert_eq!(unchecked.served_integrity(), None);
}
