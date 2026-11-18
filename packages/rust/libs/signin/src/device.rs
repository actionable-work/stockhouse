//! The RFC 8628 device authorization flow.

use crate::{Options, SigninError};
use serde::Deserialize;
use std::io::Write;
use std::time::Duration;
use stockhouse_auth::{
    AuthConfig, Failure, OAuthCode, classify_error, post_form, secure_url, token_outcome,
};
use stockhouse_token_store::TokenSet;
use zeroize::Zeroizing;

const SLOW_DOWN_SECS: u64 = 5;
const MAX_BACKOFF_SECS: u64 = 60;

#[derive(Deserialize)]
struct DeviceBody {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: Option<u64>,
}

impl Drop for DeviceBody {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.device_code.zeroize();
    }
}

struct Grant {
    device_code: Zeroizing<String>,
    interval: u64,
    deadline: u64,
}

/// A device login.
pub struct DeviceLogin<'a> {
    config: &'a AuthConfig,
    options: &'a Options,
}

/// A printable secure URL: every byte visible ASCII (no spaces, controls or
/// escape sequences reach the terminal) and `https://` or loopback `http://`.
fn uri_ok(text: &str) -> bool {
    text.bytes().all(|b| (0x21..=0x7e).contains(&b))
        && reqwest::Url::parse(text).is_ok_and(|url| secure_url(&url))
}

fn valid(body: &DeviceBody) -> bool {
    let user_ok = !body.user_code.is_empty()
        && body.user_code.len() <= 64
        && body.user_code.bytes().all(|b| (0x20..=0x7e).contains(&b));
    stockhouse_token_store::valid_token(body.device_code.as_bytes())
        && user_ok
        && uri_ok(&body.verification_uri)
        && body.verification_uri_complete.as_deref().is_none_or(uri_ok)
        && (1..=3600).contains(&body.expires_in)
        && body.interval.is_none_or(|i| (1..=60).contains(&i))
}

/// The interval after a transient failure: +5 s, capped at 60 s, and never
/// lowered.
pub(crate) fn backed_off(interval: u64) -> u64 {
    if interval >= MAX_BACKOFF_SECS {
        interval
    } else {
        (interval + SLOW_DOWN_SECS).min(MAX_BACKOFF_SECS)
    }
}

impl<'a> DeviceLogin<'a> {
    /// A device login over `config`.
    pub fn new(config: &'a AuthConfig, options: &'a Options) -> Self {
        Self { config, options }
    }

    async fn authorize(&self) -> Result<(Grant, DeviceBody), SigninError> {
        let scope = self.config.scope();
        let fields = [
            ("client_id", self.config.client_id.as_str()),
            ("scope", scope.as_str()),
        ];
        let url = &self.config.endpoints.device_authorization;
        let reply = post_form(self.config, url, &fields)
            .await
            .map_err(SigninError::TokenEndpoint)?;
        if reply.status != 200 {
            return Err(SigninError::TokenEndpoint(classify_error(&reply)));
        }
        let body: DeviceBody =
            serde_json::from_slice(&reply.body).map_err(|_| SigninError::DeviceResponse)?;
        if !valid(&body) {
            return Err(SigninError::DeviceResponse);
        }
        let grant = Grant {
            device_code: Zeroizing::new(body.device_code.clone()),
            interval: body.interval.unwrap_or(5),
            deadline: self.options.clock.now() + body.expires_in,
        };
        Ok((grant, body))
    }

    /// Prints the prompt and polls until the grant ends.
    pub async fn run<W: Write + ?Sized>(self, out: &mut W) -> Result<TokenSet, SigninError> {
        let (mut grant, body) = self.authorize().await?;
        let _ = writeln!(
            out,
            "To sign in, visit {}\nand enter the code: {}",
            body.verification_uri, body.user_code
        );
        if let Some(complete) = &body.verification_uri_complete {
            let _ = writeln!(out, "Or open {complete}");
        }
        let _ = out.flush();
        drop(body);
        loop {
            if self.options.clock.now() + grant.interval > grant.deadline {
                return Err(SigninError::DeviceExpired);
            }
            self.options
                .pacer
                .wait(Duration::from_secs(grant.interval))
                .await;
            match self.poll(&grant).await {
                Ok(set) => return Ok(set),
                Err(failure) => grant.interval = next_interval(failure, grant.interval)?,
            }
        }
    }

    async fn poll(&self, grant: &Grant) -> Result<TokenSet, Failure> {
        let fields = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", grant.device_code.as_str()),
            ("client_id", self.config.client_id.as_str()),
        ];
        let sent_at = self.options.clock.now();
        let reply = post_form(self.config, &self.config.endpoints.token, &fields).await;
        token_outcome(reply, sent_at)
    }
}

fn next_interval(failure: Failure, interval: u64) -> Result<u64, SigninError> {
    match failure {
        Failure::OAuth(OAuthCode::AuthorizationPending) => Ok(interval),
        Failure::OAuth(OAuthCode::SlowDown) => Ok(interval + SLOW_DOWN_SECS),
        Failure::OAuth(OAuthCode::AccessDenied) => Err(SigninError::DeviceDenied),
        Failure::OAuth(OAuthCode::ExpiredToken) => Err(SigninError::DeviceExpired),
        failure if failure.is_transient() => Ok(backed_off(interval)),
        failure => Err(SigninError::TokenEndpoint(failure)),
    }
}
