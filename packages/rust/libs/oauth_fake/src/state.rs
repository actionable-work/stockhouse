use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// One request the fake received.
#[derive(Clone, Debug)]
pub struct Recorded {
    /// Method.
    pub method: String,
    /// Path without query.
    pub path: String,
    /// Query pairs (authorize) or form pairs (POST).
    pub fields: Vec<(String, String)>,
    /// The `Authorization` header, if any.
    pub authorization: Option<String>,
}

impl Recorded {
    /// The first value of a field.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A scripted reply that overrides the default behavior once.
#[derive(Clone, Debug)]
pub struct Scripted {
    /// Status.
    pub status: u16,
    /// Body.
    pub body: String,
    /// Delay before answering.
    pub delay: Duration,
    /// `Location` header.
    pub location: Option<String>,
}

impl Scripted {
    /// A JSON reply.
    pub fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            body: body.to_owned(),
            delay: Duration::ZERO,
            location: None,
        }
    }

    /// An OAuth error reply.
    pub fn error(status: u16, code: &str) -> Self {
        Self::json(
            status,
            &format!("{{\"error\":\"{code}\",\"error_description\":\"canary-description-5f2\"}}"),
        )
    }
}

/// Fake server state.
#[derive(Debug)]
pub struct State {
    /// Every request, in order.
    pub requests: Vec<Recorded>,
    /// Scripted replies per path, consumed first.
    pub scripted: HashMap<String, VecDeque<Scripted>>,
    /// Access token lifetime in seconds.
    pub lifetime: u64,
    /// Whether to issue refresh tokens.
    pub issue_refresh: bool,
    /// The current access token.
    pub access: Option<String>,
    /// When the current access token expires.
    pub access_expires: Option<Instant>,
    /// The current refresh token.
    pub refresh: Option<String>,
    /// Refresh tokens already rotated away.
    pub rotated: Vec<String>,
    /// Refresh tokens used more than once (reuse events).
    pub reuse: Vec<String>,
    /// Refresh requests currently in flight, by token.
    pub in_flight: HashMap<String, usize>,
    /// Refresh tokens seen concurrently.
    pub overlaps: Vec<String>,
    /// Delay applied to refresh requests.
    pub refresh_delay: Duration,
    /// Authorization codes: code → (challenge, redirect URI).
    pub codes: HashMap<String, (String, String)>,
    /// Device codes: device code → (user code, approved).
    pub devices: HashMap<String, (String, bool)>,
    /// Device response interval.
    pub interval: u64,
    /// Device response `expires_in`.
    pub device_expires: u64,
    /// Approve device codes immediately.
    pub auto_approve: bool,
    /// Cache files under `/t/1/`.
    pub cache: HashMap<String, Vec<u8>>,
    /// Files served under `/t/1/` when absent from `cache`.
    pub cache_dir: Option<PathBuf>,
    /// Every token issued.
    pub issued: Vec<String>,
    /// Every token sent to the revocation endpoint.
    pub revoked: Vec<String>,
    /// Refresh grants received.
    pub refreshes: u64,
    /// The device `verification_uri`.
    pub verification_uri: String,
    /// The granted `scope`.
    pub scope: String,
    counter: u64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            requests: Vec::new(),
            scripted: HashMap::new(),
            lifetime: 600,
            issue_refresh: true,
            access: None,
            access_expires: None,
            refresh: None,
            rotated: Vec::new(),
            reuse: Vec::new(),
            in_flight: HashMap::new(),
            overlaps: Vec::new(),
            refresh_delay: Duration::ZERO,
            codes: HashMap::new(),
            devices: HashMap::new(),
            interval: 1,
            device_expires: 600,
            auto_approve: true,
            cache: HashMap::new(),
            cache_dir: None,
            issued: Vec::new(),
            revoked: Vec::new(),
            refreshes: 0,
            verification_uri: "http://127.0.0.1/device".to_owned(),
            scope: "cache:read cache:write".to_owned(),
            counter: 0,
        }
    }
}

impl State {
    /// Queues a scripted reply for `path` (for example `/oauth/token`).
    pub fn script(&mut self, path: &str, reply: Scripted) {
        self.scripted
            .entry(path.to_owned())
            .or_default()
            .push_back(reply);
    }

    /// Requests to `path`.
    pub fn seen(&self, path: &str) -> Vec<Recorded> {
        self.requests
            .iter()
            .filter(|r| r.path == path)
            .cloned()
            .collect()
    }

    /// A fresh unique value.
    pub fn next(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}-{}-a7f3", self.counter)
    }

    /// Issues a new access (and refresh) token pair as the current family.
    pub fn issue(&mut self) -> String {
        let access = self.next("fake-access");
        let refresh = self.issue_refresh.then(|| self.next("fake-refresh"));
        if let Some(old) = self.refresh.take() {
            self.rotated.push(old);
        }
        self.issued.push(access.clone());
        self.issued.extend(refresh.clone());
        self.access = Some(access.clone());
        self.access_expires = Some(Instant::now() + Duration::from_secs(self.lifetime));
        self.refresh = refresh.clone();
        let mut body = format!(
            "{{\"access_token\":\"{access}\",\"token_type\":\"Bearer\",\"expires_in\":{},\"scope\":\"{}\"",
            self.lifetime, self.scope
        );
        if let Some(refresh) = refresh {
            body.push_str(&format!(",\"refresh_token\":\"{refresh}\""));
        }
        body.push('}');
        body
    }

    /// The current access token, if issued and unexpired.
    pub fn current_access(&self) -> Option<&str> {
        let live = self.access_expires.is_some_and(|at| Instant::now() <= at);
        self.access.as_deref().filter(|_| live)
    }

    /// The control routes' state report, as a JSON object.
    pub fn report(&self) -> String {
        serde_json::json!({
            "lifetime": self.lifetime,
            "access": self.access,
            "refresh": self.refresh,
            "rotated": self.rotated,
            "issued": self.issued,
            "reuse": self.reuse,
            "overlaps": self.overlaps,
            "revoked": self.revoked,
            "refreshes": self.refreshes,
        })
        .to_string()
    }

    /// Ends the current grant family.
    pub fn revoke_family(&mut self) {
        self.access = None;
        if let Some(old) = self.refresh.take() {
            self.rotated.push(old);
        }
    }

    /// Every token, code and verifier the fake saw or issued, for leak checks.
    pub fn secrets(&self) -> Vec<String> {
        let mut out = self.issued.clone();
        out.extend(self.codes.keys().cloned());
        out.extend(self.devices.keys().cloned());
        for name in ["code_verifier", "code", "device_code"] {
            out.extend(
                self.requests
                    .iter()
                    .filter_map(|r| r.field(name).map(str::to_owned)),
            );
        }
        out
    }
}
