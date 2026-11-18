# P2.09 W2 plan: authenticated server upload and lease renewal

Detailed plan for workstream W2 `upload` of the P2.09 client slice (root
`PLAN.md`, "Upload and lease protocol v1"). It implements the protocol table
and outcome mapping there exactly. The normative behavior is in the specs:

- [`CLIENT/PROTO` design](../../.bloomery/specs/CLIENT/PROTO/design/proto.md#upload-and-lease-protocol-v1),
  `requirements/UPLOAD.toml`, `LEASE.toml`, `HASH.toml` (HASH-005).
- [`CLIENT/INGEST` design](../../.bloomery/specs/CLIENT/INGEST/design/ingest.md)
  (Upload identities, Server backend, Lease renewal, configuration),
  `requirements/SERVER.toml`, `LEASE.toml`, `QUEUE.toml` (017–024),
  `CONFIG.toml` (005–006).
- [`CLIENT/INTEGRATION` design](../../.bloomery/specs/CLIENT/INTEGRATION/design/integration.md)
  (commands, Server upload and leases, Server upload stories),
  `CLI.toml` (013–014), `MODULE.toml` (018–024), `VM.toml` (019–028).

Commit subject: `feat: add authenticated server upload and lease renewal`.

## Outcomes, in order

| # | Outcome | Crates and files |
| --- | --- | --- |
| O1 | Protocol v1 codecs: SRI hash parsing, `UploadDeclaration`, upload paths, `parse_reply`, `MachineId`, `LeaseSlot`, `LeaseRequest`, `parse_lease_reply` | `libs/proto/src/{hash.rs,upload.rs,reply.rs,lease.rs,lib.rs}`, `libs/proto/Cargo.toml` (+`serde`, `serde_json`) |
| O2 | Queue upload identities (`uploads/`), dependency ledger, reconcile cleanup; server backend config variant | `libs/ingest/src/{identity.rs,queue.rs,worker.rs,config.rs,lib.rs}` |
| O3 | `stockhouse-upload` crate: endpoint (URL, audience, `no_proxy`, per-request timeouts) and HTTP helper, closure query and order, verifying NAR stream with child reaping, worklist-driven `ServerUploader` | new `libs/upload/src/{lib.rs,endpoint.rs,http.rs,closure.rs,nar.rs,backend.rs}` |
| O4 | Lease client: configuration, machine identity, slots, run and exit status | `libs/upload/src/lease/{mod.rs,config.rs,machine_id.rs,slots.rs}` |
| O5 | CLI: `lease --config`, server backend construction through `stockhouse_auth::source`, endpoint, source and audience errors exit 2 | `libs/cli/src/{args.rs,commands.rs}`, `libs/cli/Cargo.toml` |
| O6 | Module: `server` backend, `privateCredential` (sign-in `auth.unitCredential` or `LoadCredential`), shared hardening, backoff options, lease service, backstop timer, activation snippet and switch timer, assertions | `nix/module.nix`, new `tests/module-upload.test.nix`, `tests/lib/module.nix`, `tests/flake.nix` |
| O7 | VM: fake protocol v1 server and three stories on both Nix pins | new `tests/lib/upload_server.py`, `tests/lib/vm.nix`, new `tests/vm-{upload,upload-recovery,lease}.test.nix`, `tests/flake.nix` |
| O8 | Proxy upstream client: add `.no_proxy()` at `libs/proxy/src/upstream.rs` (`Client::builder()`, one line; W1's area only for credentials, flagged for linearization) | `libs/proxy/src/upstream.rs` |
| O9 | Docs and generated files: CLIENT README deferral row (done in design), `AGENTS.md` layout row for `libs/upload`, `bloomery sync`, code-health rewrite | `AGENTS.md`, `Cargo.toml`, `Cargo.lock`, `bloomery.lock`, `.bloomery/code-health-summary.json` |

The slice record `docs/P2_09_CLIENT_SERVER.md` is written at linearization,
not by this workstream.

## Crates and budgets

| Crate | SLOC now | Expected after | Note |
| --- | --- | --- | --- |
| `stockhouse-proto` | 1 111 | about 1 130 (implemented: the codecs exceeded the budget at 1 720, so they moved to `stockhouse-wire`, `packages/rust/libs/wire`) | If it exceeds 1 450 after O1, move `reply.rs` and `lease.rs` into a new private crate `stockhouse-wire` (`packages/rust/libs/wire`) depending on `stockhouse-proto`, and point the specs' "system" wording at it unchanged (the crate split is not a behavior change) |
| `stockhouse-ingest` | 1 245 | about 1 420 | `identity.rs` about 140, `Server` config variant about 40. If over 1 450, move `identity.rs` with a narrow `pub` staging API into `stockhouse-upload` |
| `stockhouse-upload` (new) | 0 | about 1 250 | `packages/rust/libs/upload`, `publish = false`; added to workspace `members` and `[workspace.dependencies]` |
| `stockhouse-cli` | 307 | about 380 | |

Keep functions under 250 tokens and complexity 10: the per-path state
machine in `backend.rs` is one small function per step (declare, send,
reconcile, missing-reference round) returning a `Step` enum (`Complete`,
`SendNar`, `Declare`, `Reconcile`, `Push(Vec<StorePath>)`,
`Done(UploadOutcome)`), driven by one loop over an explicit `VecDeque`
worklist. Per path it tracks a request budget of 8 (16 HTTP requests with
the single post-`invalidate` retries) and two once-per-path-per-job flags
(missing-reference round, declare again after `unknown_upload`), so the
bounds in SERVER-023/029/035 are structural.

## Interfaces

- `stockhouse_proto`: `Sha256::parse_sri`; `UploadDeclaration::{new, render,
  parse}`, `DeclarationError`, `MAX_REFERENCES`, `MAX_BODY` (4 MiB);
  `upload_path(id)`, `nar_path(id)`; `RequestKind::{Declare, Nar, Status}`,
  `parse_reply(kind, status, &[u8]) -> Result<UploadReply, ReplyError>`,
  `UploadState`, `UploadRefusal`; `MachineId::{parse, from_bytes}`,
  `LeaseSlot::new`, `lease_path(id, slot)`, `LeaseRequest::{new, render,
  parse}`, `parse_lease_reply`, `LeaseReply`, `LeaseRefusal`.
- `stockhouse_ingest`: `Queue::{upload_state, mark_nar_sent, clear_nar_sent,
  complete_path, is_uploaded, forget_done}`, `UploadEntry` (`id`,
  `nar_sent`), `BackendConfig::Server(ServerBackendConfig)` whose
  `credential` is W1's serde `stockhouse_credentials::CredentialSpec`
  (`file | systemd | server`), not a mirror. The `Uploader` trait and
  `UploadOutcome` are unchanged.
- `stockhouse_upload`: `ServerEndpoint::new(&str, Box<dyn CredentialSource>,
  u64) -> Result<_, EndpointError>` with `EndpointError::{InvalidUrl,
  Cleartext, AudienceMismatch}`, `ServerUploader::new(endpoint, nix,
  max_closure_paths, queue) -> impl Uploader`, `parse_closure`,
  `LeaseConfig::parse`, `run_lease(&LeaseConfig, log) -> u8`.
- W1 (frozen): `CredentialSource::invalidate(&self, rejected: &Secret) ->
  bool` (default `false`); `stockhouse_auth::source(&CredentialSpec)` builds
  every source (`stockhouse_credentials::source` reports `Unsupported` for
  `Server`); `CredentialError` variants including `NotConfigured`,
  `SignedOut`, `RefreshUnavailable`, `Unsupported`. For the audience check
  the CLI needs the sign-in configuration's private tier URL from
  `stockhouse_auth`; W2 assumes an accessor on W1's loaded auth
  configuration (name to be taken from W1's crate at implementation). The
  W2 side is a pure comparison in `ServerEndpoint::check_audience(&Url)`.

## Dependencies

No new crate enters `Cargo.lock`.

| Dependency | Pin | Where | Justification |
| --- | --- | --- | --- |
| `serde`, `serde_json` | existing workspace pins | `stockhouse-proto` | strict JSON codecs with `deny_unknown_fields`; already locked and used by ingest |
| `reqwest` feature `blocking` | existing `=0.13.5`, `default-features = false`; feature added in `libs/upload/Cargo.toml` only | `stockhouse-upload` | the uploader and lease command are synchronous; `blocking::Body::sized(reader, len)` streams a `Read` with an exact `Content-Length`. Its extra dependencies (`futures-channel`, `futures-util` `io`/`sink`, `tokio/sync`) are already locked. The blocking client has no read timeout, so the client sets `.timeout(None)`, `connect_timeout`, `tcp_keepalive(60 s)` and `no_proxy()`, and every request sets `RequestBuilder::timeout`: `request_timeout` for declare, status and lease, `request_timeout + ceil(nar_size / 256 KiB)` s for a NAR. The uploader and lease command never run inside a tokio runtime (the CLI's runtime exists only for `proxy`) |
| `getrandom` | `=0.4.3`, `default-features = false`, new `[workspace.dependencies]` entry | `stockhouse-upload` | 16 bytes for the machine identity from the OS source; already locked through `uuid`'s v7 generator |
| `libc`, `toml` | existing workspace pins | `stockhouse-upload` | `O_NOFOLLOW` and owner/mode checks for the machine identity; lease configuration |
| `stockhouse-credentials` | path | `stockhouse-ingest` | the serde `CredentialSpec` in the uploader configuration |
| `stockhouse-auth` (W1) | path | `stockhouse-upload`, `stockhouse-cli` | `stockhouse_auth::source` and the sign-in configuration's private tier for the audience check |
| `stockhouse-upload` | path | `stockhouse-cli` | backend construction and `lease` |

## Tests per requirement

Rust tests trace IDs with `#[cfg_attr(any(), bloomery("<ID>"))]` and assert
exact variants. Nix tests list IDs in `passthru.bloomery`.

### `stockhouse-proto` (`libs/proto/tests/`)

| ID | Test |
| --- | --- |
| HASH-005 | `hash.rs`: SRI of `sha256("")` equals the nix32 form; `sha1-…` is `UnsupportedAlgorithm`; 43/45 chars and a 31-byte payload are `InvalidEncoding`; `Sha256::parse("sha256-…")` fails |
| UPLOAD-001 | `upload.rs`: exact rendered bytes for a fixture with and without deriver/CA |
| UPLOAD-002 | `upload.rs`: render then parse equals, for a table of declarations including 10 000 references of 244-byte names and a 1 KiB `ca` (rendered under 4 MiB) |
| UPLOAD-003 | `upload.rs`: non-object, unknown key, missing key, repeated key, `nar_size` as string: each `Malformed` |
| UPLOAD-004 | `upload.rs`: `version: 2` is `UnsupportedVersion` |
| UPLOAD-005 | `upload.rs`: one case per field rule through both `parse` and `new`, asserting `InvalidField { field }` |
| UPLOAD-006 | `upload.rs`: 4 MiB + 1 bytes of whitespace-padded JSON is `TooLarge` |
| UPLOAD-007 | `upload.rs`: paths for a fixed `UploadId` |
| UPLOAD-008/009/010 | `reply.rs`: the full design table per kind, one assertion per row (including 400 `verification_failed` for all three kinds and 400 `bad_request` for `Nar`) |
| UPLOAD-011 | `reply.rs`: off-table pairs (Declare 404, Nar 200 `open`, Status 409), an extra key, a `version` key, a JSON array: `Unexpected { status }` |
| UPLOAD-012 | `reply.rs`: 401 and 503 with HTML and empty bodies |
| UPLOAD-013 | `reply.rs`: empty, 10 001 entries, duplicate, full path instead of basename |
| UPLOAD-014 | `reply.rs`: 4 MiB + 1 body; a 10 000-entry `missing` list under the cap parses |
| LEASE-001..007 (PROTO) | `lease.rs`: identity parse/render including uppercase and 31/33 chars; `from_bytes`; slots 0, 1, 10, 11; path; request codec cases; the lease reply table and off-table cases |

### `stockhouse-ingest` (`libs/ingest/tests/`)

| ID | Test |
| --- | --- |
| QUEUE-017 | `identity.rs`: create on a groundwork-layout queue adds `uploads/`, `FORMAT` bytes unchanged; `open` without `uploads/` succeeds |
| QUEUE-018 | `identity.rs`: two calls and a call on a reopened `Queue` return one identity; a pre-placed entry wins over a concurrent create; a dependency with a pending record gets that record's identity |
| QUEUE-019 | `identity.rs`: a dependency entry then a root call replaces it with the record identity and `nar_sent = false` |
| QUEUE-020 | `identity.rs`: garbage, version 2 and wrong-path entries move to `failed/<hash>.upload.corrupt` byte-identical and `status().failed` counts them |
| QUEUE-021 | `identity.rs`: mark then reopen reads `true`; clear then reopen reads `false` |
| QUEUE-022 | `identity.rs`: after `complete_path` the done record has the entry's identity and the entry is gone; with a fault-injected removal the done record still exists |
| QUEUE-023 | `retry.rs` (worker): entries with a done record and with a nonexistent store path are removed by a pass; another entry is untouched |
| QUEUE-024 | `identity.rs`: `forget_done` removes the done record; `is_uploaded` is then false |
| CONFIG-005/006 | `config.rs`: the design TOML loads with defaults; unknown field, no credential, zero limits |

### `stockhouse-upload` (`libs/upload/tests/`)

Fixtures in `tests/common/`: a threaded `std::net` HTTP/1.1 fake server with
scripted replies per request, recording method, path, headers, body length,
body SHA-256 and whether the body arrived complete, with actions "reply",
"close without reply after the body" and "redirect"; a fake `nix` shell
script that answers `path-info` with canned JSON and `store dump-path <p>`
with a fixture NAR file per path (or a configured short, long or failing
dump) and records its arguments; temporary store directories created under
the test's temp dir with paths faked through a `store_root` test hook
(`ServerUploader::with_store_root`, `#[doc(hidden)]`, used only for the
`lstat` checks).

| ID | Test |
| --- | --- |
| SERVER-001/002 | `endpoint.rs`: tables of URLs to `InvalidUrl`, `Cleartext`, accepted (`https://cache.example.invalid/t/1/`, `http://127.0.0.1:1/t/1/`, `http://[::1]:1/`); `http://localhost/` is `Cleartext` |
| SERVER-003 | `backend.rs`: every recorded request path starts with the base path and carries `Bearer <credential>`; a 302 reply is not followed and yields `Retry(Transient)`; with `HTTPS_PROXY`/`HTTP_PROXY`/`ALL_PROXY` pointing at a second fake server (set in a dedicated test binary), that server records nothing |
| SERVER-004 | `backend.rs`: failures across every mapping row; no outcome message contains the credential or a marker string the fake puts in every body |
| SERVER-005 | `backend.rs`: a fake source returning each `CredentialError` variant on first read: `Deferred` naming the kind, zero requests, attempts unchanged through the worker |
| SERVER-006 | `backend.rs`: a source failing on its second read (before the `PUT`) and after `invalidate`: `Deferred`, no `PUT` recorded, entry keeps `nar_sent = true` |
| SERVER-007 | `backend.rs` and `lease.rs`: a fake source whose `invalidate` returns true: 401 then 200 completes, two requests; 401 twice is `Retry(Transient)` (lease: slot failed); `invalidate` returning false: one request, `Retry(Transient)` |
| SERVER-008 | `closure.rs`: absent root is `Rejected`; fake `nix` records no invocation |
| SERVER-009 | `closure.rs`: recorded arguments |
| SERVER-010 | `closure.rs`: parse fixtures captured from Nix 2.31.5 and 2.34.8 plain `--json`, and a version 2 envelope |
| SERVER-011 | `closure.rs`: each bad shape, dangling reference, missing root, a cycle, 64 MiB + 1 output, nonzero exit: `Retry(Transient)`, message at most 512 bytes, no request |
| SERVER-012 | `closure.rs`: `max_closure_paths = 2` with three entries |
| SERVER-013 | `backend.rs`: a diamond closure; declaration order recorded |
| SERVER-014 | `backend.rs`: a ledgered dependency has no request |
| SERVER-015 | `backend.rs`: recorded declaration body equals the expected render, `Content-Type` |
| SERVER-016 | `backend.rs`: `present` and `published` give no `PUT`; dependency done record written |
| SERVER-017 | `backend.rs`: the fake server's handler for `PUT` reads the `uploads/` entry and records `nar_sent = true` |
| SERVER-018 | `nar.rs`: headers and `Content-Length`; recorded body hash equals the fixture; dump arguments |
| SERVER-019 | `nar.rs`: a counting reader under the verifier with a 16 MiB generated NAR: the verifier never holds more than two chunks (instrumented high-water mark) |
| SERVER-020 | `nar.rs`: short, long and altered dumps: the fake records an incomplete body and the outcome is `Rejected` |
| SERVER-021 | `nar.rs`: a dump exiting 1 after full output, and an unstartable `nix`: incomplete body, `Retry(Transient)`; a fake dump that sleeps, with an early 413 reply and with a refused connection: the dump pid no longer exists after `upload` returns (killed and reaped), and the dump inherits null stderr |
| SERVER-022 | `backend.rs`: declare 422 naming a ledgered reference: its done record is removed first (checked by the fake server's handler), the reference is uploaded, the referrer re-declared, `Uploaded`, job `attempts` unchanged; a nested round (the reference itself answered 422) also completes |
| SERVER-023 | `backend.rs`: two 422s, two `unknown_upload`s, and a 422 naming an undeclared path |
| SERVER-024 | `backend.rs`: each permanent code on declare, NAR and status, including `verification_failed` on declare and status and `bad_request` on NAR |
| SERVER-025 | `backend.rs`: 507 on declare and on NAR |
| SERVER-026 | `backend.rs`: 503, refused connection, total timeout (fake sleeps past a 1 s `request_timeout`), off-table reply on declare |
| SERVER-027 | `backend.rs`: close-without-reply after the body and an off-table reply after the body: `Retry(Uncertain)`, entry still `nar_sent = true` |
| SERVER-028 | `reconcile.rs`: with `nar_sent = true`: `published` (no `PUT`), `open` (`PUT`), `unknown_upload` (mark cleared, `POST`), `verification_failed` (`Rejected`), 503 (`Retry(Uncertain)`) |
| SERVER-029 | `reconcile.rs`: `PUT` 404 and `GET` 404 each lead to one new `POST` and then success |
| SERVER-030 | `reconcile.rs`: a job failing transiently, a new `ServerUploader` over a reopened queue, the same identities recorded per path |
| SERVER-031 | `backend.rs`: the second of three paths fails: `Retry`, the third path never requested |
| SERVER-032 | `endpoint.rs`: `check_audience` with an equal and a different private tier URL: `AudienceMismatch`, no request |
| SERVER-033 | `endpoint.rs`: the per-request timeout computed for NarSize 0, 1, 256 KiB and 256 KiB + 1; a fake server stalling a `POST` reply beyond 1 s yields a timeout within 3 s; a fake server reading a `PUT` body slower than the floor is cut at the computed bound |
| SERVER-034 | `backend.rs`: a counting source records reads at job start and immediately before each `PUT` (one per NAR) |
| SERVER-035 | `backend.rs`: a fake answering `unknown_upload` and `open` alternately: exactly 8 requests for the path, then `Retry(Transient)` |
| INGEST-LEASE-001/002 | `lease_config.rs`: design TOML; each error variant |
| INGEST-LEASE-003/004/005 | `lease.rs`: first run creates mode 0600 content of 33 bytes; second run identical bytes and inode; symlink, 0640, wrong length, uppercase: `MachineId` error, no request, file unchanged (owner check through an injectable euid as in `OwnerOnlyFile::with_euid`) |
| INGEST-LEASE-006/007/008 | `lease.rs`: a temp profile directory with `system-1-link`, `system-2-link` and a `current_system` symlink into a fake store root; current equal to the newest; no other generation; an unresolvable extra path |
| INGEST-LEASE-009 | `lease.rs`: recorded paths and bodies |
| INGEST-LEASE-010/011 | `lease.rs`: slot 1 refused and slot 2 answered 503: both requested, exit 75; all renewed: 0; each credential error: 75 with no request; bad identity file: 1 |
| INGEST-LEASE-012 | `lease.rs`: two runs record identical request sequences; the state directory listing is unchanged by the second |

### `stockhouse-cli` (`libs/cli/tests/cli.rs`)

| ID | Test |
| --- | --- |
| CLI-013 | `lease --config` with a config pointing at a refused port returns 75; `lease` without `--config`, and `lease --config x --once`, return 2 |
| CLI-014 | `uploader --config` and `lease --config` with `http://cache.example.invalid/` (`Cleartext`), with a `server` credential naming a missing auth configuration, and with one whose private tier differs (`AudienceMismatch`): each returns 2 and creates no queue directory |

### Module and VM (`tests/`)

| ID | Test |
| --- | --- |
| MODULE-018..024 | new `tests/module-upload.test.nix`: generated uploader and lease TOML contents (`grep`) with and without `auth.enable` (using `auth.unitCredential`), unit attributes including the shared hardening, both timers' attributes, the activation snippet text, absence of `wantedBy`/`requiredBy`/`before` on the service and switch timer, and `hasFailure` cases for each assertion |
| VM-019, VM-020, VM-021 | `tests/vm-upload.test.nix` |
| VM-022, VM-023, VM-024, VM-028 | `tests/vm-upload-recovery.test.nix` |
| VM-025, VM-026, VM-027 | `tests/vm-lease.test.nix` |

`tests/lib/vm.nix` gains `story = story: { script, passthru, backend ?
"nix-copy" }`. With `backend = "server"` the node points `proxy.private` at
`http://127.0.0.1:8486/t/1/` with `credentialFile =
/var/lib/stockhouse-vm/private-token` (generated by the existing token
oneshot), runs `stockhouse-vm-fake.service`
(`python3 tests/lib/upload_server.py`, `ThreadingHTTPServer`, state under
`/var/lib/stockhouse-fake`), and sets `uploader.backend = "server"`,
`uploader.backoffMax = 10`, `lease.randomizedDelaySec = 0`. Story outlines:

- `upload`: build `a`, then `b` whose output contains `${a}`; assert log
  order `POST a` before `POST b`, both published, `b`'s declaration lists
  `a` (VM-019). Mark `c` present via control file, build `d` referencing `c`
  and the already uploaded `a`: no `PUT c`, no request for `a` (VM-020).
  Control `reject-<hash>` then build `e`: `failed/<hash>.json` reason names
  `verification_failed` (VM-021).
- `upload-recovery`: control `stall-<hash>` for `f`, wait for the stalled
  `PUT` in the log, `systemctl restart stockhouse-uploader`, release the
  stall; log shows `GET` then `PUT` with the first identity, one distinct
  identity for `f` (VM-022). Control `drop-<hash>` for `g`: record shows
  `last_error` uncertain, then done after one `GET`, `PUT g` count 1
  (VM-023). Control `quota`, build `h`: pending with quota `last_error`;
  remove control: done with the original identity (VM-024). Stop the
  uploader, build `i`, reboot, done on the fake server with the pending
  identity (VM-028).
- `lease`: the node sets `lease.randomizedDelaySec = 0`. After boot the lease log has `PUT leases/<id>/1` with
  `/run/current-system`, the identity file is `stockhouse` `0600`; after a
  reboot the same identity (VM-025). Set two system profile generations with
  `nix-env -p /nix/var/nix/profiles/system --set` (base, then the
  `upgraded` specialisation), switch to `upgraded`
  (`switch-to-configuration test`): `stockhouse-lease-switch.timer` fired
  (its `LastTriggerUSec` is set) and within 90 s slot 1 is the upgraded
  system and slot 2 the base (VM-026). Stop the fake server, switch back
  and reboot: both succeed; after the switch timer fires,
  `systemctl show -p ExecMainStatus -p Result stockhouse-lease.service`
  reports `ExecMainStatus=75` and `Result=success`; a newly built output
  stays pending (VM-027).

VM-010 already requires every VM requirement on both pins; the new stories
use the same version matrix.

## Validation

1. `git add -A` new files, `nix fmt`.
2. `nix develop . --command bloomery sync` after adding the crate and the
   `getrandom` workspace entry; commit the regenerated `Cargo.lock` and
   `bloomery.lock`.
3. `CARGO_TARGET_DIR=$HOME/.cache/stockhouse-p2-09-upload-target nix develop . --command bloomery check`
   until green; commit the rewritten `.bloomery/code-health-summary.json`
   (every crate at least 80, no regression over 2 points).
4. `nix flake check` (module evaluation and the seven VM stories on Nix
   2.34.8 and 2.31.5).
5. Independent implementation review, then `jj new -A @`.

## Decisions for review

Settled by the design review are marked "(ruled)".

1. **Responses carry no `version` key** (ruled); a response with one is
   `Unexpected`.
2. **`PUT …/nar` 400 `bad_request` is `Rejected`** (ruled), as are
   `verification_failed` on declare, NAR and status requests (a NAR that
   already failed verification is permanent).
3. **401 and 503 bodies are not parsed** (intermediaries answer those).
4. **Unexpected or malformed replies** are `Retry(Transient)` before the NAR
   is fully sent and `Retry(Uncertain)` after; both are bounded by
   `max_attempts`.
5. **Missing-reference rounds run through an explicit worklist** (no
   recursion): the named paths' done records are removed first, the paths
   and then the referrer go back to the front, and the round uses no
   attempt. It and "declare again after `unknown_upload`" run at most once
   per path per job; a repeat, or an undeclared name, is
   `Retry(Transient)`.
6. **Per-path request budget: 8** declare, NAR and status requests per job
   (16 HTTP requests counting the single post-`invalidate` retries); beyond
   it, `Retry(Transient)`. It sits above the longest legitimate sequence
   (`GET`, `POST`, `PUT`, `POST` after `unknown_upload`, `PUT`, a re-declare
   after a missing-reference round) so it only stops a misbehaving server.
7. **Caps:** declarations and upload replies are capped at 4 MiB, above the
   largest valid declaration (under 3 MiB at 10 000 references) and the
   largest valid `missing` list, so UPLOAD-002 holds at the limits. Lease
   requests stay at 4 KiB and lease replies at 64 KiB.
8. **Identities:** the queued path uses the record identity (keeping
   RETRY-008 and VM-006 true); a dependency's create-once `uploads/` entry
   takes the identity of its own pending record when it has one, otherwise a
   fresh one. A completed dependency is written to the done ledger.
   Quarantined entries land in `failed/` and count in `status.failed`.
9. **Ledgered dependencies are skipped without asking the server.** If the
   server lost one, the referrer's `missing_references` removes that ledger
   entry and re-uploads it. Switching an existing queue from `nix-copy` to
   `server` keeps its ledger: those roots are not re-uploaded (non-claim;
   clear `done/` to re-upload).
10. **Local inconsistencies:** a root missing from the store, a closure over
    `max_closure_paths` (default 50 000) and a dumped NAR that does not match
    its NarHash are `Rejected`; a failing `nix` command is
    `Retry(Transient)`. The dump child is killed and reaped on every early
    exit, with null stdin and stderr.
11. **`nix store dump-path`** (with the configured `nix`) instead of
    `nix-store --dump`; the closure uses plain `--json` on both pins, with
    the version 2 envelope accepted (string or null `ca` only). SRI parsing
    is a separate `Sha256::parse_sri`, so narinfo parsing is unchanged.
12. **Cleartext `http` only to loopback IP literals**, never `localhost`,
    matching the proxy. No environment proxy is used by the upload, lease or
    proxy upstream clients (`no_proxy()`).
13. **Every credential error defers** (ruled): `Deferred("credential
    unavailable: <kind>")`, no attempt consumed, at any read. The credential
    is read once per job, after a successful `invalidate`, and immediately
    before each NAR upload (access tokens live about ten minutes). A 401
    retries once only when `invalidate` returns true; a persistent 401 is
    `Retry(Transient)`.
14. **Audience** (ruled): a `server` credential whose private tier differs
    from the base URL is refused at startup (exit 2).
15. **Lease slot 2** is the largest-numbered system generation whose target
    differs from the current system; absent means skipped.
16. **Lease exit 75 is success for systemd** (`SuccessExitStatus = 75`):
    refused or failed slots (including a full machine table, 503) and any
    credential error. A machine identity error exits 1 and fails the unit,
    because it needs an operator. The identity is never regenerated
    automatically.
17. **Lease triggers** (ruled): an activation snippet restarts
    `stockhouse-lease-switch.timer` with `--no-block` and `|| true`;
    that timer (`OnActiveSec = 30s`, `AccuracySec = 1s`, not wanted by any
    target) starts the service after activation has replaced
    `/run/current-system`. The backstop `stockhouse-lease.timer` has
    `OnBootSec = 1min`, `OnUnitActiveSec = 1h`, `OnCalendar = daily`,
    `Persistent` and `RandomizedDelaySec` default 600. The service itself is
    wanted by no target.
18. **Leases do not upload.** A system closure the server does not hold is
    `unknown_path`, exit 75, renewed by a later trigger once the uploader
    has caught up.
19. **`Retry-After` is ignored**; the worker's backoff schedules retries.
20. **Follow-up: a persistent 401 or quota exhaustion burns attempts**
    (existing RETRY design): with the defaults a record fails after about 20
    hours of either. Kept unchanged; to be logged as a follow-up in the
    slice record.
21. **Defaults:** the backend stays `deferred`; `lease.enable` defaults to
    `backend == "server"`. The `deferred` message keeps naming P2.08
    (BACKEND-001 unchanged). New `uploader.backoffBase`/`backoffMax` options
    default to today's fixed values.
22. **Shared hardening:** the uploader unit gains the same hardening set as
    the lease unit (`ProtectSystem = strict`, `ProtectHome`, `PrivateTmp`,
    `NoNewPrivileges`, `RestrictAddressFamilies`); MODULE-007 is unaffected.

## Non-claims

- No VM story uses the sign-in credential for the uploader or lease client;
  they use `LoadCredential`. The private server's drill covers the
  combination end to end.
- No end-to-end evidence against a real server; the fake server is a test
  fixture of this repository's protocol spec.
- No chunk-level deduplication, ranges or resume.

## Interfaces with W1 (ruled)

- **401 hook:** `CredentialSource::invalidate(&self, rejected: &Secret) ->
  bool`, default `false`, provided by W1. W2 retries once only on `true`.
- **Credential configuration:** W1's serde `CredentialSpec` (`file |
  systemd | server = "<auth configuration file>"`), built only with
  `stockhouse_auth::source`. A build, configuration or audience error at
  startup is exit 2.
- **Errors:** every `CredentialError` is a deferral in the uploader and exit
  75 in the lease client.
- **Module:** `privateCredential` reads W1's internal
  `services.stockhouse.auth.unitCredential = { toml; serviceConfig; }` when
  `services.stockhouse.auth.enable`, for both the uploader and
  `stockhouse-lease.service`; otherwise `systemd = "private"` plus
  `LoadCredential`. Check the exact name against W1's INTEGRATION design
  (vyyyxwyp) when implementing. Sign-in derives its private tier from
  `proxy.private.url`; its default scopes must include `cache:read
  cache:write`.
- **Shared files** edited in W2-only sections: `nix/module.nix`,
  `libs/cli/src/{args.rs,commands.rs}`, workspace `Cargo.toml`,
  `tests/lib/vm.nix`, `tests/flake.nix`, `.bloomery/specs/CLIENT/README.md`,
  `INTEGRATION/{README.md,design/integration.md}`, `AGENTS.md`, and the
  one-line `.no_proxy()` in `libs/proxy/src/upstream.rs` (flagged).

## Follow-ups

- **Quarantine overwrite:** a second corrupt `uploads/<hash>.json` for the
  same path overwrites the earlier `failed/<hash>.upload.corrupt`; keep a
  bounded history if operators need it.
- **Entries of rejected roots:** a rejected root keeps its `uploads/` entry
  (and its dependencies' entries) until the store path is collected locally;
  reconcile could drop entries whose record is in `failed/`.
- **Crate budgets:** `stockhouse-ingest` (about 1 510 SLOC) and
  `stockhouse-upload` are near the 1 500 SLOC soft limit; split the queue's
  identity store or the lease client into their own crates before adding
  more.
- **401 and quota exhaustion burn attempts** under the existing retry design;
  revisit with the next retry change.

