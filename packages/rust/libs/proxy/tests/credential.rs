mod common;
use common::*;
use std::os::unix::fs::PermissionsExt;

const TOKEN: &str = "test-bearer-0f3c9a";

fn token_file() -> std::path::PathBuf {
    let path = temp_dir().join("token");
    std::fs::write(&path, format!("{TOKEN}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

struct Setup {
    private: Fixture,
    shared: Fixture,
    public: Fixture,
    proxy: Proxy,
}

async fn setup() -> Setup {
    let journal = journal();
    let private = Fixture::start("private", &journal).await;
    let shared = Fixture::start("shared", &journal).await;
    let public = Fixture::start("public", &journal).await;
    let cred = format!("credential = {{ file = \"{}\" }}", token_file().display());
    let tiers = [
        ("private", tier(&private.url, "")),
        ("shared", tier(&shared.url, &cred)),
        ("public", tier(&public.url, "")),
    ];
    let proxy = Proxy::start(&config(&tiers)).await;
    Setup {
        private,
        shared,
        public,
        proxy,
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CREDENTIAL-001"))]
#[tokio::test]
async fn credentials_go_only_to_their_tier() {
    let s = setup().await;
    let entry = Entry::new("hello", b"nar bytes");
    s.public.add(&entry);
    assert_eq!(
        s.proxy.status(&format!("/{}", entry.narinfo_path())).await,
        200
    );
    let shared = s.shared.seen_all();
    assert!(!shared.is_empty());
    assert!(
        shared
            .iter()
            .all(|r| r.authorization.as_deref() == Some(&format!("Bearer {TOKEN}")))
    );
    let others = [s.private.seen_all(), s.public.seen_all()].concat();
    assert!(!others.is_empty());
    assert!(
        others.iter().all(|r| r.authorization.is_none()),
        "{others:?}"
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CREDENTIAL-002"))]
#[tokio::test]
async fn redirects_are_never_followed() {
    let s = setup().await;
    let entry = Entry::new("hello", b"nar");
    s.public.add(&entry);
    let target = format!("{}{}", s.public.url, entry.narinfo_path());
    s.shared.set(
        &entry.narinfo_path(),
        Reply {
            location: Some(target),
            ..Reply::status(302)
        },
    );
    assert_eq!(
        s.proxy.status(&format!("/{}", entry.narinfo_path())).await,
        503
    );
    assert!(
        s.public.seen_all().is_empty(),
        "redirect target must not be requested"
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CREDENTIAL-003"))]
#[tokio::test]
async fn credentials_never_appear_in_errors_logs_or_responses() {
    let s = setup().await;
    let mut bodies = Vec::new();
    for (label, reply) in [
        ("a", Reply::status(403)),
        ("b", Reply::status(500)),
        ("c", Reply::ok("garbage")),
    ] {
        let hash = hash_for(label);
        s.shared.set(&format!("{hash}.narinfo"), reply);
        let (status, body) = s.proxy.get(&format!("/{hash}.narinfo")).await;
        assert!(status >= 500);
        bodies.push(String::from_utf8(body).unwrap());
    }
    let text = format!("{bodies:?} {:?}", s.proxy.log_lines());
    assert!(!text.contains(TOKEN), "{text}");
    assert_eq!(s.proxy.log_lines().len(), 3);
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CREDENTIAL-004"))]
#[tokio::test]
async fn each_failed_lookup_logs_one_line() {
    let s = setup().await;
    let entry = Entry::new("hello", b"nar");
    s.private.set(&entry.narinfo_path(), Reply::status(500));
    s.proxy.status(&format!("/{}", entry.narinfo_path())).await;
    assert_eq!(
        s.proxy.log_lines(),
        ["stockhouse-proxy: narinfo private unavailable"]
    );
    s.private.add(&entry);
    s.private.set(&entry.nar_url, Reply::status(403));
    s.proxy
        .status(&format!("/nar/private/{}/{}", entry.hash, entry.nar_url))
        .await;
    assert_eq!(
        s.proxy.log_lines().last().unwrap(),
        "stockhouse-proxy: nar private denied"
    );
    assert_eq!(s.proxy.log_lines().len(), 2);
}
