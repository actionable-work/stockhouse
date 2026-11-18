# P2.09 client server sign-in, upload and leases

Slice record for the second Stockhouse client slice. It finishes the items
the [groundwork](P2_09_CLIENT_GROUNDWORK.md) deferred: server sign-in,
protected token storage, the real credential exchange in the proxy, and
authenticated server upload with lease renewal. The specifications live in
[`.bloomery/specs/CLIENT/`](../.bloomery/specs/CLIENT/README.md). Every
behavior below is an EARS requirement there and is traced to a test. The
detailed plans are [`plans/P2_09_AUTH.md`](plans/P2_09_AUTH.md) and
[`plans/P2_09_UPLOAD.md`](plans/P2_09_UPLOAD.md).

## Implemented behavior

| Area | Crate or file | Behavior |
| --- | --- | --- |
| Credentials | `stockhouse-credentials` | `CredentialSource::invalidate` (a 401 hook, `false` for static sources). The shared serde `CredentialSpec` (`file`, `systemd`, `server`). Typed `CredentialError` variants without token material |
| Token store | `stockhouse-token-store` | Tokens sealed with XChaCha20-Poly1305 under a key held as an encrypted systemd credential, or an explicitly configured owner-only plaintext file. Atomic writes, ownership, mode and binding checks, and a cross-process lock |
| Sign-in | `stockhouse-auth`, `stockhouse-signin` | OAuth 2.0 public client: loopback authorization code with S256 PKCE (RFC 8252) and the device flow (RFC 8628, `interval` and `slow_down`). Refresh before expiry, serialized across processes, with `invalid_grant` meaning signed out. Logout revokes (RFC 7009) and deletes. The refreshing `ServerCredential` and the `source` factory every consumer uses. Endpoint requests ignore environment proxies |
| Proxy | `stockhouse-proxy` | The private tier may use the signed-in `server` credential, checked against its private tier. A 401 refreshes once and retries once, then answers 503, never a miss. Upstream requests ignore environment proxies |
| Protocol | `stockhouse-proto`, `stockhouse-wire` | SRI hash parsing. Upload and lease protocol v1 codecs: canonical declarations, request paths from typed values, strict reply classification per request kind (including 400 `verification_failed` on declaration, NAR and status replies), machine identities, lease slots and lease requests, with bounded sizes |
| Ingestion | `stockhouse-ingest` | Per-path upload identities and the durable `nar_sent` mark under `uploads/` (additive to queue format v1), a dependency ledger, `forget_done`, and stale-entry reconciliation. The `server` backend configuration. The default backend defers with `no upload backend configured (set backend = "server")` |
| Upload | `stockhouse-upload` | The `server` backend behind `Uploader`: closure from `nix path-info --json --recursive` (both pinned shapes), references first with path-level deduplication. Declarations; NARs streamed from `nix store dump-path` with the declared `Content-Length` and the final chunk held back until size and hash verify. The full outcome mapping, missing-reference rounds through a worklist, re-declaration after `unknown_upload` and GET reconciliation before resending. Per-request timeouts, a per-path request budget, and children always reaped. Every credential error defers. The bearer goes only under the base URL, never through an environment proxy. Outcome messages name the path, request and status or code |
| Leases | `stockhouse-upload` | `stockhouse lease`: a random owner-only 128-bit machine identity created once and never regenerated. Slot 1 is the current system, slot 2 the newest other system generation, slots 3–10 operator defined. Exit 0, 75 (some slot not renewed or no credential), 1 (machine identity) or 2 (configuration) |
| CLI | `stockhouse-cli` | `login [--device]`, `logout`, `refresh` and `status --config` through `--config <auth.toml>`, and `lease --config`. Endpoint, credential-source and audience errors exit 2 before the queue is touched |
| NixOS | `nix/module.nix` | `services.stockhouse.auth` with the `stockhouse-auth` wrapper and key oneshot. `uploader.backend = "server"` on the private tier with the signed-in credential (`auth.unitCredential`) or a `LoadCredential` file. Shared hardening with explicit writable paths. `fallback = true` with `configureNix`, so a proxy 503 or 502 builds locally instead of failing the build. `stockhouse-lease.service` started by a boot, hourly and daily timer and by an activation-restarted switch timer, never part of boot or activation, with exit 75 as success. `proxy.narinfoCacheNegativeTtl` (60 s) |

## Validation

- `nix develop . --command bloomery check`:
  - W1 alone (`feat: add server sign-in, sealed token storage and refreshing
    credentials`): run `r35cdb9-18e91dbb715119c0-0`, 104 checks passed.
  - The slice tip before the final review: run
    `r33a8b3-18e914ead8934964-0`, 128 checks passed.
  - The final tip, after the aggregate review: run
    `r3b2a12-18e91ebbb3abfdef-0`, 128 checks passed.
  - With Nix `fallback` (from the private end-to-end drill): run
    `r8520-18e92567bf8b8029-0`, 128 checks passed.
- `nix flake check` passes on the tip. It includes the module evaluation checks
  `tests:module-{proxy,ingest,auth,auth-wrapper,upload,lease}` and nine VM
  stories
  (`tests:vm-{proxy,substitution,durability,lifecycle,signin,refresh,upload,upload-recovery,lease}`),
  each with Nix 2.34.8 and 2.31.5 as both client and daemon.
- The sign-in stories run against `tests/lib/fake_server.py` (OAuth and a
  private cache). The upload stories run against `tests/lib/upload_server.py`,
  a fake of protocol v1. Both are fixtures written from this repository's
  specs. The stories cover:
  - closure upload references first, with present and ledgered paths not
    sent;
  - verification rejection;
  - a restart mid-upload keeping its identity;
  - lost-reply reconciliation without resending;
  - quota exhaustion and recovery;
  - uploads after reboot;
  - lease renewal on boot and after activation with a stable identity;
  - an outage blocking neither boot nor activation.

## Supported systems

Unchanged: only `x86_64-linux` NixOS with systemd, built from this flake's
pinned nixpkgs and VM-tested with Nix 2.34.8 and 2.31.5.

## Behavior notes

- A signed-out machine's pending records show
  `last_error = "credential unavailable: server credential: not signed in"`
  (the credential error's display text) and keep their attempts.
- Within 60 seconds of obtaining an access token (`INVALIDATE_GUARD_SECS`),
  `invalidate` declines a forced refresh, so a 401 in that window is a 503
  from the proxy and a transient retry for the uploader. This is intended:
  it stops a misbehaving server from forcing a refresh on every request.
- With `fallback`, sign-out, revocation and server outages fail only the
  substitution; `vm-refresh` builds a new derivation locally while the
  proxy answers 503.

## Non-claims

- Sign-in is per machine, not per user. The proxy and the uploader share the
  `stockhouse` UID, so a compromised proxy can use the uploader's token.
- A refresh whose reply was lost after the server rotated the token, or a
  resent refresh token, may sign the machine out (no grace window is
  required by the protocol).
- The VM upload and lease stories use a static `credentialFile`. The
  signed-in credential for the uploader and lease client is covered by module
  evaluation and component tests, and end to end only by the server
  operator's own drill.
- Qualification is against this repository's fake servers only. No real
  server, provider or production evidence is claimed.
- No chunk-level upload deduplication, ranges or resume (path level only).
- No release (crates.io or otherwise). All crates are `publish = false`.

## Deferred items

See the [CLIENT deferral table](../.bloomery/specs/CLIENT/README.md):
end-to-end evidence against a real server, lost-rotation recovery and
refresh resend, per-user sign-in, chunk deduplication, a persistent 401 or
quota exhaustion consuming attempts, server NixOS module integration, agent
or keyring sources, reconciling `stockhouse-proto` with the private server
types, and any release.

## Follow-ups

- Add the uploader as a further concurrent token consumer in the
  `vm-refresh` story. Both fakes listen on 8486, so this needs them on one
  node.
- A second corrupt `uploads/` entry overwrites the earlier quarantined copy.
- A rejected root keeps its `uploads/` entries until the path is collected.
- `stockhouse-ingest` and `stockhouse-upload` are near the 1,500 SLOC soft
  limit. Split before growing them.
- A persistent 401 or quota exhaustion consumes attempts. Revisit with the
  next retry change.
- Optional: list pending upload identities in `stockhouse status`.
- From the sign-in plan: a failed store write after a successful refresh may
  lose the rotated refresh token.
