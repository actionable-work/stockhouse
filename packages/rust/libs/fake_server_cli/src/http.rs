use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use std::future::Future;
use std::io;
use tokio::net::TcpListener;

/// A reply, or an error that closes the connection without replying.
pub(crate) type Outcome = Result<Response<Full<Bytes>>, io::Error>;

/// Serves every connection on `listener` with `handler`, forever.
pub(crate) async fn serve<H, F>(listener: TcpListener, handler: H)
where
    H: Fn(Request<Incoming>) -> F + Clone + Send + Sync + 'static,
    F: Future<Output = Outcome> + Send + 'static,
{
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let handler = handler.clone();
        tokio::spawn(async move {
            let service = service_fn(handler);
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}

/// The whole request body, or an error when it is cut short.
pub(crate) async fn read(body: Incoming) -> Result<Bytes, hyper::Error> {
    body.collect().await.map(|b| b.to_bytes())
}

/// A reply with the given status, headers and body.
pub(crate) fn response(
    status: u16,
    headers: &[(&str, &str)],
    body: impl Into<Bytes>,
) -> Response<Full<Bytes>> {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder
        .body(Full::new(body.into()))
        .expect("a static reply is valid")
}
