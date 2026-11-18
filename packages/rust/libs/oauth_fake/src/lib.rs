//! A loopback fake OAuth 2.0 server and private cache for component tests.
//!
//! Test code only: it implements the public RFC 6749, 7636, 7009 and 8628
//! behavior the client relies on, plus a bearer-protected cache prefix. It
//! is not a model of any real server. Every token is synthetic.

mod handlers;
mod state;

pub use state::{Recorded, Scripted, State};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::net::TcpListener;

/// A running fake server.
#[derive(Clone)]
pub struct FakeServer {
    /// `http://127.0.0.1:<port>/`.
    pub base: String,
    state: Arc<Mutex<State>>,
}

impl FakeServer {
    /// Starts the fake on an ephemeral loopback port in the current runtime.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("fake binds");
        let base = format!("http://{}/", listener.local_addr().expect("address"));
        let state = Arc::new(Mutex::new(State::default()));
        tokio::spawn(serve(listener, state.clone()));
        Self { base, state }
    }

    /// Starts the fake on its own thread and runtime, for synchronous tests.
    pub fn start_background() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("fake runtime");
            runtime.block_on(async move {
                tx.send(Self::start().await).expect("send fake");
                std::future::pending::<()>().await;
            });
        });
        rx.recv().expect("fake started")
    }

    /// The shared state, for scripting and assertions.
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// An absolute URL under the fake.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// A sign-in configuration TOML for this fake with the given store.
    pub fn config(&self, store: &str) -> String {
        format!(
            "version = 1\nserver = \"{b}\"\nclient_id = \"stockhouse-test\"\n\
scopes = [\"cache:read\", \"cache:write\"]\nprivate_tier = \"{b}t/1/\"\nrequest_timeout_secs = 2\n\
[endpoints]\nauthorization = \"{b}oauth/authorize\"\ntoken = \"{b}oauth/token\"\n\
device_authorization = \"{b}oauth/device\"\nrevocation = \"{b}oauth/revoke\"\n[store]\n{store}\n",
            b = self.base
        )
    }
}

async fn serve(listener: TcpListener, state: Arc<Mutex<State>>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let state = state.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                let state = state.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let body = body
                        .collect()
                        .await
                        .map(|b| b.to_bytes())
                        .unwrap_or_default();
                    let reply = handlers::handle(&state, &parts, &body).await;
                    Ok::<_, std::convert::Infallible>(reply.map(Full::<Bytes>::new))
                }
            });
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}
