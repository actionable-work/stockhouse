mod common;
use common::*;
use std::time::{Duration, Instant};
use stockhouse_credentials::StaticCredential;
use stockhouse_ingest::{RetryKind, UploadOutcome, Uploader};
use stockhouse_upload::{
    EndpointError, ServerEndpoint, ServerUploader, check_base_url, nar_timeout,
};

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-001"))]
#[test]
fn server_endpoints_must_be_absolute_and_clean() {
    for bad in [
        "not a url",
        "/t/1/",
        "ftp://cache.example.invalid/t/1/",
        "file:///var/cache/",
        "https://user:pw@cache.example.invalid/t/1/",
        "https://cache.example.invalid/t/1/?q=1",
        "https://cache.example.invalid/t/1/#f",
        "https://cache.example.invalid/t/1",
    ] {
        assert_eq!(
            check_base_url(bad).err(),
            Some(EndpointError::InvalidUrl),
            "{bad}"
        );
    }
    for good in [
        "https://cache.example.invalid/t/1/",
        "http://127.0.0.1:1/t/1/",
        "http://127.3.4.5/",
        "http://[::1]:1/",
    ] {
        assert!(check_base_url(good).is_ok(), "{good}");
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-002"))]
#[test]
fn cleartext_is_allowed_only_to_loopback_ip_literals() {
    for bad in [
        "http://cache.example.invalid/t/1/",
        "http://localhost/t/1/",
        "http://10.0.0.1/",
        "http://[::2]/",
    ] {
        assert_eq!(
            check_base_url(bad).err(),
            Some(EndpointError::Cleartext),
            "{bad}"
        );
        let source = std::sync::Arc::new(StaticCredential::new(b"t"));
        assert_eq!(
            ServerEndpoint::new(bad, source, 5).err(),
            Some(EndpointError::Cleartext)
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-032"))]
#[test]
fn credentials_must_be_issued_for_the_base_url() {
    let e = endpoint("http://127.0.0.1:8486/t/1/", 5);
    assert_eq!(e.check_audience("http://127.0.0.1:8486/t/1/"), Ok(()));
    // Through a `server` credential: the sign-in configuration's private tier.
    let dir = temp_dir();
    let base = "http://127.0.0.1:8486/t/1/";
    let same = stockhouse_credentials::CredentialSpec::Server(auth_config(&dir, base));
    assert!(ServerEndpoint::from_spec(base, &same, 5).is_ok());
    let other_dir = temp_dir();
    let elsewhere = stockhouse_credentials::CredentialSpec::Server(auth_config(
        &other_dir,
        "http://127.0.0.1:8486/t/2/",
    ));
    assert_eq!(
        ServerEndpoint::from_spec(base, &elsewhere, 5).err(),
        Some(EndpointError::AudienceMismatch)
    );
    let missing = stockhouse_credentials::CredentialSpec::Server(dir.join("absent.toml"));
    assert!(matches!(
        ServerEndpoint::from_spec(base, &missing, 5).err(),
        Some(EndpointError::Credential(_))
    ));
    for other in [
        "http://127.0.0.1:8486/t/2/",
        "http://127.0.0.1:8487/t/1/",
        "x",
    ] {
        assert_eq!(
            e.check_audience(other),
            Err(EndpointError::AudienceMismatch),
            "{other}"
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-033"))]
#[test]
fn each_request_has_a_bounded_total_time() {
    let base = Duration::from_secs(60);
    assert_eq!(nar_timeout(base, 0), base);
    assert_eq!(nar_timeout(base, 1), base + Duration::from_secs(1));
    assert_eq!(nar_timeout(base, 256 * 1024), base + Duration::from_secs(1));
    assert_eq!(
        nar_timeout(base, 256 * 1024 + 1),
        base + Duration::from_secs(2)
    );
    let mut store = Store::new();
    let root = store.add("slow", &[]);
    let server = FakeServer::start(|req| {
        req.body.as_ref()?;
        Some(Action::Sleep(
            Duration::from_secs(5),
            200,
            "{\"state\":\"open\"}".into(),
        ))
    });
    let queue = store.queue();
    let up = ServerUploader::new(
        endpoint(&server.base(), 1),
        store.nix.clone(),
        10,
        queue.clone(),
    );
    let started = Instant::now();
    let outcome = up.upload(&job(&queue, &root));
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    assert!(
        matches!(outcome, UploadOutcome::Retry(RetryKind::Transient, _)),
        "{outcome:?}"
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-003"))]
#[test]
fn a_public_request_cannot_escape_the_private_tier() {
    let endpoint = endpoint("http://127.0.0.1:1/t/1/", 1);
    let secret = endpoint.credential().unwrap();
    for path in [
        "../2/protocol",
        "https://example.invalid/protocol",
        "..%2f2/protocol",
    ] {
        let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = endpoint.request(reqwest::Method::GET, path, &secret, endpoint.timeout());
        }));
        assert!(rejected.is_err(), "{path}");
    }
}
