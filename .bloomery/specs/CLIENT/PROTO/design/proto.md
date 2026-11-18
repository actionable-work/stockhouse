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