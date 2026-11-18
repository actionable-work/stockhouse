//! Logout, status and secrecy of sign-in errors.

mod common;
use common::*;
use std::sync::Arc;
use std::time::Duration;
use stockhouse_auth::{Failure, LogoutOutcome, Transient, logout, status};
use stockhouse_credentials::{CredentialError, CredentialSource};
use stockhouse_oauth_fake::Scripted;
use stockhouse_token_store::FILE_NAME;

#[cfg_attr(any(), bloomery("CLIENT-CREDS-SESSION-001"))]
#[test]
fn logout_revokes_the_refresh_token_then_deletes() {
    let s = Setup::new();
    let (_, refresh) = s.sign_in(NOW, NOW + 600);
    assert_eq!(logout(&s.config), Ok(LogoutOutcome::Revoked));
    let revocations = s.fake.state().seen("/oauth/revoke");
    assert_eq!(revocations.len(), 1);
    assert_eq!(revocations[0].field("token"), Some(refresh.as_str()));
    assert_eq!(
        revocations[0].field("token_type_hint"),
        Some("refresh_token")
    );
    assert_eq!(revocations[0].field("client_id"), Some("stockhouse-test"));
    assert!(s.stored().is_none());
    assert!(s.fake.state().access.is_none(), "family ended");
    s.store("no-refresh-access", None, NOW, NOW + 600);
    assert_eq!(logout(&s.config), Ok(LogoutOutcome::NothingRevoked));
    assert_eq!(s.fake.state().seen("/oauth/revoke").len(), 1);
    assert!(s.stored().is_none());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-SESSION-002"))]
#[test]
fn logout_deletes_even_when_revocation_fails() {
    let s = Setup::new();
    let cases = [
        (
            Scripted::json(503, ""),
            Failure::Transient(Transient::Status(503)),
        ),
        (
            Scripted {
                location: Some(s.fake.url("x")),
                ..Scripted::json(302, "")
            },
            Failure::Transient(Transient::Redirect(302)),
        ),
        (
            Scripted {
                delay: Duration::from_secs(4),
                ..Scripted::json(200, "")
            },
            Failure::Transient(Transient::Transport),
        ),
    ];
    for (reply, failure) in cases {
        s.sign_in(NOW, NOW + 600);
        let before = s.fake.state().seen("/oauth/revoke").len();
        s.fake.state().script("/oauth/revoke", reply);
        assert_eq!(
            logout(&s.config),
            Ok(LogoutOutcome::RevocationFailed(failure))
        );
        assert!(s.stored().is_none());
        assert_eq!(s.fake.state().seen("/oauth/revoke").len(), before + 1);
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-SESSION-003"))]
#[test]
fn logout_without_tokens_contacts_nothing() {
    let s = Setup::new();
    assert_eq!(logout(&s.config), Ok(LogoutOutcome::NotSignedIn));
    assert!(s.fake.state().requests.is_empty());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-SESSION-004"))]
#[test]
fn logout_cannot_be_undone_by_a_concurrent_refresh() {
    let s = Arc::new(Setup::new());
    s.sign_in(NOW, NOW + 30);
    s.fake.state().script(
        "/oauth/revoke",
        Scripted {
            delay: Duration::from_millis(500),
            ..Scripted::json(200, "")
        },
    );
    let other = s.clone();
    let logout_thread = std::thread::spawn(move || logout(&other.config));
    std::thread::sleep(Duration::from_millis(200));
    let result = s.credential().credential().map(|_| ());
    assert_eq!(logout_thread.join().unwrap(), Ok(LogoutOutcome::Revoked));
    assert_eq!(result, Err(CredentialError::SignedOut));
    assert_eq!(s.token_requests(), 0);
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-SESSION-005"))]
#[test]
fn status_reports_the_session_without_tokens() {
    let s = Setup::new();
    let json = |s: &Setup| serde_json::to_string(&status(&s.config, s.clock.as_ref())).unwrap();
    assert_eq!(json(&s), r#"{"signed_in":false,"store":"file"}"#);
    let (access, refresh) = s.sign_in(NOW, NOW + 600);
    s.clock.set(NOW + 88);
    let text = json(&s);
    assert_eq!(
        text,
        format!(
            r#"{{"signed_in":true,"expires_at":{},"expires_in_secs":512,"refreshable":true,"store":"file"}}"#,
            NOW + 600
        )
    );
    assert!(!text.contains(&access) && !text.contains(&refresh));
    s.clock.set(NOW + 700);
    assert!(
        status(&s.config, s.clock.as_ref()).signed_in,
        "expired but refreshable"
    );
    s.store("expired", None, NOW, NOW + 600);
    assert!(!status(&s.config, s.clock.as_ref()).signed_in);
    std::fs::write(s.root.join("store").join(FILE_NAME), b"garbage").unwrap();
    assert_eq!(
        json(&s),
        r#"{"signed_in":false,"store":"file","error":"corrupt"}"#
    );
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-SESSION-006"))]
#[test]
fn logout_discards_an_untrusted_store_without_revoking() {
    let s = Setup::new();
    s.sign_in(NOW, NOW + 600);
    std::fs::write(s.root.join("store").join(FILE_NAME), b"garbage").unwrap();
    assert_eq!(logout(&s.config), Ok(LogoutOutcome::NothingRevoked));
    s.sign_in(NOW, NOW + 600);
    let other = s.fake.config(&format!(
        "mode = \"file\"\ndirectory = \"{}\"",
        s.root.join("store").display()
    ));
    let other =
        stockhouse_auth::AuthConfig::parse(&other.replace("stockhouse-test", "another-client"))
            .unwrap();
    assert_eq!(logout(&other), Ok(LogoutOutcome::NothingRevoked));
    assert!(s.fake.state().seen("/oauth/revoke").is_empty());
    assert!(!s.root.join("store").join(FILE_NAME).exists());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-SECRET-004"))]
#[test]
fn sign_in_errors_never_contain_secrets() {
    let s = Setup::new();
    s.sign_in(NOW, NOW + 30);
    s.clock.set(NOW + 31);
    let mut texts = Vec::new();
    for reply in [
        Scripted::error(400, "invalid_client"),
        Scripted::json(503, "{\"error\":\"canary-description-body\"}"),
        Scripted::error(400, "invalid_grant"),
    ] {
        s.fake.state().script("/oauth/token", reply);
        let error = s.credential().credential().unwrap_err();
        texts.push(format!("{error} {error:?}"));
    }
    s.sign_in(NOW, NOW + 600);
    s.fake
        .state()
        .script("/oauth/revoke", Scripted::error(400, "invalid_request"));
    let outcome = logout(&s.config).unwrap();
    texts.push(format!("{outcome:?}"));
    if let LogoutOutcome::RevocationFailed(failure) = outcome {
        texts.push(failure.to_string());
    }
    texts.push(format!("{:?}", s.config));
    for text in texts {
        s.assert_no_secret(&text);
    }
}
