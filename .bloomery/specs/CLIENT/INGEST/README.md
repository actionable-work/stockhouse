---
id: INGEST
name: Post-build Ingestion
tagline: A durable, bounded queue fed by the Nix post-build hook and drained by a retrying uploader.
description: |
  `stockhouse-ingest` records locally built store paths in a durable
  filesystem queue from the post-build hook, drains it with retries and
  backoff through a pluggable uploader, makes overflow visible, and
  reconciles missed paths by rescanning the local store. Uploader failures
  never fail or block a build.
---

# Post-build Ingestion

- [Ingestion design](design/ingest.md)
