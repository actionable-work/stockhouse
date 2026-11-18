use crate::http::{Outcome, response, serve as serve_http};
use hyper::body::Incoming;
use hyper::{Request, header};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;

struct Shared {
    log: PathBuf,
    deny: Vec<String>,
    fail: Vec<String>,
}

/// Serves the shared-tier fixture.
pub(crate) async fn serve(
    listener: TcpListener,
    log: PathBuf,
    deny: Vec<String>,
    fail: Vec<String>,
) {
    let shared = Arc::new(Shared { log, deny, fail });
    serve_http(listener, move |request| {
        let shared = shared.clone();
        async move { shared.handle(&request) }
    })
    .await;
}

impl Shared {
    fn handle(&self, request: &Request<Incoming>) -> Outcome {
        if request.method() != hyper::Method::GET {
            return Ok(response(501, &[], ""));
        }
        let target = request.uri().path_and_query().map_or("/", |p| p.as_str());
        let authorization = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");
        let line = format!("{target} {authorization}\n");
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .and_then(|mut f| f.write_all(line.as_bytes()))?;
        let status = self.status(target);
        let body = if status == 200 {
            "StoreDir: /nix/store\n"
        } else {
            ""
        };
        Ok(response(status, &[], body))
    }

    fn status(&self, target: &str) -> u16 {
        let narinfo = |hashes: &[String]| hashes.iter().any(|h| target == format!("/{h}.narinfo"));
        if target == "/nix-cache-info" {
            200
        } else if narinfo(&self.deny) {
            403
        } else if narinfo(&self.fail) {
            500
        } else {
            404
        }
    }
}
