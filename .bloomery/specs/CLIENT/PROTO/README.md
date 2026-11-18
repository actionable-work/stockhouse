---
id: PROTO
name: Public Protocol Types
tagline: Store paths, hashes, narinfo, nix-cache-info, signatures and integrity checks.
description: |
  `stockhouse-proto` defines versioned public types and typed errors for the
  Nix binary-cache protocol as the client consumes it: strict store-path,
  hash, narinfo and nix-cache-info parsing, Ed25519 narinfo signature
  verification and streaming content integrity checks. `stockhouse-wire`
  builds on these types and owns the client-to-server upload and lease
  protocol v1 codecs: declarations, lease requests, machine identities,
  request paths and strict reply classification.
---

# Public Protocol Types

- [Protocol design](design/proto.md)
