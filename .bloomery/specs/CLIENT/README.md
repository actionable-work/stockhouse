---
id: CLIENT
name: Stockhouse Linux Cache Client
tagline: Public cache protocol, loopback substitution proxy, durable ingestion and NixOS integration.
description: |
  The open-source Stockhouse client: versioned public protocol types, a
  loopback-only substitution proxy over private, shared and public caches,
  a durable bounded post-build ingestion queue, credential sources and the
  NixOS/systemd module. Server authentication, grants and authenticated
  upload are deferred to the server slices that provide them.
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
| Browser/loopback PKCE and device login, logout, refresh-family rotation, refresh races, protected token storage | P2.06 grants |
| Authenticated server narinfo/NAR fetch, the real credential exchange replacing the static bearer placeholder, current-status outage semantics from the server | P2.06, P2.08 |
| Authenticated server upload (sessions, quota reservation, uncertain-reply reconciliation with the server) behind the `Uploader` interface | P2.07, P2.08 |
| Server NixOS module integration | private server modules (P2.11) |
| Agent or keyring credential sources beyond systemd credentials and owner-only files | later client slice |
| Reconciling `stockhouse-proto` with the private P2.08a server types | follow-up after P2.08a |
| Any crates.io or other release | separate authorization |
