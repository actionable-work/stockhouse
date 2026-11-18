use crate::http::{Outcome, read, response, serve as serve_http};
use crate::ledger::{Entry, Root, base_name, nix32, store_hash};
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::HeaderMap;
use hyper::{Request, Response, header};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::net::TcpListener;

const PREFIX: &str = "/t/1/";

type Reply = Response<Full<Bytes>>;

/// A declaration's fields the fake reads.
struct Declaration {
    raw: Value,
    store_path: String,
    references: Vec<String>,
}

impl Declaration {
    fn parse(data: &[u8]) -> Option<Self> {
        let raw: Value = serde_json::from_slice(data).ok()?;
        let store_path = raw.get("store_path")?.as_str()?.to_owned();
        let references = raw
            .get("references")?
            .as_array()?
            .iter()
            .map(|r| r.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            raw,
            store_path,
            references,
        })
    }
}

struct Upload {
    root: Root,
    token_file: PathBuf,
    lock: Mutex<()>,
}

/// Serves the protocol v1 fake with its state under `root`.
pub(crate) async fn serve(listener: TcpListener, root: PathBuf, token_file: PathBuf) {
    let root = Root(root);
    if let Err(e) = root.prepare() {
        eprintln!("stockhouse-fake-server: cannot prepare the root: {e}");
        return;
    }
    let fake = Arc::new(Upload {
        root,
        token_file,
        lock: Mutex::new(()),
    });
    serve_http(listener, move |request| {
        let fake = fake.clone();
        async move { fake.handle(request).await }
    })
    .await;
}

fn reply(status: u16, body: Option<Value>) -> Reply {
    let data = body.map(|v| v.to_string()).unwrap_or_default();
    response(
        status,
        &[
            ("content-type", "application/json"),
            ("cache-control", "no-store"),
            ("connection", "close"),
        ],
        data,
    )
}

fn error(code: &str) -> Option<Value> {
    Some(json!({ "error": code }))
}

fn state(name: &str) -> Option<Value> {
    Some(json!({ "state": name }))
}

/// Closes the connection without a reply.
fn hang_up(what: &str) -> io::Error {
    io::Error::other(what.to_owned())
}

/// Replies that need no bearer: `nix-cache-info` and every narinfo.
fn open_route(method: &str, rel: Option<&str>) -> Option<Reply> {
    match rel {
        Some("nix-cache-info") if method == "GET" => {
            Some(response(200, &[], "StoreDir: /nix/store\n"))
        }
        Some(rel) if rel.ends_with(".narinfo") => Some(reply(404, None)),
        _ => None,
    }
}

impl Upload {
    fn lock(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn handle(&self, request: Request<Incoming>) -> Outcome {
        let (parts, body) = request.into_parts();
        let method = parts.method.as_str();
        let rel = parts.uri.path().strip_prefix(PREFIX);
        if let Some(reply) = open_route(method, rel) {
            return Ok(reply);
        }
        if let Some(refused) = self.gate(method, rel, &parts.headers) {
            return Ok(refused);
        }
        let segments: Vec<&str> = rel.unwrap_or_default().split('/').collect();
        match (method, segments.as_slice()) {
            ("GET", ["uploads", id]) => Ok(self.status(id)),
            ("POST", ["uploads", id]) => {
                Ok(self.declare(id, &read(body).await.unwrap_or_default()))
            }
            ("PUT", ["leases", machine, slot]) => {
                Ok(self.lease(machine, slot, &read(body).await.unwrap_or_default()))
            }
            ("PUT", ["uploads", id, "nar"]) => self.nar(id, body).await,
            _ => Ok(reply(404, None)),
        }
    }

    fn authorized(&self, headers: &HeaderMap) -> bool {
        let Ok(token) = std::fs::read_to_string(&self.token_file) else {
            return false;
        };
        let expected = format!("Bearer {}", token.trim());
        headers
            .get(header::AUTHORIZATION)
            .is_some_and(|v| v.as_bytes() == expected.as_bytes())
    }

    /// The bearer check, then the outage control.
    fn gate(&self, method: &str, rel: Option<&str>, headers: &HeaderMap) -> Option<Reply> {
        let shown = rel.unwrap_or("None");
        if !self.authorized(headers) {
            self.root.log(&format!("{method} {shown} - 401"));
            return Some(reply(401, None));
        }
        if self.root.control("down") {
            self.root.log(&format!("{method} {shown} - 503"));
            return Some(reply(503, error("unavailable")));
        }
        None
    }

    fn status(&self, id: &str) -> Reply {
        let upload = {
            let _guard = self.lock();
            self.root.load().uploads.remove(id)
        };
        let Some(upload) = upload else {
            self.root.log(&format!("GET uploads/{id} - unknown_upload"));
            return reply(404, error("unknown_upload"));
        };
        let path = upload.decl["store_path"].as_str().unwrap_or_default();
        let line = format!("GET uploads/{id} {} {}", store_hash(path), upload.state);
        self.root.log(&line);
        reply(200, state(&upload.state))
    }

    fn declare(&self, id: &str, data: &[u8]) -> Reply {
        let Some(decl) = Declaration::parse(data) else {
            self.root.log(&format!("POST uploads/{id} - bad_request"));
            return reply(400, error("bad_request"));
        };
        let (result, status, body) = {
            let _guard = self.lock();
            self.decide(id, &decl)
        };
        let hash = store_hash(&decl.store_path);
        self.root.log(&format!("POST uploads/{id} {hash} {result}"));
        reply(status, body)
    }

    fn present(&self, held: &[String], base: &str) -> bool {
        held.iter().any(|h| h == base)
            || self.root.control(&format!("present-{}", store_hash(base)))
    }

    fn decide(&self, id: &str, decl: &Declaration) -> (&'static str, u16, Option<Value>) {
        let mut ledger = self.root.load();
        let base = base_name(&decl.store_path);
        if ledger
            .uploads
            .get(id)
            .is_some_and(|u| u.state == "published")
        {
            return ("published", 200, state("published"));
        }
        if self.present(&ledger.held, base) {
            return ("present", 200, state("present"));
        }
        if self.root.control("quota") {
            return ("quota_exceeded", 507, error("quota_exceeded"));
        }
        let missing: Vec<&String> = decl
            .references
            .iter()
            .filter(|r| r.as_str() != base && !self.present(&ledger.held, r))
            .collect();
        if !missing.is_empty() {
            let body = json!({ "error": "missing_references", "missing": missing });
            return ("missing_references", 422, Some(body));
        }
        ledger.uploads.entry(id.to_owned()).or_insert(Entry {
            decl: decl.raw.clone(),
            state: "open".to_owned(),
        });
        self.root.save(&ledger);
        ("open", 200, state("open"))
    }

    fn lease(&self, machine: &str, slot: &str, data: &[u8]) -> Reply {
        let path = serde_json::from_slice::<Value>(data)
            .ok()
            .and_then(|v| v.get("store_path")?.as_str().map(str::to_owned));
        let Some(path) = path else {
            self.root
                .log(&format!("PUT leases/{machine}/{slot} - bad_request"));
            return reply(400, error("bad_request"));
        };
        self.root
            .log_to("leases.log", &format!("{machine} {slot} {path}"));
        self.root
            .log(&format!("PUT leases/{machine}/{slot} - renewed"));
        reply(200, state("renewed"))
    }

    async fn nar(&self, id: &str, body: Incoming) -> Outcome {
        let upload = {
            let _guard = self.lock();
            self.root.load().uploads.remove(id)
        };
        let Some(upload) = upload else {
            let _ = read(body).await;
            self.root
                .log(&format!("PUT uploads/{id}/nar - unknown_upload"));
            return Ok(reply(404, error("unknown_upload")));
        };
        let path = upload.decl["store_path"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let hash = store_hash(&path);
        let log = |result: &str| {
            self.root
                .log(&format!("PUT uploads/{id}/nar {hash} {result}"))
        };
        let stall = format!("stall-{hash}");
        if self.root.control(&stall) {
            log("stalled");
            while self.root.control(&stall) {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        let Ok(data) = read(body).await else {
            log("aborted");
            return Err(hang_up("aborted"));
        };
        if self.root.control(&format!("reject-{hash}")) || !verifies(&upload.decl, &data) {
            log("verification_failed");
            return Ok(reply(400, error("verification_failed")));
        }
        self.publish(id, base_name(&path));
        let lost = format!("drop-{hash}");
        if self.root.control(&lost) {
            self.root.clear(&lost);
            log("dropped");
            return Err(hang_up("dropped"));
        }
        log("published");
        Ok(reply(200, state("published")))
    }

    fn publish(&self, id: &str, base: &str) {
        let _guard = self.lock();
        let mut ledger = self.root.load();
        if let Some(upload) = ledger.uploads.get_mut(id) {
            upload.state = "published".to_owned();
        }
        if !ledger.held.iter().any(|h| h == base) {
            ledger.held.push(base.to_owned());
        }
        self.root.save(&ledger);
    }
}

/// Whether the body has the declared size and `sha256:<nix32>` hash.
fn verifies(decl: &Value, data: &[u8]) -> bool {
    let digest = format!("sha256:{}", nix32(Sha256::digest(data).as_ref()));
    decl["nar_hash"].as_str() == Some(digest.as_str())
        && decl["nar_size"].as_u64() == Some(data.len() as u64)
}
