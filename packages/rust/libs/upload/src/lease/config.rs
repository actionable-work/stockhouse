use crate::endpoint::{EndpointError, check_base_url};
use serde::Deserialize;
use std::fmt;
use std::path::{Path, PathBuf};
use stockhouse_credentials::CredentialSpec;
use stockhouse_proto::{UnsupportedVersion, check_version};
use stockhouse_wire::LeaseSlot;

/// Why a lease configuration was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LeaseConfigError {
    /// TOML syntax, an unknown or missing field, or a wrong type.
    Parse(String),
    /// A version other than 1.
    UnsupportedVersion(UnsupportedVersion),
    /// A path that must be absolute is not.
    RelativePath {
        /// The field.
        field: &'static str,
    },
    /// An extra slot outside 3–10 or repeated.
    InvalidSlot,
    /// A timeout outside 1–300 seconds.
    InvalidLimit,
    /// The base URL was refused.
    Endpoint(EndpointError),
}

impl fmt::Display for LeaseConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(m) => write!(f, "lease configuration: {m}"),
            Self::UnsupportedVersion(v) => write!(f, "lease configuration: {v}"),
            Self::RelativePath { field } => {
                write!(f, "lease configuration: {field} must be absolute")
            }
            Self::InvalidSlot => {
                f.write_str("lease configuration: extra slots must be distinct and within 3-10")
            }
            Self::InvalidLimit => f.write_str("lease configuration: invalid request_timeout_secs"),
            Self::Endpoint(e) => write!(f, "lease configuration: {e}"),
        }
    }
}

impl std::error::Error for LeaseConfigError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    version: u32,
    base_url: String,
    credential: CredentialSpec,
    machine_id_file: PathBuf,
    #[serde(default = "default_current")]
    current_system: PathBuf,
    #[serde(default = "default_profile")]
    system_profile: PathBuf,
    #[serde(default = "default_timeout")]
    request_timeout_secs: u64,
    #[serde(default)]
    extra: Vec<RawExtra>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExtra {
    slot: u8,
    path: PathBuf,
}

fn default_current() -> PathBuf {
    PathBuf::from("/run/current-system")
}

fn default_profile() -> PathBuf {
    PathBuf::from("/nix/var/nix/profiles/system")
}

fn default_timeout() -> u64 {
    60
}

/// A validated lease configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseConfig {
    /// The private tier base URL (already checked).
    pub base_url: String,
    /// Where the bearer credential lives.
    pub credential: CredentialSpec,
    /// The machine identity file.
    pub machine_id_file: PathBuf,
    /// Slot 1's path.
    pub current_system: PathBuf,
    /// The system profile whose generations give slot 2.
    pub system_profile: PathBuf,
    /// Per-request timeout, seconds.
    pub request_timeout_secs: u64,
    /// Slots 3–10.
    pub extra: Vec<(LeaseSlot, PathBuf)>,
    /// Resolved paths are read relative to this root (`/` unless a test
    /// sets it with `with_store_root`).
    pub(crate) store_root: PathBuf,
}

fn absolute(path: &Path, field: &'static str) -> Result<(), LeaseConfigError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(LeaseConfigError::RelativePath { field })
    }
}

fn extra_slots(raw: Vec<RawExtra>) -> Result<Vec<(LeaseSlot, PathBuf)>, LeaseConfigError> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for extra in raw {
        absolute(&extra.path, "extra.path")?;
        if !(3..=10).contains(&extra.slot) || seen.contains(&extra.slot) {
            return Err(LeaseConfigError::InvalidSlot);
        }
        seen.push(extra.slot);
        let slot = LeaseSlot::new(extra.slot).map_err(|_| LeaseConfigError::InvalidSlot)?;
        out.push((slot, extra.path));
    }
    Ok(out)
}

impl LeaseConfig {
    /// Resolves slot paths under `root` instead of `/` (tests only).
    #[doc(hidden)]
    pub fn with_store_root(mut self, root: &Path) -> Self {
        self.store_root = root.to_path_buf();
        self
    }

    /// Parses and validates TOML configuration.
    pub fn parse(text: &str) -> Result<Self, LeaseConfigError> {
        let raw: RawConfig =
            toml::from_str(text).map_err(|e| LeaseConfigError::Parse(e.message().to_owned()))?;
        check_version(raw.version).map_err(LeaseConfigError::UnsupportedVersion)?;
        absolute(&raw.machine_id_file, "machine_id_file")?;
        absolute(&raw.current_system, "current_system")?;
        absolute(&raw.system_profile, "system_profile")?;
        let extra = extra_slots(raw.extra)?;
        if !(1..=300).contains(&raw.request_timeout_secs) {
            return Err(LeaseConfigError::InvalidLimit);
        }
        check_base_url(&raw.base_url).map_err(LeaseConfigError::Endpoint)?;
        Ok(Self {
            base_url: raw.base_url,
            credential: raw.credential,
            machine_id_file: raw.machine_id_file,
            current_system: raw.current_system,
            system_profile: raw.system_profile,
            request_timeout_secs: raw.request_timeout_secs,
            extra,
            store_root: PathBuf::from("/"),
        })
    }
}
