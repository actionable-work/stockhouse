#![allow(dead_code)]
use stockhouse_proto::{Sha256, StorePath, UploadId};
use stockhouse_wire::UploadDeclaration;

pub const HASH: &str = "00000000000000000000000000000000";
pub const MACHINE: &str = "0123456789abcdef0123456789abcdef";
pub const ID: &str = "0198c750-1234-7000-8000-000000000000";

pub fn upload_id(n: usize) -> UploadId {
    UploadId::parse(&format!("0198c750-1234-7000-8000-{n:012x}")).unwrap()
}

pub fn path(name: &str) -> StorePath {
    StorePath::parse(&format!("/nix/store/{HASH}-{name}")).unwrap()
}

pub fn declaration(
    name: &str,
    references: Vec<StorePath>,
    ca: Option<String>,
) -> UploadDeclaration {
    UploadDeclaration::new(path(name), Sha256::digest(b"nar"), 3, references, None, ca).unwrap()
}
