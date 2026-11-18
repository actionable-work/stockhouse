//! Starts fake server modes in-process and talks raw HTTP/1.1 to them, so a
//! connection closed without a reply is observable.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub fn temp_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("stockhouse-fake-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Parses `args` (after the command name's program) and serves the mode on
/// an ephemeral loopback port.
pub async fn start(args: &[&str]) -> SocketAddr {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let mode = stockhouse_fake_server_cli::parse(&args).expect("valid arguments");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(mode.serve(listener));
    addr
}

/// A parsed reply.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub head: String,
    pub body: String,
}

impl Reply {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).expect("JSON body")
    }
}

pub fn request(method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let mut raw = head.into_bytes();
    raw.extend_from_slice(body);
    raw
}

/// Sends raw bytes and reads until the server closes; with `hang_up` the
/// write side is closed first, cutting a body short.
pub async fn send_raw(addr: SocketAddr, raw: &[u8], hang_up: bool) -> Option<Reply> {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(raw).await.unwrap();
    if hang_up {
        let _ = stream.shutdown().await;
    }
    let mut out = Vec::new();
    let _ = stream.read_to_end(&mut out).await;
    let text = String::from_utf8_lossy(&out).into_owned();
    let (head, body) = text.split_once("\r\n\r\n")?;
    let status = head.split(' ').nth(1)?.parse().ok()?;
    Some(Reply {
        status,
        head: head.to_ascii_lowercase(),
        body: body.to_owned(),
    })
}

pub async fn exchange(addr: SocketAddr, raw: &[u8]) -> Option<Reply> {
    send_raw(addr, raw, false).await
}

pub async fn call(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Option<Reply> {
    exchange(addr, &request(method, path, headers, body)).await
}

pub async fn get(addr: SocketAddr, path: &str) -> Reply {
    call(addr, "GET", path, &[], b"").await.expect("a reply")
}

/// Polls until `file` contains `needle`.
pub async fn wait_for(file: &Path, needle: &str) {
    for _ in 0..100 {
        if std::fs::read_to_string(file).is_ok_and(|t| t.contains(needle)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{needle:?} never appeared in {}", file.display());
}
