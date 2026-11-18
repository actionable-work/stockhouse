# Repository Guidelines

## Overview

Stockhouse is the open-source Linux cache client for Hammerworks: public
protocol types, a loopback substitution proxy, durable post-build ingestion,
credential sources and a NixOS module. The repository is public. Licensed
`MIT OR Apache-2.0`. The current slice is P2.09 groundwork; see
`docs/P2_09_CLIENT_GROUNDWORK.md` for behavior, non-claims and deferred work.

## Public/private boundary

This repository is public from its first commit, and the server is private.

- Depend only on public facts: the Nix binary-cache protocol, public crates
  and this repository's own specs. Never depend on, vendor or copy code, specs,
  identifiers, hostnames, bucket names, account names, keys or policy from the
  private Hammerworks Platform repository.
- Use reserved example names (`example.invalid`, `cache.example.invalid`)
  in docs and fixtures, never real internal hosts.
- Where client and server must agree (for example `stockhouse-proto` and the
  server's protocol types), record the reconciliation as a follow-up in
  `docs/`. Do not import the server's types.
- Releases (crates.io or otherwise) need separate authorization. Every crate
  stays `publish = false` until then.

## Secrets

- Never commit credentials, tokens, private keys or `.env` files. Test keys
  come from fixed public seeds in test code, are labeled synthetic, and are
  trusted only inside the test that creates them.
- Credentials never enter Nix expressions, derivations, the store, process
  arguments, environment variables passed on a command line, upstream
  requests to another tier, logs or error text. Configuration names *where*
  a credential lives (a systemd credential name or an owner-only file path),
  never its value.
- Module options that point at secrets are string paths, never Nix `path`
  values, so the file is not copied into the store.

## Repository layout

| Path | Owns |
| --- | --- |
| `packages/rust/bins/stockhouse` | Thin binary: `main.rs` forwards `std::env::args()` to `stockhouse_cli::main` |
| `packages/rust/libs/proto` | `stockhouse-proto`: store paths, hashes, narinfo, nix-cache-info, signatures, integrity, upload identity |
| `packages/rust/libs/credentials` | `stockhouse-credentials`: `Secret`, `CredentialSource` (with the `invalidate` retry hook), the shared `CredentialSpec` (`file`, `systemd`, `server`), file and systemd sources |
| `packages/rust/libs/token_store` | `stockhouse-token-store`: the sealed (XChaCha20-Poly1305, systemd credential key) or explicit plaintext token store, atomic writes and the cross-process lock |
| `packages/rust/libs/auth` | `stockhouse-auth`: sign-in configuration, token and revocation endpoint client, the refreshing `ServerCredential`, logout, status and the `source` factory every consumer uses |
| `packages/rust/libs/signin` | `stockhouse-signin`: loopback PKCE and device sign-in flows (CLI only) |
| `packages/rust/libs/oauth_fake` | `stockhouse-oauth-fake`: test-only fake OAuth server and private cache for component tests |
| `packages/rust/libs/proxy` | `stockhouse-proxy`: configuration, tier upstreams, lookup policy and the loopback HTTP server |
| `packages/rust/libs/ingest` | `stockhouse-ingest`: durable queue, post-build hook, uploader passes, backends, rescan |
| `packages/rust/libs/cli` | `stockhouse-cli`: command dispatch and exit status, including `login`, `logout`, `refresh` and `status --config` |
| `nix/module.nix` | The NixOS client module (`nixosModules.default`) |
| `tests/` | Bloomery test subflake: module evaluation and NixOS VM tests; `tests/lib/fake_server.py` is the VM stories' fake OAuth server and private cache |
| `.bloomery/specs/CLIENT/` | Code-facing specifications (EARS requirements and designs) |
| `docs/` | Slice records and follow-ups |

## Development workflow with Bloomery

Use the Nix shell from `flake.nix`. Run Bloomery natively, never through
`nix run`.

- Stage new files (`git add -A`) before any nix command, because the flake
  reads the git index.
- `nix fmt` formats Rust, TOML, Nix and shell files.
- `nix develop . --command bloomery check` validates specs, tests,
  compilation and static checks. Every run rewrites
  `.bloomery/code-health-summary.json`; commit that rewrite. Never pass
  `--no-write` to avoid it. Use it only for read-only inspection.
- On failure, use `bloomery check details fN --run <run>`. For scores, use
  `bloomery check health`.
- After dependency or crate-layout changes, run
  `nix develop . --command bloomery sync`. `Cargo.lock` and `bloomery.lock`
  are generated; never hand-edit them.
- Before committing, also run `nix flake check`, which includes the VM tests.
- Do not substitute `cargo test`, `cargo check` or `cargo clippy` for
  validation unless explicitly asked.
- Inspect changes with `jj status` and `jj diff -s`.

## Dependencies and crates

- Workspace dependencies are pinned exactly (`=x.y.z`) with
  `default-features = false`. Enable only the features you need.
- Binaries are thin. A binary holds only `Cargo.toml` and `src/main.rs`,
  which contains just
  `fn main() -> std::process::ExitCode { <entry>::main(std::env::args().collect()) }`.
  It depends only on its entry library crate
  (`CLIENT-INTEGRATION-BINARY-*`).
- Each crate must score at least 80 in code health and may not regress by
  more than 2 points. The soft limits are 1,500 production SLOC per crate,
  cyclomatic complexity 10, 250 tokens per function and nesting depth 4.
  When a crate nears its budget, split cohesive code into a new private crate
  rather than contorting code or editing the baseline.

## Specifications and traceability

- Design comes first. Add one EARS requirement per testable behavior to
  `.bloomery/specs/CLIENT/<FEATURE>/requirements/*.toml` before writing code.
  Umbrella IDs are not enough.
- Rust tests trace IDs with `#[cfg_attr(any(), bloomery("<ID>"))]`. Nix tests
  list IDs in `passthru.bloomery`. Every ID needs a test that actually asserts
  its behavior.
- Tests assert exact error variants.
- If implementation forces a spec change, update the requirement and design
  first and get an independent review.

## Phase workflow

1. Design: edit only `.bloomery/specs/`, then get an independent review.
2. Plan: write a bounded root `PLAN.md`.
3. Implement: implement the plan with component tests, then get an
   independent review.
4. Records: update the slice record in `docs/`.
5. Cleanup: remove `PLAN.md`.

Use Conventional Commit subjects. Push feature branches and open pull
requests to `main`; never push to `main` directly. Pull requests describe the
purpose, the affected specs and the validation performed.
