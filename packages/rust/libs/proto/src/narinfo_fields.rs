//! Value rules for individual narinfo fields.

use crate::lines;
use crate::narinfo::Deriver;
use crate::store_path::StorePath;
use std::collections::BTreeSet;

const URL_CHARS: &[u8] = b"+-._/~=";

/// A relative URL: allowed characters only, no empty, `.` or `..` segment.
pub(crate) fn url(text: &str) -> Option<String> {
    let chars_ok = text.bytes().all(|c| c.is_ascii_alphanumeric() || URL_CHARS.contains(&c));
    let segments_ok = text.split('/').all(|s| !s.is_empty() && s != "." && s != "..");
    (chars_ok && segments_ok).then(|| text.to_owned())
}

pub(crate) fn nar_size(text: &str) -> Option<u64> {
    lines::parse_decimal(text).filter(|&n| n > 0)
}

/// Space-separated basenames without duplicates; empty text is no references.
pub(crate) fn references(text: &str) -> Option<Vec<StorePath>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for base in text.split_ascii_whitespace() {
        let path = StorePath::from_basename(base).ok()?;
        if !seen.insert(path.clone()) {
            return None;
        }
        out.push(path);
    }
    Some(out)
}

pub(crate) fn deriver(text: &str) -> Option<Deriver> {
    if text == "unknown-deriver" {
        return Some(Deriver::Unknown);
    }
    let path = StorePath::from_basename(text).ok()?;
    path.name().ends_with(".drv").then_some(Deriver::Path(path))
}

pub(crate) fn plain_text(text: &str) -> Option<String> {
    lines::is_plain_text(text).then(|| text.to_owned())
}

pub(crate) fn render_references(references: &[StorePath]) -> String {
    references.iter().map(StorePath::basename).collect::<Vec<_>>().join(" ")
}

pub(crate) fn render_deriver(deriver: &Deriver) -> String {
    match deriver {
        Deriver::Unknown => "unknown-deriver".to_owned(),
        Deriver::Path(path) => path.basename().to_owned(),
    }
}
