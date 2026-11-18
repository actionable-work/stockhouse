use crate::reply::{Body, ReplyError, body};
use serde::{Deserialize, Serialize};
use std::fmt;
use stockhouse_proto::{StorePath, UnsupportedVersion, check_version};

const MAX_LEASE_REQUEST: usize = 4 * 1024;
const MAX_LEASE_REPLY: usize = 64 * 1024;

/// The text is not 32 lowercase hex characters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MachineIdError {
    /// Not exactly 32 lowercase hex characters.
    Invalid,
}

impl fmt::Display for MachineIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("machine identity is not 32 lowercase hex characters")
    }
}

impl std::error::Error for MachineIdError {}

/// A random 128-bit machine identity, rendered as 32 lowercase hex characters.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct MachineId([u8; 16]);

impl MachineId {
    /// Builds an identity from 16 bytes.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Parses 32 lowercase hex characters.
    pub fn parse(text: &str) -> Result<Self, MachineIdError> {
        let digit = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        let raw = text.as_bytes();
        if raw.len() != 32 {
            return Err(MachineIdError::Invalid);
        }
        let mut out = [0u8; 16];
        for (slot, pair) in out.iter_mut().zip(raw.chunks(2)) {
            let (hi, lo) = (digit(pair[0]), digit(pair[1]));
            *slot = hi
                .zip(lo)
                .map(|(h, l)| h << 4 | l)
                .ok_or(MachineIdError::Invalid)?;
        }
        Ok(Self(out))
    }
}

impl fmt::Display for MachineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl fmt::Debug for MachineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MachineId(..)")
    }
}

/// The slot is outside 1 to 10.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LeaseSlotError {
    /// Not 1 to 10.
    OutOfRange,
}

impl fmt::Display for LeaseSlotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("lease slot is not 1 to 10")
    }
}

impl std::error::Error for LeaseSlotError {}

/// A lease slot: 1 current system, 2 previous system, 3–10 operator defined.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LeaseSlot(u8);

impl LeaseSlot {
    /// Accepts 1 to 10.
    pub fn new(n: u8) -> Result<Self, LeaseSlotError> {
        if (1..=10).contains(&n) {
            Ok(Self(n))
        } else {
            Err(LeaseSlotError::OutOfRange)
        }
    }

    /// The slot number.
    pub fn get(self) -> u8 {
        self.0
    }
}

/// `leases/<MachineId>/<slot>`.
pub fn lease_path(id: &MachineId, slot: LeaseSlot) -> String {
    format!("leases/{id}/{}", slot.0)
}

/// Why a lease request was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LeaseRequestError {
    /// More than 4 KiB.
    TooLarge,
    /// An unknown, missing, repeated or mistyped key.
    Malformed,
    /// A version other than 1.
    UnsupportedVersion(UnsupportedVersion),
    /// An invalid value.
    InvalidField {
        /// The offending key.
        field: &'static str,
    },
}

impl fmt::Display for LeaseRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str("lease request too large"),
            Self::Malformed => f.write_str("malformed lease request"),
            Self::UnsupportedVersion(v) => write!(f, "lease request: {v}"),
            Self::InvalidField { field } => write!(f, "lease request: invalid {field}"),
        }
    }
}

impl std::error::Error for LeaseRequestError {}

/// The body of `PUT leases/<MachineId>/<slot>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseRequest {
    /// The store path to keep.
    pub store_path: StorePath,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LeaseJson {
    version: u32,
    store_path: String,
}

impl LeaseRequest {
    /// Renders `{"version":1,"store_path":…}`.
    pub fn render(&self) -> Vec<u8> {
        let json = LeaseJson {
            version: 1,
            store_path: self.store_path.to_string(),
        };
        serde_json::to_vec(&json).expect("lease request serializes")
    }

    /// Parses a lease request strictly.
    pub fn parse(bytes: &[u8]) -> Result<Self, LeaseRequestError> {
        if bytes.len() > MAX_LEASE_REQUEST {
            return Err(LeaseRequestError::TooLarge);
        }
        let raw: LeaseJson =
            serde_json::from_slice(bytes).map_err(|_| LeaseRequestError::Malformed)?;
        check_version(raw.version).map_err(LeaseRequestError::UnsupportedVersion)?;
        let store_path =
            StorePath::parse(&raw.store_path).map_err(|_| LeaseRequestError::InvalidField {
                field: "store_path",
            })?;
        Ok(Self { store_path })
    }
}

/// Why the server refused a lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseRefusal {
    /// 422 `unknown_path`.
    UnknownPath,
    /// 409 `conflict`.
    Conflict,
    /// 400 `bad_request`.
    BadRequest,
}

impl LeaseRefusal {
    /// The protocol code.
    pub fn code(self) -> &'static str {
        match self {
            Self::UnknownPath => "unknown_path",
            Self::Conflict => "conflict",
            Self::BadRequest => "bad_request",
        }
    }
}

/// A classified lease reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseReply {
    /// 200 `renewed`.
    Renewed,
    /// A refusal.
    Refused(LeaseRefusal),
    /// 401.
    Unauthorized,
    /// 503, including a full machine table.
    Unavailable,
}

/// Classifies a lease reply by status and body.
pub fn parse_lease_reply(status: u16, bytes: &[u8]) -> Result<LeaseReply, ReplyError> {
    if bytes.len() > MAX_LEASE_REPLY {
        return Err(ReplyError::TooLarge);
    }
    let parsed = match status {
        401 => return Ok(LeaseReply::Unauthorized),
        503 => return Ok(LeaseReply::Unavailable),
        _ => body(bytes),
    };
    let reply = match (status, parsed) {
        (200, Some(Body::State(s))) if s == "renewed" => Some(LeaseReply::Renewed),
        (422, Some(Body::Error(e, None))) if e == "unknown_path" => {
            Some(LeaseReply::Refused(LeaseRefusal::UnknownPath))
        }
        (409, Some(Body::Error(e, None))) if e == "conflict" => {
            Some(LeaseReply::Refused(LeaseRefusal::Conflict))
        }
        (400, Some(Body::Error(e, None))) if e == "bad_request" => {
            Some(LeaseReply::Refused(LeaseRefusal::BadRequest))
        }
        _ => None,
    };
    reply.ok_or(ReplyError::Unexpected { status })
}
