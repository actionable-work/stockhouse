//! Sign-in configuration parsing.

use stockhouse_auth::{AuthConfig, AuthConfigError, StoreKind};

const BASE: &str = r#"version = 1
server = "https://example.invalid/"
client_id = "stockhouse-cli"
scopes = ["cache:read", "cache:write"]
private_tier = "https://cache.example.invalid/t/1/"

[endpoints]
authorization = "https://example.invalid/oauth/authorize"
token = "https://example.invalid/oauth/token"
device_authorization = "https://example.invalid/oauth/device"
revocation = "https://example.invalid/oauth/revoke"

[store]
mode = "sealed"
directory = "/var/lib/stockhouse/auth"
key_credential = "stockhouse-token-key"
"#;

fn with(from: &str, to: &str) -> Result<AuthConfig, AuthConfigError> {
    assert!(BASE.contains(from), "{from}");
    AuthConfig::parse(&BASE.replace(from, to))
}

fn assert_err(from: &str, to: &str, expected: AuthConfigError) {
    let result = with(from, to);
    assert_eq!(result.as_ref().err(), Some(&expected), "{to}");
    let text = expected.to_string();
    let value = to.split('"').nth(1).unwrap_or_default();
    assert!(value.len() < 4 || !text.contains(value), "{text}");
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-CONFIG-001"))]
#[test]
fn valid_configuration_loads() {
    let config = AuthConfig::parse(BASE).unwrap();
    assert_eq!(config.server.as_str(), "https://example.invalid/");
    assert_eq!(
        config.endpoints.token.as_str(),
        "https://example.invalid/oauth/token"
    );
    assert_eq!(config.client_id, "stockhouse-cli");
    assert_eq!(config.scope(), "cache:read cache:write");
    assert_eq!(
        config.private_tier.as_str(),
        "https://cache.example.invalid/t/1/"
    );
    assert_eq!(config.timeout.as_secs(), 30);
    assert_eq!(
        config.store_kind,
        StoreKind::Sealed {
            key_credential: "stockhouse-token-key".to_owned()
        }
    );
    assert_eq!(
        config.store_directory.to_str(),
        Some("/var/lib/stockhouse/auth")
    );
    let local = BASE
        .replace("https://example.invalid/", "http://127.0.0.1:8486/")
        .replace("https://cache.example.invalid/", "http://[::1]:8486/");
    assert!(AuthConfig::parse(&local).is_ok());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-CONFIG-002"))]
#[test]
fn unknown_versions_are_rejected() {
    assert_err(
        "version = 1",
        "version = 2",
        AuthConfigError::UnsupportedVersion,
    );
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-CONFIG-003"))]
#[test]
fn configuration_cannot_hold_secrets() {
    let secret = "super-secret-refresh-1";
    for text in [
        format!("{BASE}token = \"{secret}\"\n").replacen(
            "[store]",
            &format!("refresh_token = \"{secret}\"\n[store]"),
            0,
        ),
        BASE.replace(
            "version = 1",
            &format!("version = 1\nrefresh_token = \"{secret}\""),
        ),
        BASE.replace("version = 1", &format!("version = \"{secret}\"")),
        BASE.replace("client_id = \"stockhouse-cli\"\n", ""),
    ] {
        let error = AuthConfig::parse(&text).unwrap_err();
        assert_eq!(error, AuthConfigError::Parse);
        assert!(!error.to_string().contains(secret));
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-CONFIG-004"))]
#[test]
fn server_origin_is_clean_and_secure() {
    for bad in [
        "http://example.invalid/",
        "https://example.invalid/base/",
        "https://example.invalid/?x=1",
        "https://example.invalid/#f",
        "https://user@example.invalid/",
        "http://localhost/",
    ] {
        assert_err(
            "server = \"https://example.invalid/\"",
            &format!("server = \"{bad}\""),
            AuthConfigError::InvalidServer,
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-CONFIG-005"))]
#[test]
fn endpoints_are_secure_urls_without_fragments() {
    let token = "token = \"https://example.invalid/oauth/token\"";
    for bad in [
        "http://example.invalid/oauth/token",
        "https://example.invalid/oauth/token#x",
        "https://u:p@example.invalid/oauth/token",
        "https://example.invalid/oauth/token?x=1",
    ] {
        assert_err(
            token,
            &format!("token = \"{bad}\""),
            AuthConfigError::InvalidEndpoint { endpoint: "token" },
        );
    }
    let auth = "authorization = \"https://example.invalid/oauth/authorize\"";
    assert!(
        with(
            auth,
            "authorization = \"https://example.invalid/oauth/authorize?tenant=1\""
        )
        .is_ok()
    );
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-CONFIG-006"))]
#[test]
fn endpoints_share_the_server_origin() {
    for bad in [
        "https://other.example.invalid/oauth/revoke",
        "https://example.invalid:8443/oauth/revoke",
    ] {
        assert_err(
            "revocation = \"https://example.invalid/oauth/revoke\"",
            &format!("revocation = \"{bad}\""),
            AuthConfigError::ForeignEndpoint {
                endpoint: "revocation",
            },
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-CONFIG-007"))]
#[test]
fn private_tier_is_a_secure_base_url() {
    let tier = "private_tier = \"https://cache.example.invalid/t/1/\"";
    for bad in [
        "http://cache.example.invalid/t/1/",
        "https://cache.example.invalid/t/1",
        "https://cache.example.invalid/t/1/?q",
        "https://cache.example.invalid/t/1/#f",
        "https://u@cache.example.invalid/t/1/",
    ] {
        assert_err(
            tier,
            &format!("private_tier = \"{bad}\""),
            AuthConfigError::InvalidPrivateTier,
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-CONFIG-008"))]
#[test]
fn client_id_and_scopes_are_well_formed() {
    let id = "client_id = \"stockhouse-cli\"";
    let long = "x".repeat(256);
    for bad in ["", "has space", long.as_str()] {
        assert_err(
            id,
            &format!("client_id = \"{bad}\""),
            AuthConfigError::InvalidClientId,
        );
    }
    let scopes = "scopes = [\"cache:read\", \"cache:write\"]";
    for bad in [
        "[]",
        "[\"a\", \"a\"]",
        "[\"a b\"]",
        "[\"quo\\\"te\"]",
        "[\"back\\\\slash\"]",
    ] {
        assert_err(
            scopes,
            &format!("scopes = {bad}"),
            AuthConfigError::InvalidScope,
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-CONFIG-009"))]
#[test]
fn store_location_and_mode_are_explicit_and_safe() {
    let dir = "directory = \"/var/lib/stockhouse/auth\"";
    for bad in ["auth", "/var/lib/../auth", "/nix/store/abc-auth"] {
        assert_err(
            dir,
            &format!("directory = \"{bad}\""),
            AuthConfigError::InvalidStore,
        );
    }
    let key = "key_credential = \"stockhouse-token-key\"";
    for bad in ["", ".", "a/b"] {
        assert_err(
            key,
            &format!("key_credential = \"{bad}\""),
            AuthConfigError::InvalidStore,
        );
    }
    assert_eq!(
        with(&format!("{key}\n"), "").err(),
        Some(AuthConfigError::InvalidStore)
    );
    assert_eq!(
        with("mode = \"sealed\"", "mode = \"file\"").err(),
        Some(AuthConfigError::InvalidStore)
    );
    let file = with("mode = \"sealed\"", "mode = \"file\"").map(|_| ());
    assert!(file.is_err());
    let plain = BASE
        .replace("mode = \"sealed\"", "mode = \"file\"")
        .replace(&format!("{key}\n"), "");
    assert_eq!(
        AuthConfig::parse(&plain).unwrap().store_kind,
        StoreKind::File
    );
    assert_eq!(
        with("mode = \"sealed\"", "mode = \"plain\"").err(),
        Some(AuthConfigError::Parse)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-CONFIG-010"))]
#[test]
fn request_timeouts_are_bounded() {
    for bad in ["0", "301"] {
        let text = BASE.replace(
            "version = 1",
            &format!("version = 1\nrequest_timeout_secs = {bad}"),
        );
        assert_eq!(
            AuthConfig::parse(&text).err(),
            Some(AuthConfigError::InvalidTimeout)
        );
    }
    let ok = BASE.replace("version = 1", "version = 1\nrequest_timeout_secs = 300");
    assert_eq!(AuthConfig::parse(&ok).unwrap().timeout.as_secs(), 300);
}
