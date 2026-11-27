//! Which store path each lease slot names.

use super::LeaseConfig;
use std::fs;
use std::path::{Path, PathBuf};
use stockhouse_proto::StorePath;
use stockhouse_wire::LeaseSlot;

/// A slot and its path, or why it was skipped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SlotPath {
    /// Renew with this path.
    Renew(LeaseSlot, StorePath),
    /// The path does not resolve to a store path.
    Unresolvable(LeaseSlot),
}

fn resolve(config: &LeaseConfig, path: &Path) -> Option<StorePath> {
    let on_disk = config
        .store_root
        .join(path.strip_prefix("/").unwrap_or(path));
    let resolved = fs::canonicalize(on_disk).ok()?;
    let relative = resolved.strip_prefix(&config.store_root).ok()?;
    StorePath::parse(&format!("/{}", relative.display())).ok()
}

fn generations(config: &LeaseConfig) -> Vec<(u64, PathBuf)> {
    let profile = &config.system_profile;
    let (Some(dir), Some(name)) = (profile.parent(), profile.file_name()) else {
        return Vec::new();
    };
    let prefix = format!("{}-", name.to_string_lossy());
    let on_disk = config.store_root.join(dir.strip_prefix("/").unwrap_or(dir));
    let Ok(entries) = fs::read_dir(on_disk) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            let n = file
                .strip_prefix(&prefix)?
                .strip_suffix("-link")?
                .parse()
                .ok()?;
            Some((n, dir.join(file)))
        })
        .collect()
}

fn slot(n: u8) -> LeaseSlot {
    LeaseSlot::new(n).expect("fixed slots are in range")
}

/// Resolves slot 1, slot 2 (when another generation exists) and the extras.
pub fn resolve_slots(config: &LeaseConfig) -> Vec<SlotPath> {
    let current = resolve(config, &config.current_system);
    let mut out = vec![match &current {
        Some(path) => SlotPath::Renew(slot(1), path.clone()),
        None => SlotPath::Unresolvable(slot(1)),
    }];
    let mut previous: Vec<(u64, StorePath)> = generations(config)
        .into_iter()
        .filter_map(|(n, link)| Some((n, resolve(config, &link)?)))
        .filter(|(_, target)| Some(target) != current.as_ref())
        .collect();
    previous.sort_by_key(|(n, _)| *n);
    if let Some((_, path)) = previous.pop() {
        out.push(SlotPath::Renew(slot(2), path));
    }
    for (extra, path) in &config.extra {
        out.push(match resolve(config, path) {
            Some(p) => SlotPath::Renew(*extra, p),
            None => SlotPath::Unresolvable(*extra),
        });
    }
    out
}
