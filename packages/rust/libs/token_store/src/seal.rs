use crate::{Binding, StoreError, TokenSet};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use stockhouse_credentials::{CredentialError, CredentialSource, SystemdCredential};
use zeroize::Zeroizing;

/// The sealed store's magic.
pub const MAGIC: &[u8; 8] = b"SHTKSL01";
const HEADER: usize = 8 + 32;
const NONCE: usize = 24;
const TAG: usize = 16;

/// The 32-byte sealing key, zeroed on drop.
pub struct Key(Zeroizing<[u8; 32]>);

impl Key {
    /// Reads 64 lowercase hex characters from a systemd credential.
    pub fn load(source: &SystemdCredential) -> Result<Self, StoreError> {
        let secret = source.credential().map_err(|e| match e {
            CredentialError::Empty | CredentialError::TooLarge | CredentialError::Malformed => {
                StoreError::KeyMalformed
            }
            _ => StoreError::KeyUnavailable,
        })?;
        let hex = secret.expose();
        let lower = |b: &u8| b.is_ascii_digit() || (b'a'..=b'f').contains(b);
        if hex.len() != 64 || !hex.iter().all(lower) {
            return Err(StoreError::KeyMalformed);
        }
        let mut key = Zeroizing::new([0u8; 32]);
        for (out, pair) in key.iter_mut().zip(hex.chunks(2)) {
            *out = (nibble(pair[0]) << 4) | nibble(pair[1]);
        }
        Ok(Self(key))
    }

    /// A key from raw bytes, for tests.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new_from_slice(self.0.as_slice()).expect("32-byte key")
    }
}

fn nibble(b: u8) -> u8 {
    if b.is_ascii_digit() {
        b - b'0'
    } else {
        b - b'a' + 10
    }
}

/// Seals a token set: magic, binding, a fresh nonce, then ciphertext.
pub(crate) fn seal(set: &TokenSet, key: &Key, binding: &Binding) -> Result<Vec<u8>, StoreError> {
    let mut nonce = [0u8; NONCE];
    crate::random(&mut nonce)?;
    let mut out = Vec::with_capacity(HEADER + NONCE + 512);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(binding.bytes());
    let plain = set.encode(binding);
    let payload = Payload {
        msg: plain.as_slice(),
        aad: &out[..HEADER],
    };
    let sealed = key
        .cipher()
        .encrypt(&XNonce::from(nonce), payload)
        .map_err(|_| StoreError::Corrupt)?;
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Opens a sealed store.
pub(crate) fn unseal(bytes: &[u8], key: &Key, binding: &Binding) -> Result<TokenSet, StoreError> {
    if bytes.len() < HEADER + NONCE + TAG || &bytes[..8] != MAGIC {
        return Err(StoreError::Corrupt);
    }
    if bytes[8..HEADER] != binding.bytes()[..] {
        return Err(StoreError::Mismatch);
    }
    let nonce: [u8; NONCE] = bytes[HEADER..HEADER + NONCE]
        .try_into()
        .expect("nonce size");
    let payload = Payload {
        msg: &bytes[HEADER + NONCE..],
        aad: &bytes[..HEADER],
    };
    let plain = key
        .cipher()
        .decrypt(&XNonce::from(nonce), payload)
        .map_err(|_| StoreError::Corrupt)?;
    // The header binding matched, so a different inner binding is damage.
    TokenSet::decode(&Zeroizing::new(plain), binding).map_err(|e| match e {
        StoreError::Mismatch => StoreError::Corrupt,
        other => other,
    })
}
