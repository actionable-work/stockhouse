//! A loopback HTTP fixture cache that records every request.

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;

#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    pub delay: Duration,
    pub location: Option<String>,
}

impl Reply {
    pub fn ok(body: impl Into<Vec<u8>>) -> Reply {
        Reply { status: 200, body: body.into(), delay: Duration::ZERO, location: None }
    }

    pub fn status(status: u16) -> Reply {
        Reply { status, body: Vec::new(), delay: Duration::ZERO, location: None }
    }
}

/// One recorded request: fixture name, path and Authorization header.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    pub fixture: String,
    pub path: String,
    pub authorization: Option<String>,
}

pub type Journal = Arc<Mutex<Vec<Seen>>>;

pub fn journal() -> Journal {
    Arc::new(Mutex::new(Vec::new()))
}

#[derive(Clone)]
pub struct Fixture {
    pub name: String,
    pub url: String,
    replies: Arc<Mutex<HashMap<String, Reply>>>,
    journal: Journal,
}

impl Fixture {
    pub async fn start(name: &str, journal: &Journal) -> Fixture {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let replies = Arc::new(Mutex::new(HashMap::new()));
        replies.lock().unwrap().insert("/nix-cache-info".to_owned(), Reply::ok("StoreDir: /nix/store\n"));
        let fixture = Fixture { name: name.to_owned(), url, replies, journal: journal.clone() };
        tokio::spawn(fixture.clone().serve(listener));
        fixture
    }

    pub fn set(&self, path: &str, reply: Reply) {
        self.replies.lock().unwrap().insert(format!("/{path}"), reply);
    }

    pub fn add(&self, entry: &super::Entry) {
        self.set(&entry.narinfo_path(), Reply::ok(entry.narinfo_text()));
        self.set(&entry.nar_url, Reply::ok(entry.nar.clone()));
    }

    /// Requests this fixture saw, excluding nix-cache-info.
    pub fn seen(&self) -> Vec<Seen> {
        let all = self.journal.lock().unwrap().clone();
        all.into_iter().filter(|s| s.fixture == self.name && s.path != "/nix-cache-info").collect()
    }

    /// Every request seen by every fixture sharing this journal, in order.
    pub fn journal_all(&self) -> Vec<Seen> {
        self.journal.lock().unwrap().clone()
    }

    pub fn seen_all(&self) -> Vec<Seen> {
        self.journal.lock().unwrap().iter().filter(|s| s.fixture == self.name).cloned().collect()
    }

    async fn serve(self, listener: TcpListener) {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let me = self.clone();
            tokio::spawn(async move {
                let service = service_fn(move |req| {
                    let me = me.clone();
                    async move { Ok::<_, std::convert::Infallible>(me.respond(req).await) }
                });
                let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
            });
        }
    }

    async fn respond<B>(&self, req: Request<B>) -> Response<Full<Bytes>> {
        let path = req.uri().path().to_owned();
        let authorization = req.headers().get("authorization").map(|v| v.to_str().unwrap().to_owned());
        self.journal.lock().unwrap().push(Seen { fixture: self.name.clone(), path: path.clone(), authorization });
        let reply = self.replies.lock().unwrap().get(&path).cloned().unwrap_or(Reply::status(404));
        tokio::time::sleep(reply.delay).await;
        let mut response = Response::new(Full::new(Bytes::from(reply.body)));
        *response.status_mut() = StatusCode::from_u16(reply.status).unwrap();
        if let Some(location) = reply.location {
            response.headers_mut().insert("location", location.parse().unwrap());
        }
        response
    }
}
