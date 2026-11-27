use crate::{
    BatchError, MAX_BATCH_ENTRIES, MAX_SMALL_BATCH_BODY, VERSION, check_header, parse_body,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use stockhouse_proto::UploadId;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Body {
    version: u64,
    uploads: Vec<String>,
}

/// Ordered, distinct upload identities for publish or status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdBatch(Vec<UploadId>);

impl IdBatch {
    pub fn new(ids: Vec<UploadId>) -> Result<Self, BatchError> {
        check_header(VERSION, ids.len(), MAX_BATCH_ENTRIES)?;
        let mut seen = HashSet::new();
        for (index, id) in ids.iter().enumerate() {
            if !seen.insert(id) {
                return Err(BatchError::Duplicate { index });
            }
        }
        Ok(Self(ids))
    }

    pub fn entries(&self) -> &[UploadId] {
        &self.0
    }

    pub fn render(&self) -> Vec<u8> {
        serde_json::to_vec(&Body {
            version: VERSION,
            uploads: self.0.iter().map(ToString::to_string).collect(),
        })
        .expect("ID batch serializes")
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, BatchError> {
        let raw: Body = parse_body(bytes, MAX_SMALL_BATCH_BODY)?;
        check_header(raw.version, raw.uploads.len(), MAX_BATCH_ENTRIES)?;
        let ids = raw
            .uploads
            .iter()
            .enumerate()
            .map(|(index, value)| {
                UploadId::parse(value).map_err(|_| BatchError::InvalidField {
                    index,
                    field: "uploads",
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(ids)
    }
}
