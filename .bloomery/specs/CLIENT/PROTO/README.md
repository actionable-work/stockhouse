---
id: PROTO
name: Public Protocol Types
tagline: Store paths, hashes, narinfo, nix-cache-info, signatures and integrity checks.
description: |
  `stockhouse-proto` defines versioned public types and typed errors for the
  Nix binary-cache protocol as the client consumes it: strict store-path,
  hash, narinfo and nix-cache-info parsing, Ed25519 narinfo signature
  verification and streaming content integrity checks.
---

# Public Protocol Types

- [Protocol design](design/proto.md)
