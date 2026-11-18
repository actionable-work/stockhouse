//! A separate binary: it sets process-wide proxy variables.

mod common;
use common::*;
use std::io::Read;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use stockhouse_auth::{block_on, post_form, token_outcome};
use stockhouse_oauth_fake::Scripted;

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

#[cfg_attr(any(), bloomery("CLIENT-CREDS-GRANT-023"))]
#[test]
fn endpoint_requests_ignore_environment_proxies() {
    let (proxy, hits) = trap();
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        // SAFETY: this binary has one test, which sets the variables before any thread reads them.
        unsafe { std::env::set_var(name, &proxy) };
    }
    let s = Setup::new();
    s.fake.state().script(
        "/oauth/token",
        Scripted::json(
            200,
            r#"{"access_token":"tok-1","token_type":"Bearer","expires_in":600}"#,
        ),
    );
    let url = s.config.endpoints.token.clone();
    let fields = [
        ("grant_type", "refresh_token"),
        ("client_id", "stockhouse-test"),
    ];
    let tokens = token_outcome(block_on(post_form(&s.config, &url, &fields)), NOW);
    assert!(tokens.is_ok(), "{:?}", tokens.err());
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}
