# P2.09 client groundwork plan

Bounded implementation plan for the specs in `.bloomery/specs/CLIENT/`.
Removed at cleanup; the lasting record is `docs/P2_09_CLIENT_GROUNDWORK.md`.

## Steps

1. **stockhouse-proto** (`libs/proto`): `nix32`, `StorePath`/`StorePathHash`,
   `Sha256`, line parser shared by narinfo and nix-cache-info, `NarInfo` parse,
   render and `with_url`, `NixCacheInfo`, `PublicKey`/`Signature`/`TrustedKeys`
   with fingerprint and verification, `ContentVerifier` and served-integrity
   selection, `UploadId`, `PROTOCOL_VERSION`. Unit tests per requirement, with
   fixture keys from fixed seeds.
2. **stockhouse-credentials** (`libs/credentials`): `Secret`, `CredentialSource`,
   `OwnerOnlyFile`, `SystemdCredential`, `CredentialConfig`, `StaticCredential`
   (tests only), `GrantFlow`/`DeferredGrantFlow`. Tests in temporary
   directories; the wrong-owner case uses an injected effective UID.
3. **stockhouse-proxy** (`libs/proxy`): `ProxyConfig` (TOML), `Upstream`
   (`FileUpstream`, `HttpUpstream` over reqwest with redirects disabled and a
   sensitive bearer header), `Tier` with a cache-info memo, `Lookup` policy that
   returns `ProxyError`, the hyper server with routes, status mapping and a
   held-back-final-chunk NAR stream, and `serve` with `AddressInUse`. Component
   tests run the real server against `file://` and loopback HTTP fixture caches
   that record requests and headers.
4. **stockhouse-ingest** (`libs/ingest`): `Queue` (layout, atomic writes,
   enqueue, due, apply, status, overflow logs), `hook::run`, `Uploader` with
   `DeferredUploader`/`NixCopyUploader`, `Scanner` with
   `NixPathInfoScanner`/`NoScanner`, `rescan`, `UploaderConfig`, `pass`.
   Tests use temporary queues, fake uploaders/scanners and fake `nix`
   scripts.
5. **stockhouse-cli** (`libs/cli`) and the thin `bins/stockhouse`: dispatch,
   usage and exit codes. Tests call `main` with argument vectors, and a
   thin-binary test inspects `packages/rust/bins/`.
6. **Nix**: `nix/module.nix`, flake `extraOutputs` (`nixosModules`), the
   `tests/` subflake with `module.test.nix` (evaluation) and `vm.test.nix`
   (two Nix versions), restricted to `x86_64-linux`.
7. **Records**: `docs/P2_09_CLIENT_GROUNDWORK.md` with behavior, non-claims,
   deferred P2.06/P2.08 items and the P2.08a reconciliation follow-up. Then the
   independent implementation review, fixes, `bloomery check`,
   `nix flake check`, the health baseline commit, the PR and cleanup.

## Limits

- Keep every crate under 1,500 production SLOC and functions under 250 tokens.
  Split a crate rather than exceed this.
- No network in tests or VMs. Live server behavior is out of scope.
