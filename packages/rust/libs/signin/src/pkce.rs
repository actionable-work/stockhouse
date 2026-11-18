use crate::SigninError;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Fresh PKCE and `state` values for one login.
pub struct Pkce {
    /// The `code_verifier`; never leaves the process except to the token
    /// endpoint.
    pub verifier: Zeroizing<String>,
    /// base64url(SHA-256(verifier)).
    pub challenge: String,
    /// The CSRF `state`.
    pub state: Zeroizing<String>,
}

fn random_b64() -> Result<Zeroizing<String>, SigninError> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    getrandom::fill(bytes.as_mut_slice()).map_err(|_| SigninError::Random)?;
    Ok(Zeroizing::new(URL_SAFE_NO_PAD.encode(bytes.as_slice())))
}

impl Pkce {
    /// Generates fresh values from the operating system's random source.
    pub fn generate() -> Result<Self, SigninError> {
        let verifier = random_b64()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Ok(Self {
            verifier,
            challenge,
            state: random_b64()?,
        })
    }
}
