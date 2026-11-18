use reqwest::Url;
use serde::Deserialize;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use stockhouse_credentials::SystemdCredential;
use stockhouse_token_store::{Binding, Store, StoreMode};

const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Why a sign-in configuration was rejected. Never includes a value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AuthConfigError {
    /// The file could not be read.
    Read,
    /// TOML syntax, an unknown field, a wrong type or a missing field.
    Parse,
    /// A version other than 1.
    UnsupportedVersion,
    /// The server origin is not a clean secure origin.
    InvalidServer,
    /// An endpoint is not an acceptable URL.
    InvalidEndpoint {
        /// Which endpoint.
        endpoint: &'static str,
    },
    /// An endpoint is not on the server origin.
    ForeignEndpoint {
        /// Which endpoint.
        endpoint: &'static str,
    },
    /// The private tier is not a secure base URL.
    InvalidPrivateTier,
    /// The client ID is empty, too long or not visible ASCII.
    InvalidClientId,
    /// The scopes are empty, repeated or malformed.
    InvalidScope,
    /// The request timeout is outside 1–300 s.
    InvalidTimeout,
    /// The store location or mode is invalid.
    InvalidStore,
}

impl fmt::Display for AuthConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self {
            Self::Read => "cannot be read",
            Self::Parse => "is not valid (syntax, unknown or missing field, or wrong type)",
            Self::UnsupportedVersion => "has an unsupported version",
            Self::InvalidServer => "has an invalid server origin",
            Self::InvalidEndpoint { endpoint } | Self::ForeignEndpoint { endpoint } => {
                return write!(f, "sign-in configuration: {endpoint} endpoint is invalid");
            }
            Self::InvalidPrivateTier => "has an invalid private tier URL",
            Self::InvalidClientId => "has an invalid client ID",
            Self::InvalidScope => "has invalid scopes",
            Self::InvalidTimeout => "has a request timeout outside 1-300 s",
            Self::InvalidStore => "has an invalid token store",
        };
        write!(f, "sign-in configuration {what}")
    }
}

impl std::error::Error for AuthConfigError {}

/// The four OAuth endpoints.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoints {
    /// The authorization endpoint (may keep a query).
    pub authorization: Url,
    /// The token endpoint.
    pub token: Url,
    /// The device authorization endpoint.
    pub device_authorization: Url,
    /// The revocation endpoint.
    pub revocation: Url,
}

/// How the token store is protected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreKind {
    /// Sealed under the named systemd credential.
    Sealed {
        /// The key credential name.
        key_credential: String,
    },
    /// Plaintext owner-only file.
    File,
}

/// A validated sign-in configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthConfig {
    /// The server origin.
    pub server: Url,
    /// The endpoints.
    pub endpoints: Endpoints,
    /// The public client ID.
    pub client_id: String,
    /// Requested scopes.
    pub scopes: Vec<String>,
    /// The private tier base URL the tokens are for.
    pub private_tier: Url,
    /// Endpoint connect and per-read timeout.
    pub timeout: Duration,
    /// The token store directory.
    pub store_directory: PathBuf,
    /// The token store mode.
    pub store_kind: StoreKind,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    version: u32,
    server: String,
    client_id: String,
    scopes: Vec<String>,
    private_tier: String,
    request_timeout_secs: Option<u64>,
    endpoints: RawEndpoints,
    store: RawStore,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEndpoints {
    authorization: String,
    token: String,
    device_authorization: String,
    revocation: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStore {
    mode: RawMode,
    directory: PathBuf,
    key_credential: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawMode {
    Sealed,
    File,
}

impl AuthConfig {
    /// Reads and parses a configuration file.
    pub fn load(path: &Path) -> Result<Self, AuthConfigError> {
        let text = std::fs::read_to_string(path).map_err(|_| AuthConfigError::Read)?;
        Self::parse(&text)
    }

    /// Parses and validates TOML configuration.
    pub fn parse(text: &str) -> Result<Self, AuthConfigError> {
        let raw: Raw = toml::from_str(text).map_err(|_| AuthConfigError::Parse)?;
        if raw.version != 1 {
            return Err(AuthConfigError::UnsupportedVersion);
        }
        let server = server(&raw.server)?;
        let endpoints = endpoints(&server, &raw.endpoints)?;
        Ok(Self {
            endpoints,
            server,
            client_id: client_id(raw.client_id)?,
            scopes: scopes(raw.scopes)?,
            private_tier: private_tier(&raw.private_tier)?,
            timeout: timeout(raw.request_timeout_secs)?,
            store_kind: store_kind(&raw.store)?,
            store_directory: store_directory(raw.store.directory)?,
        })
    }

    /// The store binding for this configuration.
    pub fn binding(&self) -> Binding {
        Binding::new(
            self.endpoints.token.as_str(),
            &self.client_id,
            self.private_tier.as_str(),
        )
    }

    /// The token store this configuration names.
    pub fn store(&self) -> Store {
        let mode = match &self.store_kind {
            StoreKind::Sealed { key_credential } => {
                StoreMode::Sealed(SystemdCredential::new(key_credential.clone()))
            }
            StoreKind::File => StoreMode::File,
        };
        Store::new(self.store_directory.clone(), mode, self.binding())
    }

    /// How long a writer waits for the store lock.
    pub fn lock_wait(&self) -> Duration {
        self.timeout * 2
    }

    /// The configured scopes joined by spaces.
    pub fn scope(&self) -> String {
        self.scopes.join(" ")
    }
}

/// Whether a URL is `https://`, or `http://` to a loopback IP literal, with
/// no userinfo or fragment.
pub fn secure_url(url: &Url) -> bool {
    let host = url.host_str().unwrap_or_default();
    let loopback = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback());
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    scheme_ok
        && url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

fn server(text: &str) -> Result<Url, AuthConfigError> {
    let url = Url::parse(text).map_err(|_| AuthConfigError::InvalidServer)?;
    if secure_url(&url) && url.path() == "/" && url.query().is_none() {
        Ok(url)
    } else {
        Err(AuthConfigError::InvalidServer)
    }
}

fn endpoint(server: &Url, name: &'static str, text: &str) -> Result<Url, AuthConfigError> {
    let invalid = AuthConfigError::InvalidEndpoint { endpoint: name };
    let url = Url::parse(text).map_err(|_| invalid)?;
    if !secure_url(&url) || (name != "authorization" && url.query().is_some()) {
        return Err(invalid);
    }
    if url.origin() != server.origin() {
        return Err(AuthConfigError::ForeignEndpoint { endpoint: name });
    }
    Ok(url)
}

fn endpoints(server: &Url, raw: &RawEndpoints) -> Result<Endpoints, AuthConfigError> {
    Ok(Endpoints {
        authorization: endpoint(server, "authorization", &raw.authorization)?,
        token: endpoint(server, "token", &raw.token)?,
        device_authorization: endpoint(server, "device_authorization", &raw.device_authorization)?,
        revocation: endpoint(server, "revocation", &raw.revocation)?,
    })
}

fn private_tier(text: &str) -> Result<Url, AuthConfigError> {
    let url = Url::parse(text).map_err(|_| AuthConfigError::InvalidPrivateTier)?;
    if secure_url(&url) && url.query().is_none() && url.path().ends_with('/') {
        Ok(url)
    } else {
        Err(AuthConfigError::InvalidPrivateTier)
    }
}

fn client_id(id: String) -> Result<String, AuthConfigError> {
    let visible = id.bytes().all(|b| (0x21..=0x7e).contains(&b));
    if id.is_empty() || id.len() > 255 || !visible {
        return Err(AuthConfigError::InvalidClientId);
    }
    Ok(id)
}

fn scope_token(scope: &str) -> bool {
    !scope.is_empty()
        && scope
            .bytes()
            .all(|b| b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b))
}

fn scopes(scopes: Vec<String>) -> Result<Vec<String>, AuthConfigError> {
    let unique = scopes
        .iter()
        .enumerate()
        .all(|(i, s)| !scopes[..i].contains(s));
    if scopes.is_empty() || !unique || !scopes.iter().all(|s| scope_token(s)) {
        return Err(AuthConfigError::InvalidScope);
    }
    Ok(scopes)
}

fn timeout(secs: Option<u64>) -> Result<Duration, AuthConfigError> {
    match secs.unwrap_or(DEFAULT_TIMEOUT_SECS) {
        secs @ 1..=300 => Ok(Duration::from_secs(secs)),
        _ => Err(AuthConfigError::InvalidTimeout),
    }
}

fn store_kind(raw: &RawStore) -> Result<StoreKind, AuthConfigError> {
    match (&raw.mode, &raw.key_credential) {
        (RawMode::Sealed, Some(name)) if SystemdCredential::valid_name(name) => {
            Ok(StoreKind::Sealed {
                key_credential: name.clone(),
            })
        }
        (RawMode::File, None) => Ok(StoreKind::File),
        _ => Err(AuthConfigError::InvalidStore),
    }
}

fn store_directory(dir: PathBuf) -> Result<PathBuf, AuthConfigError> {
    let bytes = dir.as_os_str().as_encoded_bytes();
    let normal = !bytes.split(|&b| b == b'/').any(|s| s == b"." || s == b"..");
    if dir.is_absolute() && normal && !dir.starts_with("/nix/store") {
        Ok(dir)
    } else {
        Err(AuthConfigError::InvalidStore)
    }
}
