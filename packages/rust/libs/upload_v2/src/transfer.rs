use crate::dispatch::ServerBackend;
use crate::group::{Group, Stage, Work};
use reqwest::{Method, blocking::Body, header::CONTENT_TYPE};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Arc, Mutex, mpsc};
use stockhouse_credentials::Secret;
use stockhouse_ingest::{RetryKind, UploadOutcome};
use stockhouse_proto::{ServedIntegrity, StorePath, UploadId};
use stockhouse_upload::{
    ClosureEntry, Dump, DumpFailure, RequestFailure, ServerEndpoint, nar_timeout,
};
use stockhouse_wire::UploadRefusal;
use stockhouse_wire_v2::{NarReply, UploadState, nar_path};

#[derive(Default)]
struct Stop {
    withdrawn: bool,
    credential_error: Option<String>,
}

#[derive(Default)]
pub(crate) struct CallAuth {
    stop: Mutex<Stop>,
    invalidations: Mutex<Vec<(Secret, bool)>>,
}

impl CallAuth {
    pub(crate) fn stopped(&self) -> bool {
        let stop = self.stop.lock().expect("stop lock");
        stop.withdrawn || stop.credential_error.is_some()
    }

    pub(crate) fn withdrawn(&self) -> bool {
        self.stop.lock().expect("stop lock").withdrawn
    }

    pub(crate) fn withdraw(&self) {
        self.stop.lock().expect("stop lock").withdrawn = true;
    }

    pub(crate) fn credential_error(&self) -> Option<String> {
        self.stop
            .lock()
            .expect("stop lock")
            .credential_error
            .clone()
    }

    pub(crate) fn stop_credential(&self, error: String) {
        self.stop
            .lock()
            .expect("stop lock")
            .credential_error
            .get_or_insert(error);
    }

    pub(crate) fn next_group(&self) {
        self.stop.lock().expect("stop lock").credential_error = None;
    }

    pub(crate) fn read(&self, endpoint: &ServerEndpoint) -> Option<Secret> {
        if self.stopped() {
            return None;
        }
        match endpoint.credential() {
            Ok(secret) => Some(secret),
            Err(error) => {
                self.stop_credential(format!("credential unavailable: {error}"));
                None
            }
        }
    }

    pub(crate) fn invalidate(&self, endpoint: &ServerEndpoint, sent: &Secret) -> bool {
        let mut cache = self.invalidations.lock().expect("invalidation lock");
        if let Some((_, result)) = cache
            .iter()
            .find(|(previous, _)| previous.expose() == sent.expose())
        {
            return *result;
        }
        let result = endpoint.invalidate(sent);
        cache.push((Secret::new(sent.expose().to_vec()), result));
        result
    }
}

enum TransferResult {
    Reply(NarReply),
    Ended(UploadOutcome),
    Stopped,
}

fn retry(path: &StorePath, kind: RetryKind, why: &str) -> TransferResult {
    TransferResult::Ended(UploadOutcome::Retry(
        kind,
        format!("{path}: PUT v2 nar: {why}"),
    ))
}

fn dump_failure(path: &StorePath, failure: DumpFailure) -> TransferResult {
    match failure {
        DumpFailure::Mismatch => TransferResult::Ended(UploadOutcome::Rejected(format!(
            "{path}: local NAR does not match its NarHash"
        ))),
        DumpFailure::Failed => retry(path, RetryKind::Transient, "NAR dump failed"),
    }
}

fn transport_failure(path: &StorePath, fully_sent: bool, error: RequestFailure) -> TransferResult {
    let kind = if fully_sent {
        RetryKind::Uncertain
    } else {
        RetryKind::Transient
    };
    retry(path, kind, &error.to_string())
}

fn send_once(
    backend: &ServerBackend,
    auth: &CallAuth,
    id: UploadId,
    info: &ClosureEntry,
    secret: &Secret,
) -> TransferResult {
    let endpoint = backend.v1.endpoint();
    let timeout = nar_timeout(endpoint.timeout(), info.nar_size);
    let request = match endpoint.request(Method::PUT, &nar_path(&id), secret, timeout) {
        Ok(request) => request,
        Err(error) => {
            auth.stop_credential(format!("credential unavailable: {error}"));
            return TransferResult::Stopped;
        }
    };
    let expected = ServedIntegrity {
        size: info.nar_size,
        hash: info.nar_hash,
    };
    let (dump, reader) =
        match Dump::start(backend.v1.nix(), &info.path, expected, endpoint.timeout()) {
            Ok(dump) => dump,
            Err(error) => return dump_failure(&info.path, error),
        };
    if auth.stopped() {
        dump.reap();
        return TransferResult::Stopped;
    }
    let reply = endpoint.nar_reply(
        request
            .header(CONTENT_TYPE, "application/x-nix-nar")
            .body(Body::sized(reader, info.nar_size)),
    );
    if let Ok(NarReply::NotSupported) = reply {
        auth.withdraw();
    }
    let (failure, sent) = (dump.failure(), dump.fully_sent());
    dump.reap();
    if let Some(error) = failure {
        return dump_failure(&info.path, error);
    }
    match reply {
        Ok(reply) => TransferResult::Reply(reply),
        Err(error) => transport_failure(&info.path, sent, error),
    }
}

fn send(
    backend: &ServerBackend,
    auth: &CallAuth,
    id: UploadId,
    info: &ClosureEntry,
) -> TransferResult {
    let endpoint = backend.v1.endpoint();
    let Some(secret) = auth.read(endpoint) else {
        return TransferResult::Stopped;
    };
    let result = send_once(backend, auth, id, info, &secret);
    if !matches!(result, TransferResult::Reply(NarReply::Unauthorized)) {
        return result;
    }
    if !auth.stopped() && auth.invalidate(endpoint, &secret) {
        if let Some(fresh) = auth.read(endpoint) {
            return send_once(backend, auth, id, info, &fresh);
        }
        return TransferResult::Stopped;
    }
    result
}

impl Group<'_> {
    pub(crate) fn transfer(&mut self) {
        let paths = self.paths(Work::Transfer);
        let jobs = paths
            .iter()
            .map(|path| (self.entry(path).id, &self.facts[path]));
        let results = parallel(self.backend.max_inflight, jobs, self.auth, |(id, info)| {
            send(self.backend, self.auth, id, info)
        });
        for (path, result) in results {
            match result {
                TransferResult::Reply(reply) => self.transfer_reply(&path, reply),
                TransferResult::Ended(outcome) => self.set(&path, Stage::Ended(outcome)),
                TransferResult::Stopped => {}
            }
        }
        self.complete_confirmed();
    }

    fn transfer_reply(&mut self, path: &StorePath, reply: NarReply) {
        let stage = match reply {
            NarReply::State(UploadState::Verified) => Stage::Work(Work::Ready),
            NarReply::State(UploadState::Present | UploadState::Published) => Stage::Confirmed,
            NarReply::State(UploadState::Open) => unreachable!("NAR may not return open"),
            NarReply::NotSupported => return,
            NarReply::Refused(UploadRefusal::UnknownUpload) => {
                if !self.stopped() {
                    self.unknown_nar(path);
                }
                return;
            }
            NarReply::Refused(code) => return self.refuse(path, "PUT v2 nar", &code),
            NarReply::Unauthorized => Stage::Ended(UploadOutcome::Retry(
                RetryKind::Transient,
                "unauthorized".into(),
            )),
            NarReply::Busy { .. } | NarReply::Unavailable { .. } => {
                let why = if matches!(reply, NarReply::Busy { .. }) {
                    "busy"
                } else {
                    "unavailable"
                };
                Stage::Ended(UploadOutcome::Retry(
                    RetryKind::Transient,
                    format!("{path}: PUT v2 nar: {why}"),
                ))
            }
        };
        self.set(path, stage);
    }
}

fn parallel<'a>(
    limit: usize,
    mut jobs: impl ExactSizeIterator<Item = (UploadId, &'a ClosureEntry)>,
    auth: &CallAuth,
    transfer: impl Fn((UploadId, &'a ClosureEntry)) -> TransferResult + Sync,
) -> Vec<(StorePath, TransferResult)> {
    let workers = limit.min(jobs.len());
    std::thread::scope(|scope| {
        let (work, inbox) = mpsc::sync_channel(workers);
        let inbox = Arc::new(Mutex::new(inbox));
        let (outbox, results) = mpsc::channel();
        for _ in 0..workers {
            let (inbox, outbox, transfer) = (inbox.clone(), outbox.clone(), &transfer);
            scope.spawn(move || {
                loop {
                    let job: Result<(_, &ClosureEntry), _> =
                        inbox.lock().expect("dispatch lock").recv();
                    let Ok(job) = job else { break };
                    let path = job.1.path.clone();
                    let result = catch_unwind(AssertUnwindSafe(|| transfer(job)));
                    if outbox.send((path, result)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(outbox);
        let mut count = 0;
        for job in jobs.by_ref().take(workers) {
            work.send(job).expect("workers take initial transfers");
            count += 1;
        }
        let mut outcomes = Vec::new();
        while count > 0 {
            let (path, result) = results
                .recv()
                .expect("workers return a result for each transfer");
            match result {
                Ok(result) => outcomes.push((path, result)),
                Err(panic) => {
                    drop(work);
                    resume_unwind(panic);
                }
            }
            count -= 1;
            if !auth.stopped()
                && let Some(job) = jobs.next()
            {
                work.send(job).expect("workers take further transfers");
                count += 1;
            }
        }
        drop(work);
        outcomes
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use stockhouse_proto::{Sha256, nix32};

    #[cfg_attr(any(), bloomery("CLIENT-INGEST-BATCH-012"))]
    #[test]
    fn dispatches_in_the_given_upload_order_even_when_completion_varies() {
        let entries: Vec<_> = (0..5).map(fixture).collect();
        let order: Vec<_> = entries.iter().map(|entry| entry.path.clone()).collect();
        let mut dispatched = Vec::new();
        let jobs = entries
            .iter()
            .map(|entry| (UploadId::generate(), entry))
            .inspect(|(_, entry)| dispatched.push(entry.path.clone()));
        let results = parallel(3, jobs, &CallAuth::default(), |(_, entry)| {
            if entry.path.name().ends_with('0') {
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
            TransferResult::Reply(NarReply::State(UploadState::Verified))
        });
        assert_eq!(dispatched, order);
        assert_eq!(results.len(), order.len());
    }

    #[test]
    fn a_panicking_worker_unwinds_without_stranding_the_scheduler() {
        let entry = fixture(0);
        let jobs = [(UploadId::generate(), &entry)];
        let result = catch_unwind(|| {
            parallel(1, jobs.into_iter(), &CallAuth::default(), |_| {
                panic!("test panic")
            });
        });
        assert_eq!(
            result.unwrap_err().downcast_ref::<&str>(),
            Some(&"test panic")
        );
    }

    fn fixture(i: u8) -> ClosureEntry {
        let hash = nix32::encode(&Sha256::digest(&[i]).bytes()[..20]);
        ClosureEntry {
            path: StorePath::parse(&format!("/nix/store/{hash}-entry-{i}")).unwrap(),
            nar_hash: Sha256::digest(&[]),
            nar_size: 1,
            references: Vec::new(),
            deriver: None,
            ca: None,
        }
    }
}
