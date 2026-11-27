//! A separate binary: it sets process-wide proxy variables.
mod common;
use common::*;
use std::sync::Arc;
use stockhouse_ingest::{UploadOutcome, Uploader};

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-003"))]
#[test]
fn environment_proxies_never_receive_requests() {
    let trap =
        FakeServer::start(|req| req.body.as_ref().map(|_| Action::Reply(502, String::new())));
    let proxy = format!("http://{}", trap.addr);
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        // SAFETY: this test binary has a single test and sets variables before any thread reads them.
        unsafe { std::env::set_var(name, &proxy) };
    }
    let mut store = Store::new();
    let root = store.add("root", &[]);
    let server = Protocol::start(Arc::default(), |_| None);
    let queue = store.queue();
    assert_eq!(
        uploader(&store, &server).upload(&job(&queue, &root)),
        UploadOutcome::Uploaded
    );
    assert!(trap.requests().is_empty());
}
