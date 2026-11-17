mod common;
use common::*;

struct Setup {
    private: Fixture,
    proxy: Proxy,
}

async fn setup() -> Setup {
    let journal = journal();
    let private = Fixture::start("private", &journal).await;
    let proxy = Proxy::start(&config(&[("private", tier(&private.url, ""))])).await;
    Setup { private, proxy }
}

fn route(entry: &Entry) -> String {
    format!("/nar/private/{}/{}", entry.hash, entry.nar_url)
}

fn big_nar() -> Vec<u8> {
    (0..300_000u32).map(|i| (i % 251) as u8).collect()
}

/// Reads a full body, reporting whether the transfer completed.
async fn fetch(proxy: &Proxy, path: &str) -> (u16, Option<u64>, Result<Vec<u8>, ()>) {
    let response = proxy
        .client
        .get(format!("{}{path}", proxy.base))
        .send()
        .await
        .unwrap();
    let (status, len) = (response.status().as_u16(), response.content_length());
    (
        status,
        len,
        response.bytes().await.map(|b| b.to_vec()).map_err(|_| ()),
    )
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-NAR-001"))]
#[tokio::test]
async fn nar_routes_reverify_against_the_named_tier() {
    let s = setup().await;
    let entry = Entry::new("hello", b"nar bytes");
    s.private.add(&entry);
    assert_eq!(
        fetch(&s.proxy, &route(&entry)).await.2,
        Ok(entry.nar.clone())
    );
    let paths: Vec<_> = s.private.seen().into_iter().map(|r| r.path).collect();
    assert_eq!(
        paths,
        [
            format!("/{}", entry.narinfo_path()),
            format!("/{}", entry.nar_url)
        ]
    );
    let s = setup().await;
    let unsigned = Entry::new("hello", b"nar bytes").unsigned();
    s.private.add(&unsigned);
    assert_eq!(fetch(&s.proxy, &route(&unsigned)).await.0, 502);
    assert!(
        s.private
            .seen()
            .iter()
            .all(|r| r.path.ends_with(".narinfo")),
        "no NAR bytes fetched"
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-NAR-002"))]
#[tokio::test]
async fn nar_urls_must_match_the_narinfo() {
    let s = setup().await;
    let entry = Entry::new("hello", b"nar");
    s.private.add(&entry);
    let path = format!("/nar/private/{}/nar/other.nar", entry.hash);
    assert_eq!(fetch(&s.proxy, &path).await.0, 502);
    assert!(
        s.proxy
            .log_lines()
            .iter()
            .any(|l| l.ends_with("narinfo-changed"))
    );
    assert!(
        s.private
            .seen()
            .iter()
            .all(|r| r.path.ends_with(".narinfo"))
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-NAR-003"))]
#[tokio::test]
async fn unconfigured_or_invalid_routes_are_404() {
    let s = setup().await;
    let hash = hash_for("x");
    for path in [
        format!("/nar/public/{hash}/nar/x.nar"),
        "/nar/private/bad/nar/x.nar".to_owned(),
        format!("/nar/vault/{hash}/x"),
    ] {
        assert_eq!(fetch(&s.proxy, &path).await.0, 404, "{path}");
    }
    assert!(s.private.seen_all().is_empty());
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-NAR-004"))]
#[tokio::test]
async fn verified_nar_bytes_stream_completely() {
    let s = setup().await;
    for entry in [
        Entry::new("big", &big_nar()),
        Entry::compressed("packed", &big_nar(), true),
    ] {
        s.private.add(&entry);
        let (status, len, body) = fetch(&s.proxy, &route(&entry)).await;
        assert_eq!((status, len), (200, Some(entry.nar.len() as u64)));
        assert_eq!(body, Ok(entry.nar.clone()));
    }
    let (dir, url) = file_cache();
    let entry = Entry::new("from-file", &big_nar());
    entry.write_to(&dir);
    let proxy = Proxy::start(&config(&[("private", tier(&url, ""))])).await;
    assert_eq!(fetch(&proxy, &route(&entry)).await.2, Ok(entry.nar));
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-NAR-005"))]
#[tokio::test]
async fn altered_nar_bytes_never_complete() {
    let original = big_nar();
    let mut flipped = original.clone();
    *flipped.last_mut().unwrap() ^= 1;
    let longer = [original.clone(), b"x".to_vec()].concat();
    let shorter = original[..original.len() - 1].to_vec();
    for served in [flipped, longer, shorter] {
        let s = setup().await;
        let entry = Entry::new("victim", &original);
        s.private.add(&entry);
        s.private.set(&entry.nar_url, Reply::ok(served));
        let (status, _, body) = fetch(&s.proxy, &route(&entry)).await;
        assert_eq!(status, 200);
        assert_eq!(body, Err(()), "an altered transfer must not complete");
    }
    let (dir, url) = file_cache();
    let entry = Entry::new("file-victim", &original);
    entry.write_to(&dir);
    std::fs::write(dir.join(&entry.nar_url), b"tampered").unwrap();
    let proxy = Proxy::start(&config(&[("private", tier(&url, ""))])).await;
    assert_eq!(fetch(&proxy, &route(&entry)).await.2, Err(()));
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-NAR-006"))]
#[tokio::test]
async fn missing_nar_is_an_error() {
    let s = setup().await;
    let entry = Entry::new("gone", b"nar");
    s.private
        .set(&entry.narinfo_path(), Reply::ok(entry.narinfo_text()));
    assert_eq!(fetch(&s.proxy, &route(&entry)).await.0, 502);
    let vanished = Entry::new("vanished", b"nar");
    assert_eq!(fetch(&s.proxy, &route(&vanished)).await.0, 502);
    assert_eq!(
        s.proxy
            .log_lines()
            .iter()
            .filter(|l| l.ends_with("nar-missing"))
            .count(),
        2
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-NAR-007"))]
#[tokio::test]
async fn nar_route_errors_map_like_lookup_errors() {
    let s = setup().await;
    let entry = Entry::new("hello", b"nar");
    s.private.add(&entry);
    s.private.set(&entry.nar_url, Reply::status(403));
    assert_eq!(fetch(&s.proxy, &route(&entry)).await.0, 503);
    s.private.set(&entry.nar_url, Reply::status(500));
    assert_eq!(fetch(&s.proxy, &route(&entry)).await.0, 503);
    s.private.set(&entry.narinfo_path(), Reply::ok("bad"));
    assert_eq!(fetch(&s.proxy, &route(&entry)).await.0, 502);
    let missing = temp_dir().join("absent");
    let journal = journal();
    let shared = Fixture::start("shared", &journal).await;
    let cred = format!("credential = {{ file = \"{}\" }}", missing.display());
    let proxy = Proxy::start(&config(&[("shared", tier(&shared.url, &cred))])).await;
    let path = format!("/nar/shared/{}/{}", entry.hash, entry.nar_url);
    assert_eq!(fetch(&proxy, &path).await.0, 503);
    assert!(shared.seen_all().is_empty());
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-NAR-008"))]
#[tokio::test]
async fn range_requests_receive_the_full_representation() {
    let s = setup().await;
    let entry = Entry::new("hello", &big_nar());
    s.private.add(&entry);
    let response = s
        .proxy
        .client
        .get(format!("{}{}", s.proxy.base, route(&entry)))
        .header("range", "bytes=100-")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert!(response.headers().get("accept-ranges").is_none());
    assert_eq!(response.bytes().await.unwrap().to_vec(), entry.nar);
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-NAR-009"))]
#[tokio::test]
async fn unchecked_representations_pass_through() {
    let s = setup().await;
    let entry = Entry::compressed("packed", b"opaque xz bytes", false);
    s.private.add(&entry);
    let (status, _, body) = fetch(&s.proxy, &route(&entry)).await;
    assert_eq!((status, body), (200, Ok(entry.nar.clone())));
}
