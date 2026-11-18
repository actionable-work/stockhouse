---
id: CREDS
name: Credential Sources
tagline: A credential-source interface, owner-only file and systemd credential sources, and a deferred grant flow.
description: |
  `stockhouse-credentials` resolves runtime bearer credentials only from an
  explicitly configured source, keeps them out of logs, arguments and the Nix
  store, and stubs the server grant flow until P2.06 provides it.
---

# Credential Sources

- [Credential design](design/credentials.md)
