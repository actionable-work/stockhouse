#![allow(dead_code)]
//! Shared fixtures for the sign-in component tests. Tokens are synthetic.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use stockhouse_auth::{AuthConfig, Clock, ServerCredential};
use stockhouse_credentials::Secret;
use stockhouse_oauth_fake::FakeServer;
use stockhouse_token_store::TokenSet;

pub const NOW: u64 = 1_700_000_000;

pub fn temp_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("stockhouse-auth-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("store")).unwrap();
    fs::set_permissions(dir.join("store"), fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// A settable clock.
#[derive(Default)]
pub struct ManualClock(pub AtomicU64);

impl ManualClock {
    pub fn at(now: u64) -> Arc<Self> {
        Arc::new(Self(AtomicU64::new(now)))
    }
    pub fn set(&self, now: u64) {
        self.0.store(now, Ordering::SeqCst);
    }
    pub fn now_value(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

impl Clock for ManualClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

pub struct Setup {
    pub fake: FakeServer,
    pub root: PathBuf,
    pub config: Arc<AuthConfig>,
    pub clock: Arc<ManualClock>,
}

impl Setup {
    pub fn new() -> Self {
        let fake = FakeServer::start_background();
        let root = temp_dir();
        let store = format!(
            "mode = \"file\"\ndirectory = \"{}\"",
            root.join("store").display()
        );
        let text = fake.config(&store);
        fs::write(root.join("auth.toml"), &text).unwrap();
        let config = Arc::new(AuthConfig::parse(&text).unwrap());
        Self {
            fake,
            root,
            config,
            clock: ManualClock::at(NOW),
        }
    }

    pub fn credential(&self) -> ServerCredential {
        ServerCredential::new(self.config.clone(), self.clock.clone())
    }

    /// Stores a session issued by the fake (so the fake knows its tokens).
    pub fn sign_in(&self, obtained_at: u64, expires_at: u64) -> (String, String) {
        let (access, refresh) = {
            let mut state = self.fake.state();
            state.issue();
            (
                state.access.clone().unwrap(),
                state.refresh.clone().unwrap(),
            )
        };
        self.store(&access, Some(&refresh), obtained_at, expires_at);
        (access, refresh)
    }

    pub fn store_with(&self, lock: &stockhouse_token_store::Lock, access: &str, expires_at: u64) {
        let set = TokenSet {
            access_token: Secret::new(access.as_bytes().to_vec()),
            expires_at,
            refresh_token: Some(Secret::new(b"other-refresh".to_vec())),
            scope: None,
            obtained_at: expires_at - 600,
        };
        self.config.store().write(lock, &set).unwrap();
    }

    pub fn store(&self, access: &str, refresh: Option<&str>, obtained_at: u64, expires_at: u64) {
        let store = self.config.store();
        let lock = store.lock(Duration::from_secs(1)).unwrap();
        let set = TokenSet {
            access_token: Secret::new(access.as_bytes().to_vec()),
            expires_at,
            refresh_token: refresh.map(|r| Secret::new(r.as_bytes().to_vec())),
            scope: None,
            obtained_at,
        };
        store.write(&lock, &set).unwrap();
    }

    pub fn stored(&self) -> Option<TokenSet> {
        self.config.store().read().unwrap()
    }

    pub fn token_requests(&self) -> usize {
        self.fake.state().seen("/oauth/token").len()
    }

    /// Asserts no secret the fake saw or issued appears in `text`.
    pub fn assert_no_secret(&self, text: &str) {
        for secret in self.fake.state().secrets() {
            assert!(!text.contains(&secret), "secret leaked in {text:?}");
        }
        assert!(
            !text.contains("canary-description"),
            "description leaked in {text:?}"
        );
    }
}

pub fn expose(secret: &Secret) -> String {
    String::from_utf8(secret.expose().to_vec()).unwrap()
}
