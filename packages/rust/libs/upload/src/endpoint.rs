use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use reqwest::{Method, Url};
use std::fmt;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;
use stockhouse_auth::Source;
use stockhouse_credentials::{CredentialError, CredentialSource, CredentialSpec, Secret};

const KEEPALIVE: Duration = Duration::from_secs(60);
/// The NAR upload timeout floor: one extra second per started 256 KiB.
const NAR_FLOOR_BYTES: u64 = 256 * 1024;

/// Why a server endpoint was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum EndpointError {
    /// Not an absolute, clean `http(s)` URL ending in `/`.
    InvalidUrl,
    /// `http` to a host that is not a loopback IP literal.
    Cleartext,
    /// The credential was issued for another private tier.
    AudienceMismatch,
    /// The HTTP client could not be built.
    Client,
    /// The credential source could not be built (its sign-in configuration).
    Credential(String),
}

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidUrl => "server URL must be an absolute http(s) URL ending in /",
            Self::Cleartext => "server URL would send the credential in cleartext",
            Self::AudienceMismatch => "the credential is for another private tier",
            Self::Client => "the HTTP client could not be built",
            Self::Credential(why) => return write!(f, "credential: {why}"),
        })
    }
}

impl std::error::Error for EndpointError {}

/// Checks a server base URL: absolute, `https` or loopback `http` (as
/// `stockhouse_auth::secure_url` decides), no user information, query or
/// fragment, and a path ending in `/`.
pub fn check_base_url(text: &str) -> Result<Url, EndpointError> {
    let url = Url::parse(text).map_err(|_| EndpointError::InvalidUrl)?;
    let shape = url.query().is_none() && url.path().ends_with('/');
    let clean = shape && url.username().is_empty() && url.password().is_none();
    match url.scheme() {
        _ if !clean || url.fragment().is_some() => Err(EndpointError::InvalidUrl),
        "http" | "https" if stockhouse_auth::secure_url(&url) => Ok(url),
        "http" if url.host().is_some() => Err(EndpointError::Cleartext),
        _ => Err(EndpointError::InvalidUrl),
    }
}

/// The NAR upload's total timeout for a NarSize.
pub fn nar_timeout(base: Duration, nar_size: u64) -> Duration {
    base + Duration::from_secs(nar_size.div_ceil(NAR_FLOOR_BYTES))
}

/// A validated private tier with its HTTP client and credential source.
pub struct ServerEndpoint {
    base: Url,
    source: Arc<dyn CredentialSource>,
    timeout: Duration,
    client: Client,
}

/// The credential cannot be sent as an `Authorization` header value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidCredential;

impl fmt::Display for InvalidCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("credential is not a valid header value")
    }
}

/// The outcome of one HTTP exchange.
#[derive(Debug)]
pub enum Exchange {
    /// A reply with its (capped) body; `None` when the body exceeded the cap.
    Reply(u16, Option<Vec<u8>>),
    /// A transport error or timeout.
    Failed,
}

impl ServerEndpoint {
    /// Validates `base_url` and builds the client.
    pub fn new(
        base_url: &str,
        source: Arc<dyn CredentialSource>,
        request_timeout_secs: u64,
    ) -> Result<Self, EndpointError> {
        let base = check_base_url(base_url)?;
        let timeout = Duration::from_secs(request_timeout_secs);
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(timeout)
            .tcp_keepalive(KEEPALIVE)
            .timeout(None);
        if base.scheme() == "http" {
            builder = builder.tls_certs_only(std::iter::empty::<reqwest::Certificate>());
        }
        let client = builder.build().map_err(|_| EndpointError::Client)?;
        Ok(Self {
            base,
            source,
            timeout,
            client,
        })
    }

    /// Validates `base_url`, builds the configured credential source with
    /// `stockhouse_auth::source` and, for a signed-in `server` credential,
    /// requires its private tier to be `base_url`.
    pub fn from_spec(
        base_url: &str,
        spec: &CredentialSpec,
        request_timeout_secs: u64,
    ) -> Result<Self, EndpointError> {
        check_base_url(base_url)?;
        let source = stockhouse_auth::source(Some(spec))
            .map_err(|e| EndpointError::Credential(e.to_string()))?;
        let tier = match &source {
            Source::Server(server) => Some(server.private_tier().to_string()),
            Source::Static(_) => None,
        };
        let endpoint = Self::new(base_url, source.shared(), request_timeout_secs)?;
        if let Some(tier) = tier {
            endpoint.check_audience(&tier)?;
        }
        Ok(endpoint)
    }

    /// Refuses a credential issued for another private tier.
    pub fn check_audience(&self, tier: &str) -> Result<(), EndpointError> {
        match Url::parse(tier) {
            Ok(url) if url == self.base => Ok(()),
            _ => Err(EndpointError::AudienceMismatch),
        }
    }

    /// The base URL.
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// The per-request timeout for small requests.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Reads the credential now.
    pub fn credential(&self) -> Result<Secret, CredentialError> {
        self.source.credential()
    }

    /// Reports a 401 for `sent`; `true` when a retry is worthwhile.
    pub fn invalidate(&self, sent: &Secret) -> bool {
        self.source.invalidate(sent)
    }

    /// A request to `base` joined with a protocol path. A credential that is
    /// not a valid header value is refused, never sent as an empty bearer.
    ///
    /// # Panics
    ///
    /// Panics if `rel` is not a relative protocol path made of nonempty,
    /// slash-separated segments of ASCII letters, digits and hyphens.
    pub fn request(
        &self,
        method: Method,
        rel: &str,
        secret: &Secret,
        timeout: Duration,
    ) -> Result<RequestBuilder, InvalidCredential> {
        assert!(
            rel.split('/').all(|segment| {
                !segment.is_empty()
                    && segment
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            }),
            "protocol path must be relative to the server base URL"
        );
        let url = self.base.join(rel).expect("validated protocol path");
        let mut bearer = b"Bearer ".to_vec();
        bearer.extend_from_slice(secret.expose());
        let value = HeaderValue::from_bytes(&bearer);
        bearer.fill(0);
        let mut value = value.map_err(|_| InvalidCredential)?;
        value.set_sensitive(true);
        Ok(self
            .client
            .request(method, url)
            .header(AUTHORIZATION, value)
            .timeout(timeout))
    }

    /// Sends a request and reads at most `limit` bytes of the reply.
    pub fn exchange(&self, request: RequestBuilder, limit: usize) -> Exchange {
        match request.send() {
            Ok(response) => read_reply(response, limit),
            Err(_) => Exchange::Failed,
        }
    }
}

fn read_reply(response: Response, limit: usize) -> Exchange {
    let status = response.status().as_u16();
    let mut body = Vec::new();
    match response.take(limit as u64 + 1).read_to_end(&mut body) {
        Ok(_) if body.len() > limit => Exchange::Reply(status, None),
        Ok(_) => Exchange::Reply(status, Some(body)),
        Err(_) => Exchange::Failed,
    }
}
