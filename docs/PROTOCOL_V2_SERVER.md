# Follow-up: server side of upload and lease protocol v2

The client specifies protocol v2 in
[`CLIENT/PROTO` design, "Upload and lease protocol v2"](../.bloomery/specs/CLIENT/PROTO/design/proto.md#upload-and-lease-protocol-v2)
(requirements `CLIENT-PROTO-NEGOTIATE-*`, `CLIENT-PROTO-BATCH-*`,
`CLIENT-PROTO-LEASE-008`–`009`). This note lists what a server must
implement to interoperate, as the public/private boundary requires. It is
written from this repository's specs only; it names no server, host or
deployment and does not import any server's types. Reconciliation of the
codecs with a server's own types is the server's follow-up.

## What to implement

| Item | Server behavior the client relies on |
| --- | --- |
| Probe | `GET protocol` under the tenant base URL answers 200 `{"versions":[1,2]}` (ascending, distinct). Clients ignore other keys of this one document, so it may grow; every other body keeps a closed key set. Read-only, same credential as uploads. A server that does not offer v2 may keep answering 404 |
| Keep v1 | v1 routes and semantics stay unchanged while any client may use them |
| One identity namespace | An `UploadId` names the same upload in v1 and v2 within a tenant, and identities are scoped per tenant: an id another tenant used is unknown here and never `conflict`. A v1 declare or status of an upload that is `verified` in v2 answers `open`, and a v1 NAR transfer to it publishes as v1 does. A server rolled back to v1 only either reads `verified` uploads as `open` or forgets them (`unknown_upload`); both are safe for clients |
| `v2/declare` | 1–1000 entries, 8 MiB body, strict parsing (request-level 400 `bad_request`, 413 `too_large` and nothing changed). Per-entry results in request order echoing `id`: `open`, `verified`, `present`, `published`, or `conflict`, `too_large`, `verification_failed`, `quota_exceeded`. A repeat of an existing declaration changes nothing. `conflict` means exactly: a known `id` with a different declaration, or the path held for the tenant under a different `nar_hash` (`present` never covers a different hash). No reference checks. Never charges quota; it may admission-check usage plus verified-pending bytes and answer `quota_exceeded` |
| `v2/nar/<id>` | One streamed NAR verified against the declaration. 200 `verified` only after the verified NAR is durable; `present`/`published` if already done. 404 `{"error":"unknown_upload"}` for an unknown identity. No 422. MUST cap verified-but-unpublished bytes per tenant in admission and answer 507 `quota_exceeded` beyond it; never charges quota |
| `v2/publish` | 1–1000 distinct ids, 64 KiB. Entries in request order; an entry publishes when verified, within quota and every reference other than itself is held or an earlier entry of the same request that ended `published` or `present`. Results: `published`, `present`, `open`, `unknown_upload`, `missing_references` (+ `missing`, at most 10 000 names per reply), `conflict`, `verification_failed`, `quota_exceeded`. All publications of the request are durable before the 200. Quota is charged here, and only here. Whether references may also be satisfied by another request merged into the same commit is the server's choice and invisible to clients |
| `v2/status` | Same body; strictly read-only. Results: `open`, `verified`, `present`, `published`, `unknown_upload`, `verification_failed` |
| `v2/leases` | 1–64 distinct `(machine, slot)` entries, 64 KiB; each entry means exactly a v1 lease renewal. Results echo `machine` and `slot`: `renewed`, `unknown_path`, `conflict`, `machines_full`. Durable before the 200 |
| States | Every result reports the highest state that holds: `published` > `present` > `verified` > `open` |
| Limits | A conforming server MUST accept every request within the protocol's limits (1000 entries, 8 MiB declare, 64 KiB id and lease batches). The client never shrinks a batch and treats a 413 as transient indefinitely |
| Statuses | Batch replies are 200 even when every entry is refused. 400 and 413 change nothing; 401 constant text; 429 or 503 optionally with `Retry-After` in delta-seconds, and like a lost connection they may follow a partial application, which idempotency covers; the client does not read those bodies |
| Idempotency | Every request may be resent identically, and any entry may reappear in a later batch, with no further state change |
| Identity horizon | MUST keep every upload identity for at least 7 days after its UUIDv7 timestamp; after that it may forget it. A declaration (v1 or v2) or status of a forgotten identity MUST answer `unknown_upload` (v1: 404 `{"error":"unknown_upload"}`; v2: the entry result), never `conflict`. The client then mints a fresh identity for the path and declares again, at most once per path per job |
| Retention of `verified` | MUST keep a verified, unpublished upload for at least 24 hours (the client's backoff cap defaults to one hour). A forgotten upload answers `unknown_upload`, and the client declares it again |
| Authority | No authority from identifiers: requests are authorized by the credential (`cache:read` and `cache:write`) and scoped to the base URL's tenant; another tenant's identities are `unknown_upload` |

## Cost model the design assumes

- One durable commit per `v2/declare`, `v2/publish` and `v2/leases` request,
  however many entries it carries; a declare or publish whose entries are
  all repeats needs no commit.
- One durable write per NAR transfer for the NAR bytes and its `verified`
  record. A server should record `verified` as a create-only object beside
  the content, not as an authority commit; the authority-level state change
  (publication, quota charging, catalog) is deferred to the publish batch.
  Concurrent transfers (four per client by default) can share writes.
- `GET protocol` and `v2/status` are reads.
- Quota is charged at publish only. Declare and NAR transfer may refuse
  with `quota_exceeded` from an admission check of usage plus verified,
  unpublished bytes, but never charge.
- Several small NARs per request body are deferred to a later revision.

## Client qualification

The client is qualified against `tests/lib/upload_server.py`, a fake that
implements this note from the specs and claims nothing about any real
server. End-to-end v2 evidence against a real server is deferred to that
server's own drill.
