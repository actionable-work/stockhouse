use crate::body::{self, Body};
use crate::config::TierName;
use crate::error::ProxyError;
use crate::lookup::Lookup;
use crate::nar;
use crate::server::LogSink;
use crate::upstream::Fetched;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use stockhouse_proto::{NixCacheInfo, StorePathHash};

/// Shared request-handling state.
pub(crate) struct State {
    pub(crate) lookup: Lookup,
    pub(crate) priority: u32,
    pub(crate) log: LogSink,
}

enum Route<'a> {
    CacheInfo,
    NarInfo(&'a str),
    Nar(&'a str),
    Unknown,
}

fn route(path: &str) -> Route<'_> {
    if path == "/nix-cache-info" {
        Route::CacheInfo
    } else if let Some(rest) = path.strip_prefix("/nar/") {
        Route::Nar(rest)
    } else if let Some(hash) = path
        .strip_prefix('/')
        .and_then(|p| p.strip_suffix(".narinfo"))
    {
        Route::NarInfo(hash)
    } else {
        Route::Unknown
    }
}

pub(crate) async fn handle<B>(state: &State, request: Request<B>) -> Response<Body> {
    let head = request.method() == Method::HEAD;
    if request.method() != Method::GET && !head {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    }
    match route(request.uri().path()) {
        Route::CacheInfo => without_body(head, cache_info(state.priority)),
        Route::NarInfo(hash) => without_body(head, narinfo(state, hash).await),
        Route::Nar(_) if head => status(StatusCode::METHOD_NOT_ALLOWED),
        Route::Nar(rest) => nar_route(state, rest).await,
        Route::Unknown => status(StatusCode::NOT_FOUND),
    }
}

fn status(code: StatusCode) -> Response<Body> {
    let mut response = Response::new(body::empty());
    *response.status_mut() = code;
    response
}

fn without_body(head: bool, response: Response<Body>) -> Response<Body> {
    if !head {
        return response;
    }
    let (parts, _) = response.into_parts();
    Response::from_parts(parts, body::empty())
}

fn text(content_type: &'static str, text: String) -> Response<Body> {
    let mut response = Response::new(body::full(text));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn cache_info(priority: u32) -> Response<Body> {
    let info = NixCacheInfo {
        want_mass_query: Some(true),
        priority: Some(priority),
    };
    text("text/x-nix-cache-info", info.render())
}

fn failure(state: &State, route: &str, error: &ProxyError) -> Response<Body> {
    (state.log)(&format!(
        "stockhouse-proxy: {route} {} {}",
        error.tier(),
        error.kind()
    ));
    let mut response = text("text/plain", format!("stockhouse-proxy: {error}\n"));
    *response.status_mut() = error.status();
    response
}

async fn narinfo(state: &State, hash: &str) -> Response<Body> {
    let Ok(hash) = StorePathHash::parse(hash) else {
        return status(StatusCode::BAD_REQUEST);
    };
    match state.lookup.find(&hash).await {
        Ok(Some((tier, info))) => {
            let url = format!("nar/{tier}/{hash}/{}", info.url);
            match info.with_url(&url) {
                Ok(served) => text("text/x-nix-narinfo", served.render()),
                Err(_) => status(StatusCode::BAD_GATEWAY),
            }
        }
        Ok(None) => status(StatusCode::NOT_FOUND),
        Err(error) => failure(state, "narinfo", &error),
    }
}

fn parse_nar_route(rest: &str) -> Option<(TierName, StorePathHash, &str)> {
    let (tier, rest) = rest.split_once('/')?;
    let (hash, url) = rest.split_once('/')?;
    Some((
        TierName::parse(tier)?,
        StorePathHash::parse(hash).ok()?,
        url,
    ))
}

async fn nar_route(state: &State, rest: &str) -> Response<Body> {
    let Some((name, hash, url)) = parse_nar_route(rest) else {
        return status(StatusCode::NOT_FOUND);
    };
    let Some(tier) = state.lookup.tier(name) else {
        return status(StatusCode::NOT_FOUND);
    };
    match nar_response(tier, &hash, url).await {
        Ok(response) => response,
        Err(error) => failure(state, "nar", &error),
    }
}

async fn nar_response(
    tier: &crate::lookup::Tier,
    hash: &StorePathHash,
    url: &str,
) -> Result<Response<Body>, ProxyError> {
    let missing = ProxyError::NarMissing { tier: tier.name() };
    let resolved = tier.resolve(hash).await?.ok_or_else(|| missing.clone())?;
    if resolved.narinfo.url != url {
        return Err(ProxyError::NarInfoChanged { tier: tier.name() });
    }
    let opened = tier
        .upstream()
        .open(url, resolved.auth.as_ref())
        .await
        .map_err(|e| tier.fail_upstream(e))?;
    let Fetched::Found((stream, upstream_len)) = opened else {
        return Err(missing);
    };
    let (stream, len) = match resolved.narinfo.served_integrity() {
        Some(expected) => (nar::verified(stream, expected), Some(expected.size)),
        None => (stream, upstream_len),
    };
    let mut response = Response::new(body::stream(stream));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-nix-nar"),
    );
    if let Some(len) = len {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    }
    Ok(response)
}
