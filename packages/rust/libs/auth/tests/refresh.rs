//! Refresh behavior of `ServerCredential` against the fake server.

mod common;
use common::*;
use std::sync::Arc;
use std::time::Duration;
use stockhouse_auth::{AuthConfig, AuthConfigError, ServerCredential, Source, source};
use stockhouse_credentials::{CredentialError, CredentialSource, CredentialSpec, Secret};
use stockhouse_oauth_fake::Scripted;

fn secret(text: &str) -> Secret {
    Secret::new(text.as_bytes().to_vec())
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-001"))]
#[test]
fn fresh_token_is_returned_without_network() {
    let s = Setup::new();
    let (access, _) = s.sign_in(NOW, NOW + 600);
    let _ = std::fs::remove_file(s.root.join("store/lock"));
    assert_eq!(expose(&s.credential().credential().unwrap()), access);
    assert_eq!(s.token_requests(), 0);
    assert!(!s.root.join("store/lock").exists(), "no lock taken");
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-002"))]
#[test]
fn store_is_re_read_on_every_use() {
    let s = Setup::new();
    s.sign_in(NOW, NOW + 600);
    let credential = s.credential();
    credential.credential().unwrap();
    s.store(
        "replaced-access-1",
        Some("replaced-refresh-1"),
        NOW,
        NOW + 600,
    );
    assert_eq!(
        expose(&credential.credential().unwrap()),
        "replaced-access-1"
    );
    let store = s.config.store();
    store.delete(&store.lock(Duration::ZERO).unwrap()).unwrap();
    assert_eq!(
        credential.credential().map(|_| ()),
        Err(CredentialError::SignedOut)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-003"))]
#[test]
fn refresh_happens_before_expiry() {
    for now in [NOW + 540, NOW + 700] {
        let s = Setup::new();
        let (old, refresh) = s.sign_in(NOW, NOW + 600);
        s.clock.set(now);
        let new = expose(&s.credential().credential().unwrap());
        assert_ne!(new, old);
        let requests = s.fake.state().seen("/oauth/token");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].field("grant_type"), Some("refresh_token"));
        assert_eq!(requests[0].field("refresh_token"), Some(refresh.as_str()));
        assert_eq!(requests[0].field("client_id"), Some("stockhouse-test"));
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-004"))]
#[test]
fn refresh_by_another_process_is_reused() {
    let s = Arc::new(Setup::new());
    s.sign_in(NOW, NOW + 30);
    let store = s.config.store();
    let lock = store.lock(Duration::ZERO).unwrap();
    let other = s.clone();
    let waiter = std::thread::spawn(move || other.credential().credential().map(|c| expose(&c)));
    std::thread::sleep(Duration::from_millis(300));
    s.store_with(&lock, "other-process-access", NOW + 600);
    drop(lock);
    assert_eq!(waiter.join().unwrap().unwrap(), "other-process-access");
    assert_eq!(s.token_requests(), 0);
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-005"))]
#[test]
fn no_refresh_without_the_lock() {
    let s = Setup::new();
    s.sign_in(NOW, NOW + 30);
    let store = s.config.store();
    let _held = store.lock(Duration::ZERO).unwrap();
    assert_eq!(
        s.credential().credential().map(|_| ()),
        Err(CredentialError::RefreshUnavailable)
    );
    assert_eq!(s.token_requests(), 0);
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-006"))]
#[test]
fn refreshed_tokens_are_stored_before_use() {
    let s = Setup::new();
    s.sign_in(NOW, NOW + 30);
    let new = expose(&s.credential().credential().unwrap());
    let stored = s.stored().unwrap();
    assert_eq!(expose(&stored.access_token), new);
    let current = s.fake.state().refresh.clone().unwrap();
    assert_eq!(expose(stored.refresh_token.as_ref().unwrap()), current);
    assert_eq!(stored.expires_at, NOW + 600);
    // A response without a refresh token keeps the previous one.
    s.clock.set(NOW + 590);
    s.fake.state().script(
        "/oauth/token",
        Scripted::json(
            200,
            r#"{"access_token":"no-rotation-access","token_type":"bearer","expires_in":600}"#,
        ),
    );
    assert_eq!(
        expose(&s.credential().credential().unwrap()),
        "no-rotation-access"
    );
    let stored = s.stored().unwrap();
    assert_eq!(expose(stored.refresh_token.as_ref().unwrap()), current);
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-007"))]
#[test]
fn invalid_grant_signs_the_client_out() {
    let s = Setup::new();
    s.sign_in(NOW, NOW + 30);
    s.fake.state().revoke_family();
    let credential = s.credential();
    assert_eq!(
        credential.credential().map(|_| ()),
        Err(CredentialError::SignedOut)
    );
    assert!(s.stored().is_none());
    assert_eq!(
        credential.credential().map(|_| ()),
        Err(CredentialError::SignedOut)
    );
    assert_eq!(s.token_requests(), 1);
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-008"))]
#[test]
fn transient_refresh_failures_keep_the_tokens() {
    let replies = [
        Scripted::json(503, ""),
        Scripted::error(400, "temporarily_unavailable"),
        Scripted {
            location: Some("https://elsewhere.example.invalid/".to_owned()),
            ..Scripted::json(302, "")
        },
        Scripted {
            delay: Duration::from_secs(4),
            ..Scripted::json(200, "")
        },
    ];
    for reply in replies {
        let s = Setup::new();
        let (old, refresh) = s.sign_in(NOW, NOW + 30);
        s.fake.state().script("/oauth/token", reply);
        assert_eq!(expose(&s.credential().credential().unwrap()), old);
        let stored = s.stored().unwrap();
        assert_eq!(expose(stored.refresh_token.as_ref().unwrap()), refresh);
        s.fake
            .state()
            .script("/oauth/token", Scripted::json(503, ""));
        s.clock.set(NOW + 31);
        assert_eq!(
            s.credential().credential().map(|_| ()),
            Err(CredentialError::RefreshUnavailable)
        );
        assert!(s.stored().is_some());
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-009"))]
#[test]
fn other_refresh_refusals_keep_the_tokens() {
    for reply in [
        Scripted::error(401, "invalid_client"),
        Scripted::json(
            200,
            r#"{"access_token":"x","token_type":"mac","expires_in":5}"#,
        ),
    ] {
        let s = Setup::new();
        s.sign_in(NOW, NOW + 30);
        s.fake.state().script("/oauth/token", reply);
        assert_eq!(
            s.credential().credential().map(|_| ()),
            Err(CredentialError::RefreshRejected)
        );
        assert!(s.stored().is_some());
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-010"))]
#[test]
fn tokens_without_a_refresh_token_are_not_refreshed() {
    let s = Setup::new();
    s.store("lonely-access", None, NOW - 700, NOW - 100);
    assert_eq!(
        s.credential().credential().map(|_| ()),
        Err(CredentialError::SignedOut)
    );
    s.store("lonely-access", None, NOW - 100, NOW + 500);
    let credential = s.credential();
    assert_eq!(expose(&credential.credential().unwrap()), "lonely-access");
    assert_eq!(
        credential.force_refresh().map(|_| ()),
        Err(CredentialError::SignedOut)
    );
    assert_eq!(s.token_requests(), 0);
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-011"))]
#[test]
fn invalidation_forces_one_refresh() {
    let s = Setup::new();
    let (old, _) = s.sign_in(NOW - 61, NOW + 539);
    let credential = s.credential();
    assert!(credential.invalidate(&secret(&old)));
    let new = expose(&credential.credential().unwrap());
    assert_ne!(new, old);
    assert_eq!(s.token_requests(), 1);
    // Replaced by another process meanwhile: no further request.
    let t = Setup::new();
    let (old, _) = t.sign_in(NOW - 61, NOW + 539);
    let credential = t.credential();
    t.store("fresh-from-elsewhere", Some("r"), NOW, NOW + 600);
    assert!(credential.invalidate(&secret(&old)));
    assert_eq!(
        expose(&credential.credential().unwrap()),
        "fresh-from-elsewhere"
    );
    assert_eq!(t.token_requests(), 0);
}

#[test]
#[ignore = "helper run as a child process by the concurrency test"]
fn refresh_child() {
    let Some(path) = std::env::var_os("STOCKHOUSE_REFRESH_CHILD") else {
        return;
    };
    let config = Arc::new(AuthConfig::load(std::path::Path::new(&path)).unwrap());
    let credential = ServerCredential::new(config, ManualClock::at(NOW + 590));
    credential.credential().unwrap();
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-012"))]
#[test]
fn concurrent_refreshers_never_reuse_a_refresh_token() {
    let s = Arc::new(Setup::new());
    s.sign_in(NOW, NOW + 600);
    s.clock.set(NOW + 590);
    s.fake.state().refresh_delay = Duration::from_millis(200);
    let children: Vec<_> = (0..2)
        .map(|_| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "refresh_child", "--ignored", "--test-threads=1"])
                .env("STOCKHOUSE_REFRESH_CHILD", s.root.join("auth.toml"))
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    let threads: Vec<_> = (0..16)
        .map(|_| {
            let s = s.clone();
            std::thread::spawn(move || s.credential().credential().map(|_| ()))
        })
        .collect();
    for thread in threads {
        assert_eq!(thread.join().unwrap(), Ok(()));
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let state = s.fake.state();
    assert!(state.overlaps.is_empty(), "{:?}", state.overlaps);
    assert!(state.reuse.is_empty(), "{:?}", state.reuse);
    let mut sent: Vec<_> = state
        .seen("/oauth/token")
        .iter()
        .filter_map(|r| r.field("refresh_token").map(str::to_owned))
        .collect();
    let total = sent.len();
    sent.dedup();
    assert_eq!(sent.len(), total, "no refresh token sent twice");
    assert!(total >= 1);
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-013"))]
#[test]
fn refresh_works_from_sync_and_async_callers() {
    let s = Arc::new(Setup::new());
    s.sign_in(NOW, NOW + 30);
    let plain = s.clone();
    assert!(
        std::thread::spawn(move || plain.credential().credential())
            .join()
            .unwrap()
            .is_ok()
    );
    for flavor in ["current", "multi"] {
        let s2 = s.clone();
        s2.clock.set(s2.clock.now_value() + 600);
        let runtime = match flavor {
            "current" => tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build(),
            _ => tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build(),
        }
        .unwrap();
        let result = runtime.block_on(async move {
            tokio::task::spawn_blocking(move || s2.credential().credential().map(|_| ()))
                .await
                .unwrap()
        });
        assert_eq!(result, Ok(()));
    }
    assert_eq!(s.token_requests(), 3);
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-014"))]
#[test]
fn source_factory_builds_every_configured_source() {
    let s = Setup::new();
    std::fs::write(s.root.join("token"), b"file-credential\n").unwrap();
    std::fs::set_permissions(
        s.root.join("token"),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    let file = source(Some(&CredentialSpec::File(s.root.join("token")))).unwrap();
    assert_eq!(
        expose(&file.shared().credential().unwrap()),
        "file-credential"
    );
    let systemd = source(Some(&CredentialSpec::Systemd("absent-name".to_owned()))).unwrap();
    assert!(matches!(systemd, Source::Static(_)));
    let server = source(Some(&CredentialSpec::Server(s.root.join("auth.toml")))).unwrap();
    let Source::Server(server) = server else {
        panic!("server source expected");
    };
    assert_eq!(server.private_tier().as_str(), s.fake.url("t/1/"));
    let missing = source(Some(&CredentialSpec::Server(s.root.join("absent.toml"))));
    assert_eq!(missing.err(), Some(AuthConfigError::Read));
    std::fs::write(s.root.join("bad.toml"), "version = 1\n").unwrap();
    let bad = source(Some(&CredentialSpec::Server(s.root.join("bad.toml"))));
    assert_eq!(bad.err(), Some(AuthConfigError::Parse));
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-015"))]
#[test]
fn invalidation_declines_when_a_retry_cannot_help() {
    let s = Setup::new();
    let credential = s.credential();
    assert!(!credential.invalidate(&secret("anything")), "no store");
    s.store("no-refresh", None, NOW - 120, NOW + 480);
    assert!(!credential.invalidate(&secret("no-refresh")));
    let (young, _) = s.sign_in(NOW - 59, NOW + 541);
    assert!(!credential.invalidate(&secret(&young)));
    let (old, _) = s.sign_in(NOW - 61, NOW + 539);
    assert!(credential.invalidate(&secret(&old)));
    let forced = expose(&credential.credential().unwrap());
    s.clock.set(NOW + 30);
    s.store(&forced, Some("still-valid"), NOW - 100, NOW + 600);
    assert!(
        !credential.invalidate(&secret(&forced)),
        "forced refresh 30 s ago"
    );
    assert_eq!(expose(&credential.credential().unwrap()), forced);
    assert_eq!(s.token_requests(), 1);
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-015"))]
#[test]
fn invalidation_waits_for_a_refresh_in_progress() {
    let s = Arc::new(Setup::new());
    let (young, _) = s.sign_in(NOW - 10, NOW + 590);
    let store = s.config.store();
    let lock = store.lock(Duration::ZERO).unwrap();
    let other = s.clone();
    let rejected = young.clone();
    let waiter = std::thread::spawn(move || other.credential().invalidate(&secret(&rejected)));
    std::thread::sleep(Duration::from_millis(300));
    s.store_with(&lock, "rotated-meanwhile", NOW + 600);
    drop(lock);
    assert!(
        waiter.join().unwrap(),
        "the in-progress refresh replaced it"
    );
    assert_eq!(s.token_requests(), 0);
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-REFRESH-016"))]
#[test]
fn transient_refresh_failures_back_off() {
    let s = Arc::new(Setup::new());
    let (old, _) = s.sign_in(NOW, NOW + 40);
    s.clock.set(NOW + 35);
    s.fake
        .state()
        .script("/oauth/token", Scripted::json(503, ""));
    let credential = Arc::new(s.credential());
    assert_eq!(expose(&credential.credential().unwrap()), old);
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let credential = credential.clone();
            std::thread::spawn(move || credential.credential().map(|c| expose(&c)))
        })
        .collect();
    for thread in threads {
        assert_eq!(thread.join().unwrap().unwrap(), old);
    }
    assert_eq!(s.token_requests(), 1);
    s.clock.set(NOW + 41);
    assert_eq!(
        credential.credential().map(|_| ()),
        Err(CredentialError::RefreshUnavailable)
    );
    assert_eq!(s.token_requests(), 1, "still within the backoff");
    s.clock.set(NOW + 46);
    assert!(credential.credential().is_ok());
    assert_eq!(s.token_requests(), 2);
}
