# Proxy design

The proxy is a local Nix substituter (`substituters = http://127.0.0.1:8484`).
It holds no cache of its own: every lookup consults the configured tiers and
streams verified bytes through.

## Configuration

The proxy reads one TOML file (`deny_unknown_fields`; no field can hold a
credential value):

```toml
version = 1
listen = "127.0.0.1:8484"
priority = 30               # Priority served in our nix-cache-info
request_timeout_secs = 30   # connect and per-read timeout for upstreams

[private]                   # optional
url = "https://cache.example.invalid/org/"
trusted_keys = ["org-1:<base64>"]
credential = { systemd = "private" }   # or { file = "/abs/path" }

[shared]                    # optional, same shape as private

[public]                    # optional, never has a credential
url = "https://cache.nixos.org/"
trusted_keys = ["cache.nixos.org-1:<base64>"]
```

`ProxyConfig::parse` returns a `ConfigError`:

| Condition | Error |
| --- | --- |
| TOML syntax, unknown field, wrong type | `Parse` |
| `version` other than 1 | `UnsupportedVersion` |
| `listen` is not a loopback socket address (127.0.0.0/8 or `::1`) | `NonLoopbackListen` |
| no tier configured | `NoTiers` |
| a tier without trusted keys | `MissingTrustedKeys { tier }` |
| a key that does not parse, or duplicate key names | `InvalidTrustedKey { tier }` |
| a URL that is not `file://`, `http://` or `https://`, has a query or fragment, or does not end in `/` | `InvalidUrl { tier }` |
| `public` with a `credential` | `CredentialOnPublicTier` |
| a `file://` tier with a `credential` | `CredentialOnFileTier { tier }` |
| a credential on `http://` to a host that is not a loopback address | `CleartextCredential { tier }` |

## Tiers and upstream fetches

Tiers are consulted in the fixed order `private`, `shared`, `public`;
unconfigured tiers are skipped. Each tier has an `Upstream` that fetches a
relative path under its base URL and classifies the outcome:

| Upstream result | HTTP | `file://` |
| --- | --- | --- |
| `Found(body)` | 200 | file read |
| `NotFound` (a genuine miss) | 404 | `ENOENT` |
| `Denied` | 401, 403 | `EACCES`/`EPERM` |
| `Redirect` | any 3xx (redirects are never followed) | — |
| `Status` | any other status | — |
| `Unavailable` | connect, read or timeout failure | other I/O errors |
| `TooLarge` | body over the limit for that resource | same |

A base URL must end in `/` so that relative joins stay under it (RFC 3986
would otherwise replace its last segment). `file://` URLs name a directory.

S3-style stores answer 403 for a missing key. The proxy classifies 403 as
`Denied`, never as a miss, so every tier, including the public one, must
answer 404 for a missing narinfo. `cache.nixos.org` does.

HTTP tiers with a credential send `Authorization: Bearer <secret>`, marked
sensitive, on every request to that tier and to no other tier. Because
redirects are not followed, the header cannot be forwarded to another
origin. The credential is resolved per request, before any request is sent
to that tier. The static bearer header is a placeholder until the P2.08
authenticated cache protocol and P2.06 grants define the real exchange.
Upstream connect and per-read time is limited by `request_timeout_secs`;
exceeding it is `Unavailable`.

## narinfo lookup

`GET` or `HEAD /<hash>.narinfo` where `<hash>` is a valid store path hash
(otherwise 400). For each tier in order:

1. Resolve the tier credential. Failure: `Credential { tier }`; no request is
   sent to that tier and lookup stops.
2. If the tier's `nix-cache-info` has not yet been validated in this process,
   fetch and parse it. `NotFound` is `CacheInfoMissing`, a parse error
   (including a foreign `StoreDir`) is `CacheInfo`, and `Denied` or any other
   fetch failure maps as in step 3. Success is remembered for
   the process lifetime; failures are not.
3. Fetch `<hash>.narinfo` (limit 1 MiB). `NotFound` moves to the next tier.
   `Denied` is `Denied { tier }`; `Redirect`, `Status`, `Unavailable` and
   `TooLarge` are `Unavailable { tier }`. Lookup stops on any error.
4. Parse the narinfo (`NarInfo { tier }` on error) and require its store path
   hash to equal the requested hash (`StorePathMismatch { tier }`).
5. Verify signatures with that tier's trusted keys (`Signature { tier }`).
6. Serve the narinfo with `URL` replaced by `nar/<tier>/<hash>/<original URL>`.

Only a `NotFound` at step 3 falls through. If every configured tier misses,
the proxy answers 404. Errors never become 404 or 403 (or 401), because Nix
treats those as a cache miss; they map to:

| Error | Status |
| --- | --- |
| `Credential`, `Denied`, `Unavailable` | 503 |
| `CacheInfoMissing`, `CacheInfo`, `NarInfo`, `StorePathMismatch`, `Signature`, `NarMissing`, `NarInfoChanged` | 502 |

Nix retries other statuses and then disables the substituter for a while,
silently moving on to the next substituter in its list. A 502 would therefore
still fall through if Nix had another substituter. The NixOS module makes the
proxy Nix's only substituter (the public tier lives inside the proxy), so an
error reaches the user as a failed substitution.

### Store-path collisions

A store path hash may exist in more than one tier, possibly with different
contents. The highest-priority tier with a verified hit wins and lower tiers
are not contacted. The served `URL` names the winning tier, and the NAR route
re-verifies against that same tier, so a narinfo from one tier is never paired
with NAR bytes from another.

## NAR route

`GET /nar/<tier>/<hash>/<url>`:

1. An unconfigured tier or invalid hash is 404 with no upstream contact.
2. Re-run narinfo steps 1–5 against that tier only. Errors map as in the
   lookup table. A miss is `NarMissing` (502), because the client was told
   this tier holds the path; a 404 here would let Nix try elsewhere.
3. If the narinfo `URL` differs from `<url>`, the narinfo changed since it was
   served: answer 502 (`NarInfoChanged`) without fetching.
4. Fetch `<url>` from the tier. `NotFound` is `NarMissing` (502); other
   failures map as in step 3 of the lookup.
5. When `served_integrity` gives a check, answer 200 with `Content-Length`
   equal to the expected size (never chunked) and stream the body through a
   `ContentVerifier`. The most recent chunk, which always includes at least
   the final byte, is held back until the verifier finishes. On
   `SizeExceeded`, `SizeMismatch` or `HashMismatch` the proxy closes the
   connection after fewer than `Content-Length` bytes, so the client never
   receives a complete, wrong file. This is a consistency check over unsigned
   `FileHash`; Nix's `NarHash` check is the authenticity check.
6. When there is no check (compressed without `FileHash`), stream the body
   unchanged, passing the upstream `Content-Length` when present. If the
   upstream fails or ends early mid-stream, the proxy resets the connection
   rather than ending the response cleanly.
7. `Range` request headers are ignored: the proxy always sends the complete
   representation with 200 and never sends `Accept-Ranges`, so Nix does not
   try to resume an interrupted transfer and the substitution fails closed.

## Routes, binding and logging

`GET`/`HEAD /nix-cache-info` returns
`StoreDir: /nix/store`, `WantMassQuery: 1` and the configured `Priority`
without upstream contact. Any other path is 404 without upstream contact;
methods other than `GET` and `HEAD` are 405 (`HEAD` is not accepted on NAR
routes).

The proxy binds exactly the configured loopback address (`127.0.0.0/8` or
`[::1]`). If the port is in
use it fails with `ServeError::AddressInUse { addr }` and exits nonzero; it
never chooses another port, because Nix's configuration names a fixed URL.

The proxy logs one line per failed lookup to stderr with the tier, route
kind and error kind. Logs, error bodies and error `Display` text never
include request headers or credential values.
