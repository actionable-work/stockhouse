//! The `Key: value` line syntax shared by narinfo and `nix-cache-info`.

use std::collections::BTreeMap;

/// A structural problem found while collecting fields.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum CollectError {
    Malformed(usize),
    Unknown(String),
    Duplicate(&'static str),
}

/// Fields collected from text, keyed by their canonical static names.
#[derive(Debug, Default)]
pub(crate) struct Fields<'a> {
    single: BTreeMap<&'static str, &'a str>,
    repeated: Vec<&'a str>,
}

impl<'a> Fields<'a> {
    pub(crate) fn get(&self, key: &'static str) -> Option<&'a str> {
        self.single.get(key).copied()
    }

    pub(crate) fn repeated(&self) -> &[&'a str] {
        &self.repeated
    }
}

/// Splits one line into key and value. `Key:` alone has an empty value.
fn split_line(line: &str) -> Option<(&str, &str)> {
    let (key, rest) = line.split_once(':')?;
    let value = if rest.is_empty() {
        rest
    } else {
        rest.strip_prefix(' ')?
    };
    if key.is_empty() {
        None
    } else {
        Some((key, value))
    }
}

/// Collects fields; `repeatable` names the single key that may repeat.
pub(crate) fn collect<'a>(
    text: &'a str,
    known: &[&'static str],
    repeatable: Option<&'static str>,
) -> Result<Fields<'a>, CollectError> {
    let mut fields = Fields::default();
    for (index, line) in text.split('\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let (key, value) = split_line(line).ok_or(CollectError::Malformed(index + 1))?;
        insert(&mut fields, known, repeatable, key, value)?;
    }
    Ok(fields)
}

fn insert<'a>(
    fields: &mut Fields<'a>,
    known: &[&'static str],
    repeatable: Option<&'static str>,
    key: &str,
    value: &'a str,
) -> Result<(), CollectError> {
    if repeatable == Some(key) {
        fields.repeated.push(value);
        return Ok(());
    }
    let name = known.iter().copied().find(|k| *k == key);
    let name = name.ok_or_else(|| CollectError::Unknown(key.to_owned()))?;
    match fields.single.insert(name, value) {
        Some(_) => Err(CollectError::Duplicate(name)),
        None => Ok(()),
    }
}

/// True when `text` has no control characters.
pub(crate) fn is_plain_text(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

/// Parses an unsigned decimal with digits only.
pub(crate) fn parse_decimal(text: &str) -> Option<u64> {
    let digits = !text.is_empty() && text.bytes().all(|c| c.is_ascii_digit());
    if digits { text.parse().ok() } else { None }
}
