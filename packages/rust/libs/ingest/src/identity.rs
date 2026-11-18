//! Per-path upload identities and wire state under `uploads/`.

use crate::fsutil;
use crate::queue::{Queue, QueueError, done_bytes, io_err};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use stockhouse_proto::{StorePath, UploadId, check_version};

/// The wire state of one store path's server upload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UploadEntry {
    /// The identity the server sees for this path.
    pub id: UploadId,
    /// A NAR may have been sent; ask the server before sending again.
    pub nar_sent: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EntryJson {
    version: u32,
    id: String,
    store_path: String,
    nar_sent: bool,
}

enum Read {
    Absent,
    Corrupt,
    Valid(UploadEntry),
}

impl Queue {
    fn entry_path(&self, path: &StorePath) -> std::path::PathBuf {
        self.path("uploads", path.hash().as_str(), ".json")
    }

    fn entry_bytes(path: &StorePath, entry: UploadEntry) -> Vec<u8> {
        let json = EntryJson {
            version: 1,
            id: entry.id.to_string(),
            store_path: path.to_string(),
            nar_sent: entry.nar_sent,
        };
        serde_json::to_vec(&json).expect("upload entry serializes")
    }

    fn read_entry(&self, path: &StorePath) -> Result<Read, QueueError> {
        let bytes = match fs::read(self.entry_path(path)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Read::Absent),
            Err(error) => {
                return Err(QueueError::Io {
                    op: "read upload entry",
                    error,
                });
            }
        };
        let parsed = serde_json::from_slice::<EntryJson>(&bytes)
            .ok()
            .filter(|j| check_version(j.version).is_ok() && j.store_path == path.as_str())
            .and_then(|j| {
                let id = UploadId::parse(&j.id).ok()?;
                Some(UploadEntry {
                    id,
                    nar_sent: j.nar_sent,
                })
            });
        Ok(parsed.map_or(Read::Corrupt, Read::Valid))
    }

    fn write_entry(&self, path: &StorePath, entry: UploadEntry) -> Result<UploadEntry, QueueError> {
        let bytes = Self::entry_bytes(path, entry);
        fsutil::replace(&self.tmp(), &self.entry_path(path), &bytes)
            .map_err(io_err("write upload entry"))?;
        Ok(entry)
    }

    fn pending_id(&self, path: &StorePath) -> Result<Option<UploadId>, QueueError> {
        let record = self.read_record(&self.path("pending", path.hash().as_str(), ".json"))?;
        Ok(record.and_then(Result::ok).map(|r| r.id))
    }

    /// The wire state for `path`. `root_id` is the record identity when
    /// `path` is the queued path itself; dependencies pass `None`.
    pub fn upload_state(
        &self,
        path: &StorePath,
        root_id: Option<UploadId>,
    ) -> Result<UploadEntry, QueueError> {
        match self.read_entry(path)? {
            Read::Valid(entry) if root_id.is_none_or(|id| id == entry.id) => return Ok(entry),
            Read::Corrupt => {
                let target = self.path("failed", path.hash().as_str(), ".upload.corrupt");
                fsutil::rename(&self.entry_path(path), &target)
                    .map_err(io_err("quarantine upload entry"))?;
            }
            _ => {}
        }
        if let Some(id) = root_id {
            return self.write_entry(
                path,
                UploadEntry {
                    id,
                    nar_sent: false,
                },
            );
        }
        let id = self.pending_id(path)?.unwrap_or_else(UploadId::generate);
        let bytes = Self::entry_bytes(
            path,
            UploadEntry {
                id,
                nar_sent: false,
            },
        );
        fsutil::create_new(&self.tmp(), &self.entry_path(path), &bytes)
            .map_err(io_err("create upload entry"))?;
        match self.read_entry(path)? {
            Read::Valid(entry) => Ok(entry),
            _ => Err(QueueError::Io {
                op: "read upload entry",
                error: io::Error::from(io::ErrorKind::InvalidData),
            }),
        }
    }

    /// Durably records that a NAR may be sent for this entry.
    pub fn mark_nar_sent(
        &self,
        path: &StorePath,
        entry: UploadEntry,
    ) -> Result<UploadEntry, QueueError> {
        self.write_entry(
            path,
            UploadEntry {
                nar_sent: true,
                ..entry
            },
        )
    }

    /// Durably clears the NAR-sent mark.
    pub fn clear_nar_sent(
        &self,
        path: &StorePath,
        entry: UploadEntry,
    ) -> Result<UploadEntry, QueueError> {
        self.write_entry(
            path,
            UploadEntry {
                nar_sent: false,
                ..entry
            },
        )
    }

    /// Records a dependency as uploaded: ledger first, then the entry goes.
    pub fn complete_path(
        &self,
        path: &StorePath,
        id: UploadId,
        now: u64,
    ) -> Result<(), QueueError> {
        let target = self.path("done", path.hash().as_str(), ".json");
        fsutil::create_new(&self.tmp(), &target, &done_bytes(&id, path, now))
            .map_err(io_err("write done"))?;
        self.remove_entry(path)
    }

    /// Removes a path's upload entry, if any.
    pub fn remove_entry(&self, path: &StorePath) -> Result<(), QueueError> {
        fsutil::remove(&self.entry_path(path)).map_err(io_err("remove upload entry"))
    }

    /// Removes a path's ledger entry.
    pub fn forget_done(&self, path: &StorePath) -> Result<(), QueueError> {
        let target = self.path("done", path.hash().as_str(), ".json");
        fsutil::remove(&target).map_err(io_err("forget done"))
    }

    /// Removes entries whose path is uploaded or no longer on disk.
    pub(crate) fn reconcile_uploads(&self) -> Result<(), QueueError> {
        let dir = self.root().join("uploads");
        let stems = match fsutil::stems(&dir, ".json") {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            other => other.map_err(io_err("list uploads"))?,
        };
        for stem in stems {
            let file = self.path("uploads", &stem, ".json");
            let stale_path = fs::read(&file)
                .ok()
                .and_then(|b| serde_json::from_slice::<EntryJson>(&b).ok())
                .and_then(|j| StorePath::parse(&j.store_path).ok())
                .is_some_and(|p| p.hash().as_str() == stem && !self.store_path_exists(&p));
            if self.is_uploaded_hash(&stem) || stale_path {
                fsutil::remove(&file).map_err(io_err("remove upload entry"))?;
            }
        }
        Ok(())
    }
}
