mod common;
use common::*;
use stockhouse_proxy::{ProxyConfig, ServeError, stderr_log};

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LISTEN-001"))]
#[tokio::test]
async fn binds_only_the_loopback_address() {
    let (_dir, url) = file_cache();
    let config = ProxyConfig::parse(&config(&[("private", tier(&url, ""))])).unwrap();
    let server = stockhouse_proxy::bind(&config, stderr_log()).await.unwrap();
    assert_eq!(server.local_addr().ip(), config.listen.ip());
    assert!(server.local_addr().ip().is_loopback());
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LISTEN-002"))]
#[tokio::test]
async fn occupied_port_fails_without_fallback() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap();
    let (_dir, url) = file_cache();
    let text = config(&[("private", tier(&url, ""))]).replace("127.0.0.1:0", &addr.to_string());
    let config = ProxyConfig::parse(&text).unwrap();
    match stockhouse_proxy::bind(&config, stderr_log()).await {
        Err(ServeError::AddressInUse { addr: reported }) => assert_eq!(reported, addr),
        other => panic!("expected AddressInUse, got {:?}", other.err()),
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LISTEN-003"))]
#[tokio::test]
async fn serves_its_own_cache_info() {
    let journal = journal();
    let upstream = Fixture::start("private", &journal).await;
    let proxy = Proxy::start(
        &config(&[("private", tier(&upstream.url, ""))])
            .replace("version = 1\n", "version = 1\npriority = 41\n"),
    )
    .await;
    let (status, body) = proxy.get("/nix-cache-info").await;
    assert_eq!(status, 200);
    assert_eq!(
        String::from_utf8(body).unwrap(),
        "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 41\n"
    );
    let head = proxy
        .client
        .head(format!("{}/nix-cache-info", proxy.base))
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);
    assert!(upstream.seen_all().is_empty());
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LISTEN-004"))]
#[tokio::test]
async fn unknown_routes_are_not_proxied() {
    let journal = journal();
    let upstream = Fixture::start("private", &journal).await;
    let proxy = Proxy::start(&config(&[("private", tier(&upstream.url, ""))])).await;
    for path in ["/", "/nar", "/x.ls", "/log/abc", "/realisations/x.doi"] {
        assert_eq!(proxy.status(path).await, 404, "{path}");
    }
    assert!(upstream.seen_all().is_empty());
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LISTEN-005"))]
#[tokio::test]
async fn only_get_and_head_are_accepted() {
    let journal = journal();
    let upstream = Fixture::start("private", &journal).await;
    let proxy = Proxy::start(&config(&[("private", tier(&upstream.url, ""))])).await;
    let hash = hash_for("x");
    let post = proxy
        .client
        .post(format!("{}/{hash}.narinfo", proxy.base))
        .send()
        .await
        .unwrap();
    assert_eq!(post.status(), 405);
    let head_nar = proxy
        .client
        .head(format!("{}/nar/private/{hash}/nar/{hash}.nar", proxy.base))
        .send()
        .await
        .unwrap();
    assert_eq!(head_nar.status(), 405);
    assert!(upstream.seen_all().is_empty());
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LISTEN-006"))]
#[tokio::test]
async fn invalid_narinfo_hashes_are_rejected() {
    let journal = journal();
    let upstream = Fixture::start("private", &journal).await;
    let proxy = Proxy::start(&config(&[("private", tier(&upstream.url, ""))])).await;
    for hash in ["short", "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", ""] {
        assert_eq!(
            proxy.status(&format!("/{hash}.narinfo")).await,
            400,
            "{hash}"
        );
    }
    assert!(upstream.seen_all().is_empty());
}
