use crate::nar::{poll_ms, wait_readable};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Read;
use std::path::Path;
use std::process::{ChildStdout, Command, Stdio};
use std::time::Duration;
use stockhouse_proto::{Sha256, StorePath};

const OUTPUT_CAP: u64 = 64 * 1024 * 1024;

/// Why a closure could not be read.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ClosureError {
    /// The command failed to start or exited nonzero.
    Command(String),
    /// An unknown shape or value, a dangling reference, no root or a cycle.
    Format,
    /// More than 64 MiB of output.
    TooLarge,
}

impl fmt::Display for ClosureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Command(why) => write!(f, "nix path-info failed: {why}"),
            Self::Format => f.write_str("unrecognized nix path-info closure"),
            Self::TooLarge => f.write_str("nix path-info closure too large"),
        }
    }
}

impl std::error::Error for ClosureError {}

/// One closure path's facts from `nix path-info`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosureEntry {
    /// The path.
    pub path: StorePath,
    /// Its NarHash.
    pub nar_hash: Sha256,
    /// Its NarSize.
    pub nar_size: u64,
    /// Its references, including any self-reference.
    pub references: Vec<StorePath>,
    /// Its deriver.
    pub deriver: Option<StorePath>,
    /// Its CA.
    pub ca: Option<String>,
}

/// Runs `nix path-info --json --recursive <root>` and returns stdout. A
/// command that produces nothing for `idle` is killed and reaped.
pub fn query(nix: &Path, root: &StorePath, idle: Duration) -> Result<Vec<u8>, ClosureError> {
    let mut child = Command::new(nix)
        .args([
            "--extra-experimental-features",
            "nix-command",
            "path-info",
            "--json",
            "--recursive",
        ])
        .arg(root.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| ClosureError::Command(e.to_string()))?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let read = read_bounded(&mut stdout, poll_ms(idle));
    let out = match read {
        Ok(out) => out,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let status = child
        .wait()
        .map_err(|e| ClosureError::Command(e.to_string()))?;
    if !status.success() {
        return Err(ClosureError::Command(format!("exit status {status}")));
    }
    Ok(out)
}

/// Reads stdout to end of file, refusing more than the cap and an idle gap.
fn read_bounded(stdout: &mut ChildStdout, idle_ms: i32) -> Result<Vec<u8>, ClosureError> {
    let mut out = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        if !wait_readable(stdout, idle_ms) {
            return Err(ClosureError::Command(
                "no output before the timeout".to_owned(),
            ));
        }
        let n = match stdout.read(&mut chunk) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ClosureError::Command(e.to_string())),
        };
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&chunk[..n]);
        if out.len() as u64 > OUTPUT_CAP {
            return Err(ClosureError::TooLarge);
        }
    }
}

fn any_path(text: &str) -> Option<StorePath> {
    StorePath::parse(text)
        .or_else(|_| StorePath::from_basename(text))
        .ok()
}

fn hash(value: &Value) -> Option<Sha256> {
    let text = value.as_str()?;
    Sha256::parse_sri(text)
        .or_else(|_| Sha256::parse(text))
        .ok()
}

fn optional(fields: &Map<String, Value>, key: &str) -> Option<Option<String>> {
    match fields.get(key) {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(s)) => Some(Some(s.clone())),
        Some(_) => None,
    }
}

fn entry(key: &str, info: &Value) -> Option<ClosureEntry> {
    let fields = info.as_object()?;
    let references = fields
        .get("references")?
        .as_array()?
        .iter()
        .map(|r| r.as_str().and_then(any_path))
        .collect::<Option<Vec<_>>>()?;
    let deriver = match optional(fields, "deriver")? {
        Some(text) => Some(any_path(&text)?),
        None => None,
    };
    Some(ClosureEntry {
        path: any_path(key)?,
        nar_hash: hash(fields.get("narHash")?)?,
        nar_size: fields.get("narSize")?.as_u64().filter(|&n| n > 0)?,
        references,
        deriver,
        ca: optional(fields, "ca")?,
    })
}

/// Parses both pinned `nix path-info --json` shapes into entries keyed by path.
pub fn parse_closure(bytes: &[u8]) -> Result<BTreeMap<StorePath, ClosureEntry>, ClosureError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ClosureError::Format)?;
    let top = value.as_object().ok_or(ClosureError::Format)?;
    let entries = match (top.get("version"), top.get("info")) {
        (Some(v), Some(Value::Object(info))) if v == 2 => info,
        _ => top,
    };
    let mut out = BTreeMap::new();
    for (key, info) in entries {
        let parsed = entry(key, info).ok_or(ClosureError::Format)?;
        out.insert(parsed.path.clone(), parsed);
    }
    let dangling = out
        .values()
        .flat_map(|e| &e.references)
        .any(|r| !out.contains_key(r));
    if dangling {
        return Err(ClosureError::Format);
    }
    Ok(out)
}

/// References before referrers, ties by store path, self-references ignored.
pub fn upload_order(
    closure: &BTreeMap<StorePath, ClosureEntry>,
    root: &StorePath,
) -> Result<Vec<StorePath>, ClosureError> {
    if !closure.contains_key(root) {
        return Err(ClosureError::Format);
    }
    let mut waiting: BTreeMap<&StorePath, usize> = BTreeMap::new();
    let mut referrers: BTreeMap<&StorePath, Vec<&StorePath>> = BTreeMap::new();
    for (path, entry) in closure {
        let refs: BTreeSet<&StorePath> = entry.references.iter().filter(|r| *r != path).collect();
        waiting.insert(path, refs.len());
        for r in refs {
            referrers.entry(r).or_default().push(path);
        }
    }
    let mut ready: BTreeSet<&StorePath> = waiting
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(p, _)| *p)
        .collect();
    let mut order = Vec::with_capacity(closure.len());
    while let Some(next) = ready.pop_first() {
        order.push(next.clone());
        for referrer in referrers.get(next).into_iter().flatten() {
            let count = waiting.get_mut(referrer).expect("every referrer waits");
            *count -= 1;
            if *count == 0 {
                ready.insert(referrer);
            }
        }
    }
    if order.len() == closure.len() {
        Ok(order)
    } else {
        Err(ClosureError::Format)
    }
}
