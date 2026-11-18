//! Loopback and device sign-in against the fake server. Tokens are synthetic.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use stockhouse_auth::{AuthConfig, Clock, Failure, OAuthCode, Replaced, Transient};
use stockhouse_oauth_fake::{FakeServer, Scripted};
use stockhouse_signin::{
    Flow, LoopbackLogin, Options, Pacer, SigninError, callback_outcome, login,
};
use stockhouse_token_store::StoreError;

fn temp_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("stockhouse-signin-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("store")).unwrap();
    fs::set_permissions(dir.join("store"), fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

struct TestClock(AtomicU64);

impl Clock for TestClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Records waits and advances the clock instead of sleeping.
struct TestPacer {
    clock: Arc<TestClock>,
    waits: Mutex<Vec<u64>>,
}

impl Pacer for TestPacer {
    fn wait(
        &self,
        d: Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        self.waits.lock().unwrap().push(d.as_secs());
        self.clock.0.fetch_add(d.as_secs(), Ordering::SeqCst);
        Box::pin(async {})
    }
}

#[derive(Clone, Default)]
struct Out(Arc<Mutex<Vec<u8>>>);

impl Write for Out {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Out {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

struct Setup {
    fake: FakeServer,
    root: PathBuf,
    config: Arc<AuthConfig>,
    pacer: Arc<TestPacer>,
    options: Options,
}

async fn setup() -> Setup {
    let fake = FakeServer::start().await;
    let root = temp_dir();
    let store = format!(
        "mode = \"file\"\ndirectory = \"{}\"",
        root.join("store").display()
    );
    let config = Arc::new(AuthConfig::parse(&fake.config(&store)).unwrap());
    let clock = Arc::new(TestClock(AtomicU64::new(1_700_000_000)));
    let pacer = Arc::new(TestPacer {
        clock: clock.clone(),
        waits: Mutex::new(Vec::new()),
    });
    let options = Options {
        wait: Duration::from_secs(5),
        pacer: pacer.clone(),
        clock,
    };
    Setup {
        fake,
        root,
        config,
        pacer,
        options,
    }
}

impl Setup {
    fn device(&self) -> tokio::task::JoinHandle<(Result<Replaced, SigninError>, Out)> {
        let (config, options) = (self.config.clone(), self.options.clone());
        tokio::spawn(async move {
            let mut out = Out::default();
            let result = login(&config, Flow::Device, &options, &mut out).await;
            (result, out)
        })
    }

    fn loopback(&self) -> (tokio::task::JoinHandle<Result<Replaced, SigninError>>, Out) {
        let (config, options, out) = (self.config.clone(), self.options.clone(), Out::default());
        let mut writer = out.clone();
        let task =
            tokio::spawn(
                async move { login(&config, Flow::Loopback, &options, &mut writer).await },
            );
        (task, out)
    }

    fn waits(&self) -> Vec<u64> {
        self.pacer.waits.lock().unwrap().clone()
    }

    fn stored_access(&self) -> Option<String> {
        self.config
            .store()
            .read()
            .unwrap()
            .map(|s| String::from_utf8(s.access_token.expose().to_vec()).unwrap())
    }
}

async fn printed_url(out: &Out) -> reqwest::Url {
    for _ in 0..200 {
        if let Some(line) = out.text().lines().find(|l| l.starts_with("http")) {
            return reqwest::Url::parse(line).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no authorization URL printed");
}

fn param(url: &reqwest::Url, name: &str) -> String {
    url.query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
        .unwrap()
}

fn browser() -> reqwest::Client {
    reqwest::Client::builder()
        .tls_certs_only(std::iter::empty::<reqwest::Certificate>())
        .build()
        .unwrap()
}

async fn get(url: &str) -> Result<reqwest::Response, reqwest::Error> {
    browser().get(url).send().await
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-001"))]
#[tokio::test(flavor = "multi_thread")]
async fn pkce_uses_s256_with_fresh_random_values() {
    let s = setup().await;
    let mut seen = Vec::new();
    for _ in 0..2 {
        let (task, out) = s.loopback();
        let url = printed_url(&out).await;
        get(url.as_str()).await.unwrap();
        task.await.unwrap().unwrap();
        let (state, challenge) = (param(&url, "state"), param(&url, "code_challenge"));
        assert_eq!((state.len(), challenge.len()), (43, 43));
        assert_eq!(param(&url, "code_challenge_method"), "S256");
        seen.push((state, challenge));
    }
    assert_ne!(seen[0], seen[1]);
    let exchanges = s.fake.state().seen("/oauth/token");
    for (request, (_, challenge)) in exchanges.iter().zip(&seen) {
        let verifier = request.field("code_verifier").unwrap();
        use base64::Engine;
        use sha2::Digest;
        let expected =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier));
        assert_eq!(&expected, challenge);
        assert_eq!(verifier.len(), 43);
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-002"))]
#[tokio::test(flavor = "multi_thread")]
async fn listener_binds_an_ephemeral_loopback_port() {
    let s = setup().await;
    let login = LoopbackLogin::start(&s.config).await.unwrap();
    let redirect = reqwest::Url::parse(login.redirect_uri()).unwrap();
    assert_eq!(redirect.host_str(), Some("127.0.0.1"));
    assert_eq!(redirect.path(), "/callback");
    let port = redirect.port().unwrap();
    assert!(port >= 1024);
    let listening = std::net::TcpStream::connect(("127.0.0.1", port));
    assert!(listening.is_ok());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-003"))]
#[tokio::test(flavor = "multi_thread")]
async fn authorization_url_carries_the_request_parameters() {
    let s = setup().await;
    let text = s.fake.config(&format!(
        "mode = \"file\"\ndirectory = \"{}\"",
        s.root.join("store").display()
    ));
    let text = text.replace("oauth/authorize\"", "oauth/authorize?tenant=x\"");
    let config = AuthConfig::parse(&text).unwrap();
    let login = LoopbackLogin::start(&config).await.unwrap();
    let url = login.authorization_url();
    assert_eq!(url.path(), "/oauth/authorize");
    let names: Vec<String> = url.query_pairs().map(|(k, _)| k.into_owned()).collect();
    assert_eq!(
        names,
        [
            "tenant",
            "response_type",
            "client_id",
            "redirect_uri",
            "scope",
            "state",
            "code_challenge",
            "code_challenge_method"
        ]
    );
    assert_eq!(param(&url, "response_type"), "code");
    assert_eq!(param(&url, "client_id"), "stockhouse-test");
    assert_eq!(param(&url, "redirect_uri"), login.redirect_uri());
    assert_eq!(param(&url, "scope"), "cache:read cache:write");
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-004"))]
#[tokio::test(flavor = "multi_thread")]
async fn valid_callback_exchanges_the_code_with_the_verifier() {
    let s = setup().await;
    let (task, out) = s.loopback();
    let url = printed_url(&out).await;
    let page = get(url.as_str()).await.unwrap();
    assert_eq!(page.status(), 200);
    assert_eq!(task.await.unwrap(), Ok(Replaced::Nothing));
    let exchange = &s.fake.state().seen("/oauth/token")[0];
    assert_eq!(exchange.field("grant_type"), Some("authorization_code"));
    assert!(exchange.field("code").is_some() && exchange.field("code_verifier").is_some());
    assert_eq!(
        exchange.field("redirect_uri"),
        Some(param(&url, "redirect_uri").as_str())
    );
    assert_eq!(exchange.field("client_id"), Some("stockhouse-test"));
    assert_eq!(s.stored_access(), s.fake.state().access.clone());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-005"))]
#[tokio::test(flavor = "multi_thread")]
async fn state_mismatches_abort_the_login() {
    for query in ["code=abc&state=wrong", "code=abc"] {
        let s = setup().await;
        let (task, out) = s.loopback();
        let redirect = param(&printed_url(&out).await, "redirect_uri");
        let response = get(&format!("{redirect}?{query}")).await.unwrap();
        assert_eq!(response.status(), 400);
        assert_eq!(task.await.unwrap(), Err(SigninError::StateMismatch));
        assert!(s.fake.state().seen("/oauth/token").is_empty());
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-006"))]
#[tokio::test(flavor = "multi_thread")]
async fn authorization_errors_abort_the_login() {
    let s = setup().await;
    let (task, out) = s.loopback();
    let url = printed_url(&out).await;
    let (redirect, state) = (param(&url, "redirect_uri"), param(&url, "state"));
    let query =
        format!("error=access_denied&error_description=canary-description-77&state={state}");
    let page = get(&format!("{redirect}?{query}"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let result = task.await.unwrap();
    assert_eq!(
        result,
        Err(SigninError::AuthorizationDenied(OAuthCode::AccessDenied))
    );
    let error = result.unwrap_err();
    for text in [page, out.text(), error.to_string(), format!("{error:?}")] {
        assert!(!text.contains("canary-description"), "{text}");
    }
    assert!(s.fake.state().seen("/oauth/token").is_empty());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-007"))]
#[tokio::test(flavor = "multi_thread")]
async fn callback_is_one_shot() {
    let s = setup().await;
    let (task, out) = s.loopback();
    let url = printed_url(&out).await;
    let redirect = param(&url, "redirect_uri");
    get(url.as_str()).await.unwrap();
    task.await.unwrap().unwrap();
    assert!(get(&redirect).await.is_err(), "listener closed");
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-008"))]
#[tokio::test(flavor = "multi_thread")]
async fn unrelated_listener_requests_do_not_end_the_wait() {
    let s = setup().await;
    let (task, out) = s.loopback();
    let url = printed_url(&out).await;
    let redirect = reqwest::Url::parse(&param(&url, "redirect_uri")).unwrap();
    let favicon = redirect.join("/favicon.ico?x=echo-me").unwrap();
    let response = get(favicon.as_str()).await.unwrap();
    assert_eq!(response.status(), 404);
    assert!(!response.text().await.unwrap().contains("echo-me"));
    let post = browser().post(redirect.as_str()).send().await.unwrap();
    assert_eq!(post.status(), 405);
    // An idle connection that never sends a request does not block the
    // callback: connections are served concurrently.
    let port = redirect.port().unwrap();
    let _idle = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let callback = tokio::time::timeout(Duration::from_secs(2), get(url.as_str())).await;
    assert!(callback.unwrap().is_ok());
    let done = tokio::time::timeout(Duration::from_secs(2), task).await;
    assert!(done.unwrap().unwrap().is_ok());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-009"))]
#[tokio::test(flavor = "multi_thread")]
async fn loopback_wait_is_bounded() {
    let mut s = setup().await;
    s.options.wait = Duration::from_millis(200);
    let (task, out) = s.loopback();
    let redirect = param(&printed_url(&out).await, "redirect_uri");
    assert_eq!(task.await.unwrap(), Err(SigninError::LoginTimeout));
    assert!(get(&redirect).await.is_err(), "listener closed");
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-010"))]
#[tokio::test(flavor = "multi_thread")]
async fn device_login_shows_the_verification_uri_and_user_code() {
    let s = setup().await;
    let (result, out) = s.device().await.unwrap();
    assert_eq!(result, Ok(Replaced::Nothing));
    let text = out.text();
    assert!(text.contains("http://127.0.0.1/device"));
    assert!(text.contains("USER-1"));
    assert!(text.contains("http://127.0.0.1/device?user_code=USER-1"));
    let device_code = s.fake.state().seen("/oauth/token")[0]
        .field("device_code")
        .unwrap()
        .to_owned();
    assert!(!text.contains(&device_code));
    assert!(s.stored_access().is_some());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-011"))]
#[tokio::test(flavor = "multi_thread")]
async fn malformed_device_responses_are_refused() {
    let base =
        r#""device_code":"dc","user_code":"U","verification_uri":"https://example.invalid/d""#;
    for body in [
        r#"{"user_code":"U","verification_uri":"https://example.invalid/d","expires_in":60}"#.to_owned(),
        format!(r#"{{{base},"expires_in":60,"interval":0}}"#),
        format!(r#"{{{base},"expires_in":3601}}"#),
        r#"{"device_code":"dc","user_code":"U","verification_uri":"javascript:alert(1)","expires_in":60}"#.to_owned(),
        // Terminal escapes and line breaks never reach the operator's screen.
        format!(r#"{{{base},"expires_in":60,"verification_uri_complete":"https://example.invalid/d\u001b[2J"}}"#),
        r#"{"device_code":"dc","user_code":"U","verification_uri":"https://example.invalid/d\nVisit https://evil.example.invalid","expires_in":60}"#.to_owned(),
        r#"{"device_code":"dc","user_code":"U\u001b[2J","verification_uri":"https://example.invalid/d","expires_in":60}"#.to_owned(),
    ] {
        let s = setup().await;
        s.fake.state().script("/oauth/device", Scripted::json(200, &body));
        let (result, _) = s.device().await.unwrap();
        assert_eq!(result, Err(SigninError::DeviceResponse), "{body}");
        assert!(s.fake.state().seen("/oauth/token").is_empty());
    }
}

fn pending(s: &Setup, replies: Vec<Scripted>) {
    let mut state = s.fake.state();
    state.auto_approve = true;
    for reply in replies {
        state.script("/oauth/token", reply);
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-012"))]
#[tokio::test(flavor = "multi_thread")]
async fn authorization_pending_keeps_the_interval() {
    let s = setup().await;
    s.fake.state().interval = 5;
    let p = || Scripted::error(400, "authorization_pending");
    pending(&s, vec![p(), p()]);
    assert!(s.device().await.unwrap().0.is_ok());
    assert_eq!(s.waits(), [5, 5, 5]);
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-013"))]
#[tokio::test(flavor = "multi_thread")]
async fn slow_down_adds_five_seconds() {
    let s = setup().await;
    s.fake.state().interval = 5;
    let slow = || Scripted::error(400, "slow_down");
    pending(&s, vec![slow(), slow()]);
    assert!(s.device().await.unwrap().0.is_ok());
    assert_eq!(s.waits(), [5, 10, 15]);
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-014"))]
#[tokio::test(flavor = "multi_thread")]
async fn denied_or_expired_device_grants_stop_polling() {
    for (code, expected) in [
        ("access_denied", SigninError::DeviceDenied),
        ("expired_token", SigninError::DeviceExpired),
    ] {
        let s = setup().await;
        pending(&s, vec![Scripted::error(400, code)]);
        assert_eq!(s.device().await.unwrap().0, Err(expected));
        assert_eq!(s.fake.state().seen("/oauth/token").len(), 1);
    }
    let s = setup().await;
    {
        let mut state = s.fake.state();
        state.auto_approve = false;
        state.interval = 5;
        state.device_expires = 12;
    }
    assert_eq!(s.device().await.unwrap().0, Err(SigninError::DeviceExpired));
    assert_eq!(s.waits(), [5, 5]);
    assert_eq!(s.fake.state().seen("/oauth/token").len(), 2);
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-018"))]
#[tokio::test(flavor = "multi_thread")]
async fn re_login_replaces_and_revokes_the_previous_grant() {
    let s = setup().await;
    s.device().await.unwrap().0.unwrap();
    let old = s.fake.state().refresh.clone().unwrap();
    assert_eq!(s.device().await.unwrap().0, Ok(Replaced::Revoked));
    let revoked = s.fake.state().seen("/oauth/revoke");
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].field("token"), Some(old.as_str()));
    assert_eq!(s.stored_access(), s.fake.state().access.clone());
    s.fake
        .state()
        .script("/oauth/revoke", Scripted::json(503, ""));
    assert_eq!(
        s.device().await.unwrap().0,
        Ok(Replaced::RevocationFailed(Failure::Transient(
            Transient::Status(503)
        )))
    );
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-019"))]
#[tokio::test(flavor = "multi_thread")]
async fn transient_device_polling_failures_back_off() {
    let cases: [(u64, Vec<Scripted>, Vec<u64>); 3] = [
        (
            5,
            vec![
                Scripted::json(503, ""),
                Scripted::json(429, ""),
                Scripted::error(400, "temporarily_unavailable"),
            ],
            vec![5, 10, 15, 20],
        ),
        (
            58,
            vec![Scripted::json(503, ""), Scripted::json(503, "")],
            vec![58, 60, 60],
        ),
        (
            60,
            vec![Scripted::error(400, "slow_down"), Scripted::json(503, "")],
            vec![60, 65, 65],
        ),
    ];
    for (interval, replies, waits) in cases {
        let s = setup().await;
        s.fake.state().interval = interval;
        pending(&s, replies);
        assert!(s.device().await.unwrap().0.is_ok());
        assert_eq!(s.waits(), waits);
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-020"))]
#[tokio::test(flavor = "multi_thread")]
async fn listener_responses_are_not_cached_or_referred() {
    let s = setup().await;
    let (task, out) = s.loopback();
    let url = printed_url(&out).await;
    let redirect = reqwest::Url::parse(&param(&url, "redirect_uri")).unwrap();
    let mut responses = vec![
        get(redirect.join("/other").unwrap().as_str())
            .await
            .unwrap(),
        browser().post(redirect.as_str()).send().await.unwrap(),
    ];
    responses.push(get(&format!("{redirect}?state=nope")).await.unwrap());
    let _ = task.await;
    let (task, out) = s.loopback();
    responses.push(get(printed_url(&out).await.as_str()).await.unwrap());
    task.await.unwrap().unwrap();
    let statuses: Vec<u16> = responses.iter().map(|r| r.status().as_u16()).collect();
    assert_eq!(statuses, [404, 405, 400, 200]);
    for response in responses {
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-021"))]
#[tokio::test(flavor = "multi_thread")]
async fn login_preflights_the_store() {
    let s = setup().await;
    fs::remove_dir(s.root.join("store")).unwrap();
    let (task, out) = s.loopback();
    assert_eq!(
        task.await.unwrap(),
        Err(SigninError::Store(StoreError::DirectoryMissing))
    );
    assert!(out.text().is_empty(), "no listener bound, nothing printed");
    let sealed = s.fake.config(&format!(
        "mode = \"sealed\"\ndirectory = \"{}\"\nkey_credential = \"absent-key\"",
        s.root.display()
    ));
    fs::set_permissions(&s.root, fs::Permissions::from_mode(0o700)).unwrap();
    let config = AuthConfig::parse(&sealed).unwrap();
    let mut out = Out::default();
    let result = login(&config, Flow::Device, &s.options, &mut out).await;
    assert_eq!(result, Err(SigninError::Store(StoreError::KeyUnavailable)));
    assert!(s.fake.state().requests.is_empty());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-022"))]
#[tokio::test(flavor = "multi_thread")]
async fn login_replaces_an_untrusted_store_without_revoking() {
    let s = setup().await;
    s.device().await.unwrap().0.unwrap();
    fs::write(s.root.join("store/tokens.json"), b"garbage").unwrap();
    assert_eq!(s.device().await.unwrap().0, Ok(Replaced::Nothing));
    assert!(s.fake.state().seen("/oauth/revoke").is_empty());
    assert_eq!(s.stored_access(), s.fake.state().access.clone());
}

#[test]
fn callback_outcomes_are_classified() {
    assert_eq!(callback_outcome("state=s&code=c", "s").0, 200);
    assert_eq!(
        callback_outcome("state=s", "s").1.err(),
        Some(SigninError::InvalidCallback)
    );
    assert_eq!(
        callback_outcome("state=s&error=weird", "s").1.err(),
        Some(SigninError::AuthorizationDenied(OAuthCode::Other))
    );
}
