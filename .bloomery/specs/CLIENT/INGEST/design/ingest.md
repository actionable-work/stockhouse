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
3. Uploads at most `batch_size` due records (`next_attempt_at <= now`) in
   `(next_attempt_at, id)` order, one at a time.

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
- `server`: authenticated upload to the private tier with protocol v1
  (below). It lives in the `stockhouse-upload` crate behind the same
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
either in the same job is `Retry(Transient)`.

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

[backend.credential]
systemd = "private"                        # or file = "/abs/path", or
                                           # server = "/abs/auth.toml"
```

`UploaderConfig::parse` checks its structure (unknown fields are `Parse`; a
missing `credential` is `Parse`; `credential` is the shared
`CredentialSpec`), that `request_timeout_secs` is within 1–300 like the
sign-in configuration's, and that `max_closure_paths` is not zero
(`InvalidLimit`). The base URL is checked by
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

[credential]
systemd = "private"                               # or file, or server

[[extra]]                                         # optional, slots 3–10
slot = 3
path = "/nix/var/nix/profiles/per-user/root/profile"
```

Errors (`LeaseConfigError`): `Parse` (syntax, unknown or missing field),
`UnsupportedVersion`, `RelativePath { field }` (`machine_id_file`,
`current_system`, `system_profile` or an `extra` path not absolute),
`InvalidSlot` (an `extra` slot outside 3–10 or repeated), `InvalidLimit`
(a timeout outside 1–300 seconds) and `Endpoint(EndpointError)`.
