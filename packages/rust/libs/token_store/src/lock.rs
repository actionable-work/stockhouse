use crate::StoreError;
use crate::fsx;
use std::fs::{File, TryLockError};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(100);
pub(crate) const LOCK_NAME: &str = "lock";

/// An exclusive lock on the store, released on drop (or process exit).
#[derive(Debug)]
pub struct Lock {
    _file: File,
}

/// Opens or creates the lock file, checks it and locks it within `wait`.
pub(crate) fn acquire(dir: &File, euid: u32, wait: Duration) -> Result<Lock, StoreError> {
    let file = fsx::openat(dir, LOCK_NAME, libc::O_RDWR | libc::O_CREAT, 0o600)
        .map_err(|e| fsx::open_error(&e))?;
    fsx::check_file(&file, euid)?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Lock { _file: file }),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(POLL);
            }
            Err(TryLockError::WouldBlock) => return Err(StoreError::LockTimeout),
            Err(TryLockError::Error(_)) => return Err(StoreError::Io { op: "lock" }),
        }
    }
}
