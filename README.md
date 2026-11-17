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
  errors. The proxy checks every narinfo signature and the size and hash of each
  downloaded file against its narinfo. Nix then checks the signed NAR hash.
  The proxy never sends private credentials to the public upstream.
- **Post-build ingestion**: the Nix post-build hook adds each built path to a
  durable, bounded queue and returns at once. A separate uploader drains the
  queue, retrying with backoff. When the queue is full it reports the
  overflow, then rescans the local store to recover any paths it dropped.
  Each upload keeps its identity across restarts. Uploader failures never
  fail or block a build.
- **Credential sources**: credentials come only from a configured systemd
  credential or owner-only file. They never enter derivations, the Nix store,
  process arguments, upstream requests to other caches or logs.
- **NixOS module** (`nixosModules.default`): systemd services for the proxy
  and the uploader, and the Nix substituter and post-build hook settings.

## Current limits

- **No server sign-in or upload yet.** `stockhouse login`, `logout` and
  `refresh` report that they are not available. The default uploader backend
  keeps built paths queued until authenticated server upload exists. The
  `nix-copy` backend copies paths to a local `file://` cache and is meant for
  development and tests.
- **Supported systems:** `x86_64-linux` NixOS with systemd only, tested in
  NixOS VMs with Nix 2.34.8 and 2.31.5. Other Linux systems, distributions
  and operating systems are not claimed.
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
