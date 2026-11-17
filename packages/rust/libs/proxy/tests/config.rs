mod common;
use common::*;
use std::time::Duration;
use stockhouse_credentials::CredentialSpec;
use stockhouse_proto::UnsupportedVersion;
use stockhouse_proxy::{ConfigError, ProxyConfig, TierName, TierUrl};

fn parse(text: &str) -> Result<ProxyConfig, ConfigError> {
    ProxyConfig::parse(text)
}

fn with_tier(tier_name: &str, body: &str) -> String {
    format!("version = 1\nlisten = \"127.0.0.1:8484\"\n[{tier_name}]\n{body}")
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CONFIG-001"))]
#[test]
fn valid_configuration_loads() {
    let text = format!(
        "version = 1\nlisten = \"127.0.0.1:8484\"\npriority = 20\nrequest_timeout_secs = 5\n\
[private]\n{}\n[public]\n{}",
        tier(
            "https://cache.example.invalid/org/",
            "credential = { systemd = \"private\" }"
        ),
        tier("https://cache.example.invalid/public/", ""),
    );
    let config = parse(&text).unwrap();
    assert_eq!(config.listen.to_string(), "127.0.0.1:8484");
    assert_eq!(
        (config.priority, config.timeout),
        (20, Duration::from_secs(5))
    );
    let names: Vec<_> = config.tiers.iter().map(|t| t.name).collect();
    assert_eq!(names, [TierName::Private, TierName::Public]);
    assert_eq!(
        config.tiers[0].credential,
        Some(CredentialSpec::Systemd("private".to_owned()))
    );
    assert!(
        matches!(&config.tiers[1].url, TierUrl::Http(u) if u.as_str() == "https://cache.example.invalid/public/")
    );
    assert_eq!(config.tiers[1].keys.keys()[0].name(), KEY_NAME);
    let file = parse(&with_tier("shared", &tier("file:///var/cache/shared/", ""))).unwrap();
    assert_eq!(file.tiers[0].url, TierUrl::File("/var/cache/shared".into()));
    assert_eq!((file.priority, file.timeout), (30, Duration::from_secs(30)));
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CONFIG-002"))]
#[test]
fn unknown_versions_are_rejected() {
    let text = with_tier("public", &tier("https://h.example.invalid/", ""))
        .replace("version = 1", "version = 2");
    assert_eq!(
        parse(&text),
        Err(ConfigError::UnsupportedVersion(UnsupportedVersion {
            found: 2,
            supported: 1
        }))
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CONFIG-003"))]
#[test]
fn non_loopback_listen_is_rejected() {
    for listen in [
        "0.0.0.0:8484",
        "192.0.2.1:8484",
        "[::]:8484",
        "localhost:8484",
    ] {
        let text = with_tier("public", &tier("https://h.example.invalid/", ""))
            .replace("127.0.0.1:8484", listen);
        assert_eq!(
            parse(&text),
            Err(ConfigError::NonLoopbackListen),
            "{listen}"
        );
    }
    let v6 = with_tier("public", &tier("https://h.example.invalid/", ""))
        .replace("127.0.0.1:8484", "[::1]:8484");
    assert!(parse(&v6).is_ok());
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CONFIG-004"))]
#[test]
fn at_least_one_tier_is_required() {
    assert_eq!(
        parse("version = 1\nlisten = \"127.0.0.1:8484\"\n"),
        Err(ConfigError::NoTiers)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CONFIG-005"))]
#[test]
fn tiers_need_valid_trusted_keys() {
    let none = with_tier(
        "shared",
        "url = \"https://h.example.invalid/\"\ntrusted_keys = []\n",
    );
    assert_eq!(
        parse(&none),
        Err(ConfigError::MissingTrustedKeys(TierName::Shared))
    );
    let bad = with_tier(
        "shared",
        "url = \"https://h.example.invalid/\"\ntrusted_keys = [\"nope\"]\n",
    );
    assert_eq!(
        parse(&bad),
        Err(ConfigError::InvalidTrustedKey(TierName::Shared))
    );
    let key = key_text("same", 1);
    let other = key_text("same", 2);
    let dup = with_tier(
        "shared",
        &format!("url = \"https://h.example.invalid/\"\ntrusted_keys = [\"{key}\", \"{other}\"]\n"),
    );
    assert_eq!(
        parse(&dup),
        Err(ConfigError::InvalidTrustedKey(TierName::Shared))
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CONFIG-006"))]
#[test]
fn public_tier_cannot_carry_credential() {
    let text = with_tier(
        "public",
        &tier(
            "https://h.example.invalid/",
            "credential = { file = \"/run/t\" }",
        ),
    );
    assert_eq!(parse(&text), Err(ConfigError::CredentialOnPublicTier));
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CONFIG-007"))]
#[test]
fn file_tiers_cannot_carry_credential() {
    let text = with_tier(
        "private",
        &tier("file:///var/cache/p/", "credential = { file = \"/run/t\" }"),
    );
    assert_eq!(
        parse(&text),
        Err(ConfigError::CredentialOnFileTier(TierName::Private))
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CONFIG-008"))]
#[test]
fn cleartext_credentials_off_host_are_rejected() {
    let cred = "credential = { systemd = \"shared\" }";
    let remote = with_tier("shared", &tier("http://cache.example.invalid/", cred));
    assert_eq!(
        parse(&remote),
        Err(ConfigError::CleartextCredential(TierName::Shared))
    );
    for local in ["http://127.0.0.1:9000/", "http://[::1]:9000/"] {
        assert!(
            parse(&with_tier("shared", &tier(local, cred))).is_ok(),
            "{local}"
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CONFIG-009"))]
#[test]
fn unsupported_urls_are_rejected() {
    for url in [
        "ftp://h.example.invalid/",
        "https://h.example.invalid/?x=1",
        "https://h.example.invalid/#f",
        "https://h.example.invalid/org",
        "s3://bucket/",
    ] {
        assert_eq!(
            parse(&with_tier("private", &tier(url, ""))),
            Err(ConfigError::InvalidUrl(TierName::Private)),
            "{url}"
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CONFIG-010"))]
#[test]
fn configuration_cannot_hold_credential_values() {
    let token = with_tier(
        "private",
        &tier("https://h.example.invalid/", "token = \"s3cr3t\""),
    );
    assert!(matches!(parse(&token), Err(ConfigError::Parse(_))));
    let inline = with_tier(
        "private",
        &tier(
            "https://h.example.invalid/",
            "credential = { value = \"s3cr3t\" }",
        ),
    );
    assert!(matches!(parse(&inline), Err(ConfigError::Parse(_))));
    assert!(!format!("{:?}", parse(&token)).contains("s3cr3t"));
}
