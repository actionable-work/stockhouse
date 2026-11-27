use crate::group::{Group, Stage, Work};
use crate::outcome::failure;
use std::collections::{BTreeSet, HashSet};
use stockhouse_ingest::{RetryKind, UploadOutcome};
use stockhouse_proto::StorePath;
use stockhouse_upload::{BatchFailure, BatchRequest};
use stockhouse_wire::UploadRefusal;
use stockhouse_wire_v2::{
    BatchKind, BatchRefusal, BatchReply, DeclareBatch, DeclarePushError, EntryKey, EntryResult,
    IdBatch, MAX_BATCH_ENTRIES, UploadResult, UploadState,
};

fn route(kind: BatchKind) -> &'static str {
    match kind {
        BatchKind::Status => "POST v2 status",
        BatchKind::Declare => "POST v2 declare",
        BatchKind::Publish => "POST v2 publish",
        BatchKind::Leases => unreachable!("upload paths do not renew leases"),
    }
}

impl Group<'_> {
    pub(crate) fn run_round(&mut self) -> bool {
        for paths in self.paths(Work::Status).chunks(MAX_BATCH_ENTRIES) {
            self.send_ids(BatchKind::Status, paths);
        }
        self.declare();
        if !self.stopped() {
            self.transfer();
        }
        while !self.stopped() {
            let paths = self.publishable();
            if paths.is_empty() {
                break;
            }
            self.send_ids(BatchKind::Publish, &paths);
        }
        !self.stopped()
    }

    fn declare(&mut self) {
        let candidates = self.paths(Work::Declare);
        let mut batch = DeclareBatch::new();
        let mut paths = Vec::new();
        for path in candidates {
            if self.stopped() {
                return;
            }
            let Ok(declaration) = self.facts[&path].declaration() else {
                self.set(
                    &path,
                    Stage::Ended(UploadOutcome::Rejected(format!(
                        "{path}: invalid declaration"
                    ))),
                );
                continue;
            };
            let id = self.entry(&path).id;
            match batch.push(id, declaration.clone()) {
                Ok(()) => {}
                Err(DeclarePushError::BatchFull) if !paths.is_empty() => {
                    self.flush_declare(&mut batch, &mut paths);
                    if self.stopped() {
                        return;
                    }
                    batch
                        .push(id, declaration)
                        .expect("valid declaration fits an empty batch");
                }
                Err(error) => {
                    self.set(&path, Stage::Ended(failure(error)));
                    continue;
                }
            }
            paths.push(path);
        }
        self.flush_declare(&mut batch, &mut paths);
    }

    fn flush_declare(&mut self, batch: &mut DeclareBatch, paths: &mut Vec<StorePath>) {
        if !paths.is_empty() && !self.stopped() {
            let body = std::mem::take(batch)
                .render()
                .expect("validated nonempty batch");
            self.send(BatchKind::Declare, paths, body);
            paths.clear();
        }
    }

    fn send_ids(&mut self, kind: BatchKind, paths: &[StorePath]) {
        if self.stopped() {
            return;
        }
        let ids = paths.iter().map(|path| self.entry(path).id).collect();
        match IdBatch::new(ids) {
            Ok(batch) => self.send(kind, paths, batch.render()),
            Err(error) => self.end_batch(paths, failure(error)),
        }
    }

    fn send(&mut self, kind: BatchKind, paths: &[StorePath], body: Vec<u8>) {
        let endpoint = self.backend.v1.endpoint();
        let Some(mut secret) = self.auth.read(endpoint) else {
            return;
        };
        let request = BatchRequest {
            kind,
            body,
            keys: paths
                .iter()
                .map(|path| EntryKey::Upload(self.entry(path).id))
                .collect(),
        };
        match request.send_with(
            endpoint,
            &mut secret,
            self.backend.sleep.as_ref(),
            &|sent| self.auth.invalidate(endpoint, sent),
        ) {
            Ok(BatchReply::Results(results)) => self.results(kind, paths, results),
            Ok(BatchReply::NotSupported) => self.auth.withdraw(),
            Ok(BatchReply::Refused(refusal)) => self.refused_batch(kind, paths, refusal),
            Ok(_) => unreachable!("batch sender handles retries and unauthorized"),
            Err(error) => self.failed_batch(kind, paths, error),
        }
    }

    fn refused_batch(&mut self, kind: BatchKind, paths: &[StorePath], refusal: BatchRefusal) {
        for path in paths {
            let outcome = match refusal {
                BatchRefusal::BadRequest => {
                    UploadOutcome::Rejected(format!("{path}: {}: bad_request", route(kind)))
                }
                BatchRefusal::TooLarge => UploadOutcome::Retry(
                    RetryKind::Transient,
                    format!("{path}: {}: protocol violation", route(kind)),
                ),
            };
            self.set(path, Stage::Ended(outcome));
        }
    }

    fn failed_batch(&mut self, kind: BatchKind, paths: &[StorePath], error: BatchFailure) {
        if error.is_credential_error() {
            self.auth.stop_credential(error.to_string());
        } else if error == BatchFailure::Unauthorized {
            self.end_batch(
                paths,
                UploadOutcome::Retry(RetryKind::Transient, "unauthorized".into()),
            );
        } else {
            let kind_of_retry = if kind == BatchKind::Publish && error.retryable() {
                RetryKind::Uncertain
            } else {
                RetryKind::Transient
            };
            for path in paths {
                self.set(
                    path,
                    Stage::Ended(UploadOutcome::Retry(
                        kind_of_retry,
                        format!("{path}: {}: {error}", route(kind)),
                    )),
                );
            }
        }
    }

    fn end_batch(&mut self, paths: &[StorePath], outcome: UploadOutcome) {
        for path in paths {
            self.set(path, Stage::Ended(outcome.clone()));
        }
    }

    fn results(&mut self, kind: BatchKind, paths: &[StorePath], results: Vec<EntryResult>) {
        let mut missing = BTreeSet::new();
        let confirmed: HashSet<_> = paths
            .iter()
            .zip(&results)
            .filter_map(|(path, result)| {
                matches!(
                    result,
                    EntryResult::Upload(UploadResult::State(
                        UploadState::Present | UploadState::Published
                    ))
                )
                .then_some(path)
            })
            .collect();
        for (path, result) in paths.iter().zip(results) {
            match result {
                EntryResult::Upload(UploadResult::Refused(UploadRefusal::MissingReferences(
                    names,
                ))) => {
                    self.publish_missing(path, &names, &confirmed, &mut missing);
                }
                EntryResult::Upload(result) => self.apply_upload_result(path, kind, result),
                _ => unreachable!("upload batch only accepts upload results"),
            }
        }
        self.repair_missing(missing);
        if self.local_error.is_none() {
            self.complete_confirmed();
        }
    }

    fn apply_upload_result(&mut self, path: &StorePath, kind: BatchKind, result: UploadResult) {
        match result {
            UploadResult::State(UploadState::Open) if kind == BatchKind::Publish => {
                if self.allow_rework(path) {
                    self.set(path, Stage::Next(Work::Transfer));
                }
            }
            UploadResult::State(UploadState::Open) => self.mark(path, Work::Transfer),
            UploadResult::State(UploadState::Verified) => self.mark(path, Work::Ready),
            UploadResult::State(UploadState::Present | UploadState::Published) => {
                self.set(path, Stage::Confirmed)
            }
            UploadResult::Refused(UploadRefusal::UnknownUpload) => self.unknown(path, kind),
            UploadResult::Refused(code) => self.refuse(path, route(kind), &code),
        }
    }

    fn mark(&mut self, path: &StorePath, next: Work) {
        let entry = self.entry(path);
        let result = if entry.nar_sent {
            Ok(entry)
        } else {
            self.queue().mark_nar_sent(path, entry)
        };
        self.update_wire(path, result, Stage::Work(next));
    }

    fn unknown(&mut self, path: &StorePath, kind: BatchKind) {
        if kind == BatchKind::Status {
            let result = self.queue().clear_nar_sent(path, self.entry(path));
            self.update_wire(path, result, Stage::Work(Work::Declare));
        } else if self.allow_rework(path) {
            let result = if kind == BatchKind::Declare {
                self.queue().renew_identity(path)
            } else {
                self.queue().clear_nar_sent(path, self.entry(path))
            };
            self.update_wire(path, result, Stage::Next(Work::Declare));
        }
    }

    pub(crate) fn unknown_nar(&mut self, path: &StorePath) {
        if self.allow_rework(path) {
            let result = self.queue().clear_nar_sent(path, self.entry(path));
            self.update_wire(path, result, Stage::Next(Work::Declare));
        }
    }

    pub(crate) fn refuse(&mut self, path: &StorePath, request: &str, code: &UploadRefusal) {
        let why = format!("{path}: {request}: {}", code.code());
        let outcome = if *code == UploadRefusal::QuotaExceeded {
            UploadOutcome::Retry(RetryKind::QuotaExceeded, why)
        } else {
            UploadOutcome::Rejected(why)
        };
        self.set(path, Stage::Ended(outcome));
    }

    fn publishable(&self) -> Vec<StorePath> {
        let mut paths = Vec::new();
        let mut selected = HashSet::new();
        for path in &self.order {
            if !matches!(self.states[path].stage, Stage::Work(Work::Ready)) {
                continue;
            }
            if self.facts[path].references.iter().all(|dep| {
                dep == path || self.states[dep].stage.complete() || selected.contains(dep)
            }) {
                paths.push(path.clone());
                selected.insert(path.clone());
                if paths.len() == MAX_BATCH_ENTRIES {
                    break;
                }
            }
        }
        paths
    }

    fn publish_missing(
        &mut self,
        path: &StorePath,
        names: &[StorePath],
        confirmed: &HashSet<&StorePath>,
        missing: &mut BTreeSet<StorePath>,
    ) {
        let declared: HashSet<&StorePath> = self.facts[path].references.iter().collect();
        if names
            .iter()
            .any(|dep| dep == path || !declared.contains(dep) || confirmed.contains(dep))
        {
            self.set(
                path,
                Stage::Ended(UploadOutcome::Retry(
                    RetryKind::Transient,
                    "protocol violation".into(),
                )),
            );
            return;
        }
        let eligible: Vec<_> = names
            .iter()
            .filter(|dep| self.states[*dep].stage.complete())
            .cloned()
            .collect();
        if eligible.is_empty() {
            self.set(path, Stage::Held);
        } else {
            missing.extend(eligible);
            if self.allow_rework(path) {
                self.set(path, Stage::Next(Work::Ready));
            }
        }
    }

    fn repair_missing(&mut self, missing: BTreeSet<StorePath>) {
        for path in missing {
            if let Err(error) = self.invalidate(&path) {
                self.local_error = Some(failure(error));
                return;
            }
            if self.allow_rework(&path) {
                let result = self.queue().upload_state(&path);
                let work = if result.as_ref().is_ok_and(|entry| entry.nar_sent) {
                    Work::Status
                } else {
                    Work::Declare
                };
                self.update_wire(&path, result, Stage::Next(work));
            }
        }
    }
}
