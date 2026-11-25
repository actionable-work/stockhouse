use crate::common::{Object, present};
use serde::Deserialize;
use std::collections::HashSet;
use stockhouse_proto::{StorePath, UploadId};
use stockhouse_wire::{
    LeaseRefusal, LeaseSlot, MAX_REFERENCES, MachineId, ReplyError, UploadRefusal,
};

pub const MAX_BATCH_REPLY: usize = 4 * 1024 * 1024;
pub const MAX_NAR_REPLY: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchKind {
    Declare,
    Publish,
    Status,
    Leases,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKey {
    Upload(UploadId),
    Lease(MachineId, LeaseSlot),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadState {
    Open,
    Verified,
    Present,
    Published,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UploadResult {
    State(UploadState),
    Refused(UploadRefusal),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseResult {
    Renewed,
    Refused(LeaseRefusal),
    MachinesFull,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntryResult {
    Upload(UploadResult),
    Lease(LeaseResult),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchRefusal {
    BadRequest,
    TooLarge,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BatchReply {
    Results(Vec<EntryResult>),
    Refused(BatchRefusal),
    NotSupported,
    Unauthorized,
    Busy { retry_after: Option<u32> },
    Unavailable { retry_after: Option<u32> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NarReply {
    State(UploadState),
    Refused(UploadRefusal),
    NotSupported,
    Unauthorized,
    Busy { retry_after: Option<u32> },
    Unavailable { retry_after: Option<u32> },
}

/// Parse delta-seconds only. HTTP dates and signed numbers have no effect.
pub fn parse_retry_after(value: Option<&str>) -> Option<u32> {
    let value = value?;
    if !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<u32>()
        .ok()
        .filter(|seconds| *seconds <= 86_400)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Results<T> {
    results: Vec<Object<T>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadJson {
    id: String,
    #[serde(default, deserialize_with = "present")]
    state: Option<String>,
    #[serde(default, deserialize_with = "present")]
    error: Option<String>,
    #[serde(default, deserialize_with = "present")]
    missing: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseJson {
    machine: String,
    slot: u64,
    #[serde(default, deserialize_with = "present")]
    state: Option<String>,
    #[serde(default, deserialize_with = "present")]
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SingleResult {
    #[serde(default, deserialize_with = "present")]
    state: Option<String>,
    #[serde(default, deserialize_with = "present")]
    error: Option<String>,
}

pub fn parse_batch_reply(
    kind: BatchKind,
    keys: &[EntryKey],
    status: u16,
    retry_after: Option<&str>,
    body: &[u8],
) -> Result<BatchReply, ReplyError> {
    let reply = match status {
        404 => BatchReply::NotSupported,
        401 => BatchReply::Unauthorized,
        429 => BatchReply::Busy {
            retry_after: parse_retry_after(retry_after),
        },
        503 => BatchReply::Unavailable {
            retry_after: parse_retry_after(retry_after),
        },
        _ if body.len() > MAX_BATCH_REPLY => return Err(ReplyError::TooLarge),
        200 => BatchReply::Results(
            parse_entries(kind, keys, body).ok_or(ReplyError::Unexpected { status })?,
        ),
        400 if single_error(body, "bad_request") => BatchReply::Refused(BatchRefusal::BadRequest),
        413 if single_error(body, "too_large") => BatchReply::Refused(BatchRefusal::TooLarge),
        _ => return Err(ReplyError::Unexpected { status }),
    };
    Ok(reply)
}

fn result_rows<'a, T: Deserialize<'a>>(
    keys: &[EntryKey],
    body: &'a [u8],
) -> Option<Vec<Object<T>>> {
    let Object(Results { results }) = serde_json::from_slice(body).ok()?;
    (results.len() == keys.len()).then_some(results)
}

fn parse_entries(kind: BatchKind, keys: &[EntryKey], body: &[u8]) -> Option<Vec<EntryResult>> {
    if kind == BatchKind::Leases {
        return result_rows(keys, body)?
            .into_iter()
            .zip(keys)
            .map(|(Object(row), key)| lease_result(row, *key).map(EntryResult::Lease))
            .collect();
    }

    let mut remaining = MAX_REFERENCES;
    result_rows(keys, body)?
        .into_iter()
        .zip(keys)
        .map(|(Object(row), key)| {
            upload_result(kind, row, *key, &mut remaining).map(EntryResult::Upload)
        })
        .collect()
}

fn lease_result(row: LeaseJson, key: EntryKey) -> Option<LeaseResult> {
    let machine = MachineId::parse(&row.machine).ok()?;
    let slot = LeaseSlot::new(u8::try_from(row.slot).ok()?).ok()?;
    if key != EntryKey::Lease(machine, slot) {
        return None;
    }
    match (row.state.as_deref(), row.error.as_deref()) {
        (Some("renewed"), None) => Some(LeaseResult::Renewed),
        (None, Some("unknown_path")) => Some(LeaseResult::Refused(LeaseRefusal::UnknownPath)),
        (None, Some("conflict")) => Some(LeaseResult::Refused(LeaseRefusal::Conflict)),
        (None, Some("machines_full")) => Some(LeaseResult::MachinesFull),
        _ => None,
    }
}

fn upload_result(
    kind: BatchKind,
    row: UploadJson,
    key: EntryKey,
    remaining: &mut usize,
) -> Option<UploadResult> {
    if key != EntryKey::Upload(UploadId::parse(&row.id).ok()?) {
        return None;
    }
    match (row.state.as_deref(), row.error.as_deref(), row.missing) {
        (Some("open"), None, None) => Some(UploadResult::State(UploadState::Open)),
        (Some("verified"), None, None) if kind != BatchKind::Publish => {
            Some(UploadResult::State(UploadState::Verified))
        }
        (Some("present"), None, None) => Some(UploadResult::State(UploadState::Present)),
        (Some("published"), None, None) => Some(UploadResult::State(UploadState::Published)),
        (None, Some("missing_references"), Some(names)) if kind == BatchKind::Publish => {
            missing_paths(names, remaining)
                .map(|paths| UploadResult::Refused(UploadRefusal::MissingReferences(paths)))
        }
        (None, Some(code), None) => entry_refusal(kind, code).map(UploadResult::Refused),
        _ => None,
    }
}

fn entry_refusal(kind: BatchKind, code: &str) -> Option<UploadRefusal> {
    match code {
        "verification_failed" => Some(UploadRefusal::VerificationFailed),
        "unknown_upload" => Some(UploadRefusal::UnknownUpload),
        "conflict" if kind != BatchKind::Status => Some(UploadRefusal::Conflict),
        "quota_exceeded" if kind != BatchKind::Status => Some(UploadRefusal::QuotaExceeded),
        "too_large" if kind == BatchKind::Declare => Some(UploadRefusal::TooLarge),
        _ => None,
    }
}

fn missing_paths(names: Vec<String>, remaining: &mut usize) -> Option<Vec<StorePath>> {
    if names.is_empty() {
        return None;
    }
    *remaining = remaining.checked_sub(names.len())?;
    let mut seen = HashSet::new();
    names
        .iter()
        .map(|name| {
            let path = StorePath::from_basename(name).ok()?;
            seen.insert(name).then_some(path)
        })
        .collect()
}

fn single(body: &[u8]) -> Option<SingleResult> {
    let Object(result) = serde_json::from_slice(body).ok()?;
    Some(result)
}

fn single_error(body: &[u8], expected: &str) -> bool {
    single(body)
        .is_some_and(|result| result.state.is_none() && result.error.as_deref() == Some(expected))
}

pub fn parse_nar_reply(
    status: u16,
    retry_after: Option<&str>,
    body: &[u8],
) -> Result<NarReply, ReplyError> {
    match status {
        401 => Ok(NarReply::Unauthorized),
        429 => Ok(NarReply::Busy {
            retry_after: parse_retry_after(retry_after),
        }),
        503 => Ok(NarReply::Unavailable {
            retry_after: parse_retry_after(retry_after),
        }),
        404 if body.len() > MAX_NAR_REPLY => Ok(NarReply::NotSupported),
        _ if body.len() > MAX_NAR_REPLY => Err(ReplyError::TooLarge),
        404 => Ok(single(body)
            .and_then(|result| classify_nar(status, result))
            .unwrap_or(NarReply::NotSupported)),
        _ => single(body)
            .and_then(|result| classify_nar(status, result))
            .ok_or(ReplyError::Unexpected { status }),
    }
}

fn classify_nar(status: u16, result: SingleResult) -> Option<NarReply> {
    let refusal = match (status, result.state.as_deref(), result.error.as_deref()) {
        (200, Some("verified"), None) => return Some(NarReply::State(UploadState::Verified)),
        (200, Some("present"), None) => return Some(NarReply::State(UploadState::Present)),
        (200, Some("published"), None) => return Some(NarReply::State(UploadState::Published)),
        (400, None, Some("bad_request")) => UploadRefusal::BadRequest,
        (400, None, Some("verification_failed")) => UploadRefusal::VerificationFailed,
        (404, None, Some("unknown_upload")) => UploadRefusal::UnknownUpload,
        (409, None, Some("conflict")) => UploadRefusal::Conflict,
        (413, None, Some("too_large")) => UploadRefusal::TooLarge,
        (507, None, Some("quota_exceeded")) => UploadRefusal::QuotaExceeded,
        _ => return None,
    };
    Some(NarReply::Refused(refusal))
}
