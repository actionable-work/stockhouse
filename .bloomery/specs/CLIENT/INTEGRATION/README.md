---
id: INTEGRATION
name: Binary, CLI and NixOS Integration
tagline: The thin `stockhouse` binary, its command dispatch, the NixOS module and VM qualification.
description: |
  The `stockhouse` binary forwards to `stockhouse-cli`, which dispatches the
  proxy, uploader, post-build hook, status, lease and server sign-in commands. The
  flake exports a NixOS client module whose behavior is qualified in NixOS
  VM tests using the default Nix package from the flake's pinned nixpkgs.
---

# Binary, CLI and NixOS Integration

- [Integration design](design/integration.md)
