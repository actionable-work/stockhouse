//! The private tier with a signed-in server credential: 401 handling.

mod common;
use common::*;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use stockhouse_auth::{AuthConfig, AuthConfigError};
use stockhouse_credentials::Secret;
use stockhouse_oauth_fake::{FakeServer, Scripted};
use stockhouse_proxy::{ProxyConfig, ServeError, TierName};
use stockhouse_token_store::TokenSet;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

struct Signed {
    fake: FakeServer,
    root: PathBuf,
    proxy: Proxy,
    entry: Entry,
}

fn write_auth(fake: &FakeServer, root: &std::path::Path) -> AuthConfig {
    let store = format!(
        "mode = \"file\"\ndirectory = \"{}\"",
        root.join("store").display()
    );
    let text = fake.config(&store);
    std::fs::write(root.join("auth.toml"), &text).unwrap();
    AuthConfig::parse(&text).unwrap()
}

/// A proxy whose private tier is the fake's `/t/1/`, signed in with a
/// session obtained two minutes ago.
async fn signed(expires_in: u64) -> Signed {
    let fake = FakeServer::start().await;
    let root = temp_dir();
    std::fs::create_dir_all(root.join("store")).unwrap();
    std::fs::set_permissions(root.join("store"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let auth = write_auth(&fake, &root);
    let (access, refresh) = {
        let mut state = fake.state();
        state.issue();
        (
            state.access.clone().unwrap(),
            state.refresh.clone().unwrap(),
        )
    };
    let store = auth.store();
    let lock = store.lock(Duration::from_secs(1)).unwrap();
    let set = TokenSet {
        access_token: Secret::new(access.into_bytes()),
        expires_at: now() + expires_in,
        refresh_token: Some(Secret::new(refresh.into_bytes())),
        scope: None,
        obtained_at: now() - 120,
    };
    store.write(&lock, &set).unwrap();
    drop(lock);
    let entry = Entry::new("signed", b"private nar");
    {
        let mut state = fake.state();
        state
            .cache
            .insert(entry.narinfo_path(), entry.narinfo_text().into_bytes());
        state.cache.insert(entry.nar_url.clone(), entry.nar.clone());
    }
    let cred = format!(
        "credential = {{ server = \"{}\" }}",
        root.join("auth.toml").display()
    );
    let proxy = Proxy::start(&config(&[("private", tier(&fake.url("t/1/"), &cred))])).await;
    Signed {
        fake,
        root,
        proxy,
        entry,
    }
}

impl Signed {
    fn narinfo(&self) -> String {
        format!("/{}", self.entry.narinfo_path())
    }
    fn tier_requests(&self, rel: &str) -> Vec<Option<String>> {
        self.fake
            .state()
            .seen(&format!("/t/1/{rel}"))
            .into_iter()
            .map(|r| r.authorization)
            .collect()
    }
    fn tokens(&self) -> usize {
        self.fake.state().seen("/oauth/token").len()
    }
    /// The server stops accepting the stored access token (no rotation).
    fn reject_current_access(&self) {
        self.fake.state().access = Some("server-side-replaced".to_owned());
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CREDENTIAL-005"))]
#[tokio::test(flavor = "multi_thread")]
async fn credentials_are_resolved_off_the_async_workers() {
    let s = signed(30).await;
    s.fake.state().refresh_delay = Duration::from_millis(800);
    let path = s.narinfo();
    let slow = {
        let client = s.proxy.client.clone();
        let url = format!("{}{path}", s.proxy.base);
        tokio::spawn(async move { client.get(url).send().await.map(|r| r.status().as_u16()) })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    let started = Instant::now();
    assert_eq!(s.proxy.status("/nix-cache-info").await, 200);
    assert!(started.elapsed() < Duration::from_millis(300));
    assert_eq!(slow.await.unwrap().unwrap(), 200);
    assert_eq!(s.tokens(), 1, "the slow lookup refreshed");
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CREDENTIAL-006"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_401_with_a_refreshable_credential_is_retried_once() {
    let s = signed(500).await;
    // Validates the tier's nix-cache-info, so the 401 hits the narinfo.
    let warm = format!("/{}.narinfo", hash_for("warm"));
    assert_eq!(s.proxy.status(&warm).await, 404);
    let old = s.fake.state().access.clone().unwrap();
    s.reject_current_access();
    assert_eq!(s.proxy.status(&s.narinfo()).await, 200);
    let new = s.fake.state().access.clone().unwrap();
    let seen = s.tier_requests(&s.entry.narinfo_path());
    assert_eq!(
        seen,
        [Some(format!("Bearer {old}")), Some(format!("Bearer {new}"))]
    );
    assert_eq!(s.tokens(), 1);
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CREDENTIAL-007"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_repeated_401_is_a_503_never_a_miss() {
    let s = signed(500).await;
    for _ in 0..2 {
        s.fake.state().script(
            &format!("/t/1/{}", s.entry.narinfo_path()),
            Scripted::json(401, ""),
        );
    }
    assert_eq!(s.proxy.status(&s.narinfo()).await, 503);
    assert_eq!(s.tier_requests(&s.entry.narinfo_path()).len(), 2);
    assert_eq!(s.tokens(), 1);
    assert_eq!(
        s.proxy.log_lines(),
        ["stockhouse-proxy: narinfo private denied"]
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CREDENTIAL-008"))]
#[cfg_attr(any(), bloomery("CLIENT-PROXY-CREDENTIAL-011"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_signed_out_credential_is_a_503() {
    let s = signed(500).await;
    s.fake.state().revoke_family();
    assert_eq!(s.proxy.status(&s.narinfo()).await, 503);
    assert_eq!(
        s.tier_requests("nix-cache-info").len(),
        1,
        "no request after sign-out"
    );
    assert_eq!(
        s.proxy.log_lines(),
        ["stockhouse-proxy: narinfo private credential signed-out"]
    );
    assert!(!s.root.join("store/tokens.json").exists());
    assert_eq!(s.proxy.status(&s.narinfo()).await, 503);
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CREDENTIAL-009"))]
#[tokio::test(flavor = "multi_thread")]
async fn static_credentials_and_403_are_not_retried() {
    let s = signed(500).await;
    s.fake.state().script(
        &format!("/t/1/{}", s.entry.narinfo_path()),
        Scripted::json(403, ""),
    );
    assert_eq!(s.proxy.status(&s.narinfo()).await, 503);
    assert_eq!(s.tier_requests(&s.entry.narinfo_path()).len(), 1);
    assert_eq!(s.tokens(), 0);
    let journal = journal();
    let fixture = Fixture::start("private", &journal).await;
    let token = temp_dir().join("token");
    std::fs::write(&token, "static-token\n").unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let cred = format!("credential = {{ file = \"{}\" }}", token.display());
    let proxy = Proxy::start(&config(&[("private", tier(&fixture.url, &cred))])).await;
    let entry = Entry::new("static", b"nar");
    fixture.set(&entry.narinfo_path(), Reply::status(401));
    assert_eq!(
        proxy.status(&format!("/{}", entry.narinfo_path())).await,
        503
    );
    assert_eq!(fixture.seen().len(), 1);
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CREDENTIAL-010"))]
#[tokio::test(flavor = "multi_thread")]
async fn the_retry_covers_cache_info_narinfo_and_nar_fetches() {
    // nix-cache-info first: the retried token carries on to the narinfo.
    let s = signed(500).await;
    s.reject_current_access();
    assert_eq!(s.proxy.status(&s.narinfo()).await, 200);
    let new = Some(format!("Bearer {}", s.fake.state().access.clone().unwrap()));
    assert_eq!(s.tier_requests("nix-cache-info").len(), 2);
    assert_eq!(
        s.tier_requests(&s.entry.narinfo_path()),
        std::slice::from_ref(&new)
    );
    assert_eq!(s.tokens(), 1);
    // NAR open: retried before any byte is sent.
    let s = signed(500).await;
    let nar_path = format!("/t/1/{}", s.entry.nar_url);
    s.fake.state().script(&nar_path, Scripted::json(401, ""));
    let route = format!("/nar/private/{}/{}", s.entry.hash, s.entry.nar_url);
    let (status, body) = s.proxy.get(&route).await;
    assert_eq!((status, body), (200, s.entry.nar.clone()));
    assert_eq!(s.tier_requests(&s.entry.nar_url).len(), 2);
    assert_eq!(s.tokens(), 1);
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CONFIG-011"))]
#[test]
fn server_credentials_belong_to_the_private_tier() {
    let server = "credential = { server = \"/etc/stockhouse/auth.toml\" }";
    for name in ["shared", "public"] {
        let text = config(&[(name, tier("https://cache.example.invalid/", server))]);
        let expected = if name == "public" {
            stockhouse_proxy::ConfigError::CredentialOnPublicTier
        } else {
            stockhouse_proxy::ConfigError::ServerCredential(TierName::Shared)
        };
        assert_eq!(ProxyConfig::parse(&text), Err(expected));
    }
    for bad in ["auth.toml", "/etc/../tmp/auth.toml", "/etc/./auth.toml"] {
        let cred = format!("credential = {{ server = \"{bad}\" }}");
        let text = config(&[("private", tier("https://cache.example.invalid/", &cred))]);
        assert_eq!(
            ProxyConfig::parse(&text),
            Err(stockhouse_proxy::ConfigError::ServerCredential(
                TierName::Private
            )),
            "{bad}"
        );
    }
    let text = config(&[("private", tier("https://cache.example.invalid/", server))]);
    assert!(ProxyConfig::parse(&text).is_ok());
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CONFIG-012"))]
#[tokio::test(flavor = "multi_thread")]
async fn the_access_token_is_only_for_its_own_tier() {
    let fake = FakeServer::start().await;
    let root = temp_dir();
    write_auth(&fake, &root);
    // A port that was free a moment ago; the proxy must not take it.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let listen = format!("127.0.0.1:{port}");
    let listen_in = listen.clone();
    let bind = move |url: String, path: PathBuf| {
        let listen = listen_in.clone();
        async move {
            let cred = format!("credential = {{ server = \"{}\" }}", path.display());
            let text = config(&[("private", tier(&url, &cred))]).replace("127.0.0.1:0", &listen);
            let parsed = ProxyConfig::parse(&text).unwrap();
            stockhouse_proxy::bind(&parsed, stockhouse_proxy::stderr_log())
                .await
                .err()
        }
    };
    let missing = bind(fake.url("t/1/"), root.join("absent.toml")).await;
    assert!(matches!(
        missing,
        Some(ServeError::Credential {
            tier: TierName::Private,
            error: AuthConfigError::Read
        })
    ));
    let other = bind(fake.url("t/2/"), root.join("auth.toml")).await;
    assert!(matches!(
        other,
        Some(ServeError::CredentialAudience {
            tier: TierName::Private
        })
    ));
    assert!(
        std::net::TcpStream::connect(&listen).is_err(),
        "never listened"
    );
    // Normalized comparison: an explicit default-free spelling still matches.
    let upper = fake.url("t/1/").replace("http://", "HTTP://");
    assert!(bind(upper, root.join("auth.toml")).await.is_none());
}
