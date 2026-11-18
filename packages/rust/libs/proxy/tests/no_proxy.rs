//! A separate binary: it sets process-wide proxy variables.

mod common;
use common::*;
use std::io::Read;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A listener that counts every connection made to it.
fn trap() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = stream.read(&mut [0u8; 1024]);
        }
    });
    (format!("http://{addr}"), hits)
}

#[cfg_attr(any(), bloomery("CLIENT-PROXY-CREDENTIAL-012"))]
#[tokio::test]
async fn upstream_requests_ignore_environment_proxies() {
    let (proxy, hits) = trap();
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        // SAFETY: this binary has one test, which sets the variables before the proxy starts.
        unsafe { std::env::set_var(name, &proxy) };
    }
    let journal = journal();
    let private = Fixture::start("private", &journal).await;
    let entry = Entry::new("hello", b"nar bytes");
    private.add(&entry);
    let server = Proxy::start(&config(&[("private", tier(&private.url, ""))])).await;
    // The test's own client must bypass the trap too.
    let client = reqwest::Client::builder()
        .no_proxy()
        .tls_certs_only(std::iter::empty::<reqwest::Certificate>())
        .build()
        .unwrap();
    let url = format!("{}/{}", server.base, entry.narinfo_path());
    assert_eq!(client.get(url).send().await.unwrap().status(), 200);
    assert!(!private.seen_all().is_empty());
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}
