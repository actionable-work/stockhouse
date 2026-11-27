use crate::{common::Object, parse_retry_after};
use serde::Deserialize;
use std::fmt;
use stockhouse_wire::ReplyError;

/// Wire versions accepted by this client.
pub const WIRE_VERSIONS: [u16; 2] = [1, 2];
/// Maximum size of a probe response that has a body to parse.
pub const MAX_PROBE_REPLY: usize = 4 * 1024;

/// The configured wire protocol preference.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolMode {
    #[default]
    Auto,
    V1,
    V2,
}

/// The server and client could not agree on a version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VersionError {
    NotOffered,
    NoCommonVersion,
}

impl fmt::Display for VersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotOffered => "server does not offer protocol v2",
            Self::NoCommonVersion => "no common protocol version",
        })
    }
}

impl std::error::Error for VersionError {}

/// Choose the highest allowed mutually supported version.
pub fn select_version(mode: ProtocolMode, offered: &[u16]) -> Result<u16, VersionError> {
    match mode {
        ProtocolMode::V1 => Ok(1),
        _ if offered.contains(&2) => Ok(2),
        ProtocolMode::V2 => Err(VersionError::NotOffered),
        ProtocolMode::Auto if offered.contains(&1) => Ok(1),
        ProtocolMode::Auto => Err(VersionError::NoCommonVersion),
    }
}

/// Result of probing a server for its supported versions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProbeReply {
    Offered(Vec<u16>),
    Unauthorized,
    Busy { retry_after: Option<u32> },
    Unavailable { retry_after: Option<u32> },
}

#[derive(Deserialize)]
struct Offered {
    versions: Vec<u16>,
}

/// Classify the probe reply. Status-only responses ignore the body.
pub fn parse_probe_reply(
    status: u16,
    retry_after: Option<&str>,
    body: &[u8],
) -> Result<ProbeReply, ReplyError> {
    match status {
        404 => Ok(ProbeReply::Offered(vec![1])),
        401 => Ok(ProbeReply::Unauthorized),
        429 => Ok(ProbeReply::Busy {
            retry_after: parse_retry_after(retry_after),
        }),
        503 => Ok(ProbeReply::Unavailable {
            retry_after: parse_retry_after(retry_after),
        }),
        _ if body.len() > MAX_PROBE_REPLY => Err(ReplyError::TooLarge),
        200 => parse_versions(body).map(ProbeReply::Offered),
        _ => Err(ReplyError::Unexpected { status }),
    }
}

fn parse_versions(body: &[u8]) -> Result<Vec<u16>, ReplyError> {
    let error = ReplyError::Unexpected { status: 200 };
    let Object(Offered { versions }) = serde_json::from_slice(body).map_err(|_| error)?;
    if versions.is_empty()
        || versions.len() > 16
        || versions[0] == 0
        || versions.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(error);
    }
    Ok(versions)
}
