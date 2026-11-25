# P2.11k1 Stockhouse protocol v2

The Stockhouse uploader and lease client default to `auto`. They probe the
private tier and use the highest version both sides offer. An uploader
process caches its selection for one hour. If a v2 route disappears, the
uploader stops making new requests in that pass and probes again next time.
The lease command can fall back to v1 during the same run.

The uploader groups due records whose combined closure fits the configured
path limit. It declares and reconciles paths in batches, sends up to four
NARs concurrently by default, and publishes in reference order. A lost
reply is reconciled through status before any NAR resend. The local ledger
records completion only once the server holds the path and its reference
closure. A forgotten declaration identity is renewed in the queue before the
next declaration. The queue format stays at version 1.

Lease renewal sends all resolved slots in one ordered v2 batch. A slot
continues to identify a single lease through upgrades and rollbacks.

The NixOS options are `services.stockhouse.uploader.protocol` (`auto`,
`v1` or `v2`) and `services.stockhouse.uploader.maxInflightNars` (1–16).
The protocol selection applies to both uploading and lease renewal.

The [server handoff](PROTOCOL_V2_SERVER.md) defines the counterpart for
P2.11k2. The client checks use Stockhouse's loopback fake servers as their
interoperability target. Production server qualification is part of the
server follow-up.

## Validation

- `bloomery check` passed all 173 checks in run
  `rce644-18ebf55c7ca35fe0-0`, including requirement traceability and the
  generated code-health report.
- `nix flake check` passed, including all 22 VM runs across the two pinned
  Nix versions. The new stories exercised batching, dependency progress
  under published-byte quota, lease rebinding, runtime rollback, lost
  publish replies, restart reconciliation, and `Retry-After`.
- Independent re-reviews of the implementation and VM fixtures found no
  remaining actionable issues.
