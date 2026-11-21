# Ingestion design

Build success never implies upload success. The post-build hook only
records intent durably and returns; a separate long-running uploader does the
work. The queue is a directory, so the hook (run by the Nix daemon as root)
and the uploader (a service user) share it without sockets, locks or a
running peer.

## Queue layout

```text
<queue>/               owned by the uploader user, never by root
  FORMAT               "stockhouse-queue-v1\n"
  LIMITS               "max_pending=<n>\n", written by the uploader
  pending/<hash>.json  one record per store path awaiting upload
  failed/<hash>.json   records that exhausted retries, were rejected or corrupt
  done/<hash>.json     upload ledger: identity and time of each completed upload
  uploads/<hash>.json  server backend: wire state of a closure path in progress
  tmp/                 staging for atomic writes
  overflow             one byte per path dropped while full (≤ 1 MiB)
  overflow.rescan      the overflow marker a running rescan has taken over
```

`<hash>` is the 32-character store path hash, so a store path has at most one
pending record. A record is JSON:

```json
{"version":1,"id":"<UploadId>","store_path":"/nix/store/…","enqueued_at":0,
 "attempts":0,"next_attempt_at":0,"last_error":null}
```

Times are Unix seconds. `failed/` records add `"reason"`. Done records hold
`version`, `id`, `store_path` and `uploaded_at`. A record whose `version` is
not 1 is treated as corrupt.

`Queue::create(dir, max_pending)` (uploader side) makes the layout, writes
`FORMAT` if absent and rewrites `LIMITS`, so the hook and the uploader always
share one capacity. `Queue::open` (both sides) requires `FORMAT` to equal the
current value (`QueueError::UnsupportedFormat` otherwise, and
`QueueError::NotInitialized` when `FORMAT` or `LIMITS` is missing or
`LIMITS` does not parse). `LIMITS` is replaced atomically (staged, `fsync`ed,
renamed), so a producer never sees a torn file. A rollback to an older client keeps the same format;
a future format change must ship its reader before its writer.

`uploads/` is an additive part of format v1: `Queue::create` makes it when
absent, `Queue::open` does not require it, and the hook, older clients and
the other backends never read it. Record, done and `FORMAT` contents are
unchanged, so a rollback to the groundwork client ignores the directory.

Every record write stages a file in `tmp/`, `fsync`s it, moves it into place
and `fsync`s the directory. A new pending record is placed with a hard link,
which fails if the name exists, so concurrent hooks enqueueing the same path
create exactly one record and the first identity wins. Staging names are
`<pid>-<n>.tmp`; a name already taken (left by a crashed process whose pid
was reused) is skipped for the next `n`, and a staging write that fails
removes its own file. Leftover staging files are never read.

## Enqueue

`Queue::enqueue(path, now)` returns:

1. `AlreadyUploaded` if `done/<hash>.json` exists;
2. `AlreadyQueued(id)` if `pending/<hash>.json` exists, keeping its identity;
3. `AlreadyFailed(id)` if `failed/<hash>.json` exists, keeping it visible
   for an operator rather than retrying it with a new identity;
4. `Overflow` if `pending/` already holds `max_pending` records; a producer
   appends one byte to `overflow` unless it has reached 1 MiB;
5. otherwise `Queued(id)` with a new `UploadId`, `attempts = 0` and
   `next_attempt_at = now`.

If `pending/<hash>.json` exists but cannot be read (QUEUE-015, QUEUE-016), the name stays taken
until the uploader quarantines the record. The enqueue then returns
`Overflow` and, when the producer records overflow (the hook does; rescan
keeps its own markers), appends to `overflow`, so a later rescan enqueues the
path.

The pending count is read once per producer batch (one hook invocation), so
the bound is `max_pending` plus the paths of producers racing at once.
Overflow records only a count: the rescan finds dropped paths from the store.

## Upload identities

The server backend uploads every path of a closure, not only the queued
root, and each path needs an identity that is stable across retries and
restarts, because the server treats it as an idempotency key. The queue keeps
that wire state per store path, so overlapping closures share it:

```json
{"version":1,"id":"<UploadId>","store_path":"/nix/store/…","nar_sent":false}
```

- `Queue::upload_state(path, root_id)` returns the entry for `path`. For the
  record's own path (`root_id` is `Some`) the identity is always the record's
  `id`: an existing entry with another identity, possible only when the path
  was first uploaded as a dependency and then queued, is replaced by one with
  `root_id` and `nar_sent = false`. For a dependency (`root_id` is `None`) an
  existing entry is returned unchanged; otherwise a new entry with
  `nar_sent = false` is staged and hard-linked into place, so the first
  identity wins, and the winner is read back. The new entry's identity is
  that of the path's own pending record when one exists, so a path that is
  both queued and a dependency keeps one identity, and a fresh `UploadId`
  otherwise.
- An entry that does not parse, has another version or names another store
  path is moved, bytes unchanged, to `failed/<hash>.upload.corrupt` (counted
  in `status.failed`, like quarantined records) and replaced as above. The server sees a new identity, which costs at most a
  redundant upload.
- `Queue::mark_nar_sent(path, state)` rewrites the entry with
  `nar_sent = true` (staged, `fsync`ed, renamed) and returns only after the
  write is durable. The backend calls it before the first NAR byte is sent.
- `Queue::clear_nar_sent(path, state)` rewrites it with `nar_sent = false`.
- `Queue::renew_identity(path, root)` replaces a path's identity after the
  server has forgotten it (a declaration answered `unknown_upload`): it
  generates a fresh `UploadId`, and for the record's own path (`root`) first
  rewrites `pending/<hash>.json` with that `id` (staged, `fsync`ed, renamed;
  `attempts` and the other fields unchanged), then writes the `uploads/`
  entry with the new identity and `nar_sent = false` the same way, and
  returns the new entry only after both are durable. A crash between the two
  writes leaves a record whose `id` differs from its entry, which
  `upload_state` already repairs in favor of the record. The worker applies
  an outcome to the identity currently stored in the pending record, never
  to a stale in-memory copy, so the record keeps the new identity and its
  ledger entry is written with it.
- `Queue::complete_path(path, id, now)` records a dependency as uploaded: it
  writes `done/<hash>.json` with that identity (if absent), then removes the
  `uploads/` entry. A crash between the two leaves an entry that the next
  pass removes.
- `Queue::is_uploaded(path)` is the public form of the ledger check, and
  `Queue::forget_done(path)` removes a path's `done/<hash>.json` (used when
  the server reports a ledgered path missing).

Each pass's reconcile step also removes an `uploads/<hash>.json` whose
`done/<hash>.json` exists, or whose store path no longer exists on disk
(`lstat` reports `ENOENT`), so the directory holds only paths still in
progress.

## Post-build hook

Nix runs the hook with `OUT_PATHS` (space-separated store paths) after each
build. Nix stops its build loop if the hook exits nonzero, and the hook blocks
that loop while it runs. `stockhouse post-build-hook --queue <dir>`:

1. `lstat`s the queue directory and requires a real directory (not a
   symbolic link) whose `st_uid` and `st_gid` are both non-zero. When running
   as root it then drops to exactly that `st_uid`/`st_gid` (no name lookup) with
   `setgroups([])`, `setgid`, `setuid`, while still single-threaded, before
   any other filesystem operation. A queue owner who plants symbolic links can
   then reach only what that owner could reach anyway. If any of the three
   calls fails, the hook writes one stderr line, touches nothing and exits 0;
   it never continues as root.
2. Opens the queue without creating it. If it is missing, uninitialized,
   unsupported or unwritable, the hook writes one stderr line and does
   nothing more.
3. Enqueues up to 1024 paths per invocation, counting `pending/` once, and
   records the rest as overflow. Entries that are not valid store paths are
   skipped with a stderr line. The first queue I/O error (an unwritable
   queue shows up here) is logged once and stops the loop without creating
   anything; the periodic rescan recovers the remaining paths.
4. Never contacts the uploader, the network or a server, and never waits for
   an upload.
5. Exits 0 in every case. The CLI catches panics (`catch_unwind`; release
   builds keep `panic = "unwind"`), and the module's hook script also ends in
   `|| true`. The Nix version matrix is tested because hook execution
   semantics differ across Nix releases.

## Uploader

`Uploader::upload(&UploadJob { id, store_path, attempts }) -> UploadOutcome`:

| Outcome | Effect on the record |
| --- | --- |
| `Uploaded` | write `done/<hash>.json` with the same `id`, then remove the pending record |
| `Retry(Transient \| Uncertain, msg)` | `attempts += 1`; `next_attempt_at = now + min(base·2^(attempts−1), max)`; `last_error = msg` |
| `Retry(QuotaExceeded, msg)` | as above but `next_attempt_at = now + max` |
| `Deferred(msg)` | `attempts` unchanged; `next_attempt_at = now + max` |
| `Rejected(msg)` | move the record to `failed/` |

When a `Retry` brings `attempts` to `max_attempts` the record moves to
`failed/`. The identity never changes across retries, uncertain replies,
quota exhaustion and restarts, so a server can treat it as an idempotency
key. There is no jitter, because each client retries its own queue serially.

Each pass:

1. Reconciles `pending/` with the ledger. A pending record whose `done/`
   record exists (a crash between the two writes) is removed, not
   re-uploaded. Stale `uploads/` entries are removed (see Upload
   identities). A pending record that does not parse or has another version
   moves, bytes unchanged, to `failed/<hash>.corrupt` (reason `corrupt`). Its
   identity is unreadable, so a later enqueue of that path starts afresh.
2. Rescans if needed (below).
3. Hands at most `batch_size` due records (`next_attempt_at <= now`), in
   `(next_attempt_at, id)` order, to `Uploader::upload_batch` as one slice of
   jobs, and applies the returned outcomes (one per job, in job order) to the
   records in that order.

`Uploader::upload_batch(&[UploadJob]) -> Vec<UploadOutcome>` has a default
implementation that calls `upload` for each job in order, so the `deferred`
and `nix-copy` backends and the server backend's protocol v1 path keep their
one-at-a-time behavior. The server backend's protocol v2 path overrides it
to upload the jobs' closures together (see Protocol v2 groups). The queue
format, records and outcome effects are unchanged.

The uploader process then sleeps for `poll_interval_secs` and repeats. A
failing backend only delays records; it never touches the build.

### Backends

- `deferred` (default): every job is
  `Deferred("no upload backend configured (set backend = \"server\")")`.
  Records stay pending and visible until a backend is configured. Once `pending/` is full, new builds only add to the overflow count.
  Overflow rescans are skipped while there is no free capacity.
- `nix-copy`: a local development and test backend. It runs
  `<nix> --extra-experimental-features nix-command copy --to <destination> <store path>`
  where `destination` must be an absolute `file://` URL. Exit status 0 is
  `Uploaded`; anything else is `Retry(Transient)` with at most 512 bytes of
  stderr. It passes no credential and contacts no server.
- `server`: authenticated upload to the private tier with protocol v1 or
  v2 (below), selected by the `protocol` setting. It lives in the `stockhouse-upload` crate behind the same
  `Uploader` trait.

## Server backend

`ServerUploader` implements `Uploader` for protocol v1 (see
[the protocol design](../../PROTO/design/proto.md#upload-and-lease-protocol-v1)).
It is built from the validated `ServerEndpoint`, the `nix` executable, the
opened `Queue` (for upload identities and the ledger) and a
`CredentialSource`. It never touches the build: like every backend it runs
only inside the uploader process, and its worst outcome delays a record. The
server backend and the lease client live in the `stockhouse-upload` crate.

### Endpoint and credential

`ServerEndpoint::new(base_url, credential, request_timeout_secs)` checks the
base URL `B`, in order: it parses as an absolute URL
(`EndpointError::InvalidUrl`); its scheme is `https`, or `http` with a
loopback IP literal host (`127.0.0.0/8` or `[::1]`; names such as
`localhost` are not trusted) (`EndpointError::Cleartext` for any other
`http`; `InvalidUrl` for any other scheme); it has no user information,
query or fragment and its path ends in `/` (`InvalidUrl`).

The credential is configured with the shared serde `CredentialSpec` of
`stockhouse-credentials` (`{ file = … }`, `{ systemd = … }` or
`{ server = "<auth configuration file>" }`) and always built with
`stockhouse_auth::source`. When the spec is `server`, the CLI also compares
the private tier URL of that sign-in configuration (the tier its tokens are
for) with `B`; if they differ, the endpoint is refused with
`EndpointError::AudienceMismatch`, so a token is never sent to a tier it was
not issued for. A source that cannot be built, configured or matched at
startup is a configuration error (CLI exit 2). The server must grant the
signed-in credential the scopes `cache:read cache:write`; the client
requests both.

Every request URL is `B` joined with a protocol path rendered by
`stockhouse-wire`, so the bearer credential is only ever sent to `B`'s
origin, under `B`. Redirects are never followed: a 3xx reply is an
unexpected status. The HTTP client is `reqwest`'s blocking client, used only
outside any async runtime, with `connect_timeout = request_timeout_secs`,
`tcp_keepalive = 60 s`, no environment proxy (`no_proxy()`, so
`HTTPS_PROXY` and the like can never receive the bearer), no client-wide
timeout, and, for `http` loopback, no TLS roots, as in the proxy. Each
request has its own total timeout: `request_timeout_secs` for declare,
status and lease requests, and `request_timeout_secs + ceil(nar_size / 256
KiB)` seconds for a NAR upload (a floor of 256 KiB/s). Reply bodies are read
up to the protocol's cap.

The credential comes only from that `CredentialSource`. It is read once per
job, again after a successful `invalidate`, and again immediately before
each NAR upload (an access token lives about ten minutes, and one NAR upload
is bounded by its lifetime on the server). It is sent as
`Authorization: Bearer <credential>`. Neither the credential nor any
response body is ever included in a message, record or log line; messages
name the store path, the request (`POST declare`, `PUT nar` or `GET status`)
and the status or protocol code, for example
`<path>: PUT nar: unexpected reply (status 302)`. A credential that is not a
valid `Authorization` header value is `Deferred("credential unavailable: …")`
and is never sent as an empty bearer.

- Any credential error (`NotConfigured`, `SignedOut`, `RefreshUnavailable`,
  `RefreshRejected`, a store, key or configuration error, a missing or
  unsafe file, `Unsupported`), at any read, makes the job
  `Deferred("credential unavailable: <error>")`, where `<error>` is the
  error's display text, for example
  `credential unavailable: server credential: not signed in` for a signed-out
  machine; that text becomes the record's `last_error`. No request is sent
  after the failed read, no attempt is consumed, and upload entries keep
  their `nar_sent` state for the next job.
- A 401 reply makes the backend call `CredentialSource::invalidate(&sent)`.
  The signed-in credential declines a forced refresh within 60 s
  (`INVALIDATE_GUARD_SECS`) of obtaining its token, so a 401 in that window
  is `Retry(Transient)` without a retry; this is intended.
  Only when it returns `true` does it read the credential again and send the
  same request once more (a NAR upload is dumped again). When it returns
  `false`, or the retry is also answered 401, the job is
  `Retry(Transient, "unauthorized")`. A persistent 401 therefore consumes
  attempts; that is recorded as a follow-up.

### Closure

For a job `{ id, store_path: R, attempts }`:

1. If `lstat(R)` reports `ENOENT`, the job is `Rejected("not in the local
   store")` and nothing is contacted.
2. The closure comes from
   `<nix> --extra-experimental-features nix-command path-info --json --recursive <R>`,
   stdout only, capped at 64 MiB (`ClosureError::TooLarge`). A command that
   fails to start or exits nonzero, or produces no output for
   `request_timeout_secs` (polled, like the NAR dump), is
   `ClosureError::Command`; a command still running is killed and reaped.
3. `parse_closure` accepts the shapes the scanner accepts: an object keyed by
   full store path (Nix 2.31 and 2.34 with plain `--json`) or
   `{"info": {<basename>: …}, "version": 2, …}`. For each entry it reads
   `narHash` (SRI `sha256-<base64>` or `sha256:<nix32|hex>`), `narSize`
   (greater than zero), `references` (full paths or basenames), `deriver`
   (full path, basename or `null`/absent) and `ca` (string, or `null`/absent).
   Other fields are ignored. Any other shape or value, an entry whose
   reference is not itself an entry, or a closure without `R` is
   `ClosureError::Format`.
4. A closure of more than `max_closure_paths` entries is
   `Rejected("closure too large")`.

`ClosureError::{Command, Format, TooLarge}` are `Retry(Transient)` with a
message of at most 512 bytes; they describe the local daemon or Nix, not the
path.

The upload order is a topological order of the reference graph with
references before referrers, ties broken by store path bytewise, and
self-references ignored. A cycle (impossible in a valid store) is
`ClosureError::Format`.

### Per-path upload

The job keeps an explicit worklist, initially the closure in upload order;
there is no recursion. A dependency (any path other than `R`) whose ledger
entry exists is skipped without a request: that is path-level
deduplication. `R` itself is never skipped, because the worker only hands
out records that are not in the ledger. For each other path `P` the backend
gets `Queue::upload_state(P, root_id)` and runs the steps below. The job
stops at the first path whose result is not "complete" and returns that
result. When the worklist is empty it returns `Uploaded`, and the worker
writes the record's own ledger entry as before. Completing a dependency
calls `Queue::complete_path`. Completing `R` removes its `uploads/` entry
before returning `Uploaded`.

Each path has a budget of 8 declare, NAR and status requests per job; the
single retry after a successful `invalidate` is part of the same request, so
at most 16 HTTP requests are sent for one path. A path that exhausts its
budget is `Retry(Transient, "upload step budget exhausted")`. The once-only
transitions below already bound every legitimate sequence to fewer requests,
so the budget is a backstop against a misbehaving server and is not reached
in production. Two
transitions are allowed at most once per path per job: the missing-reference
round, and declaring again after `unknown_upload`. A second occurrence of
either in the same job is `Retry(Transient)`. Renewing an identity after a
declaration answered `unknown_upload` (below) is a third such transition,
also at most once per path per job.

Declare (`POST uploads/<id>`, `Content-Type: application/json`, body
`UploadDeclaration` from the path-info entry: `store_path`, canonical
`nar_hash`, `nar_size`, `references` as basenames including any
self-reference, `deriver` as a basename, `ca`):

| Reply | Result |
| --- | --- |
| `Present` or `Published` | complete |
| `Open` | `mark_nar_sent`, read the credential, then send the NAR |
| `MissingReferences(m)` | the missing-reference round (below) |
| `Conflict`, `TooLarge`, `BadRequest`, `VerificationFailed` | `Rejected("<path>: <code>")` |
| `UnknownUpload` | the server forgot the identity: `Queue::renew_identity`, then declare again with the new identity (once per path per job; a second is `Retry(Transient)`) |
| `QuotaExceeded` | `Retry(QuotaExceeded)` |
| `Unauthorized` | the 401 rule above |
| `Unavailable`, a transport error, a timeout, `ReplyError` | `Retry(Transient)` |

Send the NAR (`PUT uploads/<id>/nar`, `Content-Type: application/x-nix-nar`,
`Content-Length` = `nar_size`): the body is the stdout of
`<nix> --extra-experimental-features nix-command store dump-path <P>`
(stdin and stderr null), read in 64 KiB chunks through a verifier that holds
back the final chunk. When the dump reaches end of file, the verifier checks
the byte count and SHA-256 against the declared `nar_size` and `nar_hash`
and that the dump exited 0, and only then releases the final chunk. On any
failure it returns a read error instead, so the request is aborted before
the server receives a complete body. On every exit before the dump has been
waited for (a failed check, an aborted or refused request, a timeout, an
early reply), the child is killed and reaped. Memory is bounded by two
chunks whatever the NAR size; no NAR is buffered whole. A size or hash
mismatch is `Rejected("<path>: local NAR does not match its NarHash")`; a
dump that fails to start or exits nonzero is `Retry(Transient)`.

| Reply | Result |
| --- | --- |
| `Present` or `Published` | complete |
| `VerificationFailed`, `Conflict`, `TooLarge`, `BadRequest` | `Rejected("<path>: <code>")` |
| `QuotaExceeded` | `Retry(QuotaExceeded)` |
| `MissingReferences(m)` | the missing-reference round, which then declares again |
| `UnknownUpload` | `clear_nar_sent` and declare again (once per path per job) |
| `Unauthorized` | the 401 rule above (the NAR is dumped again) |
| `Unavailable`, a transport error or timeout before the body was fully sent | `Retry(Transient)` |
| no reply, a transport error, a timeout or a `ReplyError` after the body was fully sent | `Retry(Uncertain)` |

"Fully sent" means the verifier released the final chunk.

Reconcile: when `upload_state` reports `nar_sent = true`, a NAR may have been
sent by an earlier attempt or process whose reply was lost, so the backend
asks first (`GET uploads/<id>`) and never re-sends a NAR without asking:

| Reply | Result |
| --- | --- |
| `Present` or `Published` | complete, with no `PUT` |
| `Open` | read the credential, then send the NAR |
| `UnknownUpload` | `clear_nar_sent` and declare again (once per path per job) |
| `VerificationFailed` | `Rejected("<path>: verification_failed")` |
| `Unauthorized` | the 401 rule above |
| `Unavailable`, a transport error, a timeout, `ReplyError` | `Retry(Uncertain)` |

Missing-reference round: every name in `m` must be one of `P`'s declared
references other than `P` itself; otherwise the result is
`Retry(Transient, "protocol violation")`. For each named path `M`, the
backend first removes `done/<M>` (the server no longer holds it, so the
ledger is wrong), then puts `M` and then `P` back at the front of the
worklist, with `M` in upload order. `M` is then uploaded with the same steps
and may itself have a round. The round uses no attempt of the record and
runs at most once per path per job.

## Server backend, protocol v2

With protocol v2 (see
[the protocol design](../../PROTO/design/proto.md#upload-and-lease-protocol-v2))
the server backend uploads the closures of several due records together and
replaces per-path requests by batches. Everything in "Server backend" above
still applies unless this section says otherwise: the endpoint, credential
and 401 rules, the closure query and its errors, the upload order, path-level
deduplication through the ledger, the `uploads/` identities and `nar_sent`
marks, the verified NAR stream with its child reaping and timeouts, the
outcome messages, and the rule that a NAR is never sent again without asking
the server first.

### Version selection

The backend's `protocol` setting is `auto` (default), `v1` or `v2`.

- `v1`: the backend never sends the probe and behaves exactly as above.
- `auto` and `v2`: before each `upload_batch` call the backend sends the
  protocol probe unless it holds a selected version younger than
  `PROBE_TTL_SECS = 3600`, and selects the version with `select_version`. The
  selection lives only in the uploader process, so the cache saves probes
  only for a long-running uploader (the module's service); a short-lived
  `stockhouse uploader` run probes once.
- A probe answered `Busy`, `Unavailable`, a transport error, a timeout or a
  `ReplyError` selects nothing: every job of the call is
  `Retry(Transient, "protocol probe: <reason>")` and no upload request is
  sent. The backend never falls back to v1 because a probe failed.
- `NoCommonVersion` (`auto`) is `Retry(Transient, "no common protocol
  version")` and `NotOffered` (`v2`) is
  `Retry(Transient, "server does not offer protocol v2")` for every job.
- A probe answered `Unauthorized` follows the 401 rule: after a successful
  `invalidate` the probe is sent once more with the new credential;
  otherwise, or if that is also `Unauthorized`, every job of the call is
  `Retry(Transient, "unauthorized")` and no upload request is sent. A
  credential error defers every job as usual.
- When any v2 request is answered `NotSupported`, the backend drops the
  selected version and sends no further v2 batch request in the group;
  transfers already running finish (their results are recorded, and
  `nar_sent` marks are kept, so a later v1 or v2 job asks the server before
  sending a NAR again). Every job not yet complete is then
  `Retry(Transient, "<path>: protocol v2 not available")`, and the backend
  probes again before the next `upload_batch` call.

With version 1 selected, `upload_batch` uses the default one-job-at-a-time
behavior, so `auto` against a v1 server behaves exactly like `v1` after one
probe per hour.

### Groups

`upload_batch(jobs)` processes the jobs as groups, in job order:

1. A job whose root fails `lstat` with `ENOENT` is `Rejected("not in the
   local store")` without contact, and a job whose closure cannot be queried
   or parsed is `Retry(Transient)` with the closure error; neither stops the
   other jobs. A closure over `max_closure_paths` is `Rejected("closure too
   large")` as in v1.
2. Jobs are added to the current group while the union of their closures
   holds at most `max_closure_paths` paths; the next job starts a new group.
   A group is uploaded completely before the next starts.
3. The group's work set is the union in upload order (a topological order of
   the union's reference graph, references first, ties by store path
   bytewise), each path once, without dependencies whose ledger entry exists.
   Every root is in the work set, because the worker hands out only records
   that are not in the ledger. Each path's identity comes from
   `Queue::upload_state(path, root_id)` as in v1, where `root_id` is the
   record's `id` when the path is a job's root.

The group then runs at most three rounds of the four phases below. Within
a group every path either becomes complete, ends with a result (rejected,
quota, uncertain, transient or deferred, as the tables below say), is
reworked, or is held back (ready to publish, waiting for a reference). The
next round has two parts:

- **Reworked paths**: a NAR or publish `unknown_upload` (declared again, with
  `nar_sent` cleared), a declare `unknown_upload` (declared again under a
  fresh identity from `Queue::renew_identity`), a publish `open` (NAR sent again, without a status
  query: the server has just reported its state), and the paths named in a
  missing-reference round (reconciled if their `nar_sent` is true, else
  declared). Reworked paths run the phases that apply to them.
- **Carried paths**: a held-back path joins the next round when any of its
  references that is not complete is reworked or is itself carried into the
  next round, decided in upload order so a chain of held paths is carried
  together. Carried paths keep `nar_sent` and go straight to the publish
  phase; they are never queried with `v2/status` and never declared or sent
  again.

A held-back path that is not carried ends as below ("waiting for a
reference"). A path is reworked at most once per group; a path needing a
second rework, or any rework after the third round, ends as
`Retry(Transient, "<path>: upload round limit")`.

### Phases

1. **Reconcile.** In the first round, paths of the work set whose entry has
   `nar_sent = true`; in later rounds, only paths reworked by a
   missing-reference round whose entry has `nar_sent = true`. They are
   queried with `POST v2/status` in id batches of up to 1000, before any
   declare or NAR request for them. Carried paths and paths reworked for
   `unknown_upload` or a publish `open` are never queried, so a path is in
   at most two status entries per group:

   | Result | Effect |
   | --- | --- |
   | `present`, `published` | complete |
   | `verified` | ready to publish; no NAR is sent |
   | `open` | send the NAR (phase 3) |
   | `unknown_upload` | `clear_nar_sent`, then declare (phase 2) |
   | `verification_failed` | the path is rejected (`<path>: verification_failed`) |

2. **Declare.** The remaining paths are declared with `POST v2/declare` in
   upload order, in batches filled until `DeclareBatch::push` returns
   `BatchFull` (1000 entries or 8 MiB), each entry built from the path-info
   facts exactly as a v1 declaration:

   | Result | Effect |
   | --- | --- |
   | `present`, `published` | complete |
   | `verified` | ready to publish |
   | `open` | `mark_nar_sent`, durable before its NAR's first byte, then send the NAR |
   | `conflict`, `too_large`, `verification_failed` | the path is rejected (`<path>: <code>`) |
   | `unknown_upload` | the server forgot the identity: `Queue::renew_identity`, and the path is reworked, declared in the next round with the new identity; no NAR is sent under the old identity. Once per path per group; a second ends as `Retry(Transient)`. Under the quota stop it ends as quota |
   | `quota_exceeded` | the path ends as quota; the group's quota stop (below) |

3. **Transfer.** Paths to send are transferred with `PUT v2/nar/<id>`, at
   most `max_inflight_nars` at a time, started in upload order, each on its
   own thread with the v1 stream: credential read just before the request,
   `Content-Length` = `nar_size`, the final chunk held back until size, hash
   and dump status verify, child killed and reaped on every early exit, and
   the v1 NAR timeout. The other transfers continue when one fails.

   | Result | Effect |
   | --- | --- |
   | `verified` | ready to publish |
   | `present`, `published` | complete |
   | `VerificationFailed`, `Conflict`, `TooLarge`, `BadRequest` | the path is rejected (`<path>: <code>`) |
   | `QuotaExceeded` | the path ends as quota; the quota stop |
   | `UnknownUpload` | `clear_nar_sent`; reworked in the next round (declared again) |
   | `Busy`, `Unavailable`, a transport error or timeout before the body was fully sent | the path ends as transient |
   | no reply, a transport error, a timeout or a `ReplyError` after the body was fully sent | the path ends as uncertain; `nar_sent` stays true |
   | a local NAR that does not match its NarHash | the path is rejected (`<path>: local NAR does not match its NarHash`) |

   A NAR transfer is not resent within the group except after the rework
   for `unknown_upload` or a publish `open` result, so each path has at most
   two NAR transfers per group.

4. **Publish.** A path is publishable when it is ready to publish and each
   of its references other than itself is complete or publishable and
   earlier in upload order. Publishable paths are published with
   `POST v2/publish` in upload order, in id batches of up to 1000, sent one
   after another, so every reference is published by an earlier batch or
   earlier in the same batch. Publishability is evaluated again before each
   publish batch, from the results of the batches before it. Paths that are
   not publishable (a reference ended with a result, is reworked or is held
   back) are not sent; they are held back.

   | Result | Effect |
   | --- | --- |
   | `published`, `present` | complete |
   | `open` | the server holds no verified NAR: reworked (NAR sent again) |
   | `unknown_upload` | `clear_nar_sent`; reworked (declared again) |
   | `missing_references` | classified per name (below): held back, the missing-reference round, or a protocol violation |
   | `conflict`, `verification_failed` | the path is rejected (`<path>: <code>`) |
   | `quota_exceeded` | the path ends as quota; the quota stop |

   A publish request whose reply is lost (a transport error or timeout after
   the request was sent, or `Busy`/`Unavailable`) is resent identically under
   the batch resend rule below; if it still has no reply its paths end as
   uncertain. Their NARs are not sent again: the next job reconciles them
   with `v2/status` (their `nar_sent` is true) and publishes them.

A publish result `missing_references` is checked and classified name by
name:

1. Every name must be one of that entry's declared references other than
   itself, whatever rule below applies; otherwise the entry's path ends as
   `Retry(Transient, "protocol violation")`.
2. A name that is an entry of the same publish request whose result was
   `published` or `present` contradicts that result: the entry's path ends
   as `Retry(Transient, "protocol violation")`.
3. A name that is a path of the group not complete at this point (an entry
   of the same request that did not end `published` or `present`, or a group
   path that ended, is reworked, is held back or was not yet published) is
   held back: the server reports, as the protocol requires, that a
   reference was not published. The referrer keeps `nar_sent`, is not
   reworked by this result, and its `done/` entry and those of the named
   paths are left alone; it is carried into the next round under the rule
   above or ends as below.
4. Any other name (a ledgered dependency outside the work set, or a path
   that became complete through an earlier request of the group) enters the
   missing-reference round (below).

When the names fall under both rules 3 and 4, only the rule 4 names enter
the missing-reference round, and the referrer is reworked by that round.

A path still ready to publish when the group ends (because a reference
ended with a result) ends as `Retry(Transient, "<path>: waiting for a
reference")`; its `nar_sent` stays true, so the next job finds it `verified`
through `v2/status` and does not send its NAR again.

Completing a dependency calls `Queue::complete_path`; completing a root
removes its `uploads/` entry, as in v1. The worker then writes the root's
ledger entry when its job is `Uploaded`.

Missing-reference round: for each name `M` under rule 4 above, the backend
removes `done/<M>`, adds `M` to the next round's work set (so it
is reconciled or declared again) and reworks the entry. It runs at most once
per path per group, within the round limit.

Quota stop: after the first quota result the group sends no further
declare request and starts no further NAR transfer; transfers already
running finish, and the publish phase still runs for publishable paths, so
uploads already verified are not lost. Paths not yet attempted end as quota,
and so do paths reworked for `unknown_upload`, a publish `open` or a
missing-reference round that would need a declare or a NAR transfer; their
held-back referrers are not carried and end as waiting for a reference.

Batch resend rule: a `v2/status`, `v2/declare` or `v2/publish` request
answered `Busy`, `Unavailable`, a transport error or a timeout is resent
identically at most twice, waiting `min(retry_after, 30)` seconds or, without
`retry_after`, 2 and then 4 seconds. If the third attempt also fails, its
entries end as transient (status and declare) or uncertain (publish), and
the group continues with the paths that do not depend on them. Requests are
never overlapped: a resend starts only after the previous attempt ended, and
status, declare and publish batches are sent one at a time, so no two
requests carrying the same entry are ever in flight together. A request
answered `Refused(BadRequest)` rejects its entries' paths
(`<path>: POST v2 <request>: bad_request`); `Refused(TooLarge)` and a
`ReplyError` end them as transient (`protocol violation` or the reply
error). A conforming server never answers 413 to a request within the
protocol's limits, so the client never shrinks a batch; a server that does
keeps the records retrying until they fail by `max_attempts`. Every v2 request follows the 401 rule; concurrent transfers share
one invalidation, so a burst of 401s calls `CredentialSource::invalidate` at
most once per credential and each request is retried at most once.

A credential error at any read stops the group at once: no further request
is sent, running transfers finish or are aborted with their children
reaped, and every job not yet complete is
`Deferred("credential unavailable: <error>")` with its `nar_sent` marks
kept.

Requests are bounded structurally: per group each path is in at most two
status entries, two declare entries, two NAR transfers and three publish
entries, besides identical resends and the single retry after a successful
invalidate.

### Job outcomes

Each job's outcome is computed from the paths of its own closure:

1. `Uploaded` when every path of its closure is complete (or was already in
   the ledger).
2. Otherwise the first, in this order, of: `Rejected` naming the first
   rejected path in upload order; `Deferred` (credential); `Retry(QuotaExceeded)`;
   `Retry(Uncertain)`; `Retry(Transient)`. The message names the path and
   request as in v1, for example
   `<path>: POST v2 publish: quota_exceeded`.

A job whose closure shares a path with a failed job therefore fails with it,
while a job whose closure is complete is `Uploaded` even when other jobs of
the group failed. Groups of one call run one after another and are not
pipelined (one group's transfers do not overlap the next group's declares).

## Lease renewal, protocol v2

The lease client has the same `protocol` setting. In `auto` and `v2` it
sends the protocol probe once per run. Probe failures (`Busy`,
`Unavailable`, a transport error, a timeout, a `ReplyError`,
`NoCommonVersion`, and `NotOffered` in `v2`) make every slot failed without
a renewal, exit 75. A probe answered `Unauthorized` follows the 401 rule
(one resend after a successful `invalidate`); if it is still unauthorized,
every slot is failed, exit 75. With version 2 it renews
every resolved slot in one `POST v2/leases` request of `(machine, slot,
store_path)` entries in slot order and maps each result as the v1 slot
table: `Renewed` renewed; `Refused(UnknownPath | Conflict)` refused, logged
with the code; `MachinesFull` failed. A request-level `Refused(BadRequest)`
makes every slot refused. `Busy`, `Unavailable`, a transport error or a
timeout follow the batch resend rule (two identical resends), then every
slot is failed. A `NotSupported` reply in `auto` makes the same run renew
each slot with v1 `PUT leases/<machine>/<slot>` instead; leases are few, so
falling back within the run costs little. In `v2` a `NotSupported` reply
makes every slot failed, exit 75, with no v1 request. Exit statuses are
unchanged.

## Lease renewal

`stockhouse lease --config <file>` renews the server's named roots for this
machine so the server keeps the system closures it needs. It lives in the
`stockhouse-upload` crate and uses the same `ServerEndpoint` rules
(including the audience check), HTTP client, per-request timeout and 401
rule as the server backend. Any credential error makes every slot failed
without a request, and the run exits 75.

### Machine identity

The machine identity is a random 128-bit `MachineId` kept at
`machine_id_file` (the module uses `/var/lib/stockhouse/machine-id`). It is
not derived from `/etc/machine-id` or any hardware value, and it is never
printed or logged.

- If the file does not exist, 16 bytes from the operating system's random
  source (`getrandom`) are rendered as 32 lowercase hex characters plus
  `\n`, staged with mode `0600` in the same directory, `fsync`ed and
  hard-linked into place, so concurrent first runs agree on one identity (the
  first wins and is read back).
- An existing file is opened with `O_NOFOLLOW`, must be a regular file owned
  by the effective UID with no group or other permission bits, and must hold
  exactly 32 lowercase hex characters and `\n`. Otherwise the run fails with
  `LeaseError::MachineId` naming the failed check, sends no request, and
  never replaces the file: a new identity would orphan the machine's leases,
  so recovery is an operator decision.

### Slots

| Slot | Path |
| --- | --- |
| 1 | `current_system` (`/run/current-system`) resolved with `realpath` |
| 2 | the previous system: among `<system_profile>-<N>-link` entries, the one with the largest `N` whose resolved target differs from slot 1 |
| 3–10 | each configured `extra` entry's path, resolved |

A slot whose path does not resolve, or resolves to something that is not a
store path, is skipped with one log line; slot 2 is skipped silently when
there is no other generation. Each remaining slot is one
`PUT leases/<machine-id>/<slot>` with `LeaseRequest { store_path }`:

| Reply | Slot result |
| --- | --- |
| `Renewed` | renewed |
| `Refused(UnknownPath \| Conflict \| BadRequest)` | refused, logged with the code |
| `Unauthorized` | the 401 rule; otherwise failed |
| `Unavailable` (including a server whose machine table is full), a transport error, a timeout, `ReplyError` | failed |

Slots are renewed in order and a failed slot does not stop later slots. A
run keeps no state besides the machine identity, so it is idempotent and
safe to repeat: the next trigger simply renews again.

Exit status: 0 when every slot was renewed or skipped; 75 (`EX_TEMPFAIL`)
when any slot was refused or failed, including any credential error; 1 for
`LeaseError::MachineId`; 2 for a configuration error. The module treats 75
as success, so a server outage never marks a unit failed during boot or
activation.

## Rescan reconciliation

The hook can miss paths, for example on overflow, when the queue is
unavailable, or before the hook was installed. A rescan runs when an overflow
marker exists and `pending/` has free capacity, and every
`rescan_interval_secs`:

1. Rename `overflow` to `overflow.rescan` unless `overflow.rescan` already
   exists. New overflow during the rescan recreates `overflow`. An append
   already in flight lands in `overflow.rescan`. That is safe, because its
   path was registered in the store before the hook ran, and so before the
   scan starts.
2. Ask the `Scanner` for candidates. On failure keep both markers and report
   `ScanError`; overflow stays visible.
3. Enqueue each candidate that is neither uploaded, pending nor failed,
   without touching the overflow markers. If capacity is reached, stop, keep
   `overflow.rescan`, and if no marker exists (an interval rescan) create
   `overflow` so the unhandled candidates stay visible.
4. If every candidate was handled, delete `overflow.rescan`. With the `none`
   scanner, which cannot recover anything, overflow is never cleared.

The uploader logs a warning when a pass first observes overflow and whenever
a rescan leaves overflow active.

The `nix-path-info` scanner runs
`<nix> --extra-experimental-features nix-command path-info --json --all`. It
reads stdout only, caps it at 256 MiB, and selects paths whose `ultimate` flag
is true (built locally; a missing flag counts as false), excluding `.drv`
files. It accepts the two JSON shapes the pinned Nix versions produce:

- an object keyed by full store path (Nix 2.31 and 2.34 with plain `--json`);
- `{"info": {<basename>: …}, "version": 2}` (`--json-format 2` on Nix 2.34,
  accepted so the later default does not break rescans).

Any other shape is `ScanError::Format`; output over the cap is
`ScanError::TooLarge`; a command that fails to start or exits nonzero is
`ScanError::Command`. The `none` scanner returns no candidates.

## Status

`Queue::status` and `stockhouse status --queue <dir>` report the pending,
failed and uploaded counts, the earliest `next_attempt_at`, and overflow as
`{active, dropped, truncated}`. `dropped` is the byte count across both
markers, and `truncated` means a marker reached its cap.

## Uploader configuration

```toml
version = 1
queue_dir = "/var/lib/stockhouse/queue"
max_pending = 10000
max_attempts = 20
batch_size = 64
backoff_base_secs = 5
backoff_max_secs = 3600
poll_interval_secs = 5
rescan_interval_secs = 3600

[backend]
kind = "deferred"            # or "nix-copy" with destination and optional nix,
                             # or "server" (below)

[scanner]
kind = "nix-path-info"       # or "none"; optional nix
```

Errors (`UploaderConfigError`): `Parse` (syntax, unknown field),
`UnsupportedVersion`, `RelativeQueueDir`, `InvalidLimit { field }` (a zero
limit, or base greater than max backoff) and `UnsupportedDestination`
(`nix-copy` without an absolute `file://` destination).

The `server` backend:

```toml
[backend]
kind = "server"
base_url = "https://cache.example.invalid/t/1/"
nix = "/run/current-system/sw/bin/nix"     # optional, default "nix"
request_timeout_secs = 60                  # optional, default 60
max_closure_paths = 50000                  # optional, default 50000
protocol = "auto"                          # optional: "auto", "v1" or "v2"
max_inflight_nars = 4                      # optional, 1–16, default 4

[backend.credential]
systemd = "private"                        # or file = "/abs/path", or
                                           # server = "/abs/auth.toml"
```

`UploaderConfig::parse` checks its structure (unknown fields are `Parse`; a
missing `credential` is `Parse`; `credential` is the shared
`CredentialSpec`), that `request_timeout_secs` is within 1–300 like the
sign-in configuration's, that `max_closure_paths` is not zero, and that
`max_inflight_nars` is within 1–16 (`InvalidLimit`). A `protocol` other than
`auto`, `v1` or `v2` is `Parse`. The new keys are optional, so configuration
version 1 is unchanged. `max_inflight_nars` is used only with protocol v2;
four concurrent transfers let the server merge their commits without letting
one machine monopolize its upload capacity. The base URL is checked by
`ServerEndpoint::new` when the CLI builds the backend, and a failure there is
also a configuration error (exit 2), as is a credential source that
cannot be built or whose private tier differs from `base_url`.

## Lease configuration

```toml
version = 1
base_url = "https://cache.example.invalid/t/1/"
machine_id_file = "/var/lib/stockhouse/machine-id"
current_system = "/run/current-system"            # optional, this default
system_profile = "/nix/var/nix/profiles/system"   # optional, this default
request_timeout_secs = 60                         # optional, default 60
protocol = "auto"                                 # optional: "auto", "v1" or "v2"

[credential]
systemd = "private"                               # or file, or server

[[extra]]                                         # optional, slots 3–10
slot = 3
path = "/nix/var/nix/profiles/per-user/root/profile"
```

Errors (`LeaseConfigError`): `Parse` (syntax, unknown or missing field),
`UnsupportedVersion`, `RelativePath { field }` (`machine_id_file`,
`current_system`, `system_profile` or an `extra` path not absolute),
`InvalidSlot` (an `extra` slot outside 3–10 or repeated), a `protocol`
other than `auto`, `v1` or `v2` (`Parse`), `InvalidLimit`
(a timeout outside 1–300 seconds) and `Endpoint(EndpointError)`.
