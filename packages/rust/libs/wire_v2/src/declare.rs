use crate::{
    BatchError, MAX_BATCH_ENTRIES, check_header,
    common::{Object, nullable},
    parse_body,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use stockhouse_proto::{StorePath, UploadId};
use stockhouse_wire::{DeclarationError, UploadDeclaration};

pub const MAX_DECLARE_BODY: usize = 8 * 1024 * 1024;
const PREFIX: &[u8] = b"{\"version\":2,\"uploads\":[";
const SUFFIX: &[u8] = b"]}";

/// An upload identity and its declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclareEntry {
    pub id: UploadId,
    pub declaration: UploadDeclaration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarePushError {
    BatchFull,
    Duplicate { index: usize },
}

impl fmt::Display for DeclarePushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BatchFull => f.write_str("declare batch full"),
            Self::Duplicate { index } => write!(f, "duplicate declare batch entry {index}"),
        }
    }
}
impl std::error::Error for DeclarePushError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IncomingEntry {
    id: String,
    store_path: String,
    nar_hash: String,
    nar_size: u64,
    references: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    deriver: Option<String>,
    #[serde(deserialize_with = "nullable")]
    ca: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Incoming {
    version: u64,
    uploads: Vec<Object<IncomingEntry>>,
}

#[derive(Serialize)]
struct Outgoing<'a> {
    id: String,
    store_path: &'a str,
    nar_hash: String,
    nar_size: u64,
    references: Vec<&'a str>,
    deriver: Option<&'a str>,
    ca: Option<&'a str>,
}

fn render_entry(entry: &DeclareEntry) -> Vec<u8> {
    let decl = &entry.declaration;
    serde_json::to_vec(&Outgoing {
        id: entry.id.to_string(),
        store_path: decl.store_path().as_str(),
        nar_hash: decl.nar_hash().to_string(),
        nar_size: decl.nar_size(),
        references: decl.references().iter().map(StorePath::basename).collect(),
        deriver: decl.deriver().map(StorePath::basename),
        ca: decl.ca(),
    })
    .expect("declare entry serializes")
}

fn parse_entry(
    (index, Object(raw)): (usize, Object<IncomingEntry>),
) -> Result<DeclareEntry, BatchError> {
    let id =
        UploadId::parse(&raw.id).map_err(|_| BatchError::InvalidField { index, field: "id" })?;
    let declaration = UploadDeclaration::from_wire_fields(
        &raw.store_path,
        &raw.nar_hash,
        raw.nar_size,
        &raw.references,
        raw.deriver.as_deref(),
        raw.ca,
    )
    .map_err(|error| match error {
        DeclarationError::InvalidField { field } => BatchError::InvalidField { index, field },
        _ => BatchError::Malformed,
    })?;
    Ok(DeclareEntry { id, declaration })
}

/// Builds a batch within both the count and serialized size limits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclareBatch {
    entries: Vec<DeclareEntry>,
    len: usize,
}

impl Default for DeclareBatch {
    fn default() -> Self {
        Self::new()
    }
}

impl DeclareBatch {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            len: PREFIX.len() + SUFFIX.len(),
        }
    }

    pub fn entries(&self) -> &[DeclareEntry] {
        &self.entries
    }

    pub fn push(
        &mut self,
        id: UploadId,
        declaration: UploadDeclaration,
    ) -> Result<(), DeclarePushError> {
        let index = self.entries.len();
        if self.entries.iter().any(|entry| {
            entry.id == id || entry.declaration.store_path() == declaration.store_path()
        }) {
            return Err(DeclarePushError::Duplicate { index });
        }
        if index == MAX_BATCH_ENTRIES {
            return Err(DeclarePushError::BatchFull);
        }
        let entry = DeclareEntry { id, declaration };
        let len = self.len + render_entry(&entry).len() + usize::from(index > 0);
        if len > MAX_DECLARE_BODY {
            return Err(DeclarePushError::BatchFull);
        }
        self.entries.push(entry);
        self.len = len;
        Ok(())
    }

    pub fn render(&self) -> Result<Vec<u8>, BatchError> {
        if self.entries.is_empty() {
            return Err(BatchError::Count);
        }
        let mut body = Vec::with_capacity(self.len);
        body.extend_from_slice(PREFIX);
        for (index, entry) in self.entries.iter().enumerate() {
            if index > 0 {
                body.push(b',');
            }
            body.extend(render_entry(entry));
        }
        body.extend_from_slice(SUFFIX);
        Ok(body)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, BatchError> {
        let raw: Incoming = parse_body(bytes, MAX_DECLARE_BODY)?;
        check_header(raw.version, raw.uploads.len(), MAX_BATCH_ENTRIES)?;
        let entries = raw
            .uploads
            .into_iter()
            .enumerate()
            .map(parse_entry)
            .collect::<Result<Vec<_>, _>>()?;
        let len = validated_len(&entries)?;
        Ok(Self { entries, len })
    }
}

fn validated_len(entries: &[DeclareEntry]) -> Result<usize, BatchError> {
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    let mut len = PREFIX.len() + SUFFIX.len() + entries.len() - 1;
    for (index, entry) in entries.iter().enumerate() {
        if !ids.insert(entry.id) || !paths.insert(entry.declaration.store_path()) {
            return Err(BatchError::Duplicate { index });
        }
        len += render_entry(entry).len();
    }
    if len > MAX_DECLARE_BODY {
        return Err(BatchError::TooLarge);
    }
    Ok(len)
}
