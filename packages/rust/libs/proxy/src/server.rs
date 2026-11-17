use crate::config::ProxyConfig;
use crate::error::ServeError;
use crate::lookup::Lookup;
use crate::routes::{self, State};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

/// Where proxy failure lines go.
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// A sink that writes each line to stderr.
pub fn stderr_log() -> LogSink {
    Arc::new(|line| eprintln!("{line}"))
}

/// A proxy bound to its loopback address, ready to serve.
pub struct BoundServer {
    listener: TcpListener,
    state: Arc<State>,
}

/// Binds exactly the configured address; an occupied port is an error.
pub async fn bind(config: &ProxyConfig, log: LogSink) -> Result<BoundServer, ServeError> {
    let addr = config.listen;
    let listener = TcpListener::bind(addr).await.map_err(|error| match error.kind() {
        ErrorKind::AddrInUse => ServeError::AddressInUse { addr },
        _ => ServeError::Bind { addr, error },
    })?;
    let state = State { lookup: Lookup::new(config)?, priority: config.priority, log };
    Ok(BoundServer { listener, state: Arc::new(state) })
}

impl BoundServer {
    /// The bound address.
    pub fn local_addr(&self) -> SocketAddr {
        self.listener.local_addr().expect("bound listener has an address")
    }

    /// Serves connections until the task is dropped.
    pub async fn run(self) {
        loop {
            let Ok((stream, _)) = self.listener.accept().await else { continue };
            let state = Arc::clone(&self.state);
            tokio::spawn(async move {
                let service = service_fn(move |request| {
                    let state = Arc::clone(&state);
                    async move { Ok::<_, std::convert::Infallible>(routes::handle(&state, request).await) }
                });
                let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
            });
        }
    }
}
