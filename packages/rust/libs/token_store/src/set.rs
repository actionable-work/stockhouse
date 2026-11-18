use crate::StoreError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use stockhouse_credentials::Secret;
use zeroize::{Zeroize, Zeroizing};

/// The longest access or refresh token accepted.
pub const MAX_TOKEN_BYTES: usize = 4096;

/// Binds a store to one token endpoint, client ID and private tier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Binding(pub(crate) [u8; 32]);

impl Binding {
    /// The SHA-256 binding of the normalized configuration values.
    pub fn new(token_endpoint: &str, client_id: &str, private_tier: &str) -> Self {
        let mut hash = Sha256::new();
        for part in [
            "stockhouse-token-binding-v1",
            token_endpoint,
            client_id,
            private_tier,
        ] {
            hash.update(part.as_bytes());
            hash.update(b"\n");
        }
        Self(hash.finalize().into())
    }

    /// The raw digest.
    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// One signed-in session. Tokens are redacted in `Debug`.
pub struct TokenSet {
    /// The opaque access token.
    pub access_token: Secret,
    /// Unix seconds when the access token expires.
    pub expires_at: u64,
    /// The refresh token, when the server issued one.
    pub refresh_token: Option<Secret>,
    /// The granted scope string.
    pub scope: Option<String>,
    /// Unix seconds when the request that produced it was sent.
    pub obtained_at: u64,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &self.access_token)
            .field("expires_at", &self.expires_at)
            .field("refresh_token", &self.refresh_token)
            .field("scope", &self.scope)
            .field("obtained_at", &self.obtained_at)
            .finish()
    }
}

/// Whether bytes are a valid token: 1–4096 visible ASCII.
pub fn valid_token(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes.len() <= MAX_TOKEN_BYTES
        && bytes.iter().all(|b| (0x21..=0x7e).contains(b))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    version: u32,
    binding: String,
    access_token: String,
    expires_at: u64,
    refresh_token: Option<String>,
    scope: Option<String>,
    obtained_at: u64,
}

impl Drop for Raw {
    fn drop(&mut self) {
        self.access_token.zeroize();
        if let Some(token) = self.refresh_token.as_mut() {
            token.zeroize();
        }
    }
}

fn text(secret: &Secret) -> String {
    String::from_utf8_lossy(secret.expose()).into_owned()
}

impl TokenSet {
    /// The JSON form, zeroed on drop.
    pub(crate) fn encode(&self, binding: &Binding) -> Zeroizing<Vec<u8>> {
        let raw = Raw {
            version: 1,
            binding: binding.hex(),
            access_token: text(&self.access_token),
            expires_at: self.expires_at,
            refresh_token: self.refresh_token.as_ref().map(text),
            scope: self.scope.clone(),
            obtained_at: self.obtained_at,
        };
        Zeroizing::new(serde_json::to_vec(&raw).expect("token set serializes"))
    }

    /// Parses and validates the JSON form.
    pub(crate) fn decode(bytes: &[u8], binding: &Binding) -> Result<Self, StoreError> {
        let mut raw: Raw = serde_json::from_slice(bytes).map_err(|_| StoreError::Corrupt)?;
        let refresh_ok = raw
            .refresh_token
            .as_ref()
            .is_none_or(|t| valid_token(t.as_bytes()));
        if raw.version != 1 || !valid_token(raw.access_token.as_bytes()) || !refresh_ok {
            return Err(StoreError::Corrupt);
        }
        if raw.binding != binding.hex() {
            return Err(StoreError::Mismatch);
        }
        Ok(Self {
            access_token: Secret::new(std::mem::take(&mut raw.access_token).into_bytes()),
            expires_at: raw.expires_at,
            refresh_token: raw
                .refresh_token
                .take()
                .map(|t| Secret::new(t.into_bytes())),
            scope: raw.scope.take(),
            obtained_at: raw.obtained_at,
        })
    }
}
