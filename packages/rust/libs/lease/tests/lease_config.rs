use std::path::PathBuf;
use stockhouse_credentials::CredentialSpec;
use stockhouse_lease::{LeaseConfig, LeaseConfigError};
use stockhouse_upload::EndpointError;
use stockhouse_wire::LeaseSlot;

const VALID: &str = r#"version = 1
base_url = "https://cache.example.invalid/t/1/"
machine_id_file = "/var/lib/stockhouse/machine-id"

[credential]
systemd = "private"
"#;

#[cfg_attr(any(), bloomery("CLIENT-INGEST-LEASE-001"))]
#[test]
fn a_valid_lease_configuration_loads() {
    let config = LeaseConfig::parse(VALID).unwrap();
    assert_eq!(config.base_url, "https://cache.example.invalid/t/1/");
    assert_eq!(config.credential, CredentialSpec::Systemd("private".into()));
    assert_eq!(
        config.machine_id_file,
        PathBuf::from("/var/lib/stockhouse/machine-id")
    );
    assert_eq!(config.current_system, PathBuf::from("/run/current-system"));
    assert_eq!(
        config.system_profile,
        PathBuf::from("/nix/var/nix/profiles/system")
    );
    assert_eq!(config.request_timeout_secs, 60);
    assert!(config.extra.is_empty());
    let custom = r#"version = 1
base_url = "https://cache.example.invalid/t/1/"
machine_id_file = "/var/lib/stockhouse/machine-id"
current_system = "/c"
system_profile = "/p/system"
request_timeout_secs = 9

[credential]
file = "/run/keys/t"

[[extra]]
slot = 3
path = "/x"
"#;
    let config = LeaseConfig::parse(custom).unwrap();
    assert_eq!(config.current_system, PathBuf::from("/c"));
    assert_eq!(config.system_profile, PathBuf::from("/p/system"));
    assert_eq!(config.request_timeout_secs, 9);
    assert_eq!(
        config.extra,
        vec![(LeaseSlot::new(3).unwrap(), PathBuf::from("/x"))]
    );
    assert_eq!(
        config.credential,
        CredentialSpec::File("/run/keys/t".into())
    );
}

fn with(line: &str) -> String {
    VALID.replace("\n[credential]", &format!("{line}\n\n[credential]"))
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-LEASE-002"))]
#[test]
fn lease_configuration_rejects_bad_input() {
    let parse = LeaseConfig::parse;
    for bad in [
        "version = ".to_owned(),
        with("extra_field = 1"),
        VALID.replace("machine_id_file = \"/var/lib/stockhouse/machine-id\"\n", ""),
        VALID.replace("[credential]\nsystemd = \"private\"\n", ""),
    ] {
        assert!(
            matches!(parse(&bad), Err(LeaseConfigError::Parse(_))),
            "{bad}"
        );
    }
    assert!(matches!(
        parse(&VALID.replace("version = 1", "version = 2")),
        Err(LeaseConfigError::UnsupportedVersion(_))
    ));
    assert_eq!(
        parse(&VALID.replace("\"/var/lib/stockhouse/machine-id\"", "\"machine-id\"")),
        Err(LeaseConfigError::RelativePath {
            field: "machine_id_file"
        })
    );
    assert_eq!(
        parse(&with("current_system = \"run/current-system\"")),
        Err(LeaseConfigError::RelativePath {
            field: "current_system"
        })
    );
    let extra = |s: &str| format!("{VALID}{s}");
    for slots in [
        "[[extra]]\nslot = 2\npath = \"/x\"\n",
        "[[extra]]\nslot = 11\npath = \"/x\"\n",
        "[[extra]]\nslot = 3\npath = \"/x\"\n[[extra]]\nslot = 3\npath = \"/y\"\n",
    ] {
        assert_eq!(
            parse(&extra(slots)),
            Err(LeaseConfigError::InvalidSlot),
            "{slots}"
        );
    }
    for timeout in ["0", "301"] {
        let text = with(&format!("request_timeout_secs = {timeout}"));
        assert_eq!(
            parse(&text),
            Err(LeaseConfigError::InvalidLimit),
            "{timeout}"
        );
    }
    assert_eq!(
        parse(&VALID.replace("https://cache", "http://cache")),
        Err(LeaseConfigError::Endpoint(EndpointError::Cleartext))
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-CONFIG-007"))]
#[test]
fn lease_protocol_defaults_to_auto_and_accepts_only_known_modes() {
    use stockhouse_wire_v2::ProtocolMode;

    assert_eq!(
        LeaseConfig::parse(VALID).unwrap().protocol,
        ProtocolMode::Auto
    );
    for (input, mode) in [
        ("auto", ProtocolMode::Auto),
        ("v1", ProtocolMode::V1),
        ("v2", ProtocolMode::V2),
    ] {
        assert_eq!(
            LeaseConfig::parse(&with(&format!("protocol = \"{input}\"")))
                .unwrap()
                .protocol,
            mode
        );
    }
    assert!(matches!(
        LeaseConfig::parse(&with("protocol = \"v3\"")),
        Err(LeaseConfigError::Parse(_))
    ));
}
