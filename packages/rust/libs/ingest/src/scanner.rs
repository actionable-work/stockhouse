use serde_json::{Map, Value};
use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use stockhouse_proto::StorePath;

const OUTPUT_CAP: u64 = 256 * 1024 * 1024;

/// Why a store scan failed.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ScanError {
    /// The output is not a known `nix path-info --json` shape.
    Format,
    /// The output exceeds 256 MiB.
    TooLarge,
    /// The command failed to start or exited nonzero.
    Command(String),
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format => f.write_str("unrecognized nix path-info output"),
            Self::TooLarge => f.write_str("nix path-info output too large"),
            Self::Command(why) => write!(f, "nix path-info failed: {why}"),
        }
    }
}

impl std::error::Error for ScanError {}

/// Lists locally built store paths for rescan reconciliation.
pub trait Scanner {
    /// Candidate paths.
    fn candidates(&self) -> Result<Vec<StorePath>, ScanError>;
    /// Whether a complete scan may clear overflow.
    fn recovers(&self) -> bool {
        true
    }
}

/// The `none` scanner: finds nothing and never clears overflow.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoScanner;

impl Scanner for NoScanner {
    fn candidates(&self) -> Result<Vec<StorePath>, ScanError> {
        Ok(Vec::new())
    }

    fn recovers(&self) -> bool {
        false
    }
}

/// Runs `nix path-info --json --all` and keeps `ultimate` paths.
#[derive(Clone, Debug)]
pub struct NixPathInfoScanner {
    nix: PathBuf,
}

impl NixPathInfoScanner {
    /// Uses the given `nix` executable.
    pub fn new(nix: PathBuf) -> Self {
        Self { nix }
    }
}

impl Scanner for NixPathInfoScanner {
    fn candidates(&self) -> Result<Vec<StorePath>, ScanError> {
        let mut child = Command::new(&self.nix)
            .args([
                "--extra-experimental-features",
                "nix-command",
                "path-info",
                "--json",
                "--all",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| ScanError::Command(e.to_string()))?;
        let mut out = Vec::new();
        let stdout = child.stdout.take().expect("stdout is piped");
        let read = stdout.take(OUTPUT_CAP + 1).read_to_end(&mut out);
        if out.len() as u64 > OUTPUT_CAP {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ScanError::TooLarge);
        }
        let status = child
            .wait()
            .map_err(|e| ScanError::Command(e.to_string()))?;
        read.map_err(|e| ScanError::Command(e.to_string()))?;
        if !status.success() {
            return Err(ScanError::Command(format!("exit status {status}")));
        }
        parse_path_info(&out)
    }
}

/// Parses both pinned `nix path-info --json` shapes.
pub fn parse_path_info(bytes: &[u8]) -> Result<Vec<StorePath>, ScanError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ScanError::Format)?;
    let top = value.as_object().ok_or(ScanError::Format)?;
    match (top.get("version"), top.get("info")) {
        (Some(v), Some(Value::Object(info))) if v == 2 => select(info, StorePath::from_basename),
        _ => select(top, StorePath::parse),
    }
}

fn select<E>(
    entries: &Map<String, Value>,
    parse: fn(&str) -> Result<StorePath, E>,
) -> Result<Vec<StorePath>, ScanError> {
    let mut out = Vec::new();
    for (key, info) in entries {
        let path = parse(key).map_err(|_| ScanError::Format)?;
        let ultimate = match info {
            Value::Object(fields) => fields
                .get("ultimate")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            Value::Null => false,
            _ => return Err(ScanError::Format),
        };
        if ultimate && !path.name().ends_with(".drv") {
            out.push(path);
        }
    }
    Ok(out)
}
