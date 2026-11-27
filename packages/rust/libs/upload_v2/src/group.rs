use crate::dispatch::ServerBackend;
use crate::known::Known;
use crate::outcome::{failure, waiting};
use crate::transfer::CallAuth;
use std::collections::{BTreeMap, HashMap, HashSet};
use stockhouse_ingest::{Queue, QueueError, RetryKind, UploadEntry, UploadOutcome};
use stockhouse_proto::{StorePath, UploadId};
use stockhouse_upload::{ClosureEntry, upload_order};

pub(crate) type Closure = BTreeMap<StorePath, ClosureEntry>;

pub(crate) struct Job {
    pub(crate) index: usize,
    pub(crate) root: StorePath,
    pub(crate) paths: HashSet<StorePath>,
}

#[derive(Default)]
pub(crate) struct GroupJobs {
    pub(crate) facts: Closure,
    pub(crate) jobs: Vec<Job>,
}

impl GroupJobs {
    pub(crate) fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    pub(crate) fn fits(&self, closure: &Closure, max: usize) -> bool {
        self.facts.len()
            + closure
                .keys()
                .filter(|path| !self.facts.contains_key(*path))
                .count()
            <= max
    }

    pub(crate) fn add(&mut self, index: usize, root: &StorePath, closure: Closure) {
        self.jobs.push(Job {
            index,
            root: root.clone(),
            paths: closure.keys().cloned().collect(),
        });
        self.facts.extend(closure);
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Work {
    Status,
    Declare,
    Transfer,
    Ready,
}

pub(crate) enum Stage {
    Ledger,
    Complete,
    Confirmed,
    Work(Work),
    Next(Work),
    Held,
    Ended(UploadOutcome),
}

impl Stage {
    pub(crate) fn complete(&self) -> bool {
        matches!(self, Self::Ledger | Self::Complete)
    }
}

pub(crate) struct Node {
    pub(crate) stage: Stage,
    pub(crate) wire: Option<UploadEntry>,
    reworked: bool,
}

impl Node {
    fn new(queue: &Queue, path: &StorePath) -> Self {
        if queue.is_uploaded(path) {
            return Self {
                stage: Stage::Ledger,
                wire: None,
                reworked: false,
            };
        }
        match queue.upload_state(path) {
            Ok(wire) => Self {
                stage: Stage::Work(if wire.nar_sent {
                    Work::Status
                } else {
                    Work::Declare
                }),
                wire: Some(wire),
                reworked: false,
            },
            Err(error) => Self {
                stage: Stage::Ended(failure(error)),
                wire: None,
                reworked: false,
            },
        }
    }
}

pub(crate) struct Group<'a> {
    pub(crate) backend: &'a ServerBackend,
    pub(crate) auth: &'a CallAuth,
    pub(crate) known: &'a mut Known,
    pub(crate) facts: Closure,
    pub(crate) order: Vec<StorePath>,
    pub(crate) states: HashMap<StorePath, Node>,
    round: u8,
    pub(crate) local_error: Option<UploadOutcome>,
}

impl<'a> Group<'a> {
    pub(crate) fn new(
        backend: &'a ServerBackend,
        auth: &'a CallAuth,
        known: &'a mut Known,
        facts: Closure,
    ) -> Result<Self, UploadOutcome> {
        let order = upload_order(&facts, facts.keys().next().expect("a group has paths"))
            .map_err(failure)?;
        let queue = backend.v1.queue();
        known.load(&facts, queue).map_err(failure)?;
        let states = order
            .iter()
            .map(|path| (path.clone(), Node::new(queue, path)))
            .collect();
        Ok(Self {
            backend,
            auth,
            known,
            facts,
            order,
            states,
            round: 0,
            local_error: None,
        })
    }

    pub(crate) fn queue(&self) -> &'a Queue {
        self.backend.v1.queue()
    }

    pub(crate) fn stopped(&self) -> bool {
        self.local_error.is_some() || self.auth.stopped()
    }

    pub(crate) fn paths(&self, work: Work) -> Vec<StorePath> {
        self.order
            .iter()
            .filter(|path| matches!(self.states[*path].stage, Stage::Work(w) if w == work))
            .cloned()
            .collect()
    }

    pub(crate) fn entry(&self, path: &StorePath) -> UploadEntry {
        self.states[path].wire.expect("wire work has an identity")
    }

    pub(crate) fn state(&mut self, path: &StorePath) -> &mut Node {
        self.states
            .get_mut(path)
            .expect("path belongs to the group")
    }

    pub(crate) fn set(&mut self, path: &StorePath, stage: Stage) {
        self.state(path).stage = stage;
    }

    pub(crate) fn update_wire(
        &mut self,
        path: &StorePath,
        result: Result<UploadEntry, QueueError>,
        stage: Stage,
    ) {
        let node = self.state(path);
        match result {
            Ok(wire) => {
                node.wire = Some(wire);
                node.stage = stage;
            }
            Err(error) => node.stage = Stage::Ended(failure(error)),
        }
    }

    pub(crate) fn allow_rework(&mut self, path: &StorePath) -> bool {
        let last_round = self.round == 3;
        let node = self.state(path);
        if last_round || node.reworked {
            node.stage = Stage::Ended(UploadOutcome::Retry(
                RetryKind::Transient,
                format!("{path}: upload round limit"),
            ));
            return false;
        }
        node.reworked = true;
        true
    }

    pub(crate) fn run(&mut self) {
        for round in 1..=3 {
            self.round = round;
            let keep_going = self.run_round();
            if !keep_going || !self.advance() {
                break;
            }
        }
    }

    fn advance(&mut self) -> bool {
        let mut next = HashSet::new();
        for path in &self.order {
            let carry = self.round < 3
                && self.facts[path]
                    .references
                    .iter()
                    .any(|dep| next.contains(dep));
            let node = self.states.get_mut(path).expect("known group path");
            node.stage = match node.stage {
                Stage::Next(work) => Stage::Work(work),
                Stage::Work(Work::Ready) | Stage::Held if carry => Stage::Work(Work::Ready),
                Stage::Confirmed if carry => {
                    next.insert(path.clone());
                    continue;
                }
                Stage::Work(Work::Ready) | Stage::Held => Stage::Ended(waiting(path)),
                _ => continue,
            };
            if matches!(node.stage, Stage::Work(_)) {
                next.insert(path.clone());
            }
        }
        !next.is_empty()
    }

    pub(crate) fn complete_confirmed(&mut self) {
        for path in &self.order {
            if !matches!(self.states[path].stage, Stage::Confirmed) || !self.refs_complete(path) {
                continue;
            }
            let id = self.states[path]
                .wire
                .map(|entry| entry.id)
                .map_or_else(|| self.queue().upload_state(path).map(|entry| entry.id), Ok);
            let result = id.and_then(|id| {
                self.queue()
                    .complete_path(path, id, self.backend.clock.now())
            });
            let node = self.states.get_mut(path).expect("known group path");
            node.stage = match result {
                Ok(()) => {
                    self.known.repaired(path);
                    Stage::Complete
                }
                Err(error) => Stage::Ended(failure(error)),
            };
        }
    }

    fn refs_complete(&self, path: &StorePath) -> bool {
        self.facts[path]
            .references
            .iter()
            .all(|dep| dep == path || self.states[dep].stage.complete())
    }

    pub(crate) fn invalidate(&mut self, missing: &StorePath) -> Result<(), QueueError> {
        self.known.missing(missing);
        for path in self.known.affected(missing) {
            let previous_id = self
                .queue()
                .done_record(&path)
                .and_then(|done| UploadId::parse(&done.id).ok());
            self.queue().forget_done(&path)?;
            if let Some(node) = self.states.get_mut(&path)
                && node.stage.complete()
            {
                if node.wire.is_none() {
                    node.wire = previous_id.map(|id| UploadEntry {
                        id,
                        nar_sent: false,
                    });
                }
                node.stage = Stage::Confirmed;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use stockhouse_credentials::StaticCredential;
    use stockhouse_test_support::{FakeServer, Store, TOKEN};
    use stockhouse_upload::{ServerEndpoint, ServerUploader, parse_closure};
    use stockhouse_wire_v2::ProtocolMode;

    #[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-028"))]
    #[test]
    fn a_confirmed_referrer_passes_rework_through_to_its_held_referrer() {
        let mut store = Store::new();
        let seed = store.add("seed", &[]);
        let bridge = store.add("bridge", &[&seed]);
        let root = store.add("held", &[&bridge]);
        let facts = parse_closure(store.closure_json(&root).as_bytes()).unwrap();
        let fake = FakeServer::start(|_| panic!("advancing a round does not use HTTP"));
        let endpoint = ServerEndpoint::new(
            &fake.base(),
            Arc::new(StaticCredential::new(TOKEN.as_bytes())),
            1,
        )
        .unwrap();
        let backend = ServerBackend::new(
            ServerUploader::new(endpoint, store.nix.clone(), 3, store.queue()),
            ProtocolMode::V2,
            1,
        );
        let auth = CallAuth::default();
        let mut known = Known::default();
        let mut group = Group::new(&backend, &auth, &mut known, facts).unwrap();
        group.set(&seed, Stage::Next(Work::Declare));
        group.set(&bridge, Stage::Confirmed);
        group.set(&root, Stage::Held);
        assert!(group.advance());
        assert!(matches!(
            group.states[&seed].stage,
            Stage::Work(Work::Declare)
        ));
        assert!(matches!(group.states[&bridge].stage, Stage::Confirmed));
        assert!(matches!(
            group.states[&root].stage,
            Stage::Work(Work::Ready)
        ));
    }
}
