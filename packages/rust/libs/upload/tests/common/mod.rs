#![allow(dead_code)]
//! Upload test fixtures: a fake store and the protocol v1 server.

mod protocol;

#[allow(unused_imports)]
pub use protocol::Protocol;
use std::path::{Path, PathBuf};
use stockhouse_credentials::StaticCredential;
#[allow(unused_imports)]
pub use stockhouse_test_support::{
    Action, FakeServer, Request, TOKEN, counting, counting_source, server, store_path, temp_dir,
};
pub use stockhouse_test_support::{Store, entry_file, job, wait_gone};
use stockhouse_upload::{ServerEndpoint, ServerUploader};

pub fn endpoint(base: &str, timeout: u64) -> ServerEndpoint {
    ServerEndpoint::new(
        base,
        std::sync::Arc::new(StaticCredential::new(TOKEN.as_bytes())),
        timeout,
    )
    .unwrap()
}

pub fn uploader(store: &Store, server: &FakeServer) -> ServerUploader {
    ServerUploader::new(
        endpoint(&server.base(), 5),
        store.nix.clone(),
        50_000,
        store.queue(),
    )
}

pub fn uploader_with(
    store: &Store,
    server: &FakeServer,
    source: std::sync::Arc<dyn stockhouse_credentials::CredentialSource>,
) -> ServerUploader {
    let endpoint = ServerEndpoint::new(&server.base(), source, 5).unwrap();
    ServerUploader::new(endpoint, store.nix.clone(), 50_000, store.queue())
}

/// The store path named in a declaration request body.
pub fn declared(req: &Request) -> Option<String> {
    let body = req.body.as_deref()?;
    stockhouse_wire::UploadDeclaration::parse(body)
        .ok()
        .map(|d| d.store_path().to_string())
}

/// Writes a sign-in configuration for `tier` and returns its path.
pub fn auth_config(dir: &Path, tier: &str) -> PathBuf {
    let text = format!(
        r#"version = 1
server = "https://example.invalid/"
client_id = "stockhouse-cli"
scopes = ["cache:read", "cache:write"]
private_tier = "{tier}"

[endpoints]
authorization = "https://example.invalid/oauth/authorize"
token = "https://example.invalid/oauth/token"
device_authorization = "https://example.invalid/oauth/device"
revocation = "https://example.invalid/oauth/revoke"

[store]
mode = "file"
directory = "{}"
"#,
        dir.join("auth").display()
    );
    let path = dir.join("auth.toml");
    std::fs::write(&path, text).unwrap();
    path
}
