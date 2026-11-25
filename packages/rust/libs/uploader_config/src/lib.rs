//! Configuration and worker limits for the Stockhouse uploader.

use serde::Deserialize;
use std::fmt;
use std::path::PathBuf;
use stockhouse_credentials::CredentialSpec;
use stockhouse_proto::{UnsupportedVersion, check_version};
use stockhouse_wire_v2::ProtocolMode;

/// Retry and batching limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerLimits {
    /// Records attempted per pass.
    pub batch_size: usize,
    /// Retry outcomes before a record fails.
    pub max_attempts: u32,
    /// First backoff, seconds.
    pub backoff_base_secs: u64,
    /// Backoff cap, seconds.
    pub backoff_max_secs: u64,
    /// Seconds between periodic rescans.
    pub rescan_interval_secs: u64,
}

/// Why an uploader configuration was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum UploaderConfigError {
    /// TOML syntax, an unknown field or a wrong type.
    Parse(String),
    /// A version other than 1.
    UnsupportedVersion(UnsupportedVersion),
    /// `queue_dir` is not absolute.
    RelativeQueueDir,
    /// A zero limit, or a backoff base above its maximum.
    InvalidLimit(&'static str),
    /// `nix-copy` without an absolute `file://` destination.
    UnsupportedDestination,
}

impl fmt::Display for UploaderConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(m) => write!(f, "uploader configuration: {m}"),
            Self::UnsupportedVersion(v) => write!(f, "uploader configuration: {v}"),
            Self::RelativeQueueDir => {
                f.write_str("uploader configuration: queue_dir must be absolute")
            }
            Self::InvalidLimit(field) => write!(f, "uploader configuration: invalid {field}"),
            Self::UnsupportedDestination => f.write_str(
                "uploader configuration: nix-copy needs an absolute file:// destination",
            ),
        }
    }
}

impl std::error::Error for UploaderConfigError {}

/// Which upload backend to use.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "kind")]
pub enum BackendConfig {
    /// Keep records pending until server upload exists.
    Deferred,
    /// Copy into a local `file://` cache.
    NixCopy {
        /// Absolute `file://` URL.
        destination: String,
        /// The `nix` executable.
        #[serde(default = "default_nix")]
        nix: PathBuf,
    },
    /// Authenticated upload to the private tier.
    Server(ServerBackendConfig),
}

/// The `server` backend's settings. The base URL is checked when the
/// backend is built (`stockhouse-upload`).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServerBackendConfig {
    /// The private tier base URL.
    pub base_url: String,
    /// Where the bearer credential lives.
    pub credential: CredentialSpec,
    /// The `nix` executable.
    #[serde(default = "default_nix")]
    pub nix: PathBuf,
    /// Connect and per-request timeout base, seconds.
    #[serde(default = "default_timeout")]
    pub request_timeout_secs: u64,
    /// The largest closure uploaded.
    #[serde(default = "default_max_closure")]
    pub max_closure_paths: usize,
    /// Upload protocol preference.
    #[serde(default)]
    pub protocol: ProtocolMode,
    /// Maximum simultaneous v2 NAR transfers.
    #[serde(default = "default_max_inflight")]
    pub max_inflight_nars: i64,
}

fn default_max_inflight() -> i64 {
    4
}

impl ServerBackendConfig {
    fn check_limits(&self) -> Result<(), UploaderConfigError> {
        let invalid = [
            (
                "request_timeout_secs",
                !(1..=300).contains(&self.request_timeout_secs),
            ),
            ("max_closure_paths", self.max_closure_paths == 0),
            (
                "max_inflight_nars",
                !(1..=16).contains(&self.max_inflight_nars),
            ),
        ];
        match invalid.into_iter().find(|(_, invalid)| *invalid) {
            Some((field, _)) => Err(UploaderConfigError::InvalidLimit(field)),
            None => Ok(()),
        }
    }
}

fn default_timeout() -> u64 {
    60
}

fn default_max_closure() -> usize {
    50_000
}

/// Which store scanner to use.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "kind")]
pub enum ScannerConfig {
    /// `nix path-info --json --all`.
    NixPathInfo {
        /// The `nix` executable.
        #[serde(default = "default_nix")]
        nix: PathBuf,
    },
    /// No scanning.
    None,
}

fn default_nix() -> PathBuf {
    PathBuf::from("nix")
}

/// A validated uploader configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UploaderConfig {
    /// Always 1.
    pub version: u32,
    /// The queue directory.
    pub queue_dir: PathBuf,
    /// Queue capacity.
    pub max_pending: usize,
    /// Retry outcomes before failing.
    pub max_attempts: u32,
    /// Records per pass.
    pub batch_size: usize,
    /// First backoff.
    pub backoff_base_secs: u64,
    /// Backoff cap.
    pub backoff_max_secs: u64,
    /// Sleep between passes.
    pub poll_interval_secs: u64,
    /// Periodic rescan interval.
    pub rescan_interval_secs: u64,
    /// Upload backend.
    pub backend: BackendConfig,
    /// Store scanner.
    pub scanner: ScannerConfig,
}

impl UploaderConfig {
    /// Parses and validates TOML configuration.
    pub fn parse(text: &str) -> Result<Self, UploaderConfigError> {
        let config: Self =
            toml::from_str(text).map_err(|e| UploaderConfigError::Parse(e.message().to_owned()))?;
        check_version(config.version).map_err(UploaderConfigError::UnsupportedVersion)?;
        if !config.queue_dir.is_absolute() {
            return Err(UploaderConfigError::RelativeQueueDir);
        }
        config.check_limits()?;
        if let BackendConfig::Server(server) = &config.backend {
            server.check_limits()?;
        }
        if let BackendConfig::NixCopy { destination, .. } = &config.backend {
            let absolute = destination
                .strip_prefix("file://")
                .is_some_and(|p| p.starts_with('/'));
            if !absolute {
                return Err(UploaderConfigError::UnsupportedDestination);
            }
        }
        Ok(config)
    }

    fn check_limits(&self) -> Result<(), UploaderConfigError> {
        let zero = [
            ("max_pending", self.max_pending == 0),
            ("max_attempts", self.max_attempts == 0),
            ("batch_size", self.batch_size == 0),
            ("backoff_base_secs", self.backoff_base_secs == 0),
            (
                "backoff_max_secs",
                self.backoff_base_secs > self.backoff_max_secs,
            ),
            ("poll_interval_secs", self.poll_interval_secs == 0),
            ("rescan_interval_secs", self.rescan_interval_secs == 0),
        ];
        match zero.into_iter().find(|(_, bad)| *bad) {
            Some((field, _)) => Err(UploaderConfigError::InvalidLimit(field)),
            None => Ok(()),
        }
    }

    /// The worker limits this configuration selects.
    pub fn limits(&self) -> WorkerLimits {
        WorkerLimits {
            batch_size: self.batch_size,
            max_attempts: self.max_attempts,
            backoff_base_secs: self.backoff_base_secs,
            backoff_max_secs: self.backoff_max_secs,
            rescan_interval_secs: self.rescan_interval_secs,
        }
    }
}
