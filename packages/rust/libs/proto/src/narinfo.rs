use crate::hash::Sha256;
use crate::lines::{self, CollectError, Fields};
use crate::narinfo_fields as field;
use crate::signature::Signature;
use crate::store_path::StorePath;
use std::fmt;

const MAX_NARINFO_BYTES: usize = 1024 * 1024;
const KNOWN: &[&str] = &[
    "StorePath",
    "URL",
    "Compression",
    "FileHash",
    "FileSize",
    "NarHash",
    "NarSize",
    "References",
    "Deriver",
    "System",
    "CA",
];
const REQUIRED: &[&str] = &["StorePath", "URL", "NarHash", "NarSize"];

/// Why a narinfo was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NarInfoError {
    /// Larger than 1 MiB.
    TooLarge,
    /// Not valid UTF-8.
    NotUtf8,
    /// A non-empty line without a `: ` separator or with an empty key.
    MalformedLine {
        /// One-based line number.
        line: usize,
    },
    /// A key outside the accepted field set.
    UnknownField(String),
    /// A single-valued key repeated.
    DuplicateField(&'static str),
    /// A required field is absent.
    MissingField(&'static str),
    /// A value violates its field rule.
    InvalidField(&'static str),
}

impl fmt::Display for NarInfoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str("narinfo is too large"),
            Self::NotUtf8 => f.write_str("narinfo is not UTF-8"),
            Self::MalformedLine { line } => write!(f, "malformed narinfo line {line}"),
            Self::UnknownField(key) => write!(f, "unknown narinfo field {key:?}"),
            Self::DuplicateField(key) => write!(f, "duplicate narinfo field {key}"),
            Self::MissingField(key) => write!(f, "missing narinfo field {key}"),
            Self::InvalidField(key) => write!(f, "invalid narinfo field {key}"),
        }
    }
}

impl std::error::Error for NarInfoError {}

impl From<CollectError> for NarInfoError {
    fn from(error: CollectError) -> Self {
        match error {
            CollectError::Malformed(line) => Self::MalformedLine { line },
            CollectError::Unknown(key) => Self::UnknownField(key),
            CollectError::Duplicate(key) => Self::DuplicateField(key),
        }
    }
}

/// Representation compression named by a narinfo.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Compression {
    /// Uncompressed NAR.
    None,
    /// xz.
    Xz,
    /// bzip2, Nix's default when the field is absent.
    Bzip2,
    /// zstd.
    Zstd,
    /// brotli.
    Br,
    /// gzip.
    Gzip,
}

impl Compression {
    const ALL: [Self; 6] = [
        Self::None,
        Self::Xz,
        Self::Bzip2,
        Self::Zstd,
        Self::Br,
        Self::Gzip,
    ];

    /// The narinfo spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Xz => "xz",
            Self::Bzip2 => "bzip2",
            Self::Zstd => "zstd",
            Self::Br => "br",
            Self::Gzip => "gzip",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == text)
    }
}

/// Hash and size of the served (possibly compressed) file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileInfo {
    /// `FileHash`.
    pub hash: Sha256,
    /// `FileSize`.
    pub size: u64,
}

/// The `Deriver` field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Deriver {
    /// `unknown-deriver`.
    Unknown,
    /// A `.drv` store path.
    Path(StorePath),
}

/// A strictly parsed narinfo.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NarInfo {
    /// `StorePath`.
    pub store_path: StorePath,
    /// `URL`, relative to the cache base.
    pub url: String,
    /// `Compression`, defaulting to bzip2.
    pub compression: Compression,
    /// `FileHash` and `FileSize`, present together or not at all.
    pub file: Option<FileInfo>,
    /// `NarHash`.
    pub nar_hash: Sha256,
    /// `NarSize`.
    pub nar_size: u64,
    /// `References`, in narinfo order.
    pub references: Vec<StorePath>,
    /// `Deriver`.
    pub deriver: Option<Deriver>,
    /// Every `Sig`, in order.
    pub signatures: Vec<Signature>,
    /// `System`.
    pub system: Option<String>,
    /// `CA`.
    pub ca: Option<String>,
}

impl NarInfo {
    /// Parses narinfo bytes strictly.
    pub fn parse(input: &[u8]) -> Result<Self, NarInfoError> {
        if input.len() > MAX_NARINFO_BYTES {
            return Err(NarInfoError::TooLarge);
        }
        let text = std::str::from_utf8(input).map_err(|_| NarInfoError::NotUtf8)?;
        let fields = lines::collect(text, KNOWN, Some("Sig"))?;
        if let Some(missing) = REQUIRED.iter().find(|k| fields.get(k).is_none()) {
            return Err(NarInfoError::MissingField(missing));
        }
        build(&fields)
    }

    /// Returns a copy with only `URL` replaced.
    pub fn with_url(&self, url: &str) -> Result<Self, NarInfoError> {
        let url = field::url(url).ok_or(NarInfoError::InvalidField("URL"))?;
        Ok(Self {
            url,
            ..self.clone()
        })
    }

    /// Renders canonical narinfo text.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut line =
            |key: &str, value: &dyn fmt::Display| out.push_str(&format!("{key}: {value}\n"));
        line("StorePath", &self.store_path);
        line("URL", &self.url);
        line("Compression", &self.compression.as_str());
        if let Some(file) = &self.file {
            line("FileHash", &file.hash);
            line("FileSize", &file.size);
        }
        line("NarHash", &self.nar_hash);
        line("NarSize", &self.nar_size);
        line("References", &field::render_references(&self.references));
        if let Some(deriver) = &self.deriver {
            line("Deriver", &field::render_deriver(deriver));
        }
        self.signatures.iter().for_each(|sig| line("Sig", sig));
        self.system.iter().for_each(|s| line("System", s));
        self.ca.iter().for_each(|s| line("CA", s));
        out
    }
}

fn required<T>(
    fields: &Fields<'_>,
    key: &'static str,
    parse: fn(&str) -> Option<T>,
) -> Result<T, NarInfoError> {
    fields
        .get(key)
        .and_then(parse)
        .ok_or(NarInfoError::InvalidField(key))
}

fn optional<T>(
    fields: &Fields<'_>,
    key: &'static str,
    parse: fn(&str) -> Option<T>,
) -> Result<Option<T>, NarInfoError> {
    fields
        .get(key)
        .map(|v| parse(v).ok_or(NarInfoError::InvalidField(key)))
        .transpose()
}

fn build(fields: &Fields<'_>) -> Result<NarInfo, NarInfoError> {
    Ok(NarInfo {
        store_path: required(fields, "StorePath", |v| StorePath::parse(v).ok())?,
        url: required(fields, "URL", field::url)?,
        compression: optional(fields, "Compression", Compression::parse)?
            .unwrap_or(Compression::Bzip2),
        file: file_info(fields)?,
        nar_hash: required(fields, "NarHash", |v| Sha256::parse(v).ok())?,
        nar_size: required(fields, "NarSize", field::nar_size)?,
        references: optional(fields, "References", field::references)?.unwrap_or_default(),
        deriver: optional(fields, "Deriver", field::deriver)?,
        signatures: signatures(fields)?,
        system: optional(fields, "System", field::plain_text)?,
        ca: optional(fields, "CA", field::plain_text)?,
    })
}

fn file_info(fields: &Fields<'_>) -> Result<Option<FileInfo>, NarInfoError> {
    let hash = optional(fields, "FileHash", |v| Sha256::parse(v).ok())?;
    let size = optional(fields, "FileSize", lines::parse_decimal)?;
    match (hash, size) {
        (Some(hash), Some(size)) => Ok(Some(FileInfo { hash, size })),
        (None, None) => Ok(None),
        (Some(_), None) => Err(NarInfoError::InvalidField("FileHash")),
        (None, Some(_)) => Err(NarInfoError::InvalidField("FileSize")),
    }
}

fn signatures(fields: &Fields<'_>) -> Result<Vec<Signature>, NarInfoError> {
    let parse = |v: &&str| Signature::parse(v).map_err(|_| NarInfoError::InvalidField("Sig"));
    fields.repeated().iter().map(parse).collect()
}
