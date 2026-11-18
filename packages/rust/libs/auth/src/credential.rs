//! `ServerCredential`: the signed-in access token, refreshed before expiry
//! under the store lock.

use crate::client::{block_on, post_form};
use crate::outcome::{Failure, OAuthCode, token_outcome};
use crate::{AuthConfig, Clock};
use reqwest::Url;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use stockhouse_credentials::{CredentialError, CredentialSource, Secret};
use stockhouse_token_store::{Lock, Store, StoreError, TokenSet};

/// Refresh this many seconds before expiry.
pub const SKEW_SECS: u64 = 60;
/// Wait this long after a transient refresh failure.
pub const BACKOFF_SECS: u64 = 10;
/// Limits on automatic invalidation.
pub const INVALIDATE_GUARD_SECS: u64 = 60;

#[derive(Default)]
struct Memory {
    rejected: Option<[u8; 32]>,
    forced_at: Option<u64>,
    backoff_until: Option<u64>,
}

/// The current access token from the signed-in token store.
pub struct ServerCredential {
    config: Arc<AuthConfig>,
    store: Store,
    clock: Arc<dyn Clock>,
    memory: Mutex<Memory>,
    refresh: Mutex<()>,
}

fn digest(secret: &Secret) -> [u8; 32] {
    Sha256::digest(secret.expose()).into()
}

fn store_error(error: StoreError) -> CredentialError {
    error.credential_error()
}

impl ServerCredential {
    /// A credential over `config`'s store with the given clock.
    pub fn new(config: Arc<AuthConfig>, clock: Arc<dyn Clock>) -> Self {
        Self::with_store(config.store(), config, clock)
    }

    /// A credential over an explicit store (tests).
    pub fn with_store(store: Store, config: Arc<AuthConfig>, clock: Arc<dyn Clock>) -> Self {
        Self {
            config,
            store,
            clock,
            memory: Mutex::new(Memory::default()),
            refresh: Mutex::new(()),
        }
    }

    /// The private tier the tokens are for.
    pub fn private_tier(&self) -> &Url {
        &self.config.private_tier
    }

    fn memory(&self) -> std::sync::MutexGuard<'_, Memory> {
        self.memory.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn rejected(&self, set: &TokenSet) -> bool {
        let mut memory = self.memory();
        match memory.rejected {
            Some(hash) if hash == digest(&set.access_token) => true,
            Some(_) => {
                memory.rejected = None;
                false
            }
            None => false,
        }
    }

    fn fresh(&self, set: &TokenSet, now: u64) -> bool {
        now + SKEW_SECS < set.expires_at && !self.rejected(set)
    }

    fn usable(&self, set: &TokenSet, now: u64) -> bool {
        now < set.expires_at && !self.rejected(set)
    }

    fn read(&self) -> Result<TokenSet, CredentialError> {
        self.store
            .read()
            .map_err(store_error)?
            .ok_or(CredentialError::SignedOut)
    }

    /// Treats the stored access token as rejected, without the automatic
    /// limits, then resolves the credential (`stockhouse refresh`).
    pub fn force_refresh(&self) -> Result<Secret, CredentialError> {
        let set = self.read()?;
        self.memory().rejected = Some(digest(&set.access_token));
        self.credential()
    }

    fn refresh_locked(&self) -> Result<Secret, CredentialError> {
        let _guard = self.refresh.lock().unwrap_or_else(|e| e.into_inner());
        let lock = self
            .store
            .lock(self.config.lock_wait())
            .map_err(store_error)?;
        let set = self.read()?;
        let now = self.clock.now();
        if self.fresh(&set, now) {
            return Ok(set.access_token);
        }
        let Some(refresh_token) = set.refresh_token.as_ref() else {
            return if self.usable(&set, now) {
                Ok(set.access_token)
            } else {
                Err(CredentialError::SignedOut)
            };
        };
        if self.memory().backoff_until.is_some_and(|until| now < until) {
            return self.fallback(set, now);
        }
        let forced = self.rejected(&set);
        let outcome = self.send_refresh(refresh_token, now);
        self.settle(&lock, set, outcome, forced, now)
    }

    fn send_refresh(&self, refresh_token: &Secret, now: u64) -> Result<TokenSet, Failure> {
        let token = String::from_utf8_lossy(refresh_token.expose()).into_owned();
        let token = zeroize::Zeroizing::new(token);
        let fields = [
            ("grant_type", "refresh_token"),
            ("refresh_token", token.as_str()),
            ("client_id", self.config.client_id.as_str()),
        ];
        let url = &self.config.endpoints.token;
        token_outcome(block_on(post_form(&self.config, url, &fields)), now)
    }

    fn fallback(&self, set: TokenSet, now: u64) -> Result<Secret, CredentialError> {
        if self.usable(&set, now) {
            Ok(set.access_token)
        } else {
            Err(CredentialError::RefreshUnavailable)
        }
    }

    fn settle(
        &self,
        lock: &Lock,
        old: TokenSet,
        outcome: Result<TokenSet, Failure>,
        forced: bool,
        now: u64,
    ) -> Result<Secret, CredentialError> {
        match outcome {
            Ok(mut new) => {
                if new.refresh_token.is_none() {
                    new.refresh_token = old.refresh_token;
                }
                self.store
                    .write(lock, &new)
                    .map_err(|_| CredentialError::RefreshUnavailable)?;
                let mut memory = self.memory();
                memory.rejected = None;
                if forced {
                    memory.forced_at = Some(now);
                }
                Ok(new.access_token)
            }
            Err(Failure::OAuth(OAuthCode::InvalidGrant)) => {
                let _ = self.store.delete(lock);
                Err(CredentialError::SignedOut)
            }
            Err(failure) if failure.is_transient() => {
                self.memory().backoff_until = Some(now + BACKOFF_SECS);
                self.fallback(old, now)
            }
            Err(_) => Err(CredentialError::RefreshRejected),
        }
    }
}

impl ServerCredential {
    /// Waits for any refresh in progress (it holds the lock) and reports
    /// whether it replaced the rejected token: a server may stop accepting
    /// the old token before the refreshing process has stored the new one.
    fn replaced_after_lock(&self, rejected: &Secret) -> bool {
        let Ok(_lock) = self.store.lock(self.config.lock_wait()) else {
            return false;
        };
        self.read()
            .is_ok_and(|set| set.access_token.expose() != rejected.expose())
    }
}

impl CredentialSource for ServerCredential {
    fn credential(&self) -> Result<Secret, CredentialError> {
        let set = self.read()?;
        if self.fresh(&set, self.clock.now()) {
            return Ok(set.access_token);
        }
        self.refresh_locked()
    }

    fn invalidate(&self, rejected: &Secret) -> bool {
        let Ok(set) = self.read() else {
            return false;
        };
        if set.access_token.expose() != rejected.expose() {
            // Another process already replaced it; retrying uses the new one.
            return true;
        }
        let now = self.clock.now();
        let recently_forced = self
            .memory()
            .forced_at
            .is_some_and(|at| now < at + INVALIDATE_GUARD_SECS);
        let young = now < set.obtained_at + INVALIDATE_GUARD_SECS;
        if set.refresh_token.is_none() || young || recently_forced {
            return self.replaced_after_lock(rejected);
        }
        self.memory().rejected = Some(digest(rejected));
        true
    }
}
