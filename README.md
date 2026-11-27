# Stockhouse

Stockhouse is a Nix-native alternative to Git, and part of Hammerworks. It
keeps source, history and the binary cache in one content-addressed store,
and lands changes on protected branches only after they have been verified.
That means the build outputs a landed commit promises are present and
signed.

**This repository holds only the open-source Linux cache client.** The
source store, history, landing queue and hosted server are not here and are
not part of this release.

## What the client does

- **`stockhouse-proto`** holds versioned public protocol types. It parses
  store paths, hashes, narinfo and `nix-cache-info` strictly, verifies
  Ed25519 narinfo signatures and checks content integrity incrementally.
- **Substitution proxy**: a loopback-only Nix substituter. It looks paths up
  in the private cache first, then the shared cache, then a configured public
  upstream such as `cache.nixos.org`. Only a genuine miss falls through to
  the next cache. Signature, integrity and authorization failures stay
  errors. The proxy checks every narinfo signature. It checks a downloaded
  file's size and hash against the narinfo when the narinfo gives them or the
  NAR is uncompressed, and otherwise passes it through. Nix then checks the
  signed NAR hash. The proxy never sends private credentials to the public
  upstream.
- **Post-build ingestion**: the Nix post-build hook adds each built path to a
  durable, bounded queue and returns at once. A separate uploader drains the
  queue, retrying with backoff. When the queue is full it reports the
  overflow, then rescans the local store to recover any paths it dropped.
  Each upload keeps its identity across restarts. Uploader failures never
  fail or block a build.
- **Server sign-in**: an OAuth 2.0 public client. `stockhouse-auth login`
  signs the machine in through a browser on the same machine (loopback
  redirect with PKCE) or, with `--device`, by entering a code on any device.
  Tokens are kept in a store sealed with XChaCha20-Poly1305 under a key held
  as an encrypted systemd credential (TPM2 or host key), refreshed before
  they expire with one refresh at a time across processes, and revoked by
  `stockhouse-auth logout`. A revoked or signed-out machine gets errors from
  the private cache, never misses.
- **Server upload and leases**: with `uploader.backend = "server"` the
  uploader uploads each built path's closure to the private cache, references
  first, skipping what the server already holds. It streams NARs without
  buffering them, keeps each path's upload identity across restarts and
  reconciles a lost reply before sending a NAR again. `stockhouse lease`
  renews the server's roots for this machine's current and previous system on
  boot, hourly, daily and after activation, without ever blocking boot or
  activation.
- **Credential sources**: credentials come only from a configured systemd
  credential, owner-only file or the signed-in token store. They never enter
  derivations, the Nix store, process arguments, upstream requests to other
  caches or logs.
- **NixOS module** (`nixosModules.default`): systemd services for the proxy
  and the uploader, and the Nix substituter and post-build hook settings.

## Current limits

- **Sign-in, upload and leases are qualified against this repository's fake
  servers only**, not a real server. Sign-in is per machine, not per user.
- **Builds never wait on the cache.** The module sets Nix's `fallback`, so
  when the proxy answers 503 (signed out, revoked, or the server is down)
  Nix builds the path locally. Within 60 seconds of a new access token, a
  401 is not retried with a forced refresh; it reads as a temporary outage.
- **Upload is opt-in.** The default uploader backend keeps built paths queued
  until a backend is configured. The `nix-copy` backend copies paths to a
  local `file://` cache and is meant for development and tests.
- **Supported systems:** `x86_64-linux` NixOS with systemd. The VM suite
  tests the default Nix package from the flake's pinned nixpkgs.
- **No releases yet:** nothing is published to crates.io or anywhere else.

See [`docs/`](docs/) for the slice record, including non-claims and deferred
work.

## Use it

Add the flake to a NixOS configuration:

```nix
{
  inputs.stockhouse.url = "github:actionable-work/stockhouse";

  outputs = { nixpkgs, stockhouse, ... }: {
    nixosConfigurations.host = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        stockhouse.nixosModules.default
        {
          services.stockhouse.proxy = {
            enable = true;
            public = {
              url = "https://cache.nixos.org/";
              trustedKeys = [ "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=" ];
            };
          };
          services.stockhouse.uploader.enable = true;
          # Optional: sign the private tier in to a server and upload to it.
          # services.stockhouse.proxy.private = { url = "https://cache.example.invalid/t/1/"; trustedKeys = [ … ]; };
          # services.stockhouse.auth = {
          #   enable = true;
          #   server = "https://example.invalid/";
          #   endpoints = { authorization = "…"; token = "…"; deviceAuthorization = "…"; revocation = "…"; };
          #   clientId = "…";
          #   scopes = [ "cache:read" "cache:write" ];
          # };
          # services.stockhouse.uploader.backend = "server";
          # then, as root: stockhouse-auth login --device
        }
      ];
    };
  };
}
```

## Develop

```sh
nix develop --command bloomery check   # specs, tests, compilation, static checks
nix flake check                        # including the NixOS VM tests
nix fmt
```

See [AGENTS.md](AGENTS.md) for the workflow and repository rules.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Unless you explicitly state
otherwise, any contribution intentionally submitted for inclusion in this work,
as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
