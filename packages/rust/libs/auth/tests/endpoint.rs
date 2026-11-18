//! Token endpoint client: response validation and classification.

mod common;
use common::*;
use std::time::Duration;
use stockhouse_auth::{Failure, OAuthCode, Transient, block_on, post_form, token_outcome};
use stockhouse_credentials::CredentialSource;
use stockhouse_oauth_fake::Scripted;
use stockhouse_token_store::TokenSet;

fn exchange(s: &Setup, reply: Scripted) -> Result<TokenSet, Failure> {
    s.fake.state().script("/oauth/token", reply);
    let url = s.config.endpoints.token.clone();
    let fields = [
        ("grant_type", "refresh_token"),
        ("client_id", "stockhouse-test"),
    ];
    token_outcome(block_on(post_form(&s.config, &url, &fields)), NOW)
}

fn body(access: &str, token_type: &str, expires: &str) -> String {
    format!(r#"{{"access_token":"{access}","token_type":"{token_type}","expires_in":{expires}}}"#)
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-015"))]
#[test]
fn token_responses_are_validated() {
    let s = Setup::new();
    let ok = exchange(&s, Scripted::json(200, &body("tok-1", "bearer", "600"))).unwrap();
    assert_eq!(expose(&ok.access_token), "tok-1");
    assert_eq!((ok.obtained_at, ok.expires_at), (NOW, NOW + 600));
    let long = "a".repeat(4097);
    let invalid = [
        body("tok", "Bearer", "0"),
        body("tok", "Bearer", "86401"),
        body("tok", "Bearer", "\"600\""),
        r#"{"access_token":"tok","token_type":"Bearer"}"#.to_owned(),
        body("tok", "mac", "600"),
        body(&long, "Bearer", "600"),
        body("has space", "Bearer", "600"),
        "not json".to_owned(),
    ];
    for text in invalid {
        let result = exchange(&s, Scripted::json(200, &text)).map(|_| ());
        assert_eq!(result, Err(Failure::InvalidResponse), "{text}");
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-016"))]
#[test]
fn token_endpoint_failures_are_classified() {
    let s = Setup::new();
    let cases = [
        (
            Scripted::error(400, "invalid_grant"),
            Failure::OAuth(OAuthCode::InvalidGrant),
        ),
        (
            Scripted::error(401, "invalid_client"),
            Failure::OAuth(OAuthCode::InvalidClient),
        ),
        (
            Scripted::error(400, "made_up"),
            Failure::OAuth(OAuthCode::Other),
        ),
        (
            Scripted::json(400, "nope"),
            Failure::OAuth(OAuthCode::Other),
        ),
        (
            Scripted::json(429, ""),
            Failure::Transient(Transient::Status(429)),
        ),
        (
            Scripted::json(503, ""),
            Failure::Transient(Transient::Status(503)),
        ),
        (
            Scripted {
                location: Some(s.fake.url("elsewhere")),
                ..Scripted::json(302, "")
            },
            Failure::Transient(Transient::Redirect(302)),
        ),
        (Scripted::json(418, ""), Failure::InvalidResponse),
        (
            Scripted::json(200, &"x".repeat(70 * 1024)),
            Failure::InvalidResponse,
        ),
        (
            Scripted {
                delay: Duration::from_secs(4),
                ..Scripted::json(200, "")
            },
            Failure::Transient(Transient::Transport),
        ),
    ];
    for (reply, expected) in cases {
        assert_eq!(exchange(&s, reply).map(|_| ()), Err(expected));
    }
    assert!(
        s.fake.state().seen("/elsewhere").is_empty(),
        "redirect not followed"
    );
    let closed = stockhouse_auth::AuthConfig::parse(
        &s.fake
            .config("mode = \"file\"\ndirectory = \"/tmp/x\"")
            .replace(&s.fake.base, "http://127.0.0.1:9/"),
    )
    .unwrap();
    let url = closed.endpoints.token.clone();
    let result = token_outcome(block_on(post_form(&closed, &url, &[])), NOW).map(|_| ());
    assert_eq!(result, Err(Failure::Transient(Transient::Transport)));
    assert!(Failure::OAuth(OAuthCode::TemporarilyUnavailable).is_transient());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-017"))]
#[test]
fn endpoint_requests_carry_no_authorization_header() {
    let s = Setup::new();
    s.sign_in(NOW, NOW + 30);
    s.credential().credential().unwrap();
    stockhouse_auth::logout(&s.config).unwrap();
    let url = s.config.endpoints.device_authorization.clone();
    block_on(post_form(
        &s.config,
        &url,
        &[("client_id", "stockhouse-test")],
    ))
    .ok();
    let requests = s.fake.state().requests.clone();
    assert!(requests.len() >= 3);
    for request in requests {
        assert_eq!(request.authorization, None, "{}", request.path);
        assert_eq!(request.field("client_id"), Some("stockhouse-test"));
    }
}
