---
id: INGEST
name: Post-build Ingestion
tagline: A durable, bounded queue fed by the Nix post-build hook and drained by a retrying uploader.
description: |
  `stockhouse-ingest` records locally built store paths in a durable
  filesystem queue from the post-build hook, drains it with retries and
  backoff through a pluggable uploader, makes overflow visible, and
  reconciles missed paths by rescanning the local store. The `server`
  backend uploads each built closure to the private tier with protocol v1,
  and the lease client renews this machine's system roots; both live in
  the `stockhouse-upload` crate. Uploader and
  lease failures never fail or block a build, boot or activation.
---

# Post-build Ingestion

- [Ingestion design](design/ingest.md)
