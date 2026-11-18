#![allow(dead_code)]
//! Upload test fixtures: a fake store with a fake `nix`, a fake protocol v1
//! server and helpers. The bearer is a synthetic test value.

pub mod server;

#[allow(unused_imports)]
pub use server::{Action, FakeServer, Protocol, Request};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use stockhouse_credentials::StaticCredential;
use stockhouse_ingest::{EnqueueOutcome, Queue, UploadJob};
use stockhouse_proto::{Sha256, StorePath, nix32};
use stockhouse_upload::{ServerEndpoint, ServerUploader};

pub const TOKEN: &str = "synthetic-test-token";

pub fn temp_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("stockhouse-upload-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn store_path(label: &str) -> StorePath {
    let hash = nix32::encode(&Sha256::digest(label.as_bytes()).bytes()[..20]);
    StorePath::parse(&format!("/nix/store/{hash}-{label}")).unwrap()
}

const SCRIPT: &str = r#"#!/bin/sh
d="@DIR@"
printf '%s\n' "$*" >> "$d/args"
case "$3" in
path-info)
  b=${6##*/}; h=${b%%-*}
  if [ -f "$d/closure.code" ]; then exit "$(cat "$d/closure.code")"; fi
  if [ -f "$d/closure.sleep" ]; then echo $$ > "$d/closure.pid"; exec sleep 30; fi
  cat "$d/closure/$h.json" ;;
store)
  b=${5##*/}; h=${b%%-*}
  if [ -f "$d/dump.sleep" ]; then echo $$ > "$d/dump.pid"; exec sleep 30; fi
  cat "$d/nar/$h"
  if [ -f "$d/dump.code" ]; then exit "$(cat "$d/dump.code")"; fi ;;
esac
"#;

struct Entry {
    path: StorePath,
    nar: Vec<u8>,
    refs: Vec<StorePath>,
}

/// A fake store root, NAR fixtures and a fake `nix`.
pub struct Store {
    pub root: PathBuf,
    pub dir: PathBuf,
    pub nix: PathBuf,
    entries: Vec<Entry>,
}

impl Store {
    pub fn new() -> Self {
        let dir = temp_dir();
        let root = dir.join("root");
        for sub in ["closure", "nar"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let nix = dir.join("nix");
        std::fs::write(&nix, SCRIPT.replace("@DIR@", &dir.display().to_string())).unwrap();
        std::fs::set_permissions(&nix, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A parallel test may fork while the script is open for writing (ETXTBSY).
        while let Err(e) = std::process::Command::new(&nix).output() {
            assert_eq!(e.raw_os_error(), Some(26), "{e}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = std::fs::remove_file(dir.join("args"));
        Self {
            root,
            dir,
            nix,
            entries: Vec::new(),
        }
    }

    /// Adds a path with a NAR of `size` bytes and the given references.
    pub fn add_sized(&mut self, label: &str, refs: &[&StorePath], size: usize) -> StorePath {
        let path = store_path(label);
        let nar: Vec<u8> = (0..size)
            .map(|i| (i % 251) as u8 ^ label.len() as u8)
            .collect();
        std::fs::create_dir_all(self.on_disk(&path)).unwrap();
        std::fs::write(self.dir.join("nar").join(path.hash().as_str()), &nar).unwrap();
        self.entries.push(Entry {
            path: path.clone(),
            nar,
            refs: refs.iter().map(|r| (*r).clone()).collect(),
        });
        self.write_closures();
        path
    }

    pub fn add(&mut self, label: &str, refs: &[&StorePath]) -> StorePath {
        self.add_sized(label, refs, 1000 + label.len())
    }

    pub fn on_disk(&self, path: &StorePath) -> PathBuf {
        self.root.join(path.as_str().trim_start_matches('/'))
    }

    pub fn nar(&self, path: &StorePath) -> &[u8] {
        &self.entry(path).nar
    }

    fn entry(&self, path: &StorePath) -> &Entry {
        self.entries.iter().find(|e| &e.path == path).unwrap()
    }

    fn closure_of(&self, path: &StorePath) -> Vec<&Entry> {
        let mut out: Vec<&Entry> = Vec::new();
        let mut todo = vec![path.clone()];
        while let Some(p) = todo.pop() {
            if out.iter().any(|e| e.path == p) {
                continue;
            }
            let entry = self.entry(&p);
            todo.extend(entry.refs.iter().filter(|r| **r != p).cloned());
            out.push(entry);
        }
        out
    }

    /// The plain `nix path-info --json --recursive` output for a path.
    pub fn closure_json(&self, path: &StorePath) -> String {
        let items: Vec<String> = self
            .closure_of(path)
            .iter()
            .map(|e| {
                let refs: Vec<String> = e.refs.iter().map(|r| format!("\"{r}\"")).collect();
                format!(
                    "\"{}\":{{\"narHash\":\"{}\",\"narSize\":{},\"references\":[{}],\"deriver\":null,\"ca\":null,\"ultimate\":true,\"registrationTime\":1}}",
                    e.path,
                    Sha256::digest(&e.nar),
                    e.nar.len(),
                    refs.join(",")
                )
            })
            .collect();
        format!("{{{}}}", items.join(","))
    }

    fn write_closures(&self) {
        for e in &self.entries {
            let file = self
                .dir
                .join("closure")
                .join(format!("{}.json", e.path.hash()));
            std::fs::write(file, self.closure_json(&e.path)).unwrap();
        }
    }

    pub fn set(&self, name: &str, value: &str) {
        std::fs::write(self.dir.join(name), value).unwrap();
    }

    pub fn args(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("args"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub fn queue(&self) -> Queue {
        Queue::create(&self.dir.join("queue"), 64)
            .unwrap()
            .with_store_root(&self.root)
    }
}

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

/// Enqueues `path` and returns its job.
pub fn job(queue: &Queue, path: &StorePath) -> UploadJob {
    let id = match queue.producer().unwrap().enqueue(path, 0, false).unwrap() {
        EnqueueOutcome::Queued(id) | EnqueueOutcome::AlreadyQueued(id) => id,
        other => panic!("{other:?}"),
    };
    UploadJob {
        id,
        store_path: path.clone(),
        attempts: 0,
    }
}

pub fn wait_gone(pid_file: &Path) {
    let pid = std::fs::read_to_string(pid_file).unwrap();
    let proc = PathBuf::from(format!("/proc/{}", pid.trim()));
    for _ in 0..100 {
        if !proc.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("dump process {pid} still running");
}

/// A source that counts reads and fails from read `fail_at` (1-based) on;
/// `invalidate` returns `refreshes` and is counted.
pub struct CountingSource {
    pub reads: std::sync::Arc<AtomicUsize>,
    pub invalidations: AtomicUsize,
    pub fail_at: Option<usize>,
    pub error: stockhouse_credentials::CredentialError,
    pub refreshes: bool,
}

impl stockhouse_credentials::CredentialSource for CountingSource {
    fn credential(
        &self,
    ) -> Result<stockhouse_credentials::Secret, stockhouse_credentials::CredentialError> {
        let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_at.is_some_and(|at| n >= at) {
            return Err(self.error);
        }
        Ok(stockhouse_credentials::Secret::new(
            TOKEN.as_bytes().to_vec(),
        ))
    }

    fn invalidate(&self, rejected: &stockhouse_credentials::Secret) -> bool {
        assert_eq!(rejected.expose(), TOKEN.as_bytes());
        self.invalidations.fetch_add(1, Ordering::SeqCst);
        self.refreshes
    }
}

pub fn counting_source(
    fail_at: Option<usize>,
    error: stockhouse_credentials::CredentialError,
    refreshes: bool,
) -> std::sync::Arc<CountingSource> {
    std::sync::Arc::new(CountingSource {
        reads: std::sync::Arc::new(AtomicUsize::new(0)),
        invalidations: AtomicUsize::new(0),
        fail_at,
        error,
        refreshes,
    })
}

pub fn counting(
    fail_at: Option<usize>,
    error: stockhouse_credentials::CredentialError,
) -> (std::sync::Arc<CountingSource>, std::sync::Arc<AtomicUsize>) {
    let source = counting_source(fail_at, error, false);
    let reads = source.reads.clone();
    (source, reads)
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

pub fn entry_file(queue: &Queue, path: &StorePath) -> PathBuf {
    queue.root().join(format!("uploads/{}.json", path.hash()))
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
