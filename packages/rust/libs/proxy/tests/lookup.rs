mod common;
use common::*;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use stockhouse_proto::NarInfo;

struct Three {
    private: Fixture,
    shared: Fixture,
    public: Fixture,
    proxy: Proxy,
}

async fn three(private_extra: &str) -> Three {
    let journal = journal();
    let private = Fixture::start("private", &journal).await;
    let shared = Fixture::start("shared", &journal).await;
    let public = Fixture::start("public", &journal).await;
    let tiers = [
        ("private", tier(&private.url, private_extra)),
        ("shared", tier(&shared.url, "")),
        ("public", tier(&public.url, "")),
    ];
    let proxy = Proxy::start(&config(&tiers)).await;
    Three { private, shared, public, proxy }
}

fn narinfo_paths(fixture: &Fixture) -> Vec<String> {
    fixture.seen().into_iter().map(|s| s.path).filter(|p| p.ends_with(".narinfo")).collect()
}

fn order(t: &Three) -> Vec<String> {
    let all = t.private.journal_all();
    all.into_iter().filter(|s| s.path.ends_with(".narinfo")).map(|s| s.fixture).collect()
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-001"))]
#[tokio::test]
async fn tiers_are_consulted_in_order() {
    let t = three("").await;
    let entry = Entry::new("hello", b"nar");
    t.public.add(&entry);
    assert_eq!(t.proxy.status(&format!("/{}", entry.narinfo_path())).await, 200);
    assert_eq!(order(&t), ["private", "shared", "public"]);
    let journal = journal();
    let only_public = Fixture::start("public", &journal).await;
    only_public.add(&entry);
    let proxy = Proxy::start(&config(&[("public", tier(&only_public.url, ""))])).await;
    assert_eq!(proxy.status(&format!("/{}", entry.narinfo_path())).await, 200);
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-002"))]
#[tokio::test]
async fn first_verified_hit_is_served() {
    let t = three("").await;
    let entry = Entry::new("hello", b"nar");
    t.private.add(&entry);
    let (status, body) = t.proxy.get(&format!("/{}", entry.narinfo_path())).await;
    assert_eq!(status, 200);
    assert_eq!(NarInfo::parse(&body).unwrap().store_path, entry.info.store_path);
    assert!(t.shared.seen_all().is_empty() && t.public.seen_all().is_empty());
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-003"))]
#[tokio::test]
async fn genuine_miss_falls_through() {
    let t = three("").await;
    let entry = Entry::new("hello", b"nar");
    t.shared.add(&entry);
    assert_eq!(t.proxy.status(&format!("/{}", entry.narinfo_path())).await, 200);
    assert_eq!(narinfo_paths(&t.private), [format!("/{}", entry.narinfo_path())]);
    assert!(t.public.seen().is_empty());
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-004"))]
#[tokio::test]
async fn miss_everywhere_is_404() {
    let t = three("").await;
    assert_eq!(t.proxy.status(&format!("/{}.narinfo", hash_for("absent"))).await, 404);
    assert_eq!(order(&t), ["private", "shared", "public"]);
}

async fn assert_stops(t: &Three, path: &str, status: u16, kind: &str) {
    assert_eq!(t.proxy.status(path).await, status, "{kind}");
    assert!(t.shared.seen_all().is_empty(), "{kind}: shared contacted");
    assert!(t.public.seen_all().is_empty(), "{kind}: public contacted");
    assert!(t.proxy.log_lines().iter().any(|l| l.ends_with(&format!("private {kind}"))), "{:?}", t.proxy.log_lines());
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-005"))]
#[tokio::test]
async fn authorization_failures_do_not_fall_through() {
    for code in [401, 403] {
        let t = three("").await;
        let hash = hash_for("denied");
        t.private.set(&format!("{hash}.narinfo"), Reply::status(code));
        assert_stops(&t, &format!("/{hash}.narinfo"), 503, "denied").await;
    }
    let t = three("").await;
    t.private.set("nix-cache-info", Reply::status(403));
    assert_stops(&t, &format!("/{}.narinfo", hash_for("x")), 503, "denied").await;
    let (dir, url) = file_cache();
    let entry = Entry::new("locked", b"nar");
    entry.write_to(&dir);
    std::fs::set_permissions(dir.join(entry.narinfo_path()), std::fs::Permissions::from_mode(0)).unwrap();
    let journal = journal();
    let public = Fixture::start("public", &journal).await;
    let proxy = Proxy::start(&config(&[("private", tier(&url, "")), ("public", tier(&public.url, ""))])).await;
    assert_eq!(proxy.status(&format!("/{}", entry.narinfo_path())).await, 503);
    assert!(public.seen_all().is_empty());
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-006"))]
#[tokio::test]
async fn upstream_outages_do_not_fall_through() {
    let hash = hash_for("outage");
    let path = format!("{hash}.narinfo");
    let redirect = Reply { status: 302, location: Some("http://127.0.0.1:9/".to_owned()), ..Reply::status(302) };
    let oversized = Reply::ok(vec![b'a'; 1024 * 1024 + 1]);
    for reply in [Reply::status(500), Reply::status(410), redirect, oversized] {
        let t = three("").await;
        t.private.set(&path, reply);
        assert_stops(&t, &format!("/{path}"), 503, "unavailable").await;
    }
    let journal = journal();
    let public = Fixture::start("public", &journal).await;
    let dead = tier("http://127.0.0.1:9/", "");
    let proxy = Proxy::start(&config(&[("private", dead), ("public", tier(&public.url, ""))])).await;
    assert_eq!(proxy.status(&format!("/{path}")).await, 503);
    assert!(public.seen_all().is_empty());
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-007"))]
#[tokio::test]
async fn malformed_narinfo_does_not_fall_through() {
    let t = three("").await;
    let hash = hash_for("bad");
    t.private.set(&format!("{hash}.narinfo"), Reply::ok("StorePath: nope\n"));
    assert_stops(&t, &format!("/{hash}.narinfo"), 502, "narinfo-invalid").await;
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-008"))]
#[tokio::test]
async fn mismatched_store_paths_do_not_fall_through() {
    let t = three("").await;
    let entry = Entry::new("other", b"nar");
    let asked = hash_for("asked");
    t.private.set(&format!("{asked}.narinfo"), Reply::ok(entry.narinfo_text()));
    assert_stops(&t, &format!("/{asked}.narinfo"), 502, "store-path-mismatch").await;
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-009"))]
#[tokio::test]
async fn signature_failures_do_not_fall_through() {
    let base = Entry::new("signed", b"nar");
    let mut tampered = base.clone();
    tampered.info.nar_size += 1;
    for entry in [base.clone().unsigned(), base.clone().signed_by("stranger-1", 9), base.signed_by(KEY_NAME, 7), tampered] {
        let t = three("").await;
        t.private.set(&entry.narinfo_path(), Reply::ok(entry.narinfo_text()));
        assert_stops(&t, &format!("/{}", entry.narinfo_path()), 502, "signature").await;
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-010"))]
#[tokio::test]
async fn compressed_without_file_hash_is_served() {
    let t = three("").await;
    let entry = Entry::compressed("packed", b"xz-bytes", false);
    t.private.add(&entry);
    assert_eq!(t.proxy.status(&format!("/{}", entry.narinfo_path())).await, 200);
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-011"))]
#[tokio::test]
async fn credential_failures_do_not_fall_through() {
    let missing = temp_dir().join("absent-token");
    let t = three(&format!("credential = {{ file = \"{}\" }}", missing.display())).await;
    let entry = Entry::new("hello", b"nar");
    t.private.add(&entry);
    t.public.add(&entry);
    assert_eq!(t.proxy.status(&format!("/{}", entry.narinfo_path())).await, 503);
    assert!(t.private.seen_all().is_empty(), "no request may reach the tier");
    assert!(t.public.seen_all().is_empty());
    assert!(t.proxy.log_lines().iter().any(|l| l.ends_with("private credential")));
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-012"))]
#[tokio::test]
async fn upstream_cache_info_is_validated() {
    let hash = hash_for("x");
    for (reply, kind) in [(Reply::status(404), "cache-info-missing"), (Reply::ok("StoreDir: /gnu/store\n"), "cache-info-invalid")] {
        let t = three("").await;
        t.private.set("nix-cache-info", reply);
        assert_stops(&t, &format!("/{hash}.narinfo"), 502, kind).await;
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-013"))]
#[tokio::test]
async fn validated_cache_info_is_remembered() {
    let t = three("").await;
    for label in ["a", "b", "c"] {
        assert_eq!(t.proxy.status(&format!("/{}.narinfo", hash_for(label))).await, 404);
    }
    let fetched = t.private.seen_all().iter().filter(|s| s.path == "/nix-cache-info").count();
    assert_eq!(fetched, 1);
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-014"))]
#[tokio::test]
async fn served_narinfo_routes_to_winning_tier() {
    let t = three("").await;
    let entry = Entry::new("hello", b"nar");
    t.shared.add(&entry);
    let (_, body) = t.proxy.get(&format!("/{}", entry.narinfo_path())).await;
    let served = NarInfo::parse(&body).unwrap();
    assert_eq!(served.url, format!("nar/shared/{}/{}", entry.hash, entry.nar_url));
    assert_eq!(served.signatures, entry.info.signatures);
    assert_eq!(NarInfo { url: entry.info.url.clone(), ..served }, entry.info);
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-015"))]
#[tokio::test]
async fn higher_tiers_win_collisions() {
    let t = three("").await;
    let private = Entry::new("same", b"private bytes");
    let public = Entry::new("same", b"public bytes, different");
    assert_eq!(private.hash, public.hash);
    t.private.add(&private);
    t.public.add(&public);
    let (_, body) = t.proxy.get(&format!("/{}", private.narinfo_path())).await;
    let served = NarInfo::parse(&body).unwrap();
    assert_eq!(served.nar_hash, private.info.nar_hash);
    assert!(served.url.starts_with("nar/private/"));
    let (status, nar) = t.proxy.get(&format!("/{}", served.url)).await;
    assert_eq!((status, nar), (200, private.nar.clone()));
    assert!(t.public.seen_all().is_empty());
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-016"))]
#[tokio::test]
async fn head_narinfo_follows_get() {
    let t = three("").await;
    let entry = Entry::new("hello", b"nar");
    t.private.add(&entry);
    let bad = hash_for("bad");
    t.private.set(&format!("{bad}.narinfo"), Reply::status(500));
    for (path, status) in [(entry.narinfo_path(), 200), (format!("{}.narinfo", hash_for("absent")), 404), (format!("{bad}.narinfo"), 503)] {
        let response = t.proxy.client.head(format!("{}/{path}", t.proxy.base)).send().await.unwrap();
        assert_eq!(response.status().as_u16(), status, "{path}");
        assert!(response.bytes().await.unwrap().is_empty());
    }
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-LOOKUP-017"))]
#[tokio::test]
async fn slow_upstreams_time_out() {
    let t = three("").await;
    let hash = hash_for("slow");
    t.private.set(&format!("{hash}.narinfo"), Reply { delay: Duration::from_secs(3), ..Reply::status(404) });
    assert_stops(&t, &format!("/{hash}.narinfo"), 503, "unavailable").await;
}
