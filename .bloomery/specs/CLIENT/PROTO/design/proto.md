# Protocol design

`stockhouse-proto` is a pure library with no I/O. It encodes the public Nix
binary-cache facts the client relies on and nothing from the private server.
Every parser is strict: input that Nix itself might tolerate but that the
client cannot verify is rejected with a typed error rather than guessed at.

## Versioning

`PROTOCOL_VERSION` is the public client protocol version, currently `1`. It
versions the client's own formats (proxy and uploader configuration files,
queue records) independently of the Nix wire formats, which are fixed by Nix.
A reader accepts exactly the versions it knows; any other version is
rejected with `UnsupportedVersion { found, supported }`. Error enums are
`#[non_exhaustive]` so new variants are additive.

`UploadId` is the client's upload identity: a UUIDv7 rendered in lowercase
hyphenated form. Parsing accepts only that form with version 7; anything else
is `UploadIdError::Invalid`. Identities are opaque and carry no authorization.

## Store paths

Only the store directory `/nix/store` is supported. A store path is
`/nix/store/<hash>-<name>` where:

- `<hash>` is exactly 32 characters of the Nix base-32 alphabet
  `0123456789abcdfghijklmnpqrsvwxyz`;
- `<name>` is 1–211 bytes of `[A-Za-z0-9+\-._?=]` and does not start with `.`;
- there is no trailing slash or further path component.

Errors (`StorePathError`): `NotInStore` (prefix differs, including other store
directories), `InvalidHash` (length or alphabet), `InvalidName` (empty, too
long, bad character, leading dot, or a `/`). The same rules parse a store
path basename (`<hash>-<name>`, used by `References`) and a bare
`StorePathHash` (used by narinfo request routes).

## Hashes

A `Sha256` digest is parsed from `sha256:<52 Nix base-32 chars>` or
`sha256:<64 lowercase hex chars>`. Its canonical rendering is always
`sha256:<nix32>`, the form Nix uses inside signature fingerprints.
Errors (`HashError`): `UnsupportedAlgorithm` (prefix other than `sha256:`),
`InvalidEncoding` (wrong length or characters, or a non-canonical Nix base-32
string whose unused high bits are set).

`Sha256::parse_sri` separately accepts the SRI form `sha256-<base64>` that
`nix path-info --json` prints: exactly 44 characters of standard padded
base64 decoding to 32 bytes. Any other prefix is `UnsupportedAlgorithm`; any
other payload is `InvalidEncoding`. `Sha256::parse`, and therefore narinfo
parsing, does not accept SRI.

Nix base-32 follows Nix exactly: for `n` from `len-1` down to `0` it emits the
character for the 5-bit group starting at bit `5n` of the little-endian digest
bytes. A 32-byte digest yields 52 characters; `sha256("")` is
`0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73`.

## narinfo

A narinfo is UTF-8 text of `Key: value` lines, at most 1 MiB. Accepted keys:

| Key | Rule |
| --- | --- |
| `StorePath` | required; a valid store path |
| `URL` | required; a relative path of `[A-Za-z0-9+\-._/~=]`, no empty, `.` or `..` segment, no leading `/`, no scheme |
| `Compression` | optional; one of `none`, `xz`, `bzip2`, `zstd`, `br`, `gzip`; absent means `bzip2` as in Nix |
| `FileHash` | optional; `Sha256` |
| `FileSize` | optional; decimal `u64` |
| `NarHash` | required; `Sha256` |
| `NarSize` | required; decimal `u64` greater than zero |
| `References` | optional; space-separated store path basenames without duplicates, may be empty |
| `Deriver` | optional; a store path basename ending in `.drv`, or `unknown-deriver` |
| `Sig` | optional, repeatable; a signature |
| `System` | optional; free text without control characters |
| `CA` | optional; free text without control characters |

Every key except `Sig` appears at most once. `FileHash` and `FileSize` appear
together or not at all. Errors (`NarInfoError`): `TooLarge`, `NotUtf8`,
`MalformedLine { line }` (no `: ` separator or empty key; `Key:` with nothing after the colon is an empty value, as Nix writes `References: ` for none), `UnknownField`,
`DuplicateField`, `MissingField`, `InvalidField { field }` (a value that
violates its rule, including a lone `FileHash` or `FileSize`).

`NarInfo::render` emits the canonical text in the table order with one `Sig`
line per signature, so a parsed narinfo round-trips through `parse(render)`
to an equal value. The proxy relies on `with_url` to replace only `URL`.

## nix-cache-info

`nix-cache-info` uses the same line syntax, at most 4 KiB. `StoreDir` is
required and must equal `/nix/store`; `WantMassQuery` is optional `0` or `1`;
`Priority` is an optional decimal `u32`. Errors (`CacheInfoError`):
`TooLarge`, `NotUtf8`, `MalformedLine`, `UnknownField`, `DuplicateField`,
`MissingField`, `InvalidField`, `StoreDirMismatch { found }`.

## Keys and signatures

A public key is `<name>:<base64 of 32 bytes>` and a signature is
`<name>:<base64 of 64 bytes>`, using standard padded base64. Errors
(`KeyError`): `Malformed` when there is no `:`; `InvalidName` when the name is
empty or contains whitespace; `InvalidBase64` when the payload is not padded
base64; `InvalidLength` for any other byte count; and `InvalidKey` for key
bytes that are not a valid Ed25519 point.

The signed fingerprint is
`1;<StorePath>;<NarHash canonical>;<NarSize>;<References as full store paths, sorted bytewise, joined by ,>`.
References are sorted because Nix signs a sorted set, not narinfo order.

`TrustedKeys::verify(narinfo)` returns the name of the key that verified, or
a `SignatureError`:

1. If any signature names a trusted key and fails strict Ed25519 verification
   over the fingerprint: `InvalidSignature { key }`. A bad signature from a
   trusted name is evidence of tampering and is never outweighed by another
   good signature.
2. Otherwise, if a signature names a trusted key: success with that name.
3. Otherwise, if the narinfo has no signatures: `Unsigned`.
4. Otherwise: `UntrustedSigner`.

Two trusted keys with the same name are a configuration error,
`KeyError::DuplicateName`.

## Content integrity

`ContentVerifier::new(expected_size, expected_sha256)` consumes bytes
incrementally. `update` returns `IntegrityError::SizeExceeded` as soon as more
than the expected number of bytes arrive. `finish` returns
`IntegrityError::SizeMismatch { expected, actual }` if fewer bytes arrived and
`IntegrityError::HashMismatch` if the SHA-256 differs; otherwise `Ok`.

`NarInfo::served_integrity` returns what a proxy can check over the served
representation: `Some(FileHash, FileSize)` when present, otherwise
`Some(NarHash, NarSize)` when `Compression` is `none`, otherwise `None`.

This is a consistency check, not an authenticity check. The signature covers
only `StorePath`, `NarHash`, `NarSize` and `References`; `URL`, `Compression`,
`FileHash` and `FileSize` are unsigned, so a malicious tier can rewrite
`FileHash` to match altered bytes. Only the `Compression: none` case checks a
signed hash. Authenticity of compressed NARs rests on Nix, which verifies the
signed `NarHash` after decompression and rejects the path otherwise. The client
does not decompress.

## Upload and lease protocol v1

`stockhouse-wire` (`packages/rust/libs/wire`) owns the client-to-server
upload and lease wire protocol (`PROTOCOL_VERSION = 1`). It is split from
`stockhouse-proto` to keep both within the crate-size budget and builds on
its types. It is pure: it renders request bodies and request paths, and
classifies a response from its status code and body bytes. HTTP,
credentials and retries belong to `stockhouse-upload`. Servers implement this
protocol; nothing here comes from a server's code.

All request paths are relative to the private tier base URL `B` (for example
`https://cache.example.invalid/t/1/`). Upload paths are rendered from typed
values only, so they never contain `/`, `.` segments or escapes:

| Request | Path |
| --- | --- |
| declare | `uploads/<UploadId>` (`POST`) |
| send the NAR | `uploads/<UploadId>/nar` (`PUT`) |
| status | `uploads/<UploadId>` (`GET`) |
| renew a lease | `leases/<MachineId>/<slot>` (`PUT`) |

### Declarations

`UploadDeclaration` is the `POST` body:

```json
{"version":1,"store_path":"/nix/store/<hash>-<name>","nar_hash":"sha256:<nix32>",
 "nar_size":1234,"references":["<hash>-<name>"],"deriver":null,"ca":null}
```

`render` emits exactly these keys in this order, with `nar_hash` in canonical
`sha256:<nix32>` form, references as basenames in the given order (a
self-reference included, as Nix records it), `deriver` as a `.drv` basename or
`null`, and `ca` as a string or `null`. `parse` (used by test fixtures and any
server written against this crate) is strict and checks, in order:

1. More than 4 MiB of input: `DeclarationError::TooLarge`. Every declaration
   that rule 4 accepts renders to less than 3 MiB (10 000 references of at
   most 244 bytes each, plus a 1 KiB `ca`), so the cap never refuses a valid
   declaration.
2. Not a JSON object of exactly these keys with these JSON types (an unknown,
   missing or repeated key included): `DeclarationError::Malformed`.
3. `version` other than 1: `DeclarationError::UnsupportedVersion`.
4. A value that breaks its rule: `DeclarationError::InvalidField { field }`.
   `store_path` must be a store path; `nar_hash` a `Sha256`; `nar_size`
   greater than zero; `references` at most `MAX_REFERENCES` (10 000) distinct
   basenames; `deriver` a basename ending in `.drv`; `ca` at most 1024 bytes
   without control characters.

The constructor `UploadDeclaration::new` applies rule 4, so a rendered
declaration always parses back to an equal value.

### Replies

Responses carry no `version` field. `parse_reply(kind, status, body)`, where
`kind` is `Declare`, `Nar` or `Status`, returns an `UploadReply`: either
`State(UploadState)` (`Open`, `Present`, `Published`) or
`Refused(UploadRefusal)`. A status and body pair outside its kind's row is
`ReplyError::Unexpected { status }`. A body over 4 MiB (room for the largest
valid `missing` list) is `ReplyError::TooLarge`. Bodies are objects with no
unknown keys.

| Status | Body | `Declare` | `Nar` | `Status` |
| --- | --- | --- | --- | --- |
| 200 | `{"state":"present"}` | `State(Present)` | `State(Present)` | `State(Present)` |
| 200 | `{"state":"published"}` | `State(Published)` | `State(Published)` | `State(Published)` |
| 200 | `{"state":"open"}` | `State(Open)` | unexpected | `State(Open)` |
| 400 | `{"error":"bad_request"}` | `BadRequest` | `BadRequest` | unexpected |
| 400 | `{"error":"verification_failed"}` | `VerificationFailed` | `VerificationFailed` | `VerificationFailed` |
| 404 | `{"error":"unknown_upload"}` | `UnknownUpload` | `UnknownUpload` | `UnknownUpload` |
| 409 | `{"error":"conflict"}` | `Conflict` | `Conflict` | unexpected |
| 413 | `{"error":"too_large"}` | `TooLarge` | `TooLarge` | unexpected |
| 422 | `{"error":"missing_references","missing":[…]}` | `MissingReferences` | `MissingReferences` | unexpected |
| 507 | `{"error":"quota_exceeded"}` | `QuotaExceeded` | `QuotaExceeded` | unexpected |
| 401 | any body | `Unauthorized` | `Unauthorized` | `Unauthorized` |
| 503 | any body | `Unavailable` | `Unavailable` | `Unavailable` |

A declaration or status request answered `verification_failed` names an
upload whose NAR already failed verification; it is permanent. The body of a
401 or 503 is not read as JSON, because an intermediary may
answer those. An upload's states are ordered `published` > `present` > `verified` >
`open`: a result reports the highest state that holds, so an upload that was
published reports `published` even though its path is also held, and a held
path reports `present` even when this upload's NAR was also verified.

`missing` must be a non-empty list of at most `MAX_REFERENCES`
distinct store path basenames; otherwise the reply is `Unexpected`. Whether
the missing names were actually declared is checked by the uploader, which
knows the declaration.

### Leases

`MachineId` is 16 bytes rendered as 32 lowercase hex characters.
`MachineId::parse` accepts only that form (`MachineIdError::Invalid`
otherwise) and `MachineId::from_bytes` builds one from 16 bytes; generating
the bytes is the caller's job, so the crate stays free of I/O and randomness.
`LeaseSlot::new(n)` accepts 1 to 10 (`LeaseSlotError::OutOfRange`
otherwise): slot 1 is the current system, slot 2 the previous system and 3 to
10 are operator defined.

`LeaseRequest` is the `PUT` body `{"version":1,"store_path":"/nix/store/…"}`,
rendered in that key order and parsed with the same strictness rules as a
declaration (`LeaseRequestError::{TooLarge, Malformed, UnsupportedVersion,
InvalidField}`, 4 KiB limit). `parse_lease_reply(status, body)` returns:

| Status | Body | Result |
| --- | --- | --- |
| 200 | `{"state":"renewed"}` | `LeaseReply::Renewed` |
| 422 | `{"error":"unknown_path"}` | `LeaseReply::Refused(UnknownPath)` |
| 409 | `{"error":"conflict"}` | `LeaseReply::Refused(Conflict)` |
| 400 | `{"error":"bad_request"}` | `LeaseReply::Refused(BadRequest)` |
| 401 | any body | `LeaseReply::Unauthorized` |
| 503 | any body | `LeaseReply::Unavailable` (also a full machine table) |

Anything else is `ReplyError::Unexpected { status }`, and a body over 64 KiB
is `ReplyError::TooLarge`.

### Server behaviors the client relies on

These are facts of the protocol as servers implement it, recorded so the
client's handling is explained:

- A 401 is a constant text response, never JSON; the client never parses it
  (UPLOAD-012) and only reports the rejected credential.
- `404 unknown_upload` from the NAR or status request means the server no
  longer knows the upload: the client declares the path again.
- Identity horizon: a server may forget an upload identity older than its
  retention horizon, which is at least 7 days after the identity's UUIDv7
  timestamp, and then answers a declaration or status of it with
  `404 unknown_upload`, never `conflict`. A declaration answered
  `unknown_upload` (v1 or v2) therefore means the identity itself is no
  longer usable: the client mints a fresh `UploadId` for that path and
  declares again with it, at most once per path per job, and never sends a
  NAR under the old identity.
- Uploads require a credential granted both `cache:read` and `cache:write`;
  the sign-in configuration requests both.
- A server whose table of machines is full answers lease renewals with 503,
  which the client treats as transient.
- A lease belongs to the account, not the grant: after signing in again under
  a new grant, the same machine identity's leases rebind to that account.

## Upload and lease protocol v2

Protocol v2 batches control requests and separates verified NAR transfer
from publication. It uses stable client `UploadId`s as idempotency keys,
strict bounded codecs, verified streaming and the same lease slot
semantics.

### Why v2

A server built on durable object storage makes every state-changing request
durable before it replies, so each such request costs one durable commit,
typically a few hundred milliseconds and sometimes about a second. A server
can merge concurrent commits, but v1 is per path and largely sequential: a
closure costs about one declaration, one NAR transfer that also publishes,
and later one lease per slot, so a system closure of thousands of paths
costs thousands of commits in series. Most paths of a rebuilt system are
already held, and in v1 learning that alone costs one request each.

v2 batches every step except the NAR bytes: one request declares up to 1000
paths, one publishes up to 1000 verified uploads, one queries up to 1000
upload states, and one renews all of a machine's lease slots. NAR bodies
still stream one NAR per request, because a stream cannot be batched, but
the client sends several at once and a NAR transfer no longer publishes on
its own. For a closure of `N` paths of which `M` are new, a v2 client sends
`ceil(N/1000)` declare requests, `M` NAR transfers (at most
`max_inflight_nars` at a time) and `ceil(M/1000)` publish requests, instead
of about `N + M` sequential requests in v1.

v2 lives in `stockhouse-wire-v2` (`packages/rust/libs/wire_v2`), a pure crate
over `stockhouse-wire` and `stockhouse-proto` with the same split of
responsibilities as v1: it renders request bodies and paths and classifies
replies; HTTP, credentials, concurrency and retries belong to the uploader
and the lease client. The client also supports protocol v1.

### Versions and negotiation

The wire versions a client speaks are `WIRE_VERSIONS = [1, 2]`. They are
independent of `PROTOCOL_VERSION`, which stays `1` and versions the client's
own formats (configuration files and queue records). Every v2 request body
carries `"version":2`; v1 bodies keep `"version":1`. As in v1, replies carry
no `version` key, and one is unexpected.

A client learns what a server speaks from the protocol probe,
`GET protocol` under the private tier base URL `B`, sent with the same
bearer as any other request:

| Status | Body | `ProbeReply` |
| --- | --- | --- |
| 200 | `{"versions":[1,2]}` | `Offered(versions)` |
| 404 | any body | `Offered([1])`: a server without the probe speaks only v1 |
| 401 | any body | `Unauthorized` |
| 429 | any body | `Busy { retry_after }` |
| 503 | any body | `Unavailable { retry_after }` |

The 200 body is an object that must hold the key `versions`: a non-empty
list of at most 16 distinct integers from 1 to 65535 in ascending order.
Other keys of the probe body are ignored, so a later protocol can advertise
more in the same document; this is the only open key set in the protocol,
and every other request and reply keeps a closed key set. A body without a
valid `versions` (a `version` key alone included), any other status, and a
body over 4 KiB are
`ReplyError::{Unexpected, TooLarge}`. Bodies of 401, 404, 429 and 503 are not
read, because an intermediary or a v1 server may answer them.

`select_version(mode, offered)` picks the version to use. The client's
`protocol` setting is `auto` (the default), `v1` or `v2`:

| Mode | Result |
| --- | --- |
| `v1` | 1, with no probe |
| `auto` | 2 when `offered` contains 2; otherwise 1 when it contains 1; otherwise `VersionError::NoCommonVersion` |
| `v2` | 2 when `offered` contains 2; otherwise `VersionError::NotOffered` |

The probe changes no server state and is cheap. It is not retried as v1 on
failure: a probe answered 429, 503, a transport error, a timeout or an
unexpected reply leaves the version unknown, and the client tries again
later rather than downgrading during an outage.

A v2 request answered 404 with any body (except the NAR route's
`unknown_upload`, below) is `NotSupported`: the server no longer offers v2,
for example after a rollback. The client then forgets the selected version
and probes again before its next job.

### Paths

All paths are relative to `B` and rendered from typed values only, as in v1:

| Request | Path |
| --- | --- |
| protocol probe | `protocol` (`GET`) |
| declare a batch | `v2/declare` (`POST`) |
| send one NAR | `v2/nar/<UploadId>` (`PUT`) |
| publish a batch | `v2/publish` (`POST`) |
| query a batch | `v2/status` (`POST`) |
| renew a batch of leases | `v2/leases` (`POST`) |

Batch requests are `Content-Type: application/json`; the NAR is
`application/x-nix-nar` with `Content-Length` equal to the declared
`nar_size`, exactly as in v1.

### Upload states

v2 adds one state to v1's `open`, `present` and `published`:

| State | Meaning |
| --- | --- |
| `open` | the server knows the declaration and waits for its NAR |
| `verified` | the server durably holds a NAR for this upload that matched its declared `nar_hash` and `nar_size`; it is not published yet |
| `present` | the server already holds this store path with this `nar_hash`, from any upload |
| `published` | this upload was published |

The split is deliberate: a NAR transfer ends in `verified`, and only a
publish request makes uploads visible. Publication is the state change that
needs the tenant's authority (reference checks, quota charging, catalog
entries), so batching it is what saves commits; the NAR transfer only has to
make its own bytes durable, which it must do in any design. It also removes
the ordering constraint from transfers: references need not be published
before a referrer's NAR is sent, only before the referrer is published, so
NARs of one closure can be sent concurrently in any order and the closure is
published in a few requests in reference order.

A `verified` upload stays `verified` until it is published or the server
forgets it. The server retains a verified, unpublished upload for at least
24 hours from verification. A forgotten upload is `unknown_upload` and
the client declares it again.

Tenant byte quota is charged at publish. Declare and NAR admission may
return `quota_exceeded` only if the path's incremental charge would exceed
the quota based on published usage alone. Retained verified bytes,
including those of the path's referrers, do not count toward that decision
or impose a separate tenant admission cap. A referrer can finish transfer
first and still needs its dependency to transfer before either can publish.
A configured cap on retained verified bytes cannot block dependencies at
any scope, even through a retryable reply. The server may return `Busy` or
`Unavailable` for a temporary staging outage.

### Declare batches

`DeclareBatch` is the `POST v2/declare` body:

```json
{"version":2,"uploads":[
 {"id":"<UploadId>","store_path":"/nix/store/<hash>-<name>","nar_hash":"sha256:<nix32>",
  "nar_size":1234,"references":["<hash>-<name>"],"deriver":null,"ca":null}]}
```

Each entry is a v1 declaration with `id` in front and no `version`: the keys
`id`, `store_path`, `nar_hash`, `nar_size`, `references`, `deriver` and `ca`
in that order, with the value rules of v1 (an `UploadDeclaration` built with
`UploadDeclaration::new`). `render` emits exactly the keys `version` and
`uploads`, in that order, and the entries in the order given.

Limits, enforced by the builder and by `parse`:

- `MAX_BATCH_ENTRIES = 1000` entries, at least one. A typical system closure
  has one to three thousand paths, so a closure needs one to three declare
  requests; a bounded entry count also bounds the server's work per commit
  and the reply size.
- `MAX_DECLARE_BODY = 8 MiB` of rendered JSON. The largest valid entry (at
  the 10 000-reference limit) renders to under 3 MiB, so a single entry
  always fits, and a thousand typical entries (about a kilobyte each) use a
  small fraction of it.

`DeclareBatch::new()` starts empty and `push(id, declaration)` rejects an
already used `id` or `store_path` with
`DeclarePushError::Duplicate { index }`, where `index` is the zero-based
position the entry would occupy. Otherwise, pushing an entry that would
exceed either limit returns `DeclarePushError::BatchFull`, so the client
starts the next batch with that entry. A failed push leaves the batch
unchanged. Rendering an empty batch returns `BatchError::Count`. Every
successful render therefore parses back to the same batch.

`parse` (for test fixtures and servers written against this crate) is
strict and checks, in order:

1. More than 8 MiB of input: `BatchError::TooLarge`, without parsing JSON.
2. Not an object of exactly `version` and `uploads`, an entry that is not an
   object of exactly the entry keys, or a value of the wrong JSON type
   (unknown, missing or repeated keys included): `BatchError::Malformed`.
3. `version` other than 2: `BatchError::UnsupportedVersion`.
4. No entries or more than 1000: `BatchError::Count`.
5. An entry value that breaks its v1 rule, or an `id` that is not an
   `UploadId`: `BatchError::InvalidField { index, field }`.
6. An `id` or a `store_path` equal to an earlier entry's:
   `BatchError::Duplicate { index }`.

Entries are independent: their order does not change their results, and an
entry's references need not be held or declared in the batch.

### Id batches

`IdBatch` is the body of `POST v2/publish` and `POST v2/status`:
`{"version":2,"uploads":["<UploadId>",…]}`, with 1 to 1000 distinct
identities in the given order and a 64 KiB cap (1000 identities render to
about 39 KiB). Its parse checks the same rules in the same order
(`TooLarge`, `Malformed`, `UnsupportedVersion`, `Count`,
`InvalidField { index, field: "uploads" }`, `Duplicate { index }`).

### Batch replies

Every batch request (declare, publish, status, leases) is classified first
at request level by `parse_batch_reply(kind, request, status, retry_after,
body)`:

| Status | Body | Result |
| --- | --- | --- |
| 200 | `{"results":[…]}` | `Results(Vec<EntryResult>)` |
| 400 | `{"error":"bad_request"}` | `Refused(BadRequest)` |
| 413 | `{"error":"too_large"}` | `Refused(TooLarge)` |
| 404 | any body | `NotSupported` |
| 401 | any body | `Unauthorized` |
| 429 | any body | `Busy { retry_after }` |
| 503 | any body | `Unavailable { retry_after }` |

A 200 means the request was processed and every state change it reports is
durable; it is 200 even when every entry was refused. A request-level 400 or
413 means the server refused the request as a whole and changed nothing. A
conforming server accepts every request within this protocol's limits, so a
client that respects them never sees 400 or 413; the client treats a 413 as
transient indefinitely and never shrinks its batches. A 401 or 404 changed
nothing. A 429, a 503, a transport error or a timeout says nothing about the
request: the server may have applied any subset of its entries, and the
client relies on per-entry idempotency when it resends. `retry_after` is the `Retry-After` header when it is
delta-seconds from 0 to 86400, and `None` otherwise (an HTTP date included).
Any other status or body is `ReplyError::Unexpected { status }`, and a body
over `MAX_BATCH_REPLY = 4 MiB` is `ReplyError::TooLarge`.

The `results` list must have exactly one result per request entry, in
request order, and each result must echo its entry's identity (`id` for
uploads; `machine` and `slot` for leases). A count or identity mismatch, an
unknown key or a `version` key is `Unexpected { status: 200 }`: the client
never guesses which result belongs to which entry, so a partial batch is
never ambiguous.

An upload result is `{"id":…,"state":<state>}` or `{"id":…,"error":<code>}`;
`missing_references` adds `"missing":[…]`. The allowed values depend on the
request:

| Result | `v2/declare` | `v2/publish` | `v2/status` |
| --- | --- | --- | --- |
| state `open` | send the NAR | NAR not verified: send it | send the NAR |
| state `verified` | publish | unexpected | publish |
| state `present` | complete | complete | complete |
| state `published` | complete | complete | complete |
| error `conflict` | permanent | permanent | unexpected |
| error `too_large` | permanent | unexpected | unexpected |
| error `verification_failed` | permanent | permanent | permanent |
| error `quota_exceeded` | quota | quota | unexpected |
| error `unknown_upload` | new identity, declare again | declare again | declare again |
| error `missing_references` | unexpected | the missing-reference round | unexpected |

`missing` must be a non-empty list of distinct store path basenames; the
lists of one reply hold at most `MAX_REFERENCES` (10 000) names in total,
so a server lacking more lists a subset and the client finds the rest in a
later round. Whether the names are the entry's declared references is
checked by the uploader, which knows the declaration. Any value outside its
column is `Unexpected { status: 200 }`.

Declare semantics, as the client relies on them: an entry whose `id` is new
opens the upload (`open`) or reports the path `present`; an entry repeating
an existing upload's declaration exactly reports that upload's current state
and changes nothing. `conflict` means one of exactly two things: the `id`
names an upload of `B`'s tenant with a different declaration, or the store
path is held for `B`'s tenant under a different `nar_hash`. `present` never
covers a different `nar_hash`. Upload identities are scoped per tenant: an
`id` that another tenant used is unknown here and never answers `conflict`.
Declaring never checks references.

Publish semantics: the server processes entries in request order. An entry
is published when its upload is `verified`, it is within quota, and each of
its references other than itself is held for `B`'s tenant or is an earlier
entry of the same request whose result is `published` or `present`.
Otherwise it gets the matching result and is not published, and a later
entry that references it reports `missing_references` naming it. Whether a
reference can also be satisfied by another request merged into the same
server commit is server-defined and invisible to clients; a client never
relies on it. All the publications of one request are durable before the
reply. Publishing an already published upload reports `published` and
changes nothing. Quota is charged when an upload is published.

Status is read-only: it never opens, charges, extends or publishes an
upload, so it is safe during quota exhaustion and for reconciliation.
Publish and status, like declare and the NAR transfer, see only uploads of
`B`'s tenant: any other identity is `unknown_upload`.

### NAR transfer

`PUT v2/nar/<UploadId>` streams one NAR as in v1. `parse_nar_reply`:

| Status | Body | Result |
| --- | --- | --- |
| 200 | `{"state":"verified"}` | `State(Verified)` |
| 200 | `{"state":"present"}` | `State(Present)` |
| 200 | `{"state":"published"}` | `State(Published)` |
| 400 | `{"error":"bad_request"}` | `Refused(BadRequest)` |
| 400 | `{"error":"verification_failed"}` | `Refused(VerificationFailed)` |
| 404 | `{"error":"unknown_upload"}` | `Refused(UnknownUpload)` |
| 404 | any other body | `NotSupported` |
| 409 | `{"error":"conflict"}` | `Refused(Conflict)` |
| 413 | `{"error":"too_large"}` | `Refused(TooLarge)` |
| 507 | `{"error":"quota_exceeded"}` | `Refused(QuotaExceeded)` |
| 401 | any body | `Unauthorized` |
| 429 | any body | `Busy { retry_after }` |
| 503 | any body | `Unavailable { retry_after }` |

There is no 422: references are checked at publish. Anything else is
`Unexpected`. A body over 64 KiB is `TooLarge` for statuses with a parsed
body, except that an oversized 404 is `NotSupported` without parsing.
`verified` is durable
before the reply, so a client that received it never sends that NAR again.
Sending a NAR to an upload that is already `verified` reports `verified`
and changes nothing.

### Lease batches

`LeaseBatch` is the `POST v2/leases` body:

```json
{"version":2,"leases":[{"machine":"<MachineId>","slot":1,"store_path":"/nix/store/…"}]}
```

Keys in that order; 1 to `MAX_LEASE_ENTRIES = 64` entries (a machine has at
most ten slots, so one machine always fits and a client may renew several
machine identities it holds); distinct `(machine, slot)` pairs; a 64 KiB
cap. Parse errors are `LeaseBatchError::{TooLarge, Malformed,
UnsupportedVersion, Count, InvalidField { index, field }, Duplicate { index
}}`, checked in that order. Each entry means exactly what one v1
`PUT leases/<machine>/<slot>` means: the same account binding, rebinding and
slot semantics. Every successful renewal sets the current binding of that
`(machine, slot)` to the requested store path and renews its lifetime. A
repeat for the currently bound path renews the same lease. A request for
A, then B, then A again binds the slot to A after the third request. The
slot key identifies a single lease, and the latest successful application
determines its binding.

A lease result echoes its entry's `machine` and `slot`:
`{"machine":…,"slot":…,"state":"renewed"}` (`Renewed`) or
`{"machine":…,"slot":…,"error":<code>}` with `unknown_path` or `conflict`
(`Refused` with that reason, as in v1) or `machines_full` (`MachinesFull`:
the server's machine table is full, which is failed and transient like v1's
503). All renewals of one request are durable before the reply. Other
values are `Unexpected { status: 200 }`.

### Retries and backoff

There is no batch identity. For uploads, repeating an entry for the same
`UploadId` returns its current state and does not repeat a completed state
change or charge. For leases, applying an entry again renews its requested
binding in the same slot without creating a second lease. Lease lifetime
and binding changes follow the order in which successful renewals are
applied.

| Reply | Class | Client behavior |
| --- | --- | --- |
| `Results` | per entry | each entry by its result |
| `Refused(BadRequest)` | permanent | the client built an invalid request: reject the affected records |
| `Refused(TooLarge)` at request level | protocol violation | retry transiently; never reject a record for it |
| `NotSupported` | version | forget the version, retry transiently, probe again |
| `Unauthorized` | unauthorized | the v1 401 rule: invalidate, and resend once only after a successful invalidate |
| `Busy`, `Unavailable`, transport error, timeout | retryable | resend the identical request at most twice in the same job, waiting `min(retry_after, 30)` seconds, or 2 then 4 seconds without `retry_after`; then retry the job |

A batch request answered by a timeout or transport error after it was sent
may have been applied. Resends use the same per-entry rules. Because a
durable commit can take about a second and a server merges concurrent
commits, the client's per-request timeout (60 seconds by default) leaves
ample room. The client never has two requests carrying the same entry in
flight at once: a resend starts only after the previous attempt ended, and
the batches of one group are sent one after another.

### Limits and security

- No new authority. Every request needs the same credential as v1
  (`cache:read` and `cache:write`), is sent only under `B`, and is scoped to
  `B`'s tenant. An `UploadId`, `MachineId` or batch grants nothing: a server
  answers `unknown_upload` for an identity the requester's tenant did not
  declare, and never reveals another tenant's uploads.
- Bodies carry no credential, token or secret; identities and store paths
  are not secrets.
- Every request and reply has a fixed size cap and entry count, enforced
  before JSON parsing, and every key set is closed.
- v2 adds no redirect, range, resume or chunk mechanism.
