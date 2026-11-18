use crate::body::ByteStream;
use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use reqwest::{Client, StatusCode, Url};
use std::fmt;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;
use stockhouse_credentials::Secret;
use tokio::io::AsyncReadExt;

const CHUNK: usize = 64 * 1024;

/// A classified upstream failure (a miss is not a failure).
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum UpstreamError {
    /// 401, 403, or a file permission error.
    Denied,
    /// A 3xx response; redirects are never followed.
    Redirect(u16),
    /// Any other non-200 status.
    Status(u16),
    /// Connect, read, timeout or other I/O failure.
    Unavailable(&'static str),
    /// The body exceeds the limit for that resource.
    TooLarge,
}

impl fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Denied => f.write_str("denied"),
            Self::Redirect(code) => write!(f, "redirect {code} refused"),
            Self::Status(code) => write!(f, "status {code}"),
            Self::Unavailable(why) => write!(f, "unavailable: {why}"),
            Self::TooLarge => f.write_str("response too large"),
        }
    }
}

/// A successful fetch: the resource or a genuine miss.
#[derive(Debug)]
pub enum Fetched<T> {
    /// The resource.
    Found(T),
    /// A genuine miss (404 or `ENOENT`).
    NotFound,
}

/// A tier's transport.
#[derive(Clone, Debug)]
pub(crate) enum Upstream {
    File(PathBuf),
    Http { base: Url, client: Client },
}

impl Upstream {
    pub(crate) fn http(base: Url, timeout: Duration) -> Result<Self, reqwest::Error> {
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(timeout)
            .read_timeout(timeout);
        if base.scheme() == "http" {
            // Cleartext tiers need no trust roots; https tiers use the system verifier.
            builder = builder.tls_certs_only(std::iter::empty::<reqwest::Certificate>());
        }
        let client = builder.build()?;
        Ok(Self::Http { base, client })
    }

    /// Fetches a small resource whole.
    pub(crate) async fn fetch(
        &self,
        rel: &str,
        auth: Option<&Secret>,
        limit: u64,
    ) -> Result<Fetched<Bytes>, UpstreamError> {
        let Fetched::Found((mut stream, _)) = self.open(rel, auth).await? else {
            return Ok(Fetched::NotFound);
        };
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.map_err(|_| UpstreamError::Unavailable("read failed"))?);
            if out.len() as u64 > limit {
                return Err(UpstreamError::TooLarge);
            }
        }
        Ok(Fetched::Found(Bytes::from(out)))
    }

    /// Opens a resource as a stream with its length when known.
    pub(crate) async fn open(
        &self,
        rel: &str,
        auth: Option<&Secret>,
    ) -> Result<Fetched<(ByteStream, Option<u64>)>, UpstreamError> {
        match self {
            Self::File(dir) => open_file(dir.join(rel)).await,
            Self::Http { base, client } => open_http(client, base, rel, auth).await,
        }
    }
}

async fn open_file(path: PathBuf) -> Result<Fetched<(ByteStream, Option<u64>)>, UpstreamError> {
    let file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Fetched::NotFound),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => return Err(UpstreamError::Denied),
        Err(_) => return Err(UpstreamError::Unavailable("file open failed")),
    };
    let len = file.metadata().await.ok().map(|m| m.len());
    let stream = futures_util::stream::unfold(file, |mut file| async move {
        let mut buf = vec![0u8; CHUNK];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(Bytes::from(buf)), file))
            }
            Err(e) => Some((Err(e), file)),
        }
    });
    Ok(Fetched::Found((stream.boxed(), len)))
}

async fn open_http(
    client: &Client,
    base: &Url,
    rel: &str,
    auth: Option<&Secret>,
) -> Result<Fetched<(ByteStream, Option<u64>)>, UpstreamError> {
    let url = base
        .join(rel)
        .map_err(|_| UpstreamError::Unavailable("bad relative URL"))?;
    let mut request = client.get(url);
    if let Some(secret) = auth {
        request = request.header(AUTHORIZATION, bearer(secret)?);
    }
    let response = request.send().await.map_err(classify)?;
    match response.status() {
        StatusCode::OK => {}
        StatusCode::NOT_FOUND => return Ok(Fetched::NotFound),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(UpstreamError::Denied),
        s if s.is_redirection() => return Err(UpstreamError::Redirect(s.as_u16())),
        s => return Err(UpstreamError::Status(s.as_u16())),
    }
    let len = response.content_length();
    let stream = response
        .bytes_stream()
        .map(|r| r.map_err(std::io::Error::other));
    Ok(Fetched::Found((stream.boxed(), len)))
}

fn bearer(secret: &Secret) -> Result<HeaderValue, UpstreamError> {
    let mut value = b"Bearer ".to_vec();
    value.extend_from_slice(secret.expose());
    let mut header = HeaderValue::from_bytes(&value)
        .map_err(|_| UpstreamError::Unavailable("credential not a header value"))?;
    header.set_sensitive(true);
    Ok(header)
}

fn classify(error: reqwest::Error) -> UpstreamError {
    if error.is_timeout() {
        UpstreamError::Unavailable("timeout")
    } else if error.is_connect() {
        UpstreamError::Unavailable("connect failed")
    } else {
        UpstreamError::Unavailable("request failed")
    }
}
