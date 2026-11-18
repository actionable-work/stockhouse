use std::path::PathBuf;
use stockhouse_ingest::{BackendConfig, ScannerConfig, UploaderConfig, UploaderConfigError};
use stockhouse_proto::UnsupportedVersion;

const VALID: &str = r#"version = 1
queue_dir = "/var/lib/stockhouse/queue"
max_pending = 10000
max_attempts = 20
batch_size = 64
backoff_base_secs = 5
backoff_max_secs = 3600
poll_interval_secs = 5
rescan_interval_secs = 3600

[backend]
kind = "nix-copy"
destination = "file:///var/lib/stockhouse/cache"

[scanner]
kind = "nix-path-info"
nix = "/run/current-system/sw/bin/nix"
"#;

#[cfg_attr(any(), bloomery("CLIENT-INGEST-CONFIG-001"))]
#[test]
fn valid_configuration_loads() {
    let config = UploaderConfig::parse(VALID).unwrap();
    assert_eq!(config.queue_dir, PathBuf::from("/var/lib/stockhouse/queue"));
    assert_eq!(
        (config.max_pending, config.max_attempts, config.batch_size),
        (10000, 20, 64)
    );
    assert_eq!(
        (
            config.backoff_base_secs,
            config.backoff_max_secs,
            config.poll_interval_secs,
            config.rescan_interval_secs
        ),
        (5, 3600, 5, 3600)
    );
    assert_eq!(
        config.backend,
        BackendConfig::NixCopy {
            destination: "file:///var/lib/stockhouse/cache".into(),
            nix: "nix".into()
        }
    );
    assert_eq!(
        config.scanner,
        ScannerConfig::NixPathInfo {
            nix: "/run/current-system/sw/bin/nix".into()
        }
    );
    let deferred = VALID.replace(
        "kind = \"nix-copy\"\ndestination = \"file:///var/lib/stockhouse/cache\"",
        "kind = \"deferred\"",
    );
    assert_eq!(
        UploaderConfig::parse(&deferred).unwrap().backend,
        BackendConfig::Deferred
    );
    let none = VALID.replace(
        "kind = \"nix-path-info\"\nnix = \"/run/current-system/sw/bin/nix\"",
        "kind = \"none\"",
    );
    assert_eq!(
        UploaderConfig::parse(&none).unwrap().scanner,
        ScannerConfig::None
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-CONFIG-002"))]
#[test]
fn bad_versions_and_fields_are_rejected() {
    assert_eq!(
        UploaderConfig::parse(&VALID.replace("version = 1", "version = 3")),
        Err(UploaderConfigError::UnsupportedVersion(
            UnsupportedVersion {
                found: 3,
                supported: 1
            }
        ))
    );
    let extra = VALID.replace("batch_size = 64", "batch_size = 64\ntoken = \"x\"");
    assert!(matches!(
        UploaderConfig::parse(&extra),
        Err(UploaderConfigError::Parse(_))
    ));
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-CONFIG-003"))]
#[test]
fn limits_must_be_usable() {
    assert_eq!(
        UploaderConfig::parse(&VALID.replace("\"/var/lib/stockhouse/queue\"", "\"queue\"")),
        Err(UploaderConfigError::RelativeQueueDir)
    );
    for (from, to, field) in [
        ("max_pending = 10000", "max_pending = 0", "max_pending"),
        ("max_attempts = 20", "max_attempts = 0", "max_attempts"),
        ("batch_size = 64", "batch_size = 0", "batch_size"),
        (
            "backoff_base_secs = 5",
            "backoff_base_secs = 4000",
            "backoff_max_secs",
        ),
        (
            "poll_interval_secs = 5",
            "poll_interval_secs = 0",
            "poll_interval_secs",
        ),
    ] {
        assert_eq!(
            UploaderConfig::parse(&VALID.replace(from, to)),
            Err(UploaderConfigError::InvalidLimit(field)),
            "{field}"
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-CONFIG-004"))]
#[test]
fn nix_copy_requires_a_local_file_destination() {
    for dest in [
        "https://cache.example.invalid/",
        "file://relative/x",
        "/var/lib/x",
    ] {
        let text = VALID.replace("file:///var/lib/stockhouse/cache", dest);
        assert_eq!(
            UploaderConfig::parse(&text),
            Err(UploaderConfigError::UnsupportedDestination),
            "{dest}"
        );
    }
}

fn server(extra: &str) -> String {
    let head = VALID.split("[backend]").next().unwrap();
    format!(
        "{head}[backend]\nkind = \"server\"\nbase_url = \"https://cache.example.invalid/t/1/\"\n{extra}\n[scanner]\nkind = \"none\"\n"
    )
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-CONFIG-005"))]
#[test]
fn server_backend_configuration_loads() {
    use stockhouse_credentials::CredentialSpec;
    let config =
        UploaderConfig::parse(&server("[backend.credential]\nsystemd = \"private\"\n")).unwrap();
    let BackendConfig::Server(backend) = config.backend else {
        panic!("not a server backend");
    };
    assert_eq!(backend.base_url, "https://cache.example.invalid/t/1/");
    assert_eq!(
        backend.credential,
        CredentialSpec::Systemd("private".into())
    );
    assert_eq!(backend.nix, PathBuf::from("nix"));
    assert_eq!(
        (backend.request_timeout_secs, backend.max_closure_paths),
        (60, 50_000)
    );
    let custom = server(
        "nix = \"/bin/nix\"\nrequest_timeout_secs = 5\nmax_closure_paths = 7\n[backend.credential]\nfile = \"/run/keys/t\"\n",
    );
    let BackendConfig::Server(backend) = UploaderConfig::parse(&custom).unwrap().backend else {
        panic!("not a server backend");
    };
    assert_eq!(
        backend.credential,
        CredentialSpec::File("/run/keys/t".into())
    );
    assert_eq!(backend.nix, PathBuf::from("/bin/nix"));
    assert_eq!(
        (backend.request_timeout_secs, backend.max_closure_paths),
        (5, 7)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-CONFIG-006"))]
#[test]
fn server_backend_configuration_rejects_bad_fields_and_limits() {
    let cred = "[backend.credential]\nsystemd = \"private\"\n";
    for bad in [
        server(&format!("extra = 1\n{cred}")),
        server(""),
        server("[backend.credential]\ntoken = \"x\"\n"),
    ] {
        assert!(
            matches!(
                UploaderConfig::parse(&bad),
                Err(UploaderConfigError::Parse(_))
            ),
            "{bad}"
        );
    }
    for (line, field) in [
        ("request_timeout_secs = 0\n", "request_timeout_secs"),
        ("request_timeout_secs = 301\n", "request_timeout_secs"),
        ("max_closure_paths = 0\n", "max_closure_paths"),
    ] {
        assert_eq!(
            UploaderConfig::parse(&server(&format!("{line}{cred}"))),
            Err(UploaderConfigError::InvalidLimit(field))
        );
    }
}
