//! The Stockhouse client-to-server upload and lease protocol v1.
//!
//! Pure codecs over `stockhouse-proto` types: upload declarations and
//! request paths, strict reply classification, machine identities, lease
//! slots and lease requests. It performs no I/O; HTTP, credentials and
//! retries belong to `stockhouse-upload`.

mod lease;
mod reply;
mod upload;

pub use lease::{
    LeaseRefusal, LeaseReply, LeaseRequest, LeaseRequestError, LeaseSlot, LeaseSlotError,
    MachineId, MachineIdError, lease_path, parse_lease_reply,
};
pub use reply::{ReplyError, RequestKind, UploadRefusal, UploadReply, UploadState, parse_reply};
pub use upload::{
    DeclarationError, MAX_BODY, MAX_REFERENCES, UploadDeclaration, nar_path, upload_path,
};
