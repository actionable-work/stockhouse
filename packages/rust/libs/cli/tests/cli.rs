use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use stockhouse_cli::{guard_hook, run};
use stockhouse_ingest::Queue;
use stockhouse_proto::{Sha256, nix32};

fn temp_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("stockhouse-cli-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn store_path(label: &str) -> String {
    format!(
        "/nix/store/{}-{label}",
        nix32::encode(&Sha256::digest(label.as_bytes()).bytes()[..20])
    )
}

/// Runs the CLI with `OUT_PATHS` and returns (exit, stdout, stderr).
fn cli(args: &[&str], out_paths: Option<&str>) -> (u8, String, String) {
    let args: Vec<String> = std::iter::once("stockhouse")
        .chain(args.iter().copied())
        .map(str::to_owned)
        .collect();
    let out_paths = out_paths.map(str::to_owned);
    let env = move |name: &str| {
        if name == "OUT_PATHS" {
            out_paths.clone()
        } else {
            None
        }
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&args, &env, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn uploader_config(queue: &std::path::Path) -> PathBuf {
    let text = format!(
        "version = 1\nqueue_dir = \"{}\"\nmax_pending = 10\nmax_attempts = 3\nbatch_size = 8\nbackoff_base_secs = 1\n\
backoff_max_secs = 10\npoll_interval_secs = 1\nrescan_interval_secs = 100\n[backend]\nkind = \"deferred\"\n[scanner]\nkind = \"none\"\n",
        queue.display()
    );
    let path = temp_dir().join("uploader.toml");
    std::fs::write(&path, text).unwrap();
    path
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-CLI-001"))]
#[test]
fn commands_dispatch_to_their_owners() {
    let (code, out, _) = cli(&["--version"], None);
    assert_eq!(
        (code, out.trim()),
        (
            0,
            format!("stockhouse {} (protocol 1)", env!("CARGO_PKG_VERSION")).as_str()
        )
    );
    let queue = temp_dir().join("queue");
    let config = uploader_config(&queue);
    assert_eq!(
        cli(
            &["uploader", "--config", config.to_str().unwrap(), "--once"],
            None
        )
        .0,
        0
    );
    assert!(Queue::open(&queue).is_ok(), "uploader created the queue");
    let built = store_path("built");
    assert_eq!(
        cli(
            &["post-build-hook", "--queue", queue.to_str().unwrap()],
            Some(&built)
        )
        .0,
        0
    );
    let (code, out, _) = cli(&["status", "--queue", queue.to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(out.contains("\"pending\":1"), "{out}");
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy = temp_dir().join("proxy.toml");
    let cache = temp_dir();
    std::fs::write(&proxy, format!(
        "version = 1\nlisten = \"{}\"\n[private]\nurl = \"file://{}/\"\ntrusted_keys = [\"k:UpT9w0BGYiXPBfpdNiWwBf1b3LAwvtjoUHLlbFaOM3E=\"]\n",
        taken.local_addr().unwrap(), cache.display()
    )).unwrap();
    let (code, _, err) = cli(&["proxy", "--config", proxy.to_str().unwrap()], None);
    assert_eq!(code, 1);
    assert!(err.contains("already in use"), "{err}");
    assert_eq!(
        cli(
            &[
                "status",
                "--queue",
                temp_dir().join("absent").to_str().unwrap()
            ],
            None
        )
        .0,
        1
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-CLI-002"))]
#[test]
fn usage_errors_exit_2() {
    for args in [
        vec![],
        vec!["frobnicate"],
        vec!["status"],
        vec!["status", "--queue"],
        vec!["proxy", "--verbose"],
        vec!["login", "extra"],
        vec!["proxy", "--config", "a", "--config", "b"],
    ] {
        let (code, _, err) = cli(&args, None);
        assert_eq!(code, 2, "{args:?}");
        assert!(err.contains("usage: stockhouse"), "{args:?}");
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-CLI-003"))]
#[test]
fn no_argument_accepts_a_credential() {
    for args in [
        vec!["proxy", "--config", "/x", "--token", "s3cr3t"],
        vec!["uploader", "--token=s3cr3t", "--config", "/x"],
        vec!["login", "--password", "s3cr3t"],
        vec!["login", "--config", "/x", "--token", "s3cr3t"],
        vec!["refresh", "--config", "/x", "--refresh-token=s3cr3t"],
    ] {
        let (code, out, err) = cli(&args, None);
        assert_eq!(code, 2, "{args:?}");
        assert!(
            !format!("{out}{err}").contains("s3cr3t"),
            "values are never echoed"
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-CLI-005"))]
#[test]
fn status_prints_one_json_object() {
    let queue = temp_dir().join("queue");
    Queue::create(&queue, 4).unwrap();
    let (code, out, _) = cli(&["status", "--queue", queue.to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert_eq!(out.lines().count(), 1);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["pending"], 0);
    assert_eq!(value["overflow"]["active"], false);
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-CLI-006"))]
#[test]
fn configuration_errors_exit_2() {
    let bad = temp_dir().join("bad.toml");
    std::fs::write(&bad, "version = 9\n").unwrap();
    for command in ["proxy", "uploader"] {
        let (code, _, err) = cli(&[command, "--config", bad.to_str().unwrap()], None);
        assert_eq!(code, 2, "{command}");
        assert!(err.contains("configuration"), "{err}");
        let missing = cli(&[command, "--config", "/nonexistent/stockhouse.toml"], None);
        assert_eq!(missing.0, 2);
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-HOOK-004"))]
#[test]
fn hook_always_exits_successfully() {
    let queue = temp_dir().join("queue");
    Queue::create(&queue, 1).unwrap();
    let q = queue.to_str().unwrap();
    let paths = format!("{} {} not-a-path", store_path("a"), store_path("b"));
    assert_eq!(
        cli(&["post-build-hook", "--queue", q], Some(&paths)).0,
        0,
        "overflow and invalid input"
    );
    assert_eq!(
        cli(
            &["post-build-hook", "--queue", "/nonexistent/queue"],
            Some(&paths)
        )
        .0,
        0,
        "missing queue"
    );
    assert_eq!(
        cli(&["post-build-hook", "--queue", q], None).0,
        0,
        "no OUT_PATHS"
    );
    assert_eq!(guard_hook(|| panic!("simulated hook panic")), 0);
}

fn lease_config(dir: &std::path::Path, base: &str) -> PathBuf {
    let text = format!(
        "version = 1\nbase_url = \"{base}\"\nmachine_id_file = \"{}\"\n[credential]\nsystemd = \"private\"\n",
        dir.join("machine-id").display()
    );
    let path = dir.join("lease.toml");
    std::fs::write(&path, text).unwrap();
    path
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-CLI-013"))]
#[test]
fn the_lease_command_dispatches_to_the_lease_client() {
    let dir = temp_dir();
    let config = lease_config(&dir, "http://127.0.0.1:9/t/1/");
    // No systemd credential directory here: the lease client reports the
    // credential error and exits 75 without contacting anything.
    let (code, _, err) = cli(&["lease", "--config", config.to_str().unwrap()], None);
    assert_eq!(code, 75, "{err}");
    assert!(
        err.contains("stockhouse lease: credential unavailable"),
        "{err}"
    );
    assert!(dir.join("machine-id").exists());
    for args in [
        vec!["lease"],
        vec!["lease", "--config"],
        vec!["lease", "--config", "x", "--once"],
    ] {
        assert_eq!(cli(&args, None).0, 2, "{args:?}");
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-CLI-014"))]
#[test]
fn invalid_server_endpoints_are_configuration_errors() {
    let dir = temp_dir();
    let queue = dir.join("queue");
    let text = std::fs::read_to_string(uploader_config(&queue)).unwrap().replace(
        "[backend]\nkind = \"deferred\"\n",
        "[backend]\nkind = \"server\"\nbase_url = \"http://cache.example.invalid/t/1/\"\n[backend.credential]\nsystemd = \"private\"\n",
    );
    let config = dir.join("server.toml");
    std::fs::write(&config, text).unwrap();
    let (code, _, err) = cli(
        &["uploader", "--config", config.to_str().unwrap(), "--once"],
        None,
    );
    assert_eq!(code, 2);
    assert!(err.contains("cleartext"), "{err}");
    assert!(!queue.exists());
    let lease = lease_config(&dir, "http://cache.example.invalid/t/1/");
    let (code, _, err) = cli(&["lease", "--config", lease.to_str().unwrap()], None);
    assert_eq!(code, 2);
    assert!(err.contains("cleartext"), "{err}");
    assert!(!dir.join("machine-id").exists());
    // A `server` credential whose sign-in configuration is missing, or is
    // for another private tier, is a configuration error too.
    let auth = dir.join("auth.toml");
    std::fs::write(
        &auth,
        AUTH.replace("@TIER@", "http://127.0.0.1:8486/t/2/")
            .replace("@DIR@", &dir.display().to_string()),
    )
    .unwrap();
    for (credential, needle) in [
        (
            format!("server = \"{}\"", dir.join("absent.toml").display()),
            "credential",
        ),
        (
            format!("server = \"{}\"", auth.display()),
            "another private tier",
        ),
    ] {
        let lease = lease_config(&dir, "http://127.0.0.1:8486/t/1/");
        let text = std::fs::read_to_string(&lease)
            .unwrap()
            .replace("systemd = \"private\"", &credential);
        std::fs::write(&lease, text).unwrap();
        let (code, _, err) = cli(&["lease", "--config", lease.to_str().unwrap()], None);
        assert_eq!(code, 2, "{err}");
        assert!(err.contains(needle), "{err}");
        let uploader = dir.join("server-auth.toml");
        let text = std::fs::read_to_string(&config)
            .unwrap()
            .replace(
                "http://cache.example.invalid/t/1/",
                "http://127.0.0.1:8486/t/1/",
            )
            .replace("systemd = \"private\"", &credential);
        std::fs::write(&uploader, text).unwrap();
        let (code, _, err) = cli(
            &["uploader", "--config", uploader.to_str().unwrap(), "--once"],
            None,
        );
        assert_eq!(code, 2, "{err}");
        assert!(err.contains(needle), "{err}");
        assert!(!queue.exists());
    }
}

const AUTH: &str = r#"version = 1
server = "https://example.invalid/"
client_id = "stockhouse-cli"
scopes = ["cache:read", "cache:write"]
private_tier = "@TIER@"

[endpoints]
authorization = "https://example.invalid/oauth/authorize"
token = "https://example.invalid/oauth/token"
device_authorization = "https://example.invalid/oauth/device"
revocation = "https://example.invalid/oauth/revoke"

[store]
mode = "file"
directory = "@DIR@/auth"
"#;
