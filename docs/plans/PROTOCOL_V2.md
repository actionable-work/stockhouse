# Plan: upload and lease protocol v2 (client)

Implementation plan for the dedicated implementer of protocol v2. The
normative behavior is in the specs; this plan orders the work and maps
tests to requirement IDs. Do not change behavior here: if implementation
forces a spec change, update the requirement and design first and get an
independent review (AGENTS.md).

- [`CLIENT/PROTO` design, "Upload and lease protocol v2"](../../.bloomery/specs/CLIENT/PROTO/design/proto.md#upload-and-lease-protocol-v2),
  `requirements/NEGOTIATE.toml`, `BATCH.toml`, `LEASE.toml` (008–009).
- [`CLIENT/INGEST` design](../../.bloomery/specs/CLIENT/INGEST/design/ingest.md)
  ("Uploader" pass step 3, "Server backend, protocol v2", "Lease renewal,
  protocol v2", configuration), `requirements/BATCH.toml`, `CONFIG.toml`
  (007–008), `LEASE.toml` (013–019).
- [`CLIENT/INTEGRATION` design](../../.bloomery/specs/CLIENT/INTEGRATION/design/integration.md)
  ("Server upload and leases", "Server upload stories", "Protocol v2
  stories"), `MODULE.toml` (027–028), `VM.toml` (029–035).
- Server counterpart: [`docs/PROTOCOL_V2_SERVER.md`](../PROTOCOL_V2_SERVER.md).

Suggested commit subject: `feat: add batched upload and lease protocol v2`.
Write a bounded root `PLAN.md` from this file before starting, and remove
it at cleanup.

## Outcomes, in order

| # | Outcome | Crates and files |
| --- | --- | --- |
| O0 | Budget splits, no behavior change: move `libs/upload/src/lease/` into a new private crate `stockhouse-lease` (`packages/rust/libs/lease`) that depends on `stockhouse-upload`, making the endpoint items it uses public there (`Exchange` and the request helpers, besides the already public `ServerEndpoint`); move the uploader configuration (`libs/ingest/src/config.rs`) together with `WorkerLimits` (from `worker.rs`, which `UploaderConfig::limits` builds) into a new private crate `stockhouse-uploader-config` (`packages/rust/libs/uploader_config`) that does not depend on `stockhouse-ingest`; `stockhouse-ingest` depends on it and re-exports both so callers keep their paths. Existing tests move with their code and keep their IDs | `Cargo.toml` (`members`, `[workspace.dependencies]`), new `libs/lease`, new `libs/uploader_config`, `libs/upload`, `libs/ingest`, `libs/cli/Cargo.toml`, `bloomery sync` |
| O1 | v2 codecs: `WIRE_VERSIONS`, probe path and `parse_probe_reply`, `ProtocolMode` and `select_version`, v2 paths, `DeclareBatch` (builder with `BatchFull`, render, parse), `IdBatch`, `LeaseBatch`, `parse_batch_reply` with entry results per kind, `parse_nar_reply`, `parse_retry_after`, `BatchError`, `LeaseBatchError`, `VersionError` | new crate `stockhouse-wire-v2` (`packages/rust/libs/wire_v2`): `src/{lib.rs,version.rs,paths.rs,declare.rs,ids.rs,lease.rs,reply.rs}` over `stockhouse-wire` and `stockhouse-proto` |
| O2 | `Uploader::upload_batch` with the sequential default; worker hands due records to it as one slice | `libs/ingest/src/{uploader.rs,worker.rs}` |
| O2b | Identity horizon (v1 and v2): v1 declare classifies 404 `unknown_upload`; `Queue::renew_identity`; the worker applies outcomes to the identity stored in the pending record; the v1 declare table gains the `UnknownUpload` row | `libs/wire/src/reply.rs`, `libs/ingest/src/{identity.rs,worker.rs}`, `libs/upload/src/backend.rs` |
| O3 | Configuration: `protocol` (backend and lease) and `max_inflight_nars` | `libs/uploader_config`, `libs/lease/src/config.rs` |
| O4 | Negotiation in the uploader process: `Negotiator` (probe, 3600 s cache, `NotSupported` invalidation), shared by the backend and the lease client | `libs/upload/src/negotiate.rs` |
| O5 | v2 group engine: grouping, rounds, the four phases, concurrent transfers on scoped threads (one blocking client shared), shared invalidation, batch resend rule, quota stop, credential stop, job outcome precedence; and the dispatcher `ServerBackend`, which implements `Uploader` by wrapping v1's unchanged `ServerUploader` and a `Negotiator`, routing each `upload_batch` call to v1 (sequential `upload`) or to the v2 engine by the selected version | new crate `stockhouse-upload-v2` (`packages/rust/libs/upload_v2`) depending on `stockhouse-upload`, `stockhouse-ingest` and `stockhouse-wire-v2`: `src/{lib.rs,dispatch.rs,group.rs,phases.rs,transfer.rs,outcome.rs,resend.rs}`. `stockhouse-upload` does not depend on it, so there is no cycle |
| O6 | Lease client v2: one batch, per-entry mapping, `NotSupported` fallback to v1 within the run, probe failures, resends | `libs/lease/src/{mod.rs,batch.rs}` |
| O7 | CLI: build `stockhouse_upload_v2::ServerBackend` (wrapping `ServerUploader`) and the lease client with the new settings; no new command or flag | `libs/cli/src/commands.rs`, `libs/cli/Cargo.toml` |
| O8 | Module: `uploader.protocol`, `uploader.maxInflightNars`, assertion; `tests/module-upload.test.nix` and `tests/module-lease.test.nix` checks | `nix/module.nix`, `tests/module-{upload,lease}.test.nix` |
| O9 | Fake server v2 and the VM stories (below) | `tests/lib/upload_server.py`, `tests/lib/vm.nix`, `tests/vm-{upload,upload-recovery,lease}.test.nix`, new `tests/vm-upload-v2{,-recovery}.test.nix`, `tests/flake.nix` |
| O10 | Docs and generated files: `AGENTS.md` layout rows (`wire_v2`, `upload_v2`, `lease`, `uploader_config`), module option description for `backend = "server"` (v1 or v2), slice record, `bloomery sync`, code-health rewrite | `AGENTS.md`, `nix/module.nix`, `docs/`, `Cargo.lock`, `bloomery.lock`, `.bloomery/code-health-summary.json` |

## Crates and budgets

| Crate | SLOC now | Expected after | Note |
| --- | --- | --- | --- |
| `stockhouse-wire` | 599 | 599 | unchanged; v2 goes to `stockhouse-wire-v2` |
| `stockhouse-wire-v2` (new) | 0 | about 900 | `publish = false`; reuses `UploadDeclaration`, `MachineId`, `LeaseSlot`, `ReplyError` from `stockhouse-wire` |
| `stockhouse-upload` | 1 532 | about 1 100 | lease moves out (O0), `negotiate.rs` added (used by `stockhouse-lease` and `stockhouse-upload-v2`) |
| `stockhouse-lease` (new) | 0 | about 650 | lease client v1 plus v2 batch |
| `stockhouse-upload-v2` (new) | 0 | about 1 250 | the dispatcher and the group engine; split `transfer.rs` further if it nears 1 450 |
| `stockhouse-ingest` | 1 497 | about 1 330 | config moves out (O0), `upload_batch` and worker change about +30 |
| `stockhouse-uploader-config` (new) | 0 | about 240 | with `WorkerLimits` |

Dependency order (no cycles; `A → B` means A depends on B):

- `wire → proto`; `wire-v2 → wire, proto`.
- `uploader-config → wire-v2` (for `ProtocolMode`), `credentials`.
- `ingest → uploader-config`.
- `upload → ingest, wire, wire-v2` (the `Negotiator` uses the probe codec).
- `lease → upload, wire, wire-v2`.
- `upload-v2 → upload, ingest, wire-v2`.
- `cli →` all of them; nothing depends on `cli`, `lease` or `upload-v2`.

Keep each phase a small function returning per-path results into one
`GroupState` (`BTreeMap<StorePath, PathStatus>` with `Pending`, `Send`,
`Ready`, `Complete`, `Ended(EndKind, String)`, rework flags), so the bounds
in BATCH-020 and BATCH-025 are structural and complexity stays under 10.
Concurrent transfers use `std::thread::scope` with a bounded work channel;
the credential and invalidation sit behind a `Mutex` so BATCH-023's "once per
sent credential" holds. Waits go through an injectable `Sleep` (tests use a
recording fake) so BATCH-017 and LEASE-015 run without real delays.

## Interfaces

- `stockhouse_wire_v2`: `WIRE_VERSIONS`, `probe_path()`,
  `parse_probe_reply(status, body) -> Result<ProbeReply, ReplyError>`,
  `ProtocolMode::{Auto, V1, V2}`, `select_version(mode, &[u16]) ->
  Result<u16, VersionError>`; `declare_path()`, `nar_path(id)`,
  `publish_path()`, `status_path()`, `leases_path()`;
  `DeclareBatch::{new, push, render, parse, entries}`, `BatchFull`,
  `IdBatch::{new, render, parse}`, `LeaseBatch::{new, render, parse}`;
  `BatchKind::{Declare, Publish, Status, Leases}`, `parse_batch_reply(kind,
  &request_keys, status, retry_after, body) -> Result<BatchReply,
  ReplyError>`, `BatchReply::{Results(Vec<EntryResult>), Refused(..),
  NotSupported, Unauthorized, Busy { retry_after }, Unavailable { retry_after
  }}`, `parse_nar_reply(status, retry_after, body)`,
  `parse_retry_after(Option<&str>) -> Option<u32>`.
- `stockhouse_ingest::Uploader::upload_batch(&self, &[UploadJob]) ->
  Vec<UploadOutcome>` (default: `upload` per job).
- `stockhouse_upload::Negotiator::{new(mode), version(&endpoint, now) ->
  Result<u16, NegotiationFailure>, forget()}`.

## Tests per requirement

Rust tests trace IDs with `#[cfg_attr(any(), bloomery("<ID>"))]` and assert
exact variants. Upload-engine tests run against an in-process fake (extend
the `tests/common` fake of `libs/upload` into a v2-capable one in
`libs/upload_v2/tests/common`, written from the specs) that records every
request, entry and peak concurrency.

| IDs | Test file | What it asserts |
| --- | --- | --- |
| PROTO-NEGOTIATE-001, 007 | `wire_v2/tests/version.rs` | `protocol`; `WIRE_VERSIONS == [1, 2]`, `PROTOCOL_VERSION == 1`, every v2 render has `"version":2` |
| PROTO-NEGOTIATE-002–005 | `wire_v2/tests/probe.rs` | valid lists, also with extra keys ignored; missing `versions`, unsorted, duplicate, empty, 17 entries, 0 and 65536 unexpected; 404, 401, 429, 503 with non-JSON bodies; 418 and 4 KiB + 1 |
| PROTO-NEGOTIATE-006 | `wire_v2/tests/version.rs` | the full mode × offered table |
| PROTO-BATCH-001 | `wire_v2/tests/paths.rs` | the five paths |
| PROTO-BATCH-002–004 | `wire_v2/tests/declare.rs` | exact bytes; `BatchFull` at 1001 entries and at the 8 MiB edge; one 10 000-reference entry fits an empty batch; round-trip at 1000 |
| PROTO-BATCH-005–010 | `wire_v2/tests/batch_parse.rs` | each error in rule order, with index and field, for declare, id and lease batches |
| PROTO-BATCH-011 | `wire_v2/tests/ids.rs` | render bytes, 1000 ids under 64 KiB, round-trip |
| PROTO-BATCH-012–014, 019 | `wire_v2/tests/batch_reply.rs` | request-level table; `Retry-After` values `0`, `86400`, `86401`, `-1`, HTTP date, absent; count and id mismatches; 4 MiB + 1 |
| PROTO-BATCH-015–018 | `wire_v2/tests/entry_results.rs` | each column of the result table, including `verified` in publish and `unknown_upload` in declare as unexpected; missing-list rules and the 10 000 total |
| PROTO-BATCH-020–021 | `wire_v2/tests/nar_reply.rs` | the NAR table, 404 split, 422 and 200 `open` unexpected, 64 KiB + 1 |
| PROTO-LEASE-008–009 | `wire_v2/tests/lease_batch.rs` | render bytes, round-trip at 64 entries, per-entry results, echo mismatch |
| PROTO-UPLOAD-008 | `wire/tests/reply.rs` | the declare row 404 `unknown_upload` is `Refused(UnknownUpload)` (no longer unexpected) |
| INGEST-QUEUE-025 | `ingest/tests/identity.rs` | root and dependency renewal write a fresh UUIDv7 with `nar_sent = false`, durable before return; a crash after the record write is repaired by `upload_state` in favor of the record; a `Retry` and an `Uploaded` outcome after renewal keep the new identity in `pending/` and `done/` |
| INGEST-SERVER-036 | `upload/tests/backend.rs` | a declare answered `unknown_upload` renews the identity, sends no NAR under the old one and declares with the new one; a second occurrence in the job is `Retry(Transient)` |
| INGEST-BATCH-035 | `upload_v2/tests/declare.rs` | a declare result `unknown_upload` renews the identity and declares it in the next round with no NAR under the old identity; a second occurrence ends transient; under the quota stop it ends as quota |
| INGEST-BATCH-001 | `ingest/tests/batch.rs` | worker passes one slice in order and applies outcomes in order; default impl calls `upload` per job |
| INGEST-BATCH-002–006, 029 | `upload_v2/tests/negotiate.rs` | no probe in `v1`; one probe per hour (fake clock); fallback to v1 on 404; probe failures retry every job with no upload request; `NotOffered`/`NoCommonVersion` messages; `NotSupported` mid-group lets running transfers finish, sends no further batch, keeps `nar_sent`, and the next call probes; a 401 probe resends once after a successful invalidate and otherwise retries every job |
| INGEST-BATCH-007–008 | `upload_v2/tests/group.rs` | union limit splits groups; each shared path declared once; missing root, closure error and oversized closure fail alone |
| INGEST-BATCH-009 | `upload_v2/tests/reconcile.rs` | `nar_sent` entries hit `v2/status` before any declare or NAR; each result's effect |
| INGEST-BATCH-010–011 | `upload_v2/tests/declare.rs` | chunking at 1000 entries; entry facts equal the v1 declaration; `mark_nar_sent` durable before the first NAR byte (fake server checks the queue file when the request starts); each result |
| INGEST-BATCH-012–013 | `upload_v2/tests/transfer.rs` | peak concurrency equals `max_inflight_nars`; start order; one failure does not stop others; each reply; uncertain keeps `nar_sent`; children reaped |
| INGEST-BATCH-014–016, 028 | `upload_v2/tests/publish.rs` | only ready paths sent, in order; a failed reference holds back its referrers (`waiting for a reference`, `nar_sent` kept, next job status then publish with no NAR); each result; a lost publish reply sends no NAR and the next job publishes through status |
| INGEST-BATCH-017–018, 030 | `upload_v2/tests/resend.rs` | two identical resends with recorded waits (`Retry-After` capped at 30, else 2 then 4); end kinds; `BadRequest`, `TooLarge`, `ReplyError` mapping; the fake records the peak of concurrent batch requests, which stays 1 |
| INGEST-BATCH-019–020, 025, 027–028, 031–034 | `upload_v2/tests/rounds.rs` | missing-reference round clears `done/` only for names outside the group; a name that is an undeclared reference, or a same-request entry that ended `published`/`present`, is a protocol violation; a `missing` name that is an earlier entry of the same request (refused for quota or verification) or another incomplete group path holds the referrer back without clearing any `done/` or reworking it, and a mixed list rounds only the outside names; a chain of held paths is carried together when its bottom reference is reworked, and carried paths reach only the publish phase; publishability is re-evaluated before each publish batch; after the quota stop a reworked path that needs a declare or NAR ends as quota and its held referrers end waiting; quota results from declare, NAR and publish end the path as quota; `unknown_upload` from NAR and publish clears `nar_sent` and declares again; publish `open` sends the NAR once more; second rework and fourth round end transient; a hostile fake that always asks for rework stays within the per-path counts, including a path held back across rounds 1, 2 and 3 whose `v2/status` count stays at most two (the fake fails the test on a third) |
| INGEST-BATCH-021–023 | `upload_v2/tests/stops.rs` | quota stop still publishes verified paths; credential error mid-transfer defers every incomplete job and reaps the dump; concurrent 401s invalidate once and retry each request once |
| INGEST-BATCH-024, 026 | `upload_v2/tests/outcome.rs` | precedence table; a complete job is `Uploaded` while another fails; `complete_path` for dependencies and root entry removal |
| INGEST-CONFIG-007–008 | `uploader_config/tests/config.rs`, `lease/tests/lease_config.rs` | defaults, accepted values, `Parse` and `InvalidLimit { field: "max_inflight_nars" }` |
| INGEST-LEASE-013–019 | `lease/tests/lease_v2.rs` | one batch in slot order and mapping; fallback to v1 PUTs in the same `auto` run; in `v2` a `NotSupported` fails every slot with no v1 request; resends then exit 75; probe failure and `NotOffered` exit 75 with no renewal; a 401 probe resends once after a successful invalidate; `auto` against `[1]`, 404 and `[1, 2]` |
| INTEGRATION-MODULE-027–028 | `tests/module-upload.test.nix`, `tests/module-lease.test.nix` | generated TOML values and defaults; assertion outside 1–16 |
| INTEGRATION-VM-029 | `tests/vm-{upload,upload-recovery,lease}.test.nix` (add the ID to one of them, keeping at most five per story) | each existing story starts the fake with `v1-only` and asserts a 404 probe and no `v2/` request |
| INTEGRATION-VM-030–032 | `tests/vm-upload-v2.test.nix` | request counts from the fake log, peak concurrency, no NAR for the held path, one lease batch, runtime fallback |
| INTEGRATION-VM-033–035 | `tests/vm-upload-v2-recovery.test.nix` | while `drop-publish` exists the job ends uncertain after its resends, and after removing it a later job publishes through status with every NAR sent once; status before resend after a restart, same identities; `busy-once` leaves `attempts` unchanged |

## Fake server changes (`tests/lib/upload_server.py`)

Keep the v1 behavior byte for byte and add, written from the specs only:

- `GET protocol`: 200 `{"versions":[1,2]}`, or 404 when `control/v1-only`
  exists (also 404 for every `v2/` route then).
- State: each upload gains `"verified"`; `v2/nar` sets it after verifying,
  `v2/publish` moves it to `published` and adds the path to `held`. Results
  follow the precedence `published` > `present` > `verified` > `open`;
  `conflict` only for an `id` with a different declaration or a path held
  under a different `nar_hash`. A v1 declare or status of a `verified`
  upload answers `open`; a v1 NAR to it publishes.
- `v2/declare`, `v2/publish`, `v2/status`, `v2/leases` with the protocol's
  limits and strict parsing (400 `bad_request`, 413 `too_large`), per-entry
  results in order, in-order publish with references checked against `held`
  and earlier entries, and `quota` applied per entry.
- Logging: one `requests.log` line per request
  (`POST v2/declare - entries=<n>`), one line per entry
  (`ENTRY v2/declare <id> <hash> <result>`), NAR lines as in v1 with
  `verified`; a `peak.log` holding the highest number of concurrent
  `v2/nar` bodies; `leases.log` lines as in v1.
- Identity horizon: a control file `forget-<UploadId>` makes the fake
  forget that identity and answer its declarations (v1 and v2) and status
  with `unknown_upload`, so the component tests' fake and the VM fake share
  one model.
- Controls: `drop-publish` (while the file exists, commit every publish
  batch, then close without replying; the test removes it), `busy-once` (answer the next batch
  request 429 with `Retry-After: 1` and remove the file), and the existing
  `stall-<hash>`, `present-<hash>`, `reject-<hash>`, `quota` and `down`
  applying to v2 routes too.
- Concurrency: the server is already threaded; guard state with the
  existing lock and never hold it while reading a NAR body.

## VM changes

- `tests/lib/vm.nix`: a `fakeProtocol ? "v2"` story argument; `"v1"` creates
  `control/v1-only` before the uploader and lease units start. The `upload`,
  `upload-recovery` and `lease` stories pass `"v1"`.
- New `tests/vm-upload-v2.test.nix` (VM-030–032) and
  `tests/vm-upload-v2-recovery.test.nix` (VM-033–035), each on both Nix
  pins, registered in `tests/flake.nix`; the twenty-path closure is a chain
  plus fan-out of trivial derivations so it builds quickly.
- Update the story count in the integration design only if it changes from
  eleven.

## Validation

- `nix fmt`; `git add -A` before any nix command.
- `nix develop . --command bloomery sync` after O0, O1 and O5 (new crates).
- `nix develop . --command bloomery check` passes with every new ID traced;
  commit the rewritten `.bloomery/code-health-summary.json`. Every crate
  scores at least 80 and regresses by at most 2.
- `nix flake check` passes, including the module checks and the eleven VM
  stories on Nix 2.34.8 and 2.31.5.
- Independent implementation review, then the slice record in `docs/` with
  the Bloomery run IDs, the non-claims (fake server only, no real-server
  evidence) and follow-ups.
