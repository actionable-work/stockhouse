#![allow(dead_code)]
//! Proxy component-test fixtures. Keys come from fixed public seeds.

pub mod cache;
pub mod http;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use stockhouse_proxy::{LogSink, ProxyConfig};

pub use cache::*;
pub use http::*;

pub fn temp_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("stockhouse-proxy-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A running proxy with captured log lines.
pub struct Proxy {
    pub base: String,
    pub logs: Arc<Mutex<Vec<String>>>,
    pub client: reqwest::Client,
}

impl Proxy {
    pub async fn start(config: &str) -> Proxy {
        let config = ProxyConfig::parse(config).expect("test config parses");
        let logs = Arc::new(Mutex::new(Vec::new()));
        let captured = logs.clone();
        let sink: LogSink = Arc::new(move |line| captured.lock().unwrap().push(line.to_owned()));
        let server = stockhouse_proxy::bind(&config, sink).await.expect("proxy binds");
        let base = format!("http://{}", server.local_addr());
        tokio::spawn(server.run());
        Proxy { base, logs, client: reqwest::Client::new() }
    }

    pub async fn get(&self, path: &str) -> (u16, Vec<u8>) {
        let response = self.client.get(format!("{}{path}", self.base)).send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.bytes().await.map(|b| b.to_vec()).unwrap_or_default())
    }

    pub async fn status(&self, path: &str) -> u16 {
        self.get(path).await.0
    }

    pub fn log_lines(&self) -> Vec<String> {
        self.logs.lock().unwrap().clone()
    }
}

/// Builds proxy TOML from tier tables.
pub fn config(tiers: &[(&str, String)]) -> String {
    let mut out = String::from("version = 1\nlisten = \"127.0.0.1:0\"\nrequest_timeout_secs = 1\n");
    for (name, body) in tiers {
        out.push_str(&format!("\n[{name}]\n{body}\n"));
    }
    out
}

/// A tier table with the fixture key trusted.
pub fn tier(url: &str, extra: &str) -> String {
    format!("url = \"{url}\"\ntrusted_keys = [\"{}\"]\n{extra}", fixture_key().to_string())
}
