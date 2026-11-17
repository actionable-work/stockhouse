//! Signed narinfo and NAR fixtures.

use ed25519_dalek::{Signer, SigningKey};
use stockhouse_proto::{NarInfo, PublicKey, Sha256, Signature, nix32};

pub const KEY_NAME: &str = "stockhouse-test-1";

fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

pub fn key_text(name: &str, seed: u8) -> String {
    use base64::Engine;
    let bytes = signing_key(seed).verifying_key().to_bytes();
    format!("{name}:{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

pub fn fixture_key() -> PublicKey {
    PublicKey::parse(&key_text(KEY_NAME, 1)).unwrap()
}

/// A valid store path hash derived from a label.
pub fn hash_for(label: &str) -> String {
    nix32::encode(&Sha256::digest(label.as_bytes()).bytes()[..20])
}

/// One cache entry: a narinfo and the NAR bytes it describes.
#[derive(Clone)]
pub struct Entry {
    pub hash: String,
    pub nar_url: String,
    pub nar: Vec<u8>,
    pub info: NarInfo,
}

impl Entry {
    /// An uncompressed entry signed by the fixture key.
    pub fn new(label: &str, nar: &[u8]) -> Entry {
        Self::build(label, nar, "none", true, 1)
    }

    /// A compressed-label entry (bytes are opaque) with or without FileHash.
    pub fn compressed(label: &str, nar: &[u8], file_hash: bool) -> Entry {
        Self::build(label, nar, "xz", file_hash, 1)
    }

    fn build(label: &str, nar: &[u8], compression: &str, file_hash: bool, seed: u8) -> Entry {
        let hash = hash_for(label);
        let digest = Sha256::digest(nar);
        let nar_url = format!("nar/{hash}.nar");
        let file = if file_hash { format!("FileHash: {digest}\nFileSize: {}\n", nar.len()) } else { String::new() };
        let text = format!(
            "StorePath: /nix/store/{hash}-{label}\nURL: {nar_url}\nCompression: {compression}\n{file}\
NarHash: {digest}\nNarSize: {}\nReferences: \n",
            nar.len()
        );
        let mut info = NarInfo::parse(text.as_bytes()).unwrap();
        let entry = Entry { hash, nar_url, nar: nar.to_vec(), info: info.clone() };
        info.signatures.push(sign(&info, KEY_NAME, seed));
        Entry { info, ..entry }
    }

    /// The same entry re-signed by another key.
    pub fn signed_by(mut self, name: &str, seed: u8) -> Entry {
        self.info.signatures = vec![sign(&self.info, name, seed)];
        self
    }

    pub fn unsigned(mut self) -> Entry {
        self.info.signatures.clear();
        self
    }

    pub fn narinfo_text(&self) -> String {
        self.info.render()
    }

    pub fn narinfo_path(&self) -> String {
        format!("{}.narinfo", self.hash)
    }

    /// Writes the entry into a `file://` cache directory.
    pub fn write_to(&self, dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join("nar")).unwrap();
        std::fs::write(dir.join("nix-cache-info"), "StoreDir: /nix/store\n").unwrap();
        std::fs::write(dir.join(self.narinfo_path()), self.narinfo_text()).unwrap();
        std::fs::write(dir.join(&self.nar_url), &self.nar).unwrap();
    }
}

pub fn sign(info: &NarInfo, name: &str, seed: u8) -> Signature {
    let sig = signing_key(seed).sign(info.fingerprint().as_bytes());
    Signature::from_parts(name, sig.to_bytes())
}

/// An empty `file://` cache directory with nix-cache-info.
pub fn file_cache() -> (std::path::PathBuf, String) {
    let dir = super::temp_dir();
    std::fs::write(dir.join("nix-cache-info"), "StoreDir: /nix/store\n").unwrap();
    let url = format!("file://{}/", dir.display());
    (dir, url)
}
