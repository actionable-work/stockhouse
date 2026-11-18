use crate::state::{Recorded, Scripted, State};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use hyper::http::request::Parts;
use hyper::{Response, StatusCode, header};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

type Reply = Response<Bytes>;

fn lock(state: &Arc<Mutex<State>>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

fn reply(status: u16, body: impl Into<Bytes>) -> Reply {
    let mut response = Response::new(body.into());
    *response.status_mut() = StatusCode::from_u16(status).expect("status");
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/json".parse().expect("header"),
    );
    response
}

fn error(code: &str) -> Reply {
    reply(400, format!("{{\"error\":\"{code}\"}}"))
}

fn record(parts: &Parts, body: &[u8]) -> Recorded {
    let query = parts.uri.query().unwrap_or_default().as_bytes();
    let source = if parts.method == "POST" { body } else { query };
    Recorded {
        method: parts.method.to_string(),
        path: parts.uri.path().to_owned(),
        fields: form_urlencoded::parse(source).into_owned().collect(),
        authorization: parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    }
}

fn scripted(scripted: Scripted) -> Reply {
    let mut response = reply(scripted.status, scripted.body);
    if let Some(location) = scripted.location {
        response
            .headers_mut()
            .insert(header::LOCATION, location.parse().expect("location"));
    }
    response
}

pub(crate) async fn handle(state: &Arc<Mutex<State>>, parts: &Parts, body: &[u8]) -> Reply {
    let request = record(parts, body);
    let script = {
        let mut state = lock(state);
        state.requests.push(request.clone());
        state
            .scripted
            .get_mut(&request.path)
            .and_then(|queue| queue.pop_front())
    };
    if let Some(script) = script {
        tokio::time::sleep(script.delay).await;
        return scripted(script);
    }
    match request.path.as_str() {
        "/oauth/token" => token(state, &request).await,
        "/oauth/device" => device(&mut lock(state)),
        "/oauth/revoke" => revoke(&mut lock(state), &request),
        "/oauth/authorize" => authorize(&mut lock(state), &request),
        path => cache(&lock(state), path, &request),
    }
}

async fn token(state: &Arc<Mutex<State>>, request: &Recorded) -> Reply {
    match request.field("grant_type").unwrap_or_default() {
        "refresh_token" => refresh(state, request).await,
        "authorization_code" => code(&mut lock(state), request),
        "urn:ietf:params:oauth:grant-type:device_code" => device_poll(&mut lock(state), request),
        _ => error("unsupported_grant_type"),
    }
}

async fn refresh(state: &Arc<Mutex<State>>, request: &Recorded) -> Reply {
    let token = request
        .field("refresh_token")
        .unwrap_or_default()
        .to_owned();
    let delay = {
        let mut state = lock(state);
        let count = state.in_flight.entry(token.clone()).or_default();
        *count += 1;
        if *count > 1 {
            state.overlaps.push(token.clone());
        }
        state.refresh_delay
    };
    tokio::time::sleep(delay).await;
    let mut state = lock(state);
    *state.in_flight.entry(token.clone()).or_default() -= 1;
    if state.refresh.as_deref() == Some(token.as_str()) {
        return reply(200, state.issue());
    }
    if state.rotated.contains(&token) {
        state.reuse.push(token);
        state.revoke_family();
    }
    error("invalid_grant")
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn code(state: &mut State, request: &Recorded) -> Reply {
    let code = request.field("code").unwrap_or_default();
    let Some((expected, redirect)) = state.codes.remove(code) else {
        return error("invalid_grant");
    };
    let verifier = request.field("code_verifier").unwrap_or_default();
    let redirect_ok = request.field("redirect_uri") == Some(redirect.as_str());
    if challenge(verifier) != expected || !redirect_ok {
        return error("invalid_grant");
    }
    reply(200, state.issue())
}

fn device(state: &mut State) -> Reply {
    let device_code = state.next("fake-device");
    let user_code = format!("USER-{}", state.devices.len() + 1);
    let approved = state.auto_approve;
    state
        .devices
        .insert(device_code.clone(), (user_code.clone(), approved));
    let base = "http://127.0.0.1/device";
    reply(
        200,
        format!(
            "{{\"device_code\":\"{device_code}\",\"user_code\":\"{user_code}\",\"verification_uri\":\"{base}\",\
\"verification_uri_complete\":\"{base}?user_code={user_code}\",\"expires_in\":{},\"interval\":{}}}",
            state.device_expires, state.interval
        ),
    )
}

fn device_poll(state: &mut State, request: &Recorded) -> Reply {
    let code = request.field("device_code").unwrap_or_default();
    match state.devices.get(code).map(|(_, approved)| *approved) {
        Some(true) => {
            state.devices.remove(code);
            reply(200, state.issue())
        }
        Some(false) => error("authorization_pending"),
        None => error("expired_token"),
    }
}

fn revoke(state: &mut State, request: &Recorded) -> Reply {
    let token = request.field("token").unwrap_or_default();
    if state.refresh.as_deref() == Some(token) {
        state.revoke_family();
    }
    reply(200, "")
}

fn authorize(state: &mut State, request: &Recorded) -> Reply {
    let method_ok = request.field("code_challenge_method") == Some("S256");
    let (Some(redirect), Some(challenge), true) = (
        request.field("redirect_uri"),
        request.field("code_challenge"),
        method_ok,
    ) else {
        return reply(400, "");
    };
    let code = state.next("fake-code");
    state
        .codes
        .insert(code.clone(), (challenge.to_owned(), redirect.to_owned()));
    let state_param = request.field("state").unwrap_or_default();
    let location = format!("{redirect}?code={code}&state={state_param}");
    let mut response = reply(302, "");
    response
        .headers_mut()
        .insert(header::LOCATION, location.parse().expect("location"));
    response
}

fn cache(state: &State, path: &str, request: &Recorded) -> Reply {
    let Some(rest) = path.strip_prefix("/t/1/") else {
        return reply(404, "");
    };
    let expected = state.access.as_ref().map(|a| format!("Bearer {a}"));
    if expected.is_none() || request.authorization != expected {
        return reply(401, "");
    }
    match state.cache.get(rest) {
        Some(body) => reply(200, body.clone()),
        None if rest == "nix-cache-info" => reply(200, "StoreDir: /nix/store\n"),
        None => reply(404, ""),
    }
}
