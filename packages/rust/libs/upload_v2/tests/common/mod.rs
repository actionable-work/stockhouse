#![allow(dead_code)]

use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use stockhouse_auth::Clock;
use stockhouse_credentials::{CredentialSource, StaticCredential};
use stockhouse_proto::{StorePath, UploadId};
use stockhouse_upload::{ServerEndpoint, ServerUploader};
use stockhouse_upload_v2::ServerBackend;
use stockhouse_wire::UploadDeclaration;
use stockhouse_wire_v2::{DeclareBatch, IdBatch, ProtocolMode};

#[allow(unused_imports)]
pub use stockhouse_test_support::{
    Action, FakeServer, Request, Store, TOKEN, entry_file, job, server, store_path, wait_gone,
};

pub fn source() -> Arc<dyn CredentialSource> {
    Arc::new(StaticCredential::new(TOKEN.as_bytes()))
}

pub fn backend(
    store: &Store,
    server: &FakeServer,
    mode: ProtocolMode,
    max_paths: usize,
    inflight: usize,
) -> ServerBackend {
    setup(store, server, mode, max_paths, inflight, source())
        .with_time(Arc::new(Tick::default()), Arc::new(|_| {}))
}

pub fn setup(
    store: &Store,
    server: &FakeServer,
    mode: ProtocolMode,
    max_paths: usize,
    inflight: usize,
    credential: Arc<dyn CredentialSource>,
) -> ServerBackend {
    let endpoint = ServerEndpoint::new(&server.base(), credential, 2).unwrap();
    ServerBackend::new(
        ServerUploader::new(endpoint, store.nix.clone(), max_paths, store.queue()),
        mode,
        inflight,
    )
}

#[derive(Default)]
pub struct Tick(pub std::sync::atomic::AtomicU64);

impl Clock for Tick {
    fn now(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub id: UploadId,
    pub path: Option<StorePath>,
}

impl Row {
    pub fn path(&self) -> &StorePath {
        self.path.as_ref().expect("upload has a known path")
    }

    pub fn state(&self, state: &str) -> Value {
        json!({"id": self.id.to_string(), "state": state})
    }

    pub fn error(&self, code: &str) -> Value {
        json!({"id": self.id.to_string(), "error": code})
    }

    pub fn missing(&self, names: &[&StorePath]) -> Value {
        let missing: Vec<_> = names.iter().map(|path| path.basename()).collect();
        json!({"id": self.id.to_string(), "error": "missing_references", "missing": missing})
    }
}

pub fn reply(rows: Vec<Value>) -> Action {
    Action::Reply(200, json!({"results": rows}).to_string())
}

pub fn rows_in(request: &Request, paths: &HashMap<UploadId, StorePath>) -> Vec<Row> {
    let body = request.body.as_deref().unwrap();
    match request.rel() {
        "v2/declare" => DeclareBatch::parse(body)
            .unwrap()
            .entries()
            .iter()
            .map(|entry| Row {
                id: entry.id,
                path: Some(entry.declaration.store_path().clone()),
            })
            .collect(),
        "v2/publish" | "v2/status" => IdBatch::parse(body)
            .unwrap()
            .entries()
            .iter()
            .map(|id| Row {
                id: *id,
                path: paths.get(id).cloned(),
            })
            .collect(),
        rel if rel.starts_with("v2/nar/") => {
            let id = UploadId::parse(rel.strip_prefix("v2/nar/").unwrap()).unwrap();
            vec![Row {
                id,
                path: paths.get(&id).cloned(),
            }]
        }
        _ => Vec::new(),
    }
}

#[derive(Default)]
struct State {
    names: HashMap<UploadId, StorePath>,
    verified: HashSet<UploadId>,
    published: HashSet<UploadId>,
}

pub fn fake(
    hook: impl Fn(&Request, &[Row]) -> Option<Action> + Send + Sync + 'static,
) -> FakeServer {
    fake_with(HashMap::new(), hook)
}

pub fn fake_with(
    known: HashMap<UploadId, StorePath>,
    hook: impl Fn(&Request, &[Row]) -> Option<Action> + Send + Sync + 'static,
) -> FakeServer {
    let state = Mutex::new(State {
        names: known,
        ..State::default()
    });
    FakeServer::start(move |request| {
        let body = request.body.as_deref()?;
        assert_eq!(
            request.header("Authorization"),
            Some(format!("Bearer {TOKEN}").as_str())
        );
        let rows = {
            let mut state = state.lock().unwrap();
            let rows = rows_in(request, &state.names);
            if request.rel() == "v2/declare" {
                for row in &rows {
                    state.names.insert(row.id, row.path().clone());
                }
            }
            rows
        };
        if let Some(action) = hook(request, &rows) {
            return Some(action);
        }
        let mut state = state.lock().unwrap();
        Some(match request.rel() {
            "protocol" => offer("[1,2]"),
            "v2/declare" | "v2/status" => reply(
                rows.iter()
                    .map(|row| {
                        if state.published.contains(&row.id) {
                            row.state("published")
                        } else if state.verified.contains(&row.id) {
                            row.state("verified")
                        } else if row.path.is_some() {
                            row.state("open")
                        } else {
                            row.error("unknown_upload")
                        }
                    })
                    .collect(),
            ),
            rel if rel.starts_with("v2/nar/") => {
                assert_eq!(
                    request.header("Content-Type"),
                    Some("application/x-nix-nar")
                );
                assert_eq!(
                    request
                        .header("Content-Length")
                        .unwrap()
                        .parse::<usize>()
                        .unwrap(),
                    body.len()
                );
                state.verified.insert(rows[0].id);
                server::state("verified")
            }
            "v2/publish" => reply(
                rows.iter()
                    .map(|row| {
                        state.published.insert(row.id);
                        row.state("published")
                    })
                    .collect(),
            ),
            rel if rel.starts_with("uploads/")
                && !rel.ends_with("/nar")
                && request.method == "POST" =>
            {
                UploadDeclaration::parse(body).unwrap();
                server::state("present")
            }
            _ => panic!("unexpected request: {}", request.rel()),
        })
    })
}

pub fn offer(versions: &str) -> Action {
    Action::Reply(200, format!(r#"{{"versions":{versions}}}"#))
}

pub fn nar_count(fake: &FakeServer) -> usize {
    fake.requests()
        .iter()
        .filter(|request| request.rel().starts_with("v2/nar/"))
        .count()
}

pub fn route_count(fake: &FakeServer, route: &str) -> usize {
    fake.requests()
        .iter()
        .filter(|request| request.rel() == route)
        .count()
}

pub fn mark(store: &Store, path: &StorePath) -> UploadId {
    let queue = store.queue();
    queue
        .mark_nar_sent(path, queue.upload_state(path).unwrap())
        .unwrap()
        .id
}

pub fn no_wait(_: Duration) {}
