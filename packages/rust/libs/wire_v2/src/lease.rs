use crate::{BatchError, MAX_SMALL_BATCH_BODY, VERSION, check_header, common::Object, parse_body};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fmt};
use stockhouse_proto::StorePath;
use stockhouse_wire::{LeaseSlot, MachineId};

pub const MAX_LEASE_ENTRIES: usize = 64;

/// Why a lease batch failed validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseBatchError {
    TooLarge,
    Malformed,
    UnsupportedVersion,
    Count,
    InvalidField { index: usize, field: &'static str },
    Duplicate { index: usize },
}

impl From<BatchError> for LeaseBatchError {
    fn from(error: BatchError) -> Self {
        match error {
            BatchError::TooLarge => Self::TooLarge,
            BatchError::Malformed => Self::Malformed,
            BatchError::UnsupportedVersion => Self::UnsupportedVersion,
            BatchError::Count => Self::Count,
            BatchError::InvalidField { index, field } => Self::InvalidField { index, field },
            BatchError::Duplicate { index } => Self::Duplicate { index },
        }
    }
}

impl fmt::Display for LeaseBatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::TooLarge => BatchError::TooLarge,
            Self::Malformed => BatchError::Malformed,
            Self::UnsupportedVersion => BatchError::UnsupportedVersion,
            Self::Count => BatchError::Count,
            Self::InvalidField { index, field } => BatchError::InvalidField { index, field },
            Self::Duplicate { index } => BatchError::Duplicate { index },
        }
        .fmt(f)
    }
}

impl std::error::Error for LeaseBatchError {}

/// A request to renew one slot for a store path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseEntry {
    pub machine: MachineId,
    pub slot: LeaseSlot,
    pub store_path: StorePath,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    machine: String,
    slot: u64,
    store_path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Incoming {
    version: u64,
    leases: Vec<Object<Entry>>,
}

#[derive(Serialize)]
struct Outgoing {
    version: u64,
    leases: Vec<Entry>,
}

/// A nonempty ordered batch of distinct machine and slot pairs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseBatch(Vec<LeaseEntry>);

impl LeaseBatch {
    pub fn new(entries: Vec<LeaseEntry>) -> Result<Self, LeaseBatchError> {
        check_header(VERSION, entries.len(), MAX_LEASE_ENTRIES)?;
        let mut seen = HashSet::new();
        for (index, entry) in entries.iter().enumerate() {
            if !seen.insert((entry.machine, entry.slot)) {
                return Err(LeaseBatchError::Duplicate { index });
            }
        }
        Ok(Self(entries))
    }

    pub fn entries(&self) -> &[LeaseEntry] {
        &self.0
    }

    pub fn render(&self) -> Vec<u8> {
        let leases = self
            .0
            .iter()
            .map(|entry| Entry {
                machine: entry.machine.to_string(),
                slot: u64::from(entry.slot.get()),
                store_path: entry.store_path.to_string(),
            })
            .collect();
        serde_json::to_vec(&Outgoing {
            version: VERSION,
            leases,
        })
        .expect("lease batch serializes")
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, LeaseBatchError> {
        let raw: Incoming = parse_body(bytes, MAX_SMALL_BATCH_BODY)?;
        check_header(raw.version, raw.leases.len(), MAX_LEASE_ENTRIES)?;
        let entries = raw
            .leases
            .into_iter()
            .enumerate()
            .map(parse_entry)
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(entries)
    }
}

fn parse_entry(
    (index, Object(raw)): (usize, Object<Entry>),
) -> Result<LeaseEntry, LeaseBatchError> {
    let invalid = |field| LeaseBatchError::InvalidField { index, field };
    Ok(LeaseEntry {
        machine: MachineId::parse(&raw.machine).map_err(|_| invalid("machine"))?,
        slot: u8::try_from(raw.slot)
            .ok()
            .and_then(|n| LeaseSlot::new(n).ok())
            .ok_or_else(|| invalid("slot"))?,
        store_path: StorePath::parse(&raw.store_path).map_err(|_| invalid("store_path"))?,
    })
}
