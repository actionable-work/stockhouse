use crate::group::Closure;
use std::collections::{BTreeSet, HashMap, HashSet};
use stockhouse_ingest::{Queue, QueueError};
use stockhouse_proto::StorePath;

/// Reference edges seen in this call, plus missing paths still being repaired.
#[derive(Default)]
pub(crate) struct Known {
    referrers: HashMap<StorePath, BTreeSet<StorePath>>,
    missing: BTreeSet<StorePath>,
}

impl Known {
    pub(crate) fn load(&mut self, facts: &Closure, queue: &Queue) -> Result<(), QueueError> {
        for (path, info) in facts {
            for reference in info
                .references
                .iter()
                .filter(|reference| *reference != path)
            {
                self.referrers
                    .entry(reference.clone())
                    .or_default()
                    .insert(path.clone());
            }
        }
        for seed in &self.missing {
            for path in self.affected(seed) {
                queue.forget_done(&path)?;
            }
        }
        Ok(())
    }

    pub(crate) fn missing(&mut self, path: &StorePath) {
        self.missing.insert(path.clone());
    }

    pub(crate) fn repaired(&mut self, path: &StorePath) {
        self.missing.remove(path);
    }

    /// Visits each referrer before its reference, so each removal is crash-safe.
    pub(crate) fn affected(&self, seed: &StorePath) -> Vec<StorePath> {
        let mut seen = HashSet::new();
        let mut stack = vec![(seed, false)];
        let mut order = Vec::new();
        while let Some((path, expanded)) = stack.pop() {
            if expanded {
                order.push(path.clone());
            } else if seen.insert(path) {
                stack.push((path, true));
                if let Some(referrers) = self.referrers.get(path) {
                    stack.extend(referrers.iter().map(|path| (path, false)));
                }
            }
        }
        order
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stockhouse_test_support::store_path;

    #[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-037"))]
    #[test]
    fn invalidation_visits_every_referrer_before_its_reference() {
        let [base, left, right, top, unrelated] =
            ["base", "left", "right", "top", "unrelated"].map(store_path);
        let graph = Known {
            referrers: HashMap::from([
                (base.clone(), BTreeSet::from([left.clone(), right.clone()])),
                (left.clone(), BTreeSet::from([top.clone()])),
                (right.clone(), BTreeSet::from([top.clone()])),
            ]),
            missing: BTreeSet::new(),
        };
        let affected = graph.affected(&base);
        assert_eq!(affected.len(), 4);
        assert_eq!(affected.first(), Some(&top));
        assert_eq!(affected.last(), Some(&base));
        assert!(affected.contains(&left) && affected.contains(&right));
        assert!(!affected.contains(&unrelated));
    }
}
