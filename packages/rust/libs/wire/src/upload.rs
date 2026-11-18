use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use stockhouse_proto::{Sha256, StorePath, UnsupportedVersion, UploadId, check_version};

/// The most references a declaration or `missing` list may hold.
pub const MAX_REFERENCES: usize = 10_000;
/// The largest declaration or upload reply body, in bytes.
pub const MAX_BODY: usize = 4 * 1024 * 1024;
const MAX_CA: usize = 1024;

/// Why a declaration was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DeclarationError {
    /// More than 4 MiB of input.
    TooLarge,
    /// Not an object of exactly the declaration keys and JSON types.
    Malformed,
    /// A version other than 1.
    UnsupportedVersion(UnsupportedVersion),
    /// A value that breaks its rule.
    InvalidField {
        /// The offending key.
        field: &'static str,
    },
}

impl fmt::Display for DeclarationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str("declaration too large"),
            Self::Malformed => f.write_str("malformed declaration"),
            Self::UnsupportedVersion(v) => write!(f, "declaration: {v}"),
            Self::InvalidField { field } => write!(f, "declaration: invalid {field}"),
        }
    }
}

impl std::error::Error for DeclarationError {}

/// The body of `POST uploads/<UploadId>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UploadDeclaration {
    store_path: StorePath,
    nar_hash: Sha256,
    nar_size: u64,
    references: Vec<StorePath>,
    deriver: Option<StorePath>,
    ca: Option<String>,
}

#[derive(Serialize)]
struct DeclarationOut<'a> {
    version: u32,
    store_path: &'a str,
    nar_hash: String,
    nar_size: u64,
    references: Vec<&'a str>,
    deriver: Option<&'a str>,
    ca: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeclarationIn {
    version: u32,
    store_path: String,
    nar_hash: String,
    nar_size: u64,
    references: Vec<String>,
    #[serde(default, deserialize_with = "present")]
    deriver: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    ca: Option<Option<String>>,
}

/// Distinguishes a key given as `null` (`Some(None)`) from a missing key (`None`).
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

fn invalid(field: &'static str) -> DeclarationError {
    DeclarationError::InvalidField { field }
}

impl UploadDeclaration {
    /// Builds a declaration, applying every value rule.
    pub fn new(
        store_path: StorePath,
        nar_hash: Sha256,
        nar_size: u64,
        references: Vec<StorePath>,
        deriver: Option<StorePath>,
        ca: Option<String>,
    ) -> Result<Self, DeclarationError> {
        if nar_size == 0 {
            return Err(invalid("nar_size"));
        }
        let mut sorted: Vec<&StorePath> = references.iter().collect();
        sorted.sort();
        sorted.dedup();
        if references.len() > MAX_REFERENCES || sorted.len() != references.len() {
            return Err(invalid("references"));
        }
        if deriver
            .as_ref()
            .is_some_and(|d| !d.name().ends_with(".drv"))
        {
            return Err(invalid("deriver"));
        }
        let ca_ok = |c: &String| c.len() <= MAX_CA && !c.chars().any(char::is_control);
        if ca.as_ref().is_some_and(|c| !ca_ok(c)) {
            return Err(invalid("ca"));
        }
        Ok(Self {
            store_path,
            nar_hash,
            nar_size,
            references,
            deriver,
            ca,
        })
    }

    /// The declared store path.
    pub fn store_path(&self) -> &StorePath {
        &self.store_path
    }

    /// The declared NarHash.
    pub fn nar_hash(&self) -> Sha256 {
        self.nar_hash
    }

    /// The declared NarSize.
    pub fn nar_size(&self) -> u64 {
        self.nar_size
    }

    /// The declared references, in order.
    pub fn references(&self) -> &[StorePath] {
        &self.references
    }

    /// The declared deriver.
    pub fn deriver(&self) -> Option<&StorePath> {
        self.deriver.as_ref()
    }

    /// The declared CA.
    pub fn ca(&self) -> Option<&str> {
        self.ca.as_deref()
    }

    /// Renders the canonical JSON body.
    pub fn render(&self) -> Vec<u8> {
        let out = DeclarationOut {
            version: 1,
            store_path: self.store_path.as_str(),
            nar_hash: self.nar_hash.to_string(),
            nar_size: self.nar_size,
            references: self.references.iter().map(StorePath::basename).collect(),
            deriver: self.deriver.as_ref().map(StorePath::basename),
            ca: self.ca.as_deref(),
        };
        serde_json::to_vec(&out).expect("declaration serializes")
    }

    /// Parses a declaration strictly.
    pub fn parse(bytes: &[u8]) -> Result<Self, DeclarationError> {
        if bytes.len() > MAX_BODY {
            return Err(DeclarationError::TooLarge);
        }
        let raw: DeclarationIn =
            serde_json::from_slice(bytes).map_err(|_| DeclarationError::Malformed)?;
        let (Some(deriver), Some(ca)) = (raw.deriver, raw.ca) else {
            return Err(DeclarationError::Malformed);
        };
        check_version(raw.version).map_err(DeclarationError::UnsupportedVersion)?;
        let store_path = StorePath::parse(&raw.store_path).map_err(|_| invalid("store_path"))?;
        let nar_hash = Sha256::parse(&raw.nar_hash).map_err(|_| invalid("nar_hash"))?;
        if raw.references.len() > MAX_REFERENCES {
            return Err(invalid("references"));
        }
        let references = raw
            .references
            .iter()
            .map(|r| StorePath::from_basename(r))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| invalid("references"))?;
        let deriver = deriver
            .map(|d| StorePath::from_basename(&d))
            .transpose()
            .map_err(|_| invalid("deriver"))?;
        Self::new(store_path, nar_hash, raw.nar_size, references, deriver, ca)
    }
}

/// `uploads/<UploadId>`: declare (`POST`) and status (`GET`).
pub fn upload_path(id: &UploadId) -> String {
    format!("uploads/{id}")
}

/// `uploads/<UploadId>/nar`: the NAR (`PUT`).
pub fn nar_path(id: &UploadId) -> String {
    format!("uploads/{id}/nar")
}
