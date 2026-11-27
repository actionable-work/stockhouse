//! A threaded loopback HTTP/1.1 stub for component tests.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One recorded request.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    /// `None` while the server decides whether to answer before the body.
    pub body: Option<Vec<u8>>,
    /// The whole declared body arrived.
    pub complete: bool,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The path below `/t/1/`.
    pub fn rel(&self) -> &str {
        self.path.strip_prefix("/t/1/").unwrap_or(&self.path)
    }
}

/// How the server answers.
#[derive(Clone, Debug)]
pub enum Action {
    Reply(u16, String),
    /// Send only headers, including a positive Content-Length and Retry-After.
    Truncated(u16, u32),
    /// Close without replying.
    Close,
    /// Wait, then reply.
    Sleep(Duration, u16, String),
}

type Handler = dyn Fn(&Request) -> Option<Action> + Send + Sync;

/// A loopback fake server.
pub struct FakeServer {
    pub addr: SocketAddr,
    log: Arc<Mutex<Vec<Request>>>,
}

impl FakeServer {
    /// `handler` sees each request twice: before the body (`body: None`,
    /// returning `Some` answers early) and after it.
    pub fn start(handler: impl Fn(&Request) -> Option<Action> + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let (handler, shared): (Arc<Handler>, _) = (Arc::new(handler), log.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (handler, log) = (handler.clone(), shared.clone());
                std::thread::spawn(move || serve(stream, &*handler, &log));
            }
        });
        Self { addr, log }
    }

    pub fn base(&self) -> String {
        format!("http://{}/t/1/", self.addr)
    }

    pub fn requests(&self) -> Vec<Request> {
        self.log.lock().unwrap().clone()
    }

    /// `METHOD rel` lines, in arrival order.
    pub fn lines(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|r| format!("{} {}", r.method, r.rel()))
            .collect()
    }
}

fn head(stream: &mut TcpStream) -> Option<(Request, usize)> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).ok()? == 0 || buf.len() > 64 * 1024 {
            return None;
        }
        buf.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, path) = (first.next()?.to_owned(), first.next()?.to_owned());
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(": "))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    let len = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let request = Request {
        method,
        path,
        headers,
        body: None,
        complete: false,
    };
    Some((request, len))
}

fn serve(mut stream: TcpStream, handler: &Handler, log: &Mutex<Vec<Request>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
    let Some((mut request, len)) = head(&mut stream) else {
        return;
    };
    let action = match handler(&request) {
        Some(early) => early,
        None => {
            let mut body = Vec::new();
            let read = (&mut stream).take(len as u64).read_to_end(&mut body);
            request.complete = read.is_ok() && body.len() == len;
            request.body = Some(body);
            handler(&request).unwrap_or(Action::Close)
        }
    };
    log.lock().unwrap().push(request);
    let (status, body) = match action {
        Action::Close => return,
        Action::Truncated(status, retry_after) => {
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nRetry-After: {retry_after}\r\nContent-Length: 100\r\nConnection: close\r\n\r\n"
            );
            return;
        }
        Action::Reply(status, body) => (status, body),
        Action::Sleep(wait, status, body) => {
            std::thread::sleep(wait);
            (status, body)
        }
    };
    let location = if status / 100 == 3 {
        "Location: http://127.0.0.1:9/x\r\n"
    } else {
        ""
    };
    let reply = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n{location}Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.flush();
}

pub fn state(name: &str) -> Action {
    Action::Reply(200, format!("{{\"state\":\"{name}\"}}"))
}

pub fn error(status: u16, code: &str) -> Action {
    Action::Reply(status, format!("{{\"error\":\"{code}\"}}"))
}
