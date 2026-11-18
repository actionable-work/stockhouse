//! The sign-in commands against the fake server. Tokens are synthetic.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use stockhouse_cli::run;
use stockhouse_oauth_fake::{FakeServer, Scripted};

fn temp_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("stockhouse-cli-auth-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("store")).unwrap();
    std::fs::set_permissions(dir.join("store"), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
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
    config: PathBuf,
    outputs: Mutex<Vec<String>>,
}

impl Setup {
    fn new() -> Self {
        let fake = FakeServer::start_background();
        let root = temp_dir();
        let store = format!(
            "mode = \"file\"\ndirectory = \"{}\"",
            root.join("store").display()
        );
        let config = root.join("auth.toml");
        std::fs::write(&config, fake.config(&store)).unwrap();
        Self {
            fake,
            config,
            outputs: Mutex::new(Vec::new()),
        }
    }

    /// Runs `stockhouse <command> --config <auth.toml> [extra]`.
    fn cli(&self, command: &str, extra: &[&str]) -> (u8, String, String) {
        let mut args = vec![
            "stockhouse",
            command,
            "--config",
            self.config.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        let (out, mut err) = (Out::default(), Vec::new());
        let code = run(&args, &|_| None, &mut out.clone(), &mut err);
        let result = (code, out.text(), String::from_utf8(err).unwrap());
        self.outputs
            .lock()
            .unwrap()
            .push(format!("{}{}", result.1, result.2));
        result
    }

    /// Runs `login` (loopback) while a test "browser" opens the printed
    /// URL; returns the exit status, the output and the URL.
    fn loopback_login(&self) -> (u8, String, String) {
        let out = Out::default();
        let args = [
            "stockhouse",
            "login",
            "--config",
            self.config.to_str().unwrap(),
        ]
        .map(str::to_owned);
        let mut writer = out.clone();
        let login = std::thread::spawn(move || run(&args, &|_| None, &mut writer, &mut Vec::new()));
        let url = loop {
            if let Some(line) = out.text().lines().find(|l| l.starts_with("http")) {
                break line.to_owned();
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let client = reqwest::Client::builder()
                    .tls_certs_only(std::iter::empty::<reqwest::Certificate>())
                    .build()
                    .unwrap();
                assert_eq!(client.get(&url).send().await.unwrap().status(), 200);
            });
        let code = login.join().unwrap();
        self.outputs.lock().unwrap().push(out.text());
        (code, out.text(), url)
    }

    fn device_login(&self) -> u8 {
        self.cli("login", &["--device"]).0
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-CLI-007"))]
#[test]
fn login_signs_in_through_the_chosen_flow() {
    let s = Setup::new();
    let (code, out, _) = s.cli("login", &["--device"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("USER-1") && out.contains("Signed in."),
        "{out}"
    );
    s.fake
        .state()
        .script("/oauth/token", Scripted::error(400, "access_denied"));
    let (code, _, err) = s.cli("login", &["--device"]);
    assert_eq!(code, 1);
    assert!(err.contains("denied"), "{err}");
    let code = run(
        &["stockhouse", "login", "--config", "/nonexistent/auth.toml"].map(str::to_owned),
        &|_| None,
        &mut Vec::new(),
        &mut Vec::new(),
    );
    assert_eq!(code, 2);
    // Loopback: a test "browser" opens the printed URL.
    let (code, out, url) = s.loopback_login();
    assert_eq!(code, 0);
    // `state` and `code_challenge` appear only inside the printed URL.
    let parsed = reqwest::Url::parse(&url).unwrap();
    for (name, value) in parsed.query_pairs() {
        if name == "state" || name == "code_challenge" {
            let lines = out.lines().filter(|l| l.contains(value.as_ref())).count();
            assert_eq!(lines, 1, "{name}");
        }
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-CLI-008"))]
#[test]
fn logout_reports_a_failed_revocation() {
    let s = Setup::new();
    assert_eq!(s.cli("logout", &[]).0, 0, "not signed in");
    assert_eq!(s.device_login(), 0);
    assert_eq!(
        s.cli("logout", &[]),
        (0, "Signed out.\n".to_owned(), String::new())
    );
    assert_eq!(s.device_login(), 0);
    s.fake
        .state()
        .script("/oauth/revoke", Scripted::json(503, ""));
    let (code, _, err) = s.cli("logout", &[]);
    assert_eq!(code, 1);
    assert!(err.contains("signed out locally"), "{err}");
    assert_eq!(s.cli("status", &[]).0, 1, "store deleted");
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-CLI-009"))]
#[test]
fn refresh_reports_the_outcome() {
    let s = Setup::new();
    assert_eq!(s.device_login(), 0);
    let before = s.fake.state().access.clone();
    assert_eq!(s.cli("refresh", &[]).0, 0);
    assert_ne!(s.fake.state().access, before);
    s.fake.state().revoke_family();
    let (code, _, err) = s.cli("refresh", &[]);
    assert_eq!(code, 1);
    assert!(err.contains("sign in again"), "{err}");
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-CLI-010"))]
#[test]
fn session_status_prints_one_json_object() {
    let s = Setup::new();
    let (code, out, _) = s.cli("status", &[]);
    assert_eq!(
        (code, out.as_str()),
        (1, "{\"signed_in\":false,\"store\":\"file\"}\n")
    );
    assert_eq!(s.device_login(), 0);
    let (code, out, _) = s.cli("status", &[]);
    assert_eq!(code, 0);
    assert_eq!(out.lines().count(), 1);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["signed_in"], true);
    assert_eq!(value["refreshable"], true);
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-CLI-011"))]
#[test]
fn status_needs_exactly_one_target() {
    for args in [
        vec!["stockhouse", "status"],
        vec!["stockhouse", "status", "--queue", "/q", "--config", "/c"],
        vec!["stockhouse", "status", "--config", "/c", "--queue", "/q"],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        let mut err = Vec::new();
        assert_eq!(
            run(&args, &|_| None, &mut Vec::new(), &mut err),
            2,
            "{args:?}"
        );
        assert!(
            String::from_utf8(err)
                .unwrap()
                .contains("usage: stockhouse")
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-CLI-012"))]
#[test]
fn sign_in_output_never_contains_secrets() {
    let s = Setup::new();
    s.device_login();
    s.cli("status", &[]);
    s.cli("refresh", &[]);
    s.fake
        .state()
        .script("/oauth/token", Scripted::error(400, "invalid_client"));
    s.cli("refresh", &[]);
    s.cli("logout", &[]);
    s.cli("refresh", &[]);
    s.fake
        .state()
        .script("/oauth/token", Scripted::error(400, "access_denied"));
    s.cli("login", &["--device"]);
    let (code, _, _) = s.loopback_login();
    assert_eq!(code, 0);
    let state = s.fake.state();
    let secrets = state.secrets();
    // Everything issued or received: tokens, the code and the verifier.
    let exchange = state
        .seen("/oauth/token")
        .into_iter()
        .find(|r| r.field("grant_type") == Some("authorization_code"))
        .unwrap();
    for name in ["code", "code_verifier"] {
        let value = exchange.field(name).unwrap().to_owned();
        assert!(secrets.contains(&value), "{name} is scanned");
    }
    drop(state);
    assert!(!secrets.is_empty());
    for output in s.outputs.lock().unwrap().iter() {
        for secret in &secrets {
            assert!(!output.contains(secret.as_str()), "{output}");
        }
        assert!(!output.contains("canary-description"), "{output}");
    }
}
