use crate::narinfo::NarInfo;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::VerifyingKey;
use std::fmt;

/// Why a key, signature or trusted key set was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum KeyError {
    /// No `:` separator.
    Malformed,
    /// Empty name or a name containing whitespace.
    InvalidName,
    /// The payload is not padded standard base64.
    InvalidBase64,
    /// The payload has the wrong byte count.
    InvalidLength,
    /// The key bytes are not a valid Ed25519 point.
    InvalidKey,
    /// Two trusted keys share a name.
    DuplicateName(String),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => f.write_str("key or signature lacks a name separator"),
            Self::InvalidName => f.write_str("invalid key name"),
            Self::InvalidBase64 => f.write_str("invalid base64 payload"),
            Self::InvalidLength => f.write_str("payload has the wrong length"),
            Self::InvalidKey => f.write_str("not a valid Ed25519 key"),
            Self::DuplicateName(name) => write!(f, "duplicate trusted key name {name:?}"),
        }
    }
}

impl std::error::Error for KeyError {}

/// Why narinfo signature verification failed.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SignatureError {
    /// The narinfo has no `Sig`.
    Unsigned,
    /// No signature names a trusted key.
    UntrustedSigner,
    /// A signature naming a trusted key failed verification.
    InvalidSignature {
        /// The trusted key name.
        key: String,
    },
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsigned => f.write_str("narinfo is unsigned"),
            Self::UntrustedSigner => f.write_str("narinfo has no signature from a trusted key"),
            Self::InvalidSignature { key } => {
                write!(f, "invalid signature from trusted key {key:?}")
            }
        }
    }
}

impl std::error::Error for SignatureError {}

fn split_named<const N: usize>(text: &str) -> Result<(String, [u8; N]), KeyError> {
    let (name, payload) = text.split_once(':').ok_or(KeyError::Malformed)?;
    if name.is_empty() || name.chars().any(char::is_whitespace) {
        return Err(KeyError::InvalidName);
    }
    let bytes = STANDARD
        .decode(payload)
        .map_err(|_| KeyError::InvalidBase64)?;
    let bytes = <[u8; N]>::try_from(bytes).map_err(|_| KeyError::InvalidLength)?;
    Ok((name.to_owned(), bytes))
}

/// A named Ed25519 public key, `<name>:<base64>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicKey {
    name: String,
    key: VerifyingKey,
}

impl PublicKey {
    /// Parses `<name>:<base64 of 32 bytes>`.
    pub fn parse(text: &str) -> Result<Self, KeyError> {
        let (name, bytes) = split_named::<32>(text)?;
        let key = VerifyingKey::from_bytes(&bytes).map_err(|_| KeyError::InvalidKey)?;
        Ok(Self { name, key })
    }

    /// The key name.
    pub fn name(&self) -> &str {
        &self.name
    }

    fn verifies(&self, message: &[u8], signature: &Signature) -> bool {
        let sig = ed25519_dalek::Signature::from_bytes(&signature.bytes);
        self.key.verify_strict(message, &sig).is_ok()
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.name, STANDARD.encode(self.key.as_bytes()))
    }
}

/// A named Ed25519 signature, `<name>:<base64>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Signature {
    name: String,
    bytes: [u8; 64],
}

impl Signature {
    /// Parses `<name>:<base64 of 64 bytes>`.
    pub fn parse(text: &str) -> Result<Self, KeyError> {
        let (name, bytes) = split_named::<64>(text)?;
        Ok(Self { name, bytes })
    }

    /// Builds a signature from parts.
    pub fn from_parts(name: &str, bytes: [u8; 64]) -> Self {
        Self {
            name: name.to_owned(),
            bytes,
        }
    }

    /// The signing key name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The signature bytes.
    pub fn bytes(&self) -> &[u8; 64] {
        &self.bytes
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.name, STANDARD.encode(self.bytes))
    }
}

impl NarInfo {
    /// The Nix signature fingerprint, with references sorted bytewise.
    pub fn fingerprint(&self) -> String {
        let mut refs: Vec<&str> = self.references.iter().map(|p| p.as_str()).collect();
        refs.sort_unstable();
        let (path, hash, size) = (&self.store_path, &self.nar_hash, self.nar_size);
        format!("1;{path};{hash};{size};{}", refs.join(","))
    }
}

/// A set of trusted keys with unique names.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrustedKeys {
    keys: Vec<PublicKey>,
}

impl TrustedKeys {
    /// Builds a set, rejecting duplicate names.
    pub fn new(keys: Vec<PublicKey>) -> Result<Self, KeyError> {
        for (index, key) in keys.iter().enumerate() {
            if keys[..index].iter().any(|k| k.name == key.name) {
                return Err(KeyError::DuplicateName(key.name.clone()));
            }
        }
        Ok(Self { keys })
    }

    /// The trusted keys.
    pub fn keys(&self) -> &[PublicKey] {
        &self.keys
    }

    /// Verifies a narinfo, returning the name of the verifying key.
    pub fn verify(&self, narinfo: &NarInfo) -> Result<String, SignatureError> {
        let message = narinfo.fingerprint();
        let mut verified = None;
        for sig in &narinfo.signatures {
            let Some(key) = self.keys.iter().find(|k| k.name == sig.name) else {
                continue;
            };
            if !key.verifies(message.as_bytes(), sig) {
                return Err(SignatureError::InvalidSignature {
                    key: key.name.clone(),
                });
            }
            verified.get_or_insert_with(|| key.name.clone());
        }
        verified.ok_or_else(|| unverified(narinfo))
    }
}

fn unverified(narinfo: &NarInfo) -> SignatureError {
    if narinfo.signatures.is_empty() {
        SignatureError::Unsigned
    } else {
        SignatureError::UntrustedSigner
    }
}
