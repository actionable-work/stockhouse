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
| 404 | `{"error":"unknown_upload"}` | unexpected | `UnknownUpload` | `UnknownUpload` |
| 409 | `{"error":"conflict"}` | `Conflict` | `Conflict` | unexpected |
| 413 | `{"error":"too_large"}` | `TooLarge` | `TooLarge` | unexpected |
| 422 | `{"error":"missing_references","missing":[…]}` | `MissingReferences` | `MissingReferences` | unexpected |
| 507 | `{"error":"quota_exceeded"}` | `QuotaExceeded` | `QuotaExceeded` | unexpected |
| 401 | any body | `Unauthorized` | `Unauthorized` | `Unauthorized` |
| 503 | any body | `Unavailable` | `Unavailable` | `Unavailable` |

A declaration or status request answered `verification_failed` names an
upload whose NAR already failed verification; it is permanent. The body of a
401 or 503 is not read as JSON, because an intermediary may
answer those. `missing` must be a non-empty list of at most `MAX_REFERENCES`
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
- Uploads require a credential granted both `cache:read` and `cache:write`;
  the sign-in configuration requests both.
- A server whose table of machines is full answers lease renewals with 503,
  which the client treats as transient.
- A lease belongs to the account, not the grant: after signing in again under
  a new grant, the same machine identity's leases rebind to that account.
