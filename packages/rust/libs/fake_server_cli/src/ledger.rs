use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

const ALPHABET: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// Nix's base-32 rendering of a digest, written independently of the client.
pub(crate) fn nix32(digest: &[u8]) -> String {
    (0..(digest.len() * 8).div_ceil(5))
        .rev()
        .map(|n| {
            let (i, j) = (n * 5 / 8, n * 5 % 8);
            let low = u32::from(digest[i]) >> j;
            let high = digest.get(i + 1).map_or(0, |&b| u32::from(b) << (8 - j));
            char::from(ALPHABET[((low | high) & 0x1f) as usize])
        })
        .collect()
}

/// The first 32 characters of a store path's base name.
pub(crate) fn store_hash(path: &str) -> String {
    base_name(path).chars().take(32).collect()
}

/// The last component of a store path.
pub(crate) fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// One upload: its declaration as received and its state.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Entry {
    pub(crate) decl: Value,
    pub(crate) state: String,
}

/// `state.json`: uploads by identity and the held store path base names.
#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct Ledger {
    pub(crate) uploads: BTreeMap<String, Entry>,
    pub(crate) held: Vec<String>,
}

/// The fake's directory: `state.json`, the logs and `control/`.
#[derive(Debug)]
pub(crate) struct Root(pub(crate) PathBuf);

impl Root {
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    pub(crate) fn load(&self) -> Ledger {
        std::fs::read(self.path("state.json"))
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save(&self, ledger: &Ledger) {
        let tmp = self.path("state.json.tmp");
        let data = serde_json::to_vec(ledger).expect("the ledger serializes");
        if std::fs::write(&tmp, data).is_ok() {
            let _ = std::fs::rename(&tmp, self.path("state.json"));
        }
    }

    /// Appends one line to `requests.log` (or another log).
    pub(crate) fn log_to(&self, name: &str, line: &str) {
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path(name))
            .and_then(|mut f| f.write_all(format!("{line}\n").as_bytes()));
    }

    pub(crate) fn log(&self, line: &str) {
        self.log_to("requests.log", line);
    }

    fn control_file(&self, name: &str) -> PathBuf {
        self.path("control").join(name)
    }

    /// Whether `control/<name>` exists.
    pub(crate) fn control(&self, name: &str) -> bool {
        self.control_file(name).exists()
    }

    pub(crate) fn clear(&self, name: &str) {
        let _ = std::fs::remove_file(self.control_file(name));
    }

    pub(crate) fn prepare(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.path("control"))
    }
}
