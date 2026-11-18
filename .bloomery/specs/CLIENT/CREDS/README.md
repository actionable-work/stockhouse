---
id: CREDS
name: Credential Sources
tagline: Credential sources, server sign-in over OAuth 2.0, a sealed token store and refreshing credentials.
description: |
  `stockhouse-credentials` resolves runtime bearer credentials only from an
  explicitly configured source and keeps them out of logs, arguments and the
  Nix store. `stockhouse-signin` signs in to the server (RFC 8252 loopback
  authorization code with PKCE, RFC 8628 device flow), `stockhouse-token-store`
  keeps the tokens sealed under a systemd credential key, and
  `stockhouse-auth` refreshes them before expiry under a cross-process lock,
  revokes them on logout and yields the current access token as a
  credential source.
---

# Credential Sources

- [Credential design](design/credentials.md)
- [Server sign-in and refresh](design/oauth.md)
- [Token store](design/store.md)
