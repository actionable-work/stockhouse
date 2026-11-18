//! A threaded HTTP/1.1 fake of a protocol v1 server for component tests.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use stockhouse_proto::Sha256;
use stockhouse_wire::UploadDeclaration;

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
    /// Close without replying.
    Close,
    /// Wait, then reply.
    Sleep(Duration, u16, String),
}

type Handler = dyn Fn(&Request) -> Option<Action> + Send + Sync;

/// A loopback fake server.
pub struct FakeServer {
    pub addr: SocketAddr,
    pub log: Arc<Mutex<Vec<Request>>>,
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

    /// A protocol server, with `before` consulted first on full requests.
    pub fn protocol(
        proto: Arc<Protocol>,
        before: impl Fn(&Request) -> Option<Action> + Send + Sync + 'static,
    ) -> Self {
        Self::start(move |req| {
            req.body.as_ref()?;
            before(req).or_else(|| Some(proto.handle(req)))
        })
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

fn head(stream: &mut TcpStream) -> Option<(Request, usize, Vec<u8>)> {
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
    Some((request, len, Vec::new()))
}

fn serve(mut stream: TcpStream, handler: &Handler, log: &Mutex<Vec<Request>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
    let Some((mut request, len, mut body)) = head(&mut stream) else {
        return;
    };
    let action = match handler(&request) {
        Some(early) => early,
        None => {
            let read = (&mut stream).take(len as u64).read_to_end(&mut body);
            request.complete = read.is_ok() && body.len() == len;
            request.body = Some(body);
            handler(&request).unwrap_or(Action::Close)
        }
    };
    log.lock().unwrap().push(request);
    let (status, body) = match action {
        Action::Close => return,
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

#[derive(Default)]
struct State {
    uploads: HashMap<String, (UploadDeclaration, &'static str)>,
    held: HashSet<String>,
}

/// A minimal protocol v1 server over in-memory state.
#[derive(Default)]
pub struct Protocol {
    state: Mutex<State>,
}

impl Protocol {
    /// Marks a store path basename as already held.
    pub fn hold(&self, basename: &str) {
        self.state.lock().unwrap().held.insert(basename.to_owned());
    }

    /// Forgets a held path (as server GC would).
    pub fn forget(&self, basename: &str) {
        self.state.lock().unwrap().held.remove(basename);
    }

    pub fn holds(&self, basename: &str) -> bool {
        self.state.lock().unwrap().held.contains(basename)
    }

    pub fn handle(&self, req: &Request) -> Action {
        let bearer = format!("Bearer {}", super::TOKEN);
        if req.header("authorization") != Some(bearer.as_str()) {
            return Action::Reply(401, String::new());
        }
        let rel = req.rel().to_owned();
        let parts: Vec<&str> = rel.split('/').collect();
        let mut state = self.state.lock().unwrap();
        match (req.method.as_str(), parts.as_slice()) {
            ("POST", ["uploads", id]) => declare(&mut state, id, req),
            ("GET", ["uploads", id]) => match state.uploads.get(*id) {
                Some((_, s)) => self::state(s),
                None => error(404, "unknown_upload"),
            },
            ("PUT", ["uploads", id, "nar"]) => nar(&mut state, id, req),
            ("PUT", ["leases", _, _]) => self::state("renewed"),
            _ => Action::Reply(404, String::new()),
        }
    }
}

fn declare(state: &mut State, id: &str, req: &Request) -> Action {
    let Ok(decl) = UploadDeclaration::parse(req.body.as_deref().unwrap_or_default()) else {
        return error(400, "bad_request");
    };
    let base = decl.store_path().basename().to_owned();
    if let Some((_, "published")) = state.uploads.get(id) {
        return self::state("published");
    }
    if state.held.contains(&base) {
        return self::state("present");
    }
    let missing: Vec<String> = decl
        .references()
        .iter()
        .map(|r| r.basename().to_owned())
        .filter(|r| *r != base && !state.held.contains(r))
        .map(|r| format!("\"{r}\""))
        .collect();
    if !missing.is_empty() {
        let body = format!(
            "{{\"error\":\"missing_references\",\"missing\":[{}]}}",
            missing.join(",")
        );
        return Action::Reply(422, body);
    }
    state.uploads.entry(id.to_owned()).or_insert((decl, "open"));
    self::state("open")
}

fn nar(state: &mut State, id: &str, req: &Request) -> Action {
    let Some((decl, _)) = state.uploads.get(id).cloned() else {
        return error(404, "unknown_upload");
    };
    if !req.complete {
        return Action::Close;
    }
    let body = req.body.as_deref().unwrap_or_default();
    if Sha256::digest(body) != decl.nar_hash() || body.len() as u64 != decl.nar_size() {
        return error(400, "verification_failed");
    }
    state.held.insert(decl.store_path().basename().to_owned());
    state.uploads.insert(id.to_owned(), (decl, "published"));
    self::state("published")
}
