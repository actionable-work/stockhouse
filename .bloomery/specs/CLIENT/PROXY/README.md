---
id: PROXY
name: Substitution Proxy
tagline: A loopback-only Nix substituter over private, shared and public upstream caches.
description: |
  `stockhouse-proxy` serves a Nix binary-cache HTTP interface on loopback. It
  looks narinfo up in the private, shared, then public tier, verifies
  signatures and served bytes, keeps credentials on the tier they belong to,
  and lets only a genuine authorized miss fall through to the next tier.
---

# Substitution Proxy

- [Proxy design](design/proxy.md)
