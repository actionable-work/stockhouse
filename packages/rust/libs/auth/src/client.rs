//! The endpoint HTTP client: form `POST`s with redirects disabled, bounded
//! bodies and no `Authorization` header.

use crate::AuthConfig;
use crate::outcome::{Failure, Transient};
use futures_util::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{Client, Url};
use std::future::Future;
use std::time::Duration;
use zeroize::Zeroizing;

/// The largest endpoint response body read.
pub const MAX_BODY: usize = 65_536;

/// A complete endpoint reply.
pub struct Reply {
    /// The HTTP status.
    pub status: u16,
    /// The body, zeroed on drop.
    pub body: Zeroizing<Vec<u8>>,
}

/// Builds `application/x-www-form-urlencoded` bytes, zeroed on drop.
pub fn form(fields: &[(&str, &str)]) -> Zeroizing<String> {
    let mut out = form_urlencoded::Serializer::new(String::new());
    for (name, value) in fields {
        out.append_pair(name, value);
    }
    Zeroizing::new(out.finish())
}

fn client(timeout: Duration, url: &Url) -> Result<Client, Failure> {
    let mut builder = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(timeout)
        .read_timeout(timeout)
        // A total bound too, so a slowly trickling reply cannot hold the
        // store lock indefinitely.
        .timeout(timeout);
    if url.scheme() == "http" {
        builder = builder.tls_certs_only(std::iter::empty::<reqwest::Certificate>());
    }
    builder
        .build()
        .map_err(|_| Failure::Transient(Transient::Transport))
}

/// `POST`s a form to `url` and reads the reply (at most [`MAX_BODY`] bytes).
///
/// The form body is zeroed here on drop, but `reqwest` takes its own copy of
/// the bytes for the request, which is not zeroed (as for the proxy's
/// `Authorization` header).
pub async fn post_form(
    config: &AuthConfig,
    url: &Url,
    fields: &[(&str, &str)],
) -> Result<Reply, Failure> {
    let body = form(fields);
    let response = client(config.timeout, url)?
        .post(url.clone())
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .body(body.as_bytes().to_vec())
        .send()
        .await
        .map_err(|_| Failure::Transient(Transient::Transport))?;
    let status = response.status().as_u16();
    let mut out = Zeroizing::new(Vec::new());
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Failure::Transient(Transient::Transport))?;
        if out.len() + chunk.len() > MAX_BODY {
            return Err(Failure::InvalidResponse);
        }
        out.extend_from_slice(&chunk);
    }
    Ok(Reply { status, body: out })
}

/// Runs `future` to completion on a scoped OS thread with its own
/// current-thread runtime, so it works from synchronous code and from a
/// blocking-pool thread of any runtime without nesting runtimes.
pub fn block_on<F>(future: F) -> F::Output
where
    F: Future + Send,
    F::Output: Send,
{
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("refresh runtime starts")
                    .block_on(future)
            })
            .join()
            .expect("refresh thread completes")
    })
}
