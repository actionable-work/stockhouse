//! The RFC 8252 loopback listener: one shot, bounded, static responses.

use crate::{Options, Pkce, SigninError};
use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode, header};
use hyper_util::rt::TokioIo;
use reqwest::Url;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use stockhouse_auth::{AuthConfig, OAuthCode, post_form, token_outcome};
use stockhouse_token_store::TokenSet;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::time::Instant;
use zeroize::Zeroizing;

/// How long the listener waits for the callback.
pub const LOGIN_WAIT: Duration = Duration::from_secs(300);

const DONE_PAGE: &str =
    "<!doctype html><title>Stockhouse</title><p>Sign-in finished. You can close this window.</p>\n";
const FAILED_PAGE: &str =
    "<!doctype html><title>Stockhouse</title><p>Sign-in failed. Return to the terminal.</p>\n";

type Outcome = Result<Zeroizing<String>, SigninError>;

/// A bound loopback login.
pub struct LoopbackLogin<'a> {
    config: &'a AuthConfig,
    pkce: Pkce,
    listener: TcpListener,
    redirect_uri: String,
}

/// Classifies a callback query against the expected `state`, returning the
/// HTTP status to answer and the outcome.
pub fn callback_outcome(query: &str, expected_state: &str) -> (u16, Outcome) {
    let pairs: Vec<(String, Zeroizing<String>)> = form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), Zeroizing::new(v.into_owned())))
        .collect();
    let field = |name: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    if field("state") != Some(expected_state) {
        return (400, Err(SigninError::StateMismatch));
    }
    if let Some(error) = field("error") {
        return (
            200,
            Err(SigninError::AuthorizationDenied(OAuthCode::parse(error))),
        );
    }
    match field("code") {
        Some(code) if stockhouse_token_store::valid_token(code.as_bytes()) => {
            (200, Ok(Zeroizing::new(code.to_owned())))
        }
        _ => (400, Err(SigninError::InvalidCallback)),
    }
}

fn page(status: u16, body: &'static str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
    *response.status_mut() = StatusCode::from_u16(status).expect("status");
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        "text/html; charset=utf-8".parse().expect("header"),
    );
    headers.insert(header::CACHE_CONTROL, "no-store".parse().expect("header"));
    headers.insert("referrer-policy", "no-referrer".parse().expect("header"));
    headers.insert(header::CONNECTION, "close".parse().expect("header"));
    response
}

fn respond<B>(
    request: &Request<B>,
    state: &str,
    slot: &Mutex<Option<Outcome>>,
) -> Response<Full<Bytes>> {
    if request.uri().path() != "/callback" {
        return page(404, FAILED_PAGE);
    }
    if request.method() != Method::GET {
        return page(405, FAILED_PAGE);
    }
    let (status, outcome) = callback_outcome(request.uri().query().unwrap_or_default(), state);
    let body = if outcome.is_ok() {
        DONE_PAGE
    } else {
        FAILED_PAGE
    };
    // The first callback decides; any later one is answered but ignored.
    slot.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert(outcome);
    page(status, body)
}

impl<'a> LoopbackLogin<'a> {
    /// Generates PKCE values and binds `127.0.0.1:0`.
    pub async fn start(config: &'a AuthConfig) -> Result<Self, SigninError> {
        let pkce = Pkce::generate()?;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| SigninError::Listener)?;
        let port = listener
            .local_addr()
            .map_err(|_| SigninError::Listener)?
            .port();
        Ok(Self {
            config,
            pkce,
            listener,
            redirect_uri: format!("http://127.0.0.1:{port}/callback"),
        })
    }

    /// The redirect URI.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// The authorization URL to open.
    pub fn authorization_url(&self) -> Url {
        let mut url = self.config.endpoints.authorization.clone();
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("scope", &self.config.scope())
            .append_pair("state", &self.pkce.state)
            .append_pair("code_challenge", &self.pkce.challenge)
            .append_pair("code_challenge_method", "S256");
        url
    }

    /// Prints the URL, waits for the callback and exchanges the code.
    pub async fn run<W: Write + ?Sized>(
        self,
        options: &Options,
        out: &mut W,
    ) -> Result<TokenSet, SigninError> {
        let _ = writeln!(
            out,
            "Open this URL in a browser on this machine to sign in:\n{}",
            self.authorization_url()
        );
        let _ = out.flush();
        let Self {
            config,
            pkce,
            listener,
            redirect_uri,
        } = self;
        let code = wait(listener, &pkce.state, options.wait).await?;
        let fields = [
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", config.client_id.as_str()),
            ("code_verifier", pkce.verifier.as_str()),
        ];
        let sent_at = options.clock.now();
        let reply = post_form(config, &config.endpoints.token, &fields).await;
        token_outcome(reply, sent_at).map_err(SigninError::TokenEndpoint)
    }
}

/// State shared by the listener's connections.
struct Shared {
    state: Arc<str>,
    slot: Mutex<Option<Outcome>>,
    done: Notify,
}

async fn serve(stream: TcpStream, shared: Arc<Shared>, deadline: Instant) {
    let inner = shared.clone();
    let service = service_fn(move |request: Request<hyper::body::Incoming>| {
        let response = respond(&request, &inner.state, &inner.slot);
        async move { Ok::<_, std::convert::Infallible>(response) }
    });
    let connection = http1::Builder::new()
        .keep_alive(false)
        .serve_connection(TokioIo::new(stream), service);
    let _ = tokio::time::timeout_at(deadline, connection).await;
    // Wake the waiter only once the callback's response has been written.
    if shared
        .slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some()
    {
        shared.done.notify_one();
    }
}

/// Serves connections concurrently until the first `/callback` has been
/// answered, then drops the listener (one shot).
async fn wait(listener: TcpListener, state: &str, wait: Duration) -> Outcome {
    let deadline = Instant::now() + wait;
    let shared = Arc::new(Shared {
        state: Arc::from(state),
        slot: Mutex::new(None),
        done: Notify::new(),
    });
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    return Err(SigninError::Listener);
                };
                tokio::spawn(serve(stream, shared.clone(), deadline));
            }
            () = shared.done.notified() => {
                let outcome = shared.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
                return outcome.unwrap_or(Err(SigninError::InvalidCallback));
            }
            () = tokio::time::sleep_until(deadline) => return Err(SigninError::LoginTimeout),
        }
    }
}
