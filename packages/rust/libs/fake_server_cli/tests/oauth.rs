//! `oauth` mode: device approval, lifetime enforcement, control routes and
//! the private cache directory.
mod common;

use common::{Reply, call, get, start, temp_dir};
use serde_json::Value;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

const FORM: (&str, &str) = ("Content-Type", "application/x-www-form-urlencoded");
const DEVICE_GRANT: &str = "urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code";

async fn post(addr: SocketAddr, path: &str, form: &str) -> Reply {
    call(addr, "POST", path, &[FORM], form.as_bytes())
        .await
        .expect("a reply")
}

async fn start_oauth(cache: &Path) -> SocketAddr {
    start(&[
        "oauth",
        "--listen",
        "127.0.0.1:0",
        "--cache-dir",
        cache.to_str().unwrap(),
    ])
    .await
}

async fn cached(addr: SocketAddr, rel: &str, access: &str) -> Reply {
    let bearer = format!("Bearer {access}");
    call(
        addr,
        "GET",
        &format!("/t/1/{rel}"),
        &[("Authorization", &bearer)],
        b"",
    )
    .await
    .expect("a reply")
}

/// Signs in through the device flow, approving with the control route.
async fn device_tokens(addr: SocketAddr) -> Value {
    let device = post(addr, "/oauth/device", "client_id=vm").await.json();
    let code = device["device_code"].as_str().unwrap().to_owned();
    let user = device["user_code"].as_str().unwrap().to_owned();
    let poll = format!("grant_type={DEVICE_GRANT}&device_code={code}");
    let pending = post(addr, "/oauth/token", &poll).await;
    assert_eq!(
        (pending.status, pending.json()["error"].as_str()),
        (400, Some("authorization_pending"))
    );
    get(addr, &format!("/__fake/approve?user_code={user}")).await;
    let tokens = post(addr, "/oauth/token", &poll).await;
    assert_eq!(tokens.status, 200, "{}", tokens.body);
    tokens.json()
}

fn token(tokens: &Value, name: &str) -> String {
    tokens[name].as_str().unwrap().to_owned()
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-006"))]
#[tokio::test(flavor = "multi_thread")]
async fn device_codes_wait_for_approval_at_the_fake_origin() {
    let addr = start_oauth(&temp_dir()).await;
    let device = post(addr, "/oauth/device", "client_id=vm").await.json();
    assert_eq!(device["verification_uri"], format!("http://{addr}/device"));
    let tokens = device_tokens(addr).await;
    assert_eq!(tokens["scope"], "cache:read");
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-006"))]
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_refreshes_of_one_token_are_overlaps() {
    let addr = start_oauth(&temp_dir()).await;
    let refresh = token(&device_tokens(addr).await, "refresh_token");
    let form = format!("grant_type=refresh_token&refresh_token={refresh}");
    let (a, b) = tokio::join!(
        post(addr, "/oauth/token", &form),
        post(addr, "/oauth/token", &form)
    );
    let mut statuses = [a.status, b.status];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 400]);
    let state = get(addr, "/__fake/state").await.json();
    assert_eq!(state["overlaps"], serde_json::json!([refresh]));
    assert_eq!(state["reuse"], serde_json::json!([refresh]));
    assert_eq!(state["refreshes"], 2);
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-003"))]
#[tokio::test(flavor = "multi_thread")]
async fn only_the_current_unexpired_access_token_reads_the_cache() {
    let addr = start_oauth(&temp_dir()).await;
    assert_eq!(get(addr, "/t/1/nix-cache-info").await.status, 401);
    let first = device_tokens(addr).await;
    let access = token(&first, "access_token");
    assert_eq!(cached(addr, "nix-cache-info", &access).await.status, 200);
    let form = format!(
        "grant_type=refresh_token&refresh_token={}",
        token(&first, "refresh_token")
    );
    let second = post(addr, "/oauth/token", &form).await.json();
    assert_eq!(
        cached(addr, "nix-cache-info", &access).await.status,
        401,
        "superseded"
    );
    let current = token(&second, "access_token");
    assert_eq!(cached(addr, "nix-cache-info", &current).await.status, 200);
    get(addr, "/__fake/config?lifetime=1").await;
    let short = post(
        addr,
        "/oauth/token",
        &format!(
            "grant_type=refresh_token&refresh_token={}",
            token(&second, "refresh_token")
        ),
    )
    .await
    .json();
    assert_eq!(short["expires_in"], 1);
    let short = token(&short, "access_token");
    assert_eq!(cached(addr, "nix-cache-info", &short).await.status, 200);
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(
        cached(addr, "nix-cache-info", &short).await.status,
        401,
        "expired"
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-004"))]
#[tokio::test(flavor = "multi_thread")]
async fn the_cache_directory_is_served_with_containment() {
    let root = temp_dir();
    let cache = root.join("cache");
    std::fs::create_dir_all(cache.join("nar")).unwrap();
    std::fs::write(cache.join("abc.narinfo"), "StorePath: x\n").unwrap();
    std::fs::write(cache.join("nar/abc.nar.xz"), "nar-bytes").unwrap();
    std::fs::write(cache.join("nix-cache-info"), "StoreDir: /elsewhere\n").unwrap();
    std::fs::write(root.join("secret"), "outside").unwrap();
    let addr = start_oauth(&cache).await;
    let access = token(&device_tokens(addr).await, "access_token");
    let info = cached(addr, "nix-cache-info", &access).await;
    assert_eq!(
        (info.status, info.body.as_str()),
        (200, "StoreDir: /nix/store\n")
    );
    let narinfo = cached(addr, "abc.narinfo", &access).await;
    assert_eq!(
        (narinfo.status, narinfo.body.as_str()),
        (200, "StorePath: x\n")
    );
    assert_eq!(
        cached(addr, "nar/abc.nar.xz", &access).await.body,
        "nar-bytes"
    );
    for rel in [
        "missing.narinfo",
        "../secret",
        "nar/../abc.narinfo",
        "./abc.narinfo",
        "nar//abc.nar.xz",
        "nar",
    ] {
        assert_eq!(cached(addr, rel, &access).await.status, 404, "{rel}");
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-005"))]
#[tokio::test(flavor = "multi_thread")]
async fn control_routes_revoke_configure_and_report() {
    let addr = start_oauth(&temp_dir()).await;
    let fresh = get(addr, "/__fake/state").await.json();
    assert_eq!(fresh["access"], Value::Null);
    assert_eq!(fresh["lifetime"], 600);
    let config = get(addr, "/__fake/config?lifetime=70").await.json();
    assert_eq!(config["lifetime"], 70);
    assert_eq!(get(addr, "/__fake/config?lifetime=soon").await.status, 400);
    let tokens = device_tokens(addr).await;
    assert_eq!(tokens["expires_in"], 70);
    let (access, refresh) = (
        token(&tokens, "access_token"),
        token(&tokens, "refresh_token"),
    );
    let state = get(addr, "/__fake/state").await.json();
    assert_eq!(state["access"], access.as_str());
    assert_eq!(state["refresh"], refresh.as_str());
    assert_eq!(state["issued"], serde_json::json!([access, refresh]));
    let revoked = post(addr, "/oauth/revoke", "token=not-a-token").await;
    assert_eq!(revoked.status, 200);
    let ended = get(addr, "/__fake/revoke").await.json();
    assert_eq!(
        (ended["access"].clone(), ended["refresh"].clone()),
        (Value::Null, Value::Null)
    );
    assert_eq!(ended["rotated"], serde_json::json!([refresh]));
    assert_eq!(ended["revoked"], serde_json::json!(["not-a-token"]));
    assert_eq!(ended["refreshes"], 0);
    assert_eq!(cached(addr, "nix-cache-info", &access).await.status, 401);
}
