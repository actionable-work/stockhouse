#![allow(dead_code)]
//! Synthetic fixtures. Keys come from fixed public seeds and are trusted only here.

use ed25519_dalek::{Signer, SigningKey};
use stockhouse_proto::{NarInfo, PublicKey, Signature};

pub const HASH_A: &str = "pkf547jp8a92k4wmxikkyby58w10pysr";
pub const HASH_B: &str = "49ma9vimj6wlsbrhqsra6g6fzvsj6shh";
pub const HASH_C: &str = "m07h00fl6538s4gavrp66a20ka4hg7fy";
pub const EMPTY_SHA256: &str = "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73";

pub fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

pub fn public_key(name: &str, seed: u8) -> PublicKey {
    use base64::Engine;
    let bytes = signing_key(seed).verifying_key().to_bytes();
    let text = format!("{name}:{}", base64::engine::general_purpose::STANDARD.encode(bytes));
    PublicKey::parse(&text).expect("fixture key parses")
}

pub fn sign(narinfo: &NarInfo, name: &str, seed: u8) -> Signature {
    let sig = signing_key(seed).sign(narinfo.fingerprint().as_bytes());
    Signature::from_parts(name, sig.to_bytes())
}

/// A minimal valid narinfo body without signatures.
pub fn unsigned_text() -> String {
    format!(
        "StorePath: /nix/store/{HASH_A}-hello-1.0\nURL: nar/{HASH_A}.nar.xz\nCompression: xz\n\
FileHash: {EMPTY_SHA256}\nFileSize: 120\nNarHash: {EMPTY_SHA256}\nNarSize: 400\n\
References: {HASH_C}-glibc {HASH_B}-ncurses\nDeriver: {HASH_B}-hello-1.0.drv\n"
    )
}

pub fn signed(name: &str, seed: u8) -> NarInfo {
    let mut info = NarInfo::parse(unsigned_text().as_bytes()).expect("fixture parses");
    let sig = sign(&info, name, seed);
    info.signatures.push(sig);
    info
}
