use serde::{Deserialize, Serialize};
use stockhouse_proto::{StorePath, UploadId, check_version};

/// A pending or failed queue record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Record {
    /// Upload identity, stable until a server reports that it was forgotten.
    pub id: UploadId,
    /// The store path to upload.
    pub store_path: StorePath,
    /// When it was first queued (Unix seconds).
    pub enqueued_at: u64,
    /// Attempts consumed.
    pub attempts: u32,
    /// Earliest time of the next attempt.
    pub next_attempt_at: u64,
    /// The last retry message.
    pub last_error: Option<String>,
    /// Why a failed record failed.
    pub reason: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecordJson {
    version: u32,
    id: String,
    store_path: String,
    enqueued_at: u64,
    attempts: u32,
    next_attempt_at: u64,
    last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl Record {
    /// A fresh record with a new identity.
    pub fn new(store_path: StorePath, now: u64) -> Self {
        let id = UploadId::generate();
        Self {
            id,
            store_path,
            enqueued_at: now,
            attempts: 0,
            next_attempt_at: now,
            last_error: None,
            reason: None,
        }
    }

    /// Parses version 1 JSON; anything else is `None` (corrupt).
    pub fn from_json(bytes: &[u8]) -> Option<Self> {
        let json: RecordJson = serde_json::from_slice(bytes).ok()?;
        check_version(json.version).ok()?;
        Some(Self {
            id: UploadId::parse(&json.id).ok()?,
            store_path: StorePath::parse(&json.store_path).ok()?,
            enqueued_at: json.enqueued_at,
            attempts: json.attempts,
            next_attempt_at: json.next_attempt_at,
            last_error: json.last_error,
            reason: json.reason,
        })
    }

    /// Renders version 1 JSON.
    pub fn to_json(&self) -> Vec<u8> {
        let json = RecordJson {
            version: 1,
            id: self.id.to_string(),
            store_path: self.store_path.to_string(),
            enqueued_at: self.enqueued_at,
            attempts: self.attempts,
            next_attempt_at: self.next_attempt_at,
            last_error: self.last_error.clone(),
            reason: self.reason.clone(),
        };
        serde_json::to_vec(&json).expect("record serializes")
    }
}

/// A completed upload in the ledger.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DoneRecord {
    /// Always 1.
    pub version: u32,
    /// The identity it was uploaded with.
    pub id: String,
    /// The store path.
    pub store_path: String,
    /// When it completed.
    pub uploaded_at: u64,
}
