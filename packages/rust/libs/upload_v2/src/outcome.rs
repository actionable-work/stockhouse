use crate::group::{Group, Job, Stage};
use std::fmt::Display;
use stockhouse_ingest::{RetryKind, UploadOutcome};
use stockhouse_proto::StorePath;

pub(crate) fn failure(error: impl Display) -> UploadOutcome {
    let mut text = error.to_string();
    let mut end = text.len().min(512);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    UploadOutcome::Retry(RetryKind::Transient, text)
}

pub(crate) fn waiting(path: &StorePath) -> UploadOutcome {
    UploadOutcome::Retry(
        RetryKind::Transient,
        format!("{path}: waiting for a reference"),
    )
}

pub(crate) fn withdrawn(path: &StorePath) -> UploadOutcome {
    UploadOutcome::Retry(
        RetryKind::Transient,
        format!("{path}: protocol v2 not available"),
    )
}

impl Group<'_> {
    pub(crate) fn outcome(&self, job: &Job) -> UploadOutcome {
        let stages = self
            .order
            .iter()
            .filter(|path| job.paths.contains(*path))
            .map(|path| &self.states[path].stage);
        if stages.clone().all(Stage::complete) {
            return UploadOutcome::Uploaded;
        }
        let mut retries = [None, None, None];
        for stage in stages {
            if let Stage::Ended(outcome) = stage {
                let rank = match outcome {
                    UploadOutcome::Rejected(_) => return outcome.clone(),
                    UploadOutcome::Retry(RetryKind::QuotaExceeded, _) => 0,
                    UploadOutcome::Retry(RetryKind::Uncertain, _) => 1,
                    UploadOutcome::Retry(RetryKind::Transient, _) => 2,
                    _ => continue,
                };
                retries[rank].get_or_insert(outcome);
            }
        }
        if let Some(error) = self.auth.credential_error() {
            return UploadOutcome::Deferred(error);
        }
        if self.auth.withdrawn() {
            return withdrawn(&job.root);
        }
        retries
            .into_iter()
            .flatten()
            .next()
            .cloned()
            .or_else(|| self.local_error.clone())
            .unwrap_or_else(|| waiting(&job.root))
    }
}
