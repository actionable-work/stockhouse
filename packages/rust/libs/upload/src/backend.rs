//! The `server` backend: protocol v1 closure upload behind `Uploader`.

use crate::closure::{ClosureEntry, ClosureError, parse_closure, query, upload_order};
use crate::endpoint::{Exchange, InvalidCredential, ServerEndpoint, nar_timeout};
use crate::nar::{Dump, DumpFailure};
use reqwest::Method;
use reqwest::blocking::Body;
use reqwest::header::CONTENT_TYPE;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use stockhouse_auth::{Clock, SystemClock};
use stockhouse_credentials::{CredentialError, Secret};
use stockhouse_ingest::{
    Queue, QueueError, RetryKind, UploadEntry, UploadJob, UploadOutcome, Uploader,
};
use stockhouse_proto::{ServedIntegrity, StorePath, UploadId};
use stockhouse_wire::{
    MAX_BODY, RequestKind, UploadRefusal, UploadReply, UploadState, nar_path, parse_reply,
    upload_path,
};

/// Declare, NAR and status requests allowed per path per job.
pub const PATH_BUDGET: u8 = 8;
const MESSAGE_LIMIT: usize = 512;

/// Authenticated upload of each queued path's closure to the private tier.
pub struct ServerUploader {
    endpoint: ServerEndpoint,
    nix: PathBuf,
    max_closure_paths: usize,
    queue: Queue,
    budget: u8,
}

impl ServerUploader {
    /// A backend over a validated endpoint and the opened queue.
    pub fn new(
        endpoint: ServerEndpoint,
        nix: PathBuf,
        max_closure_paths: usize,
        queue: Queue,
    ) -> Self {
        Self {
            endpoint,
            nix,
            max_closure_paths,
            queue,
            budget: PATH_BUDGET,
        }
    }

    pub fn endpoint(&self) -> &ServerEndpoint {
        &self.endpoint
    }

    pub fn nix(&self) -> &std::path::Path {
        &self.nix
    }

    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    pub fn max_closure_paths(&self) -> usize {
        self.max_closure_paths
    }

    /// Lowers the per-path request budget (tests only).
    #[doc(hidden)]
    pub fn with_path_budget(mut self, budget: u8) -> Self {
        self.budget = budget;
        self
    }
}

type Done = UploadOutcome;

/// The request named in outcome messages.
fn label(kind: RequestKind) -> &'static str {
    match kind {
        RequestKind::Declare => "POST declare",
        RequestKind::Nar => "PUT nar",
        RequestKind::Status => "GET status",
    }
}

fn invalid_credential(error: InvalidCredential) -> Done {
    UploadOutcome::Deferred(format!("credential unavailable: {error}"))
}

/// Truncates to at most `limit` bytes on a character boundary.
fn bounded(mut message: String, limit: usize) -> String {
    let mut end = message.len().min(limit);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message
}

fn transient(path: &StorePath, why: &str) -> Done {
    UploadOutcome::Retry(RetryKind::Transient, format!("{path}: {why}"))
}

fn uncertain(path: &StorePath, why: &str) -> Done {
    UploadOutcome::Retry(RetryKind::Uncertain, format!("{path}: {why}"))
}

fn deferred(error: CredentialError) -> Done {
    UploadOutcome::Deferred(format!("credential unavailable: {error}"))
}

fn queue_failed(error: QueueError) -> Done {
    let message = bounded(format!("queue: {error}"), MESSAGE_LIMIT);
    UploadOutcome::Retry(RetryKind::Transient, message)
}

fn closure_failed(error: ClosureError) -> Done {
    UploadOutcome::Retry(
        RetryKind::Transient,
        bounded(error.to_string(), MESSAGE_LIMIT),
    )
}

impl Uploader for ServerUploader {
    fn upload(&self, job: &UploadJob) -> UploadOutcome {
        match self.run(job) {
            Ok(outcome) | Err(outcome) => outcome,
        }
    }
}

impl ServerUploader {
    fn run(&self, job: &UploadJob) -> Result<Done, Done> {
        let root = &job.store_path;
        if !self.queue.store_path_exists(root) {
            return Ok(UploadOutcome::Rejected("not in the local store".to_owned()));
        }
        let secret = self.endpoint.credential().map_err(deferred)?;
        let idle = self.endpoint.timeout();
        let bytes = query(&self.nix, root, idle).map_err(closure_failed)?;
        let closure = parse_closure(&bytes).map_err(closure_failed)?;
        if closure.len() > self.max_closure_paths {
            return Ok(UploadOutcome::Rejected("closure too large".to_owned()));
        }
        let order = upload_order(&closure, root).map_err(closure_failed)?;
        let rank = order
            .iter()
            .enumerate()
            .map(|(i, p)| (p.clone(), i))
            .collect();
        let mut run = Job {
            up: self,
            root,
            closure,
            rank,
            secret,
            states: HashMap::new(),
            forced: HashSet::new(),
        };
        run.drain(order.into())
    }
}

#[derive(Default)]
struct PathState {
    requests: u8,
    missing_round: bool,
    redeclared: bool,
    renewed: bool,
}

enum Step {
    Declare,
    SendNar,
    Reconcile,
}

enum Flow {
    Next(Step),
    Complete,
    Push(Vec<StorePath>),
    Unauthorized(RequestKind),
}

enum PathEnd {
    Complete(UploadId),
    Push(Vec<StorePath>),
}

struct Job<'a> {
    up: &'a ServerUploader,
    root: &'a StorePath,
    closure: BTreeMap<StorePath, ClosureEntry>,
    rank: HashMap<StorePath, usize>,
    secret: Secret,
    states: HashMap<StorePath, PathState>,
    forced: HashSet<StorePath>,
}

impl Job<'_> {
    fn drain(&mut self, mut work: VecDeque<StorePath>) -> Result<Done, Done> {
        while let Some(path) = work.pop_front() {
            let dependency = &path != self.root;
            if dependency && !self.forced.contains(&path) && self.up.queue.is_uploaded(&path) {
                continue;
            }
            match self.upload_path(&path)? {
                PathEnd::Complete(id) => self.complete(&path, id, dependency)?,
                PathEnd::Push(mut missing) => {
                    missing.sort_by_key(|m| self.rank.get(m).copied());
                    work.push_front(path);
                    for m in missing.into_iter().rev() {
                        self.forced.insert(m.clone());
                        work.push_front(m);
                    }
                }
            }
        }
        Ok(UploadOutcome::Uploaded)
    }

    fn complete(&mut self, path: &StorePath, id: UploadId, dependency: bool) -> Result<(), Done> {
        self.forced.remove(path);
        let queue = &self.up.queue;
        let done = if dependency {
            queue.complete_path(path, id, SystemClock.now())
        } else {
            queue.remove_entry(path)
        };
        done.map_err(queue_failed)
    }

    fn upload_path(&mut self, path: &StorePath) -> Result<PathEnd, Done> {
        let mut entry = self.up.queue.upload_state(path).map_err(queue_failed)?;
        let mut step = if entry.nar_sent {
            Step::Reconcile
        } else {
            Step::Declare
        };
        let mut retried = false;
        loop {
            if !retried {
                let state = self.states.entry(path.clone()).or_default();
                if state.requests >= self.up.budget {
                    return Err(transient(path, "upload step budget exhausted"));
                }
                state.requests += 1;
            }
            let flow = match step {
                Step::Declare => self.declare(path, &mut entry)?,
                Step::SendNar => self.send_nar(path, &mut entry)?,
                Step::Reconcile => self.reconcile(path, &mut entry)?,
            };
            match flow {
                Flow::Next(next) => (step, retried) = (next, false),
                Flow::Complete => return Ok(PathEnd::Complete(entry.id)),
                Flow::Push(missing) => return Ok(PathEnd::Push(missing)),
                Flow::Unauthorized(_) if !retried && self.up.endpoint.invalidate(&self.secret) => {
                    self.secret = self.up.endpoint.credential().map_err(deferred)?;
                    retried = true;
                }
                Flow::Unauthorized(kind) => {
                    return Err(transient(path, &format!("{}: unauthorized", label(kind))));
                }
            }
        }
    }

    fn state(&mut self, path: &StorePath) -> &mut PathState {
        self.states.entry(path.clone()).or_default()
    }

    fn declaration(&self, path: &StorePath) -> Result<Vec<u8>, Done> {
        self.closure[path]
            .declaration()
            .map(|d| d.render())
            .map_err(|_| UploadOutcome::Rejected(format!("{path}: invalid declaration")))
    }

    fn reply(
        &self,
        path: &StorePath,
        kind: RequestKind,
        exchange: Exchange,
        sent: bool,
    ) -> Result<UploadReply, Done> {
        let failed = |why: String| {
            let why = format!("{}: {why}", label(kind));
            if sent {
                uncertain(path, &why)
            } else {
                transient(path, &why)
            }
        };
        match exchange {
            Exchange::Failed => Err(failed("request failed".to_owned())),
            Exchange::Reply(status, None) => {
                Err(failed(format!("reply too large (status {status})")))
            }
            Exchange::Reply(status, Some(body)) => parse_reply(kind, status, &body)
                .map_err(|_| failed(format!("unexpected reply (status {status})"))),
        }
    }

    fn refusal(
        &self,
        path: &StorePath,
        kind: RequestKind,
        refusal: UploadRefusal,
        sent: bool,
    ) -> Result<Flow, Done> {
        let why = format!("{}: {}", label(kind), refusal.code());
        match refusal {
            UploadRefusal::Unauthorized => Ok(Flow::Unauthorized(kind)),
            UploadRefusal::QuotaExceeded => Err(UploadOutcome::Retry(
                RetryKind::QuotaExceeded,
                format!("{path}: {why}"),
            )),
            UploadRefusal::Unavailable if sent => Err(uncertain(path, &why)),
            UploadRefusal::Unavailable | UploadRefusal::UnknownUpload => Err(transient(path, &why)),
            UploadRefusal::MissingReferences(_) => unreachable!("handled by each step"),
            _ => Err(UploadOutcome::Rejected(format!("{path}: {why}"))),
        }
    }

    fn missing_round(
        &mut self,
        path: &StorePath,
        kind: RequestKind,
        missing: Vec<StorePath>,
    ) -> Result<Flow, Done> {
        let declared = &self.closure[path].references;
        if missing.iter().any(|m| m == path || !declared.contains(m)) {
            return Err(transient(
                path,
                &format!("{}: protocol violation", label(kind)),
            ));
        }
        if std::mem::replace(&mut self.state(path).missing_round, true) {
            let why = format!("{}: references still missing", label(kind));
            return Err(transient(path, &why));
        }
        for m in &missing {
            self.up.queue.forget_done(m).map_err(queue_failed)?;
        }
        Ok(Flow::Push(missing))
    }

    fn redeclare(
        &mut self,
        path: &StorePath,
        kind: RequestKind,
        entry: &mut UploadEntry,
    ) -> Result<Flow, Done> {
        if std::mem::replace(&mut self.state(path).redeclared, true) {
            return Err(transient(path, &format!("{}: unknown_upload", label(kind))));
        }
        *entry = self
            .up
            .queue
            .clear_nar_sent(path, *entry)
            .map_err(queue_failed)?;
        Ok(Flow::Next(Step::Declare))
    }

    fn declare(&mut self, path: &StorePath, entry: &mut UploadEntry) -> Result<Flow, Done> {
        let body = self.declaration(path)?;
        let endpoint = &self.up.endpoint;
        let request = endpoint
            .request(
                Method::POST,
                &upload_path(&entry.id),
                &self.secret,
                endpoint.timeout(),
            )
            .map_err(invalid_credential)?
            .header(CONTENT_TYPE, "application/json")
            .body(body);
        let exchange = endpoint.exchange(request, MAX_BODY);
        match self.reply(path, RequestKind::Declare, exchange, false)? {
            UploadReply::State(UploadState::Open) => {
                *entry = self
                    .up
                    .queue
                    .mark_nar_sent(path, *entry)
                    .map_err(queue_failed)?;
                Ok(Flow::Next(Step::SendNar))
            }
            UploadReply::State(_) => Ok(Flow::Complete),
            UploadReply::Refused(UploadRefusal::MissingReferences(m)) => {
                self.missing_round(path, RequestKind::Declare, m)
            }
            UploadReply::Refused(UploadRefusal::UnknownUpload) => {
                if std::mem::replace(&mut self.state(path).renewed, true) {
                    return Err(transient(path, "POST declare: unknown_upload"));
                }
                *entry = self.up.queue.renew_identity(path).map_err(queue_failed)?;
                Ok(Flow::Next(Step::Declare))
            }
            UploadReply::Refused(refusal) => {
                self.refusal(path, RequestKind::Declare, refusal, false)
            }
        }
    }

    fn send_nar(&mut self, path: &StorePath, entry: &mut UploadEntry) -> Result<Flow, Done> {
        self.secret = self.up.endpoint.credential().map_err(deferred)?;
        let info = &self.closure[path];
        let expected = ServedIntegrity {
            size: info.nar_size,
            hash: info.nar_hash,
        };
        let idle = self.up.endpoint.timeout();
        let (dump, reader) = Dump::start(&self.up.nix, path, expected, idle)
            .map_err(|_| transient(path, "NAR dump failed"))?;
        let endpoint = &self.up.endpoint;
        let timeout = nar_timeout(endpoint.timeout(), info.nar_size);
        let request = endpoint
            .request(Method::PUT, &nar_path(&entry.id), &self.secret, timeout)
            .map_err(|e| {
                dump.reap();
                invalid_credential(e)
            })?
            .header(CONTENT_TYPE, "application/x-nix-nar")
            .body(Body::sized(reader, info.nar_size));
        let exchange = endpoint.exchange(request, MAX_BODY);
        let (sent, failure) = (dump.fully_sent(), dump.failure());
        dump.reap();
        match failure {
            Some(DumpFailure::Mismatch) => {
                let why = format!("{path}: local NAR does not match its NarHash");
                return Err(UploadOutcome::Rejected(why));
            }
            Some(DumpFailure::Failed) => return Err(transient(path, "NAR dump failed")),
            None => {}
        }
        match self.reply(path, RequestKind::Nar, exchange, sent)? {
            UploadReply::State(_) => Ok(Flow::Complete),
            UploadReply::Refused(UploadRefusal::MissingReferences(m)) => {
                *entry = self
                    .up
                    .queue
                    .clear_nar_sent(path, *entry)
                    .map_err(queue_failed)?;
                self.missing_round(path, RequestKind::Nar, m)
            }
            UploadReply::Refused(UploadRefusal::UnknownUpload) => {
                self.redeclare(path, RequestKind::Nar, entry)
            }
            UploadReply::Refused(refusal) => self.refusal(path, RequestKind::Nar, refusal, sent),
        }
    }

    fn reconcile(&mut self, path: &StorePath, entry: &mut UploadEntry) -> Result<Flow, Done> {
        let endpoint = &self.up.endpoint;
        let request = endpoint
            .request(
                Method::GET,
                &upload_path(&entry.id),
                &self.secret,
                endpoint.timeout(),
            )
            .map_err(invalid_credential)?;
        let exchange = endpoint.exchange(request, MAX_BODY);
        match self.reply(path, RequestKind::Status, exchange, true)? {
            UploadReply::State(UploadState::Open) => Ok(Flow::Next(Step::SendNar)),
            UploadReply::State(_) => Ok(Flow::Complete),
            UploadReply::Refused(UploadRefusal::UnknownUpload) => {
                self.redeclare(path, RequestKind::Status, entry)
            }
            UploadReply::Refused(refusal) => self.refusal(path, RequestKind::Status, refusal, true),
        }
    }
}
