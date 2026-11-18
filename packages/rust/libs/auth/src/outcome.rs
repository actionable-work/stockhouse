//! Classification of endpoint replies. Errors carry only classifications:
//! never a body, `error_description` or header value.

use crate::client::Reply;
use serde::Deserialize;
use std::fmt;
use stockhouse_credentials::Secret;
use stockhouse_token_store::{MAX_TOKEN_BYTES, TokenSet};

/// Known OAuth error codes; anything else is `Other`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum OAuthCode {
    InvalidRequest,
    InvalidClient,
    InvalidGrant,
    UnauthorizedClient,
    UnsupportedGrantType,
    InvalidScope,
    AccessDenied,
    AuthorizationPending,
    SlowDown,
    ExpiredToken,
    TemporarilyUnavailable,
    Other,
}

const CODES: [(&str, OAuthCode); 11] = [
    ("invalid_request", OAuthCode::InvalidRequest),
    ("invalid_client", OAuthCode::InvalidClient),
    ("invalid_grant", OAuthCode::InvalidGrant),
    ("unauthorized_client", OAuthCode::UnauthorizedClient),
    ("unsupported_grant_type", OAuthCode::UnsupportedGrantType),
    ("invalid_scope", OAuthCode::InvalidScope),
    ("access_denied", OAuthCode::AccessDenied),
    ("authorization_pending", OAuthCode::AuthorizationPending),
    ("slow_down", OAuthCode::SlowDown),
    ("expired_token", OAuthCode::ExpiredToken),
    ("temporarily_unavailable", OAuthCode::TemporarilyUnavailable),
];

impl OAuthCode {
    /// Classifies an `error` value.
    pub fn parse(text: &str) -> Self {
        CODES
            .iter()
            .find(|(name, _)| *name == text)
            .map_or(Self::Other, |(_, code)| *code)
    }

    /// The wire spelling (`other` for unknown codes).
    pub fn as_str(&self) -> &'static str {
        CODES
            .iter()
            .find(|(_, code)| code == self)
            .map_or("other", |(name, _)| name)
    }
}

/// Why a request may succeed later.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transient {
    /// 429 or 5xx.
    Status(u16),
    /// A 3xx, never followed.
    Redirect(u16),
    /// Connect, read or timeout failure.
    Transport,
}

/// A classified endpoint failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    /// 400/401 with this `error`.
    OAuth(OAuthCode),
    /// May succeed later.
    Transient(Transient),
    /// A malformed, oversized or unexpected reply.
    InvalidResponse,
}

impl Failure {
    /// Whether retrying later may help (`temporarily_unavailable` counts).
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Transient(_) | Self::OAuth(OAuthCode::TemporarilyUnavailable)
        )
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OAuth(code) => write!(f, "server answered {}", code.as_str()),
            Self::Transient(Transient::Status(code)) => write!(f, "server status {code}"),
            Self::Transient(Transient::Redirect(code)) => write!(f, "redirect {code} refused"),
            Self::Transient(Transient::Transport) => f.write_str("server unreachable"),
            Self::InvalidResponse => f.write_str("invalid server response"),
        }
    }
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

/// Classifies a non-200 reply.
pub fn classify_error(reply: &Reply) -> Failure {
    match reply.status {
        400 | 401 => {
            let code = serde_json::from_slice::<ErrorBody>(&reply.body)
                .map_or(OAuthCode::Other, |b| OAuthCode::parse(&b.error));
            Failure::OAuth(code)
        }
        status @ (429 | 500..=599) => Failure::Transient(Transient::Status(status)),
        status @ 300..=399 => Failure::Transient(Transient::Redirect(status)),
        _ => Failure::InvalidResponse,
    }
}

#[derive(Deserialize)]
struct TokenBody {
    access_token: String,
    token_type: String,
    expires_in: u64,
    refresh_token: Option<String>,
    scope: Option<String>,
}

impl Drop for TokenBody {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.access_token.zeroize();
        if let Some(token) = self.refresh_token.as_mut() {
            token.zeroize();
        }
    }
}

fn token_ok(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_TOKEN_BYTES
        && text.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// Classifies a token endpoint reply; `sent_at` is the request send time.
pub fn token_outcome(reply: Result<Reply, Failure>, sent_at: u64) -> Result<TokenSet, Failure> {
    let reply = reply?;
    if reply.status != 200 {
        return Err(classify_error(&reply));
    }
    let mut body: TokenBody =
        serde_json::from_slice(&reply.body).map_err(|_| Failure::InvalidResponse)?;
    let refresh_ok = body.refresh_token.as_deref().is_none_or(token_ok);
    let valid = token_ok(&body.access_token)
        && body.token_type.eq_ignore_ascii_case("bearer")
        && (1..=86_400).contains(&body.expires_in)
        && refresh_ok;
    if !valid {
        return Err(Failure::InvalidResponse);
    }
    Ok(TokenSet {
        access_token: Secret::new(std::mem::take(&mut body.access_token).into_bytes()),
        expires_at: sent_at + body.expires_in,
        refresh_token: body
            .refresh_token
            .take()
            .map(|t| Secret::new(t.into_bytes())),
        scope: body.scope.take(),
        obtained_at: sent_at,
    })
}
