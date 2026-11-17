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

If `pending/<hash>.json` exists but cannot be read, the name stays taken
until the uploader quarantines the record. The enqueue then returns
`Overflow` and, when the producer records overflow (the hook does; rescan
keeps its own markers), appends to `overflow`, so a later rescan enqueues the
path.

The pending count is read once per producer batch (one hook invocation), so
the bound is `max_pending` plus the paths of producers racing at once.
Overflow records only a count: the rescan finds dropped paths from the store.

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
   queue shows up here) is logged once and stops the loop, after a best-effort
   overflow record so a rescan recovers the remaining paths.
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
   re-uploaded. A pending record that does not parse or has another version
   moves, bytes unchanged, to `failed/<hash>.corrupt` (reason `corrupt`). Its
   identity is unreadable, so a later enqueue of that path starts afresh.
2. Rescans if needed (below).
3. Uploads at most `batch_size` due records (`next_attempt_at <= now`) in
   `(next_attempt_at, id)` order, one at a time.

The uploader process then sleeps for `poll_interval_secs` and repeats. A
failing backend only delays records; it never touches the build.

### Backends

- `deferred` (default): every job is `Deferred("server upload requires P2.08")`.
  Records stay pending and visible until an authenticated server uploader
  exists. Once `pending/` is full, new builds only add to the overflow count.
  Overflow rescans are skipped while there is no free capacity.
- `nix-copy`: a local development and test backend. It runs
  `<nix> --extra-experimental-features nix-command copy --to <destination> <store path>`
  where `destination` must be an absolute `file://` URL. Exit status 0 is
  `Uploaded`; anything else is `Retry(Transient)` with at most 512 bytes of
  stderr. It passes no credential and contacts no server.

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
kind = "deferred"            # or "nix-copy" with destination and optional nix

[scanner]
kind = "nix-path-info"       # or "none"; optional nix
```

Errors (`UploaderConfigError`): `Parse` (syntax, unknown field),
`UnsupportedVersion`, `RelativeQueueDir`, `InvalidLimit { field }` (a zero
limit, or base greater than max backoff) and `UnsupportedDestination`
(`nix-copy` without an absolute `file://` destination).
