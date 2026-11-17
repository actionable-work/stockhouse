# P2.09 client groundwork

Slice record for the first Stockhouse client slice: the parts of the P2.09
Linux client that do not need server authentication. The specifications live
in [`.bloomery/specs/CLIENT/`](../.bloomery/specs/CLIENT/README.md); every
behavior below is an EARS requirement there and is traced to a test.

## Implemented behavior

| Area | Crate or file | Behavior |
| --- | --- | --- |
| Protocol | `stockhouse-proto` | Strict parsing of store paths, SHA-256 (Nix base-32 and hex), narinfo (render, round-trip, URL replacement) and `nix-cache-info`. Ed25519 narinfo verification: sorted-reference fingerprint, and a failing trusted signature is a hard error. Incremental size and hash checks. UUIDv7 `UploadId`. `PROTOCOL_VERSION = 1` |
| Credentials | `stockhouse-credentials` | Redacted, zeroed `Secret`. Explicit sources only: an owner-only file (`O_NOFOLLOW`/`O_NONBLOCK`, owner and mode, at most 4096 bytes, visible ASCII) or a systemd credential (no owner or mode check). No default discovery. `DeferredGrantFlow` stubs login, logout and refresh |
| Proxy | `stockhouse-proxy` | Loopback-only bind, failing on an occupied port. Tiers are tried private, shared, then public, and only a genuine 404 falls through. Credential, denial, outage, timeout, redirect, malformed narinfo, store-path mismatch and signature failures map to 503 or 502, never a miss. Upstream `nix-cache-info` is validated once per process. The bearer credential is sent only to its own tier and redirects are never followed. Store-path collisions go to the highest tier, with NARs routed back to the winning tier. NARs are re-verified and streamed with the final chunk held back, so a NAR whose size or hash differs from its narinfo is never delivered complete. Range is ignored. Each failure logs one line |
| Ingestion | `stockhouse-ingest` | A durable `FORMAT`/`LIMITS` queue with atomic record writes and hard-link enqueue (concurrent producers create one record). The post-build hook refuses symlinked or root-owned queues, drops to the queue owner, enqueues at most 1024 paths and always exits 0. Overflow uses bounded markers. An uploader pass reconciles, quarantines corrupt records, rescans when needed and uploads due records in order. Backoff is `min(base·2^(n-1), max)`; quota waits use the maximum backoff; deferred waits consume no attempts; failed records stay failed. Identity is stable across restarts. Rescan reconciliation uses `nix path-info --json --all` (Nix 2.31 and 2.34 shapes). Backends: `deferred` (default) and `nix-copy` (local `file://`, development and test only) |
| CLI | `stockhouse-cli`, `bins/stockhouse` | Thin binary. Commands are `proxy`, `uploader [--once]`, `post-build-hook`, `status` (JSON), deferred `login`/`logout`/`refresh` and `--version`. Usage errors exit 2. No flag accepts a value it would echo |
| NixOS | `nix/module.nix`, `nixosModules.{default,client}` | Hardened `DynamicUser` proxy unit with `LoadCredential`. The uploader runs as the `stockhouse` user. Nix's substituters are forced to the proxy alone and every tier's keys are trusted. The post-build hook script ignores the hook's exit status. Assertions require a loopback address and runtime, non-store credential paths |

## Validation

- `nix develop . --command bloomery check`: specs, traceability, per-crate
  test, clippy, doc and doctest checks, bans, licenses, vulnerabilities,
  `noDefaultFeatures`, lock freshness and code health. The baseline is
  committed in `.bloomery/code-health-summary.json`.
- `nix flake check`: includes the module evaluation checks
  `tests:module-{proxy,ingest}` and four NixOS VM stories
  (`tests:vm-{proxy,substitution,durability,lifecycle}-nix-{2_34,2_31}`), each
  run with Nix 2.34.8 and with Nix 2.31.5 as both client and daemon.
- The VM tests cover the loopback listener and credential isolation (bearer
  header present, absent from argv, units, config and journal). They also
  cover hook → queue → `nix-copy` upload with a daemon-signed path, deletion
  and substitution through the proxy, and an unwritable queue not failing the
  build (which also shows the hook really dropped privileges). Further cases:
  reboot with a pending record keeping its identity, overflow made visible
  then reconciled by rescan, specialisation upgrade and rollback, a tampered
  narinfo (502) and NAR (substitution fails), and 403/500 tiers failing
  without fall-through.

## Supported systems

Only `x86_64-linux` NixOS with systemd is supported, built from this flake's
pinned nixpkgs and VM-tested with Nix 2.34.8 and 2.31.5. `aarch64-linux`,
other distributions, non-systemd init and non-Linux systems are not claimed.
The flake exposes `x86_64-linux` only.

## Non-claims

- No server authentication, authorization or upload is implemented. The
  static bearer header is a placeholder until the real credential exchange
  exists.
- The proxy's `FileHash` check is a consistency check: `FileHash` is
  unsigned. Authenticity of compressed NARs rests on Nix's check of the
  signed `NarHash`. The proxy does not decompress.
- The `nix-copy` backend is a local development and test tool, not a release
  upload path, and it signs nothing.
- The VM signing key is a synthetic fixture derived from a public seed. It
  shows the mechanism only.
- No cache-side behavior (catalog, quotas, GC, signing) is exercised. No live
  network, provider or production evidence is claimed.
- No release (crates.io or otherwise). All crates are `publish = false`.

## Deferred items

These depend on P2.06 (grants) and P2.08 (authenticated cache HTTP):

| Item | Depends on |
| --- | --- |
| Browser/loopback PKCE and device login, logout, refresh-family rotation and refresh races through the real proxy | P2.06 |
| Protected token storage written by login (beyond systemd credentials and owner-only files) | P2.06 |
| Authenticated server narinfo/NAR fetch, the real credential exchange replacing the static bearer, server current-status outages and denied private paths against a real server | P2.06, P2.08 |
| Authenticated server upload behind `Uploader`: upload sessions, `UploadId` as the server idempotency key, quota exhaustion and uncertain-reply reconciliation with the server | P2.07, P2.08 |
| "Freshly installed machine authenticates, uploads and substitutes a closure" end to end | P2.06, P2.08 |

Other deferrals:

- Server NixOS module integration (private server modules, P2.11).
- Agent or keyring credential sources.
- A bound on concurrent proxy connections.
- Decompressing in the proxy to verify `NarHash` itself.
- An `aarch64-linux` qualification.
- Any release.

## Follow-ups

- **Reconcile `stockhouse-proto` with the private P2.08a server types** once
  P2.08a lands. Compare narinfo field rules, fingerprint, signature policy
  and error classification. Change the public crate only through its specs.
  Do not import server types.
- **Watch `nix path-info --json`**: Nix 2.34 warns that `--json` without
  `--json-format` is deprecated. The scanner already accepts the
  `--json-format 2` shape. Switch to it when the oldest pinned Nix supports
  the flag.
