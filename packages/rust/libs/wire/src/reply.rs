use crate::upload::{MAX_BODY, MAX_REFERENCES};
use serde_json::{Map, Value};
use std::fmt;
use stockhouse_proto::StorePath;

/// Which upload request a reply answers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestKind {
    /// `POST uploads/<id>`.
    Declare,
    /// `PUT uploads/<id>/nar`.
    Nar,
    /// `GET uploads/<id>`.
    Status,
}

/// The server's state for an upload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadState {
    /// The server waits for the NAR.
    Open,
    /// The server already holds the path with this NarHash.
    Present,
    /// This upload completed.
    Published,
}

/// A refusal from the protocol table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UploadRefusal {
    /// 400 `bad_request`.
    BadRequest,
    /// 400 `verification_failed`.
    VerificationFailed,
    /// 404 `unknown_upload`.
    UnknownUpload,
    /// 409 `conflict`.
    Conflict,
    /// 413 `too_large`.
    TooLarge,
    /// 422 `missing_references` with the missing basenames.
    MissingReferences(Vec<StorePath>),
    /// 507 `quota_exceeded`.
    QuotaExceeded,
    /// 401.
    Unauthorized,
    /// 503.
    Unavailable,
}

impl UploadRefusal {
    /// The protocol code, for messages.
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::VerificationFailed => "verification_failed",
            Self::UnknownUpload => "unknown_upload",
            Self::Conflict => "conflict",
            Self::TooLarge => "too_large",
            Self::MissingReferences(_) => "missing_references",
            Self::QuotaExceeded => "quota_exceeded",
            Self::Unauthorized => "unauthorized",
            Self::Unavailable => "unavailable",
        }
    }
}

/// A classified upload reply.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UploadReply {
    /// A 200 state.
    State(UploadState),
    /// A refusal.
    Refused(UploadRefusal),
}

/// A reply outside the protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ReplyError {
    /// The body exceeds the cap.
    TooLarge,
    /// The status and body pair is not in the table.
    Unexpected {
        /// The HTTP status.
        status: u16,
    },
}

impl fmt::Display for ReplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str("reply too large"),
            Self::Unexpected { status } => write!(f, "unexpected reply (status {status})"),
        }
    }
}

impl std::error::Error for ReplyError {}

/// A reply body: `{"state":…}`, `{"error":…}` or `{"error":…,"missing":[…]}`.
pub(crate) enum Body {
    State(String),
    Error(String, Option<Vec<Value>>),
}

/// Parses a reply body with no unknown keys.
pub(crate) fn body(bytes: &[u8]) -> Option<Body> {
    let Value::Object(mut map) = serde_json::from_slice::<Value>(bytes).ok()? else {
        return None;
    };
    let text = |map: &mut Map<String, Value>, key| match map.remove(key) {
        Some(Value::String(s)) => Some(s),
        _ => None,
    };
    let parsed = if map.contains_key("state") {
        Body::State(text(&mut map, "state")?)
    } else {
        let error = text(&mut map, "error")?;
        let missing = match map.remove("missing") {
            Some(Value::Array(items)) => Some(items),
            Some(_) => return None,
            None => None,
        };
        Body::Error(error, missing)
    };
    map.is_empty().then_some(parsed)
}

fn missing(items: Vec<Value>) -> Option<Vec<StorePath>> {
    if items.is_empty() || items.len() > MAX_REFERENCES {
        return None;
    }
    let paths = items
        .iter()
        .map(|v| v.as_str().and_then(|s| StorePath::from_basename(s).ok()))
        .collect::<Option<Vec<_>>>()?;
    let mut sorted: Vec<&StorePath> = paths.iter().collect();
    sorted.sort();
    sorted.dedup();
    (sorted.len() == paths.len()).then_some(paths)
}

fn state(kind: RequestKind, name: &str) -> Option<UploadState> {
    match name {
        "present" => Some(UploadState::Present),
        "published" => Some(UploadState::Published),
        "open" if kind != RequestKind::Nar => Some(UploadState::Open),
        _ => None,
    }
}

fn refusal(kind: RequestKind, status: u16, code: &str) -> Option<UploadRefusal> {
    use RequestKind::Status;
    let found = match (status, code) {
        (400, "verification_failed") => UploadRefusal::VerificationFailed,
        (400, "bad_request") if kind != Status => UploadRefusal::BadRequest,
        (404, "unknown_upload") => UploadRefusal::UnknownUpload,
        (409, "conflict") if kind != Status => UploadRefusal::Conflict,
        (413, "too_large") if kind != Status => UploadRefusal::TooLarge,
        (507, "quota_exceeded") if kind != Status => UploadRefusal::QuotaExceeded,
        _ => return None,
    };
    Some(found)
}

fn classify(kind: RequestKind, status: u16, parsed: Body) -> Option<UploadReply> {
    match (status, parsed) {
        (200, Body::State(name)) => state(kind, &name).map(UploadReply::State),
        (422, Body::Error(code, Some(items)))
            if code == "missing_references" && kind != RequestKind::Status =>
        {
            missing(items).map(|m| UploadReply::Refused(UploadRefusal::MissingReferences(m)))
        }
        (_, Body::Error(code, None)) => refusal(kind, status, &code).map(UploadReply::Refused),
        _ => None,
    }
}

/// Classifies an upload reply by request kind, status and body.
pub fn parse_reply(
    kind: RequestKind,
    status: u16,
    bytes: &[u8],
) -> Result<UploadReply, ReplyError> {
    if bytes.len() > MAX_BODY {
        return Err(ReplyError::TooLarge);
    }
    match status {
        401 => return Ok(UploadReply::Refused(UploadRefusal::Unauthorized)),
        503 => return Ok(UploadReply::Refused(UploadRefusal::Unavailable)),
        _ => {}
    }
    body(bytes)
        .and_then(|parsed| classify(kind, status, parsed))
        .ok_or(ReplyError::Unexpected { status })
}
