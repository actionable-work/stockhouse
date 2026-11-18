//! The NAR body: `nix store dump-path` streamed through a verifier that holds
//! back the final chunk until the size, hash and exit status check out.

use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use stockhouse_proto::{ContentVerifier, ServedIntegrity, StorePath};

/// Waits at most `idle_ms` for `fd` to become readable (data or end of
/// file), retrying when a signal interrupts the wait.
pub(crate) fn wait_readable(fd: &impl AsRawFd, idle_ms: i32) -> bool {
    let mut pfd = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: one valid pollfd for an open descriptor the caller owns.
        let ready = unsafe { libc::poll(&mut pfd, 1, idle_ms) };
        if ready >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return ready > 0;
        }
    }
}

/// Milliseconds of a duration, saturated for `poll`.
pub(crate) fn poll_ms(idle: Duration) -> i32 {
    i32::try_from(idle.as_millis()).unwrap_or(i32::MAX)
}

/// Bytes read from the dump per chunk.
pub const CHUNK: usize = 64 * 1024;

/// Why a dump was not released whole.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DumpFailure {
    /// The bytes differ from the declared NarSize or NarHash.
    Mismatch,
    /// The dump could not be started, read or exited nonzero.
    Failed,
}

#[derive(Default)]
struct Shared {
    child: Option<Child>,
    failure: Option<DumpFailure>,
    released: bool,
    high_water: usize,
}

/// The caller's handle on a running dump.
#[derive(Clone)]
pub struct Dump {
    shared: Arc<Mutex<Shared>>,
}

/// The `Read` side handed to the HTTP client as the request body.
pub struct NarReader {
    stdout: ChildStdout,
    shared: Arc<Mutex<Shared>>,
    verifier: Option<ContentVerifier>,
    held: Vec<u8>,
    out: Vec<u8>,
    pos: usize,
    done: bool,
    failed: bool,
    idle_ms: i32,
}

impl Dump {
    /// Starts `<nix> … store dump-path <path>` with null stdin and stderr.
    /// A dump that produces nothing for `idle` fails.
    pub fn start(
        nix: &Path,
        path: &StorePath,
        expected: ServedIntegrity,
        idle: Duration,
    ) -> Result<(Self, NarReader), DumpFailure> {
        let mut child = Command::new(nix)
            .args([
                "--extra-experimental-features",
                "nix-command",
                "store",
                "dump-path",
            ])
            .arg(path.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| DumpFailure::Failed)?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let shared = Arc::new(Mutex::new(Shared {
            child: Some(child),
            ..Shared::default()
        }));
        let reader = NarReader {
            stdout,
            shared: shared.clone(),
            verifier: Some(ContentVerifier::new(expected)),
            held: Vec::new(),
            out: Vec::new(),
            pos: 0,
            done: false,
            failed: false,
            idle_ms: poll_ms(idle),
        };
        Ok((Self { shared }, reader))
    }

    /// Whether the final chunk was released, so the server may hold the NAR.
    pub fn fully_sent(&self) -> bool {
        self.lock().released
    }

    /// Why the body was aborted, if it was.
    pub fn failure(&self) -> Option<DumpFailure> {
        self.lock().failure
    }

    /// The most chunk bytes held at once.
    pub fn high_water(&self) -> usize {
        self.lock().high_water
    }

    /// Kills and reaps the dump if it is still running.
    pub fn reap(&self) {
        if let Some(mut child) = self.lock().child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl NarReader {
    fn fail(&mut self, failure: DumpFailure) -> io::Error {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        shared.failure.get_or_insert(failure);
        if let Some(mut child) = shared.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.failed = true;
        io::Error::other("NAR dump aborted")
    }

    fn finish(&mut self) -> io::Result<()> {
        let verified = self.verifier.take().map(ContentVerifier::finish);
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        // Stdout reached end of file, so the dump has closed it and is
        // exiting: this wait does not block on a running dump.
        let exited = shared.child.take().map(|mut c| c.wait());
        drop(shared);
        if !matches!(verified, Some(Ok(()))) {
            return Err(self.fail(DumpFailure::Mismatch));
        }
        if !matches!(exited, Some(Ok(status)) if status.success()) {
            return Err(self.fail(DumpFailure::Failed));
        }
        self.out = std::mem::take(&mut self.held);
        self.pos = 0;
        self.done = true;
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .released = true;
        Ok(())
    }

    fn fill(&mut self) -> io::Result<()> {
        if !wait_readable(&self.stdout, self.idle_ms) {
            return Err(self.fail(DumpFailure::Failed));
        }
        let mut chunk = vec![0u8; CHUNK];
        let n = match self.stdout.read(&mut chunk) {
            Ok(n) => n,
            Err(_) => return Err(self.fail(DumpFailure::Failed)),
        };
        if n == 0 {
            return self.finish();
        }
        chunk.truncate(n);
        let verifier = self.verifier.as_mut().expect("verifier until finish");
        if verifier.update(&chunk).is_err() {
            return Err(self.fail(DumpFailure::Mismatch));
        }
        {
            let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            shared.high_water = shared.high_water.max(self.held.len() + chunk.len());
        }
        self.out = std::mem::replace(&mut self.held, chunk);
        self.pos = 0;
        Ok(())
    }
}

impl Read for NarReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.failed {
                return Err(io::Error::other("NAR dump aborted"));
            }
            if self.pos < self.out.len() {
                let n = buf.len().min(self.out.len() - self.pos);
                buf[..n].copy_from_slice(&self.out[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            if self.done {
                return Ok(0);
            }
            self.fill()?;
        }
    }
}
