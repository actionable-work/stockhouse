use reqwest::Url;
use serde::Deserialize;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use stockhouse_credentials::CredentialSpec;
use stockhouse_proto::{PublicKey, TrustedKeys, UnsupportedVersion, check_version};

const DEFAULT_PRIORITY: u32 = 30;
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// The three tiers, in lookup order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TierName {
    /// The authorized private cache.
    Private,
    /// The intentionally shared cache.
    Shared,
    /// A configured public upstream.
    Public,
}

impl TierName {
    /// The route and configuration spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Shared => "shared",
            Self::Public => "public",
        }
    }

    /// Parses a route segment.
    pub fn parse(text: &str) -> Option<Self> {
        [Self::Private, Self::Shared, Self::Public]
            .into_iter()
            .find(|t| t.as_str() == text)
    }
}

impl fmt::Display for TierName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a proxy configuration was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ConfigError {
    /// TOML syntax, an unknown field or a wrong type.
    Parse(String),
    /// A version other than 1.
    UnsupportedVersion(UnsupportedVersion),
    /// `listen` is not a loopback socket address.
    NonLoopbackListen,
    /// No tier is configured.
    NoTiers,
    /// A tier has no trusted keys.
    MissingTrustedKeys(TierName),
    /// A trusted key does not parse or a name repeats.
    InvalidTrustedKey(TierName),
    /// An unsupported or malformed tier URL.
    InvalidUrl(TierName),
    /// The public tier declares a credential.
    CredentialOnPublicTier,
    /// A `file://` tier declares a credential.
    CredentialOnFileTier(TierName),
    /// A credential over `http://` to a non-loopback host.
    CleartextCredential(TierName),
    /// A `server` credential outside the private tier or with a relative path.
    ServerCredential(TierName),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "proxy configuration: {message}"),
            Self::UnsupportedVersion(v) => write!(f, "proxy configuration: {v}"),
            Self::NonLoopbackListen => {
                f.write_str("proxy configuration: listen must be a loopback address")
            }
            Self::NoTiers => f.write_str("proxy configuration: no tier configured"),
            Self::MissingTrustedKeys(t) => {
                write!(f, "proxy configuration: {t} tier has no trusted keys")
            }
            Self::InvalidTrustedKey(t) => write!(
                f,
                "proxy configuration: {t} tier has an invalid trusted key"
            ),
            Self::InvalidUrl(t) => write!(f, "proxy configuration: {t} tier URL is invalid"),
            Self::CredentialOnPublicTier => {
                f.write_str("proxy configuration: the public tier cannot have a credential")
            }
            Self::CredentialOnFileTier(t) => write!(
                f,
                "proxy configuration: file tier {t} cannot have a credential"
            ),
            Self::CleartextCredential(t) => write!(
                f,
                "proxy configuration: {t} credential would be sent in cleartext"
            ),
            Self::ServerCredential(t) => write!(
                f,
                "proxy configuration: {t} tier cannot use a server credential"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Where a tier lives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TierUrl {
    /// A local directory.
    File(PathBuf),
    /// An HTTP(S) base URL ending in `/`.
    Http(Url),
}

/// One validated tier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TierConfig {
    /// Which tier.
    pub name: TierName,
    /// Its location.
    pub url: TierUrl,
    /// Keys whose signatures it may serve.
    pub keys: TrustedKeys,
    /// Where its credential lives, if it needs one.
    pub credential: Option<CredentialSpec>,
}

/// A validated proxy configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyConfig {
    /// The loopback listen address.
    pub listen: SocketAddr,
    /// `Priority` served in our nix-cache-info.
    pub priority: u32,
    /// Upstream connect and per-read timeout.
    pub timeout: Duration,
    /// Configured tiers in lookup order.
    pub tiers: Vec<TierConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    version: u32,
    listen: String,
    priority: Option<u32>,
    request_timeout_secs: Option<u64>,
    private: Option<RawTier>,
    shared: Option<RawTier>,
    public: Option<RawTier>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTier {
    url: String,
    trusted_keys: Vec<String>,
    credential: Option<CredentialSpec>,
}

impl ProxyConfig {
    /// Parses and validates TOML configuration.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig =
            toml::from_str(text).map_err(|e| ConfigError::Parse(e.message().to_owned()))?;
        check_version(raw.version).map_err(ConfigError::UnsupportedVersion)?;
        let listen: SocketAddr = raw
            .listen
            .parse()
            .map_err(|_| ConfigError::NonLoopbackListen)?;
        if !listen.ip().is_loopback() {
            return Err(ConfigError::NonLoopbackListen);
        }
        let named = [
            (TierName::Private, raw.private),
            (TierName::Shared, raw.shared),
            (TierName::Public, raw.public),
        ];
        let tiers = named
            .into_iter()
            .filter_map(|(n, t)| t.map(|t| tier(n, t)))
            .collect::<Result<Vec<_>, _>>()?;
        if tiers.is_empty() {
            return Err(ConfigError::NoTiers);
        }
        let timeout = Duration::from_secs(raw.request_timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));
        Ok(Self {
            listen,
            priority: raw.priority.unwrap_or(DEFAULT_PRIORITY),
            timeout,
            tiers,
        })
    }
}

fn tier(name: TierName, raw: RawTier) -> Result<TierConfig, ConfigError> {
    let url = tier_url(name, &raw.url)?;
    let keys = trusted_keys(name, &raw.trusted_keys)?;
    let credential = raw.credential;
    if let Some(spec) = &credential {
        check_credential_placement(name, &url)?;
        check_server_credential(name, spec)?;
    }
    Ok(TierConfig {
        name,
        url,
        keys,
        credential,
    })
}

fn tier_url(name: TierName, text: &str) -> Result<TierUrl, ConfigError> {
    let url = Url::parse(text).map_err(|_| ConfigError::InvalidUrl(name))?;
    let clean = url.query().is_none() && url.fragment().is_none() && url.path().ends_with('/');
    match url.scheme() {
        _ if !clean => Err(ConfigError::InvalidUrl(name)),
        "file" => url
            .to_file_path()
            .map(TierUrl::File)
            .map_err(|()| ConfigError::InvalidUrl(name)),
        "http" | "https" if url.host().is_some() => Ok(TierUrl::Http(url)),
        _ => Err(ConfigError::InvalidUrl(name)),
    }
}

fn trusted_keys(name: TierName, texts: &[String]) -> Result<TrustedKeys, ConfigError> {
    if texts.is_empty() {
        return Err(ConfigError::MissingTrustedKeys(name));
    }
    let keys = texts
        .iter()
        .map(|t| PublicKey::parse(t))
        .collect::<Result<Vec<_>, _>>();
    keys.and_then(TrustedKeys::new)
        .map_err(|_| ConfigError::InvalidTrustedKey(name))
}

fn check_credential_placement(name: TierName, url: &TierUrl) -> Result<(), ConfigError> {
    match url {
        _ if name == TierName::Public => Err(ConfigError::CredentialOnPublicTier),
        TierUrl::File(_) => Err(ConfigError::CredentialOnFileTier(name)),
        TierUrl::Http(url) if url.scheme() == "http" && !is_loopback_host(url) => {
            Err(ConfigError::CleartextCredential(name))
        }
        TierUrl::Http(_) => Ok(()),
    }
}

fn check_server_credential(name: TierName, spec: &CredentialSpec) -> Result<(), ConfigError> {
    match spec {
        CredentialSpec::Server(path) if name != TierName::Private || !normal_absolute(path) => {
            Err(ConfigError::ServerCredential(name))
        }
        _ => Ok(()),
    }
}

/// Absolute with no `.` or `..` component.
fn normal_absolute(path: &std::path::Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    path.is_absolute() && !bytes.split(|&b| b == b'/').any(|s| s == b"." || s == b"..")
}

fn is_loopback_host(url: &Url) -> bool {
    let host = url.host_str().unwrap_or_default();
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}
