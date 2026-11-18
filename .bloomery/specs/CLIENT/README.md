---
id: CLIENT
name: Stockhouse Linux Cache Client
tagline: Public cache protocol, loopback substitution proxy, durable ingestion and NixOS integration.
description: |
  The open-source Stockhouse client: versioned public protocol types, a
  loopback-only substitution proxy over private, shared and public caches,
  a durable bounded post-build ingestion queue, credential sources and the
  NixOS/systemd module. Server sign-in is an OAuth 2.0 public client
  (loopback PKCE and device flow) with a sealed token store and refreshing
  credentials for the private tier.
---

# Stockhouse Linux Cache Client

The client is the public half of the Stockhouse binary cache. It depends only
on public Nix cache protocol facts; it has no dependency on private server
crates, policy, keys or databases.

- [Public protocol types](PROTO/README.md)
- [Credential sources](CREDS/README.md)
- [Substitution proxy](PROXY/README.md)
- [Post-build ingestion](INGEST/README.md)
- [Binary, CLI, NixOS module and VM qualification](INTEGRATION/README.md)

Supported systems are recorded in
[the integration design](INTEGRATION/design/integration.md#supported-systems).

## Deferred from this slice

| Item | Waits on |
| --- | --- |
| End-to-end sign-in, refresh rotation and authenticated narinfo/NAR fetch against a real server (the client side is qualified against this repository's fake server) | that server's own drill |
| Recovering a refresh whose reply was lost after the server rotated the token (the client may then be signed out and must log in again) | later client slice |
| Resending a refresh token whose request got no reply may trip server-side reuse detection (no grace window is required by the protocol), signing the client out | later client slice |
| Per-user sign-in and per-user isolation of private-tier substitution (sign-in is per machine) | later client slice |
| Authenticated server upload (sessions, quota reservation, uncertain-reply reconciliation with the server) behind the `Uploader` interface | P2.07, P2.08 |
| Server NixOS module integration | private server modules (P2.11) |
| Agent or keyring credential sources beyond systemd credentials and owner-only files | later client slice |
| Reconciling `stockhouse-proto` with the private P2.08a server types | follow-up after P2.08a |
| Any crates.io or other release | separate authorization |
