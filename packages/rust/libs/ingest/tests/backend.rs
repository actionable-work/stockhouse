mod common;
use common::*;
use stockhouse_ingest::{
    DeferredUploader, NixCopyUploader, RetryKind, UploadJob, UploadOutcome, Uploader,
};
use stockhouse_proto::UploadId;

fn job() -> UploadJob {
    UploadJob {
        id: UploadId::generate(),
        store_path: path("hello"),
        attempts: 0,
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BACKEND-001"))]
#[test]
fn default_backend_defers_to_the_server_slice() {
    assert_eq!(
        DeferredUploader.upload(&job()),
        UploadOutcome::Deferred("server upload requires P2.08".to_owned())
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BACKEND-002"))]
#[test]
fn nix_copy_success_uploads() {
    let nix = fake_nix(b"", b"", 0);
    assert_eq!(
        NixCopyUploader::new(nix, "file:///var/cache/x".into()).upload(&job()),
        UploadOutcome::Uploaded
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BACKEND-003"))]
#[test]
fn nix_copy_failure_retries_with_bounded_message() {
    let nix = fake_nix(b"", &vec![b'e'; 2000], 1);
    let UploadOutcome::Retry(RetryKind::Transient, message) =
        NixCopyUploader::new(nix, "file:///x".into()).upload(&job())
    else {
        panic!("expected a transient retry")
    };
    assert_eq!(message.len(), 512);
    let missing = NixCopyUploader::new(temp_dir().join("absent-nix"), "file:///x".into());
    assert!(
        matches!(missing.upload(&job()), UploadOutcome::Retry(RetryKind::Transient, m) if m.len() <= 512)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-BACKEND-004"))]
#[test]
fn nix_copy_passes_only_destination_and_path() {
    let nix = fake_nix(b"", b"", 0);
    let job = job();
    NixCopyUploader::new(nix.clone(), "file:///var/cache/x".into()).upload(&job);
    let expected = [
        "--extra-experimental-features",
        "nix-command",
        "copy",
        "--to",
        "file:///var/cache/x",
        job.store_path.as_str(),
    ];
    assert_eq!(recorded_args(&nix), expected);
}
