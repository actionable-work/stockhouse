//! `shared` mode: request log and per-hash replies.
mod common;

use common::{call, start, temp_dir};

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-013"))]
#[tokio::test(flavor = "multi_thread")]
async fn shared_mode_logs_and_answers_per_hash() {
    let log = temp_dir().join("shared-requests.log");
    let addr = start(&[
        "shared",
        "--listen",
        "127.0.0.1:0",
        "--log",
        log.to_str().unwrap(),
        "--deny",
        "d0",
        "--fail",
        "f0",
    ])
    .await;
    let info = call(
        addr,
        "GET",
        "/nix-cache-info",
        &[("Authorization", "Bearer s3")],
        b"",
    )
    .await
    .unwrap();
    assert_eq!(
        (info.status, info.body.as_str()),
        (200, "StoreDir: /nix/store\n")
    );
    for (path, status) in [
        ("/d0.narinfo", 403),
        ("/f0.narinfo", 500),
        ("/other.narinfo", 404),
        ("/d0.narinfo?x=1", 404),
    ] {
        assert_eq!(
            call(addr, "GET", path, &[], b"").await.unwrap().status,
            status,
            "{path}"
        );
    }
    assert_eq!(
        call(addr, "POST", "/nix-cache-info", &[], b"")
            .await
            .unwrap()
            .status,
        501
    );
    let lines = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        lines,
        "/nix-cache-info Bearer s3\n/d0.narinfo -\n/f0.narinfo -\n/other.narinfo -\n/d0.narinfo?x=1 -\n"
    );
}
