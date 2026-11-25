use crate::group::{Closure, Group, GroupJobs};
use crate::known::Known;
use crate::outcome::{failure, waiting};
use crate::transfer::CallAuth;
use std::{sync::Arc, time::Duration};
use stockhouse_auth::{Clock, SystemClock};
use stockhouse_ingest::{RetryKind, UploadJob, UploadOutcome, Uploader};
use stockhouse_proto::StorePath;
use stockhouse_upload::{Negotiator, ServerUploader, parse_closure, query, upload_order};
use stockhouse_wire_v2::ProtocolMode;

/// Chooses one upload protocol for each worker batch.
pub struct ServerBackend {
    pub(crate) v1: ServerUploader,
    negotiator: Negotiator,
    pub(crate) max_inflight: usize,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) sleep: Arc<dyn Fn(Duration) + Send + Sync>,
}

impl ServerBackend {
    pub fn new(v1: ServerUploader, protocol: ProtocolMode, max_inflight: usize) -> Self {
        Self {
            v1,
            negotiator: Negotiator::new(protocol),
            max_inflight: max_inflight.clamp(1, 16),
            clock: Arc::new(SystemClock),
            sleep: Arc::new(std::thread::sleep),
        }
    }

    /// Injects time and the resend wait for component tests.
    #[doc(hidden)]
    pub fn with_time(
        mut self,
        clock: Arc<dyn Clock>,
        sleep: Arc<dyn Fn(Duration) + Send + Sync>,
    ) -> Self {
        self.clock = clock;
        self.sleep = sleep;
        self
    }
}

impl Uploader for ServerBackend {
    fn upload(&self, job: &UploadJob) -> UploadOutcome {
        self.upload_batch(std::slice::from_ref(job)).remove(0)
    }

    fn upload_batch(&self, jobs: &[UploadJob]) -> Vec<UploadOutcome> {
        if jobs.is_empty() {
            return Vec::new();
        }
        match self
            .negotiator
            .version(self.v1.endpoint(), self.clock.now())
        {
            Ok(1) => self.v1.upload_batch(jobs),
            Ok(2) => self.v2(jobs),
            Ok(_) => unreachable!("the negotiator only selects implemented versions"),
            Err(error) => {
                let outcome = if error.is_credential_error() {
                    UploadOutcome::Deferred(error.to_string())
                } else {
                    UploadOutcome::Retry(RetryKind::Transient, error.to_string())
                };
                vec![outcome; jobs.len()]
            }
        }
    }
}

impl ServerBackend {
    fn closure(&self, root: &StorePath) -> Result<Closure, UploadOutcome> {
        if !self.v1.queue().store_path_exists(root) {
            return Err(UploadOutcome::Rejected("not in the local store".into()));
        }
        let bytes = query(self.v1.nix(), root, self.v1.endpoint().timeout()).map_err(failure)?;
        let closure = parse_closure(&bytes).map_err(failure)?;
        if closure.len() > self.v1.max_closure_paths() {
            return Err(UploadOutcome::Rejected("closure too large".into()));
        }
        upload_order(&closure, root).map_err(failure)?;
        Ok(closure)
    }

    fn v2(&self, jobs: &[UploadJob]) -> Vec<UploadOutcome> {
        let auth = CallAuth::default();
        let mut known = Known::default();
        let mut outcomes = vec![None; jobs.len()];
        let mut plan = GroupJobs::default();
        for (index, job) in jobs.iter().enumerate() {
            let root = &job.store_path;
            let closure = match self.closure(root) {
                Ok(closure) => closure,
                Err(error) => {
                    outcomes[index] = Some(error);
                    continue;
                }
            };
            if !plan.fits(&closure, self.v1.max_closure_paths()) {
                self.run_group(std::mem::take(&mut plan), &auth, &mut known, &mut outcomes);
                if auth.withdrawn() {
                    break;
                }
                auth.next_group();
            }
            plan.add(index, root, closure);
        }
        if !plan.is_empty() && !auth.withdrawn() {
            self.run_group(plan, &auth, &mut known, &mut outcomes);
        }
        if auth.withdrawn() {
            self.negotiator.forget();
        }
        self.finish(jobs, outcomes)
    }

    fn run_group(
        &self,
        plan: GroupJobs,
        auth: &CallAuth,
        known: &mut Known,
        outcomes: &mut [Option<UploadOutcome>],
    ) {
        let GroupJobs { facts, jobs } = plan;
        match Group::new(self, auth, known, facts) {
            Ok(mut group) => {
                group.run();
                for job in jobs {
                    outcomes[job.index] = Some(group.outcome(&job));
                }
            }
            Err(error) => {
                for job in jobs {
                    outcomes[job.index] = Some(error.clone());
                }
            }
        }
    }

    fn finish(
        &self,
        jobs: &[UploadJob],
        outcomes: Vec<Option<UploadOutcome>>,
    ) -> Vec<UploadOutcome> {
        jobs.iter()
            .zip(outcomes)
            .map(|(job, outcome)| match outcome {
                Some(UploadOutcome::Uploaded) if !self.v1.queue().is_uploaded(&job.store_path) => {
                    waiting(&job.store_path)
                }
                Some(outcome) => outcome,
                None => crate::outcome::withdrawn(&job.store_path),
            })
            .collect()
    }
}
