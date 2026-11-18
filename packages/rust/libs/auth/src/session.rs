//! Login storage, logout and status.

use crate::client::{block_on, post_form};
use crate::outcome::{Failure, classify_error};
use crate::{AuthConfig, Clock};
use serde::Serialize;
use stockhouse_credentials::Secret;
use stockhouse_token_store::{StoreError, TokenSet};

/// How a logout ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogoutOutcome {
    /// No token set was stored; nothing was sent.
    NotSignedIn,
    /// The refresh token was revoked and the store deleted.
    Revoked,
    /// The store was deleted without revoking (no refresh token, or an
    /// untrusted store).
    NothingRevoked,
    /// The store was deleted but the revocation failed.
    RevocationFailed(Failure),
}

/// What happened to a replaced session when a login was stored.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Replaced {
    /// Nothing usable was replaced.
    Nothing,
    /// The previous refresh token was revoked.
    Revoked,
    /// Revoking the previous refresh token failed (a warning).
    RevocationFailed(Failure),
}

/// Revokes one refresh token (RFC 7009); only 200 counts.
pub async fn revoke(config: &AuthConfig, refresh_token: &Secret) -> Result<(), Failure> {
    let token =
        zeroize::Zeroizing::new(String::from_utf8_lossy(refresh_token.expose()).into_owned());
    let fields = [
        ("token", token.as_str()),
        ("token_type_hint", "refresh_token"),
        ("client_id", config.client_id.as_str()),
    ];
    let reply = post_form(config, &config.endpoints.revocation, &fields).await?;
    match reply.status {
        200 => Ok(()),
        _ => Err(classify_error(&reply)),
    }
}

fn untrusted(error: StoreError) -> bool {
    matches!(error, StoreError::Corrupt | StoreError::Mismatch)
}

/// Stores a new login under the lock, then revokes a replaced refresh token.
pub async fn store_login(config: &AuthConfig, set: &TokenSet) -> Result<Replaced, StoreError> {
    let store = config.store();
    let previous = {
        let lock = store.lock(config.lock_wait())?;
        let previous = match store.read() {
            Ok(previous) => previous,
            Err(error) if untrusted(error) => None,
            Err(error) => return Err(error),
        };
        store.write(&lock, set)?;
        previous
    };
    let replaced = previous.and_then(|p| p.refresh_token).filter(|old| {
        set.refresh_token
            .as_ref()
            .is_none_or(|new| new.expose() != old.expose())
    });
    let Some(old) = replaced else {
        return Ok(Replaced::Nothing);
    };
    Ok(match revoke(config, &old).await {
        Ok(()) => Replaced::Revoked,
        Err(failure) => Replaced::RevocationFailed(failure),
    })
}

/// Revokes the refresh token and deletes the store (RFC 7009).
pub fn logout(config: &AuthConfig) -> Result<LogoutOutcome, StoreError> {
    let store = config.store();
    let lock = store.lock(config.lock_wait())?;
    let set = match store.read() {
        Ok(None) => return Ok(LogoutOutcome::NotSignedIn),
        Ok(Some(set)) => Some(set),
        Err(error) if untrusted(error) => None,
        Err(error) => return Err(error),
    };
    let outcome = match set.as_ref().and_then(|s| s.refresh_token.as_ref()) {
        Some(token) => match block_on(revoke(config, token)) {
            Ok(()) => LogoutOutcome::Revoked,
            Err(failure) => LogoutOutcome::RevocationFailed(failure),
        },
        None => LogoutOutcome::NothingRevoked,
    };
    store.delete(&lock)?;
    Ok(outcome)
}

/// The session status printed by `stockhouse status --config`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Status {
    /// Whether a usable session is stored.
    pub signed_in: bool,
    /// Access token expiry, Unix seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// Seconds until expiry (0 when expired).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in_secs: Option<u64>,
    /// Whether a refresh token is stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refreshable: Option<bool>,
    /// The granted scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// `sealed` or `file`.
    pub store: &'static str,
    /// The store error kind, when it cannot be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'static str>,
}

/// Reads the store without the lock or network.
pub fn status(config: &AuthConfig, clock: &dyn Clock) -> Status {
    let store = config.store();
    let mut status = Status {
        signed_in: false,
        expires_at: None,
        expires_in_secs: None,
        refreshable: None,
        scope: None,
        store: store.mode_name(),
        error: None,
    };
    match store.read() {
        Ok(Some(set)) => {
            let now = clock.now();
            let refreshable = set.refresh_token.is_some();
            status.signed_in = now < set.expires_at || refreshable;
            status.expires_at = Some(set.expires_at);
            status.expires_in_secs = Some(set.expires_at.saturating_sub(now));
            status.refreshable = Some(refreshable);
            status.scope = set.scope.clone();
        }
        Ok(None) => {}
        Err(error) => status.error = Some(error.kind()),
    }
    status
}
