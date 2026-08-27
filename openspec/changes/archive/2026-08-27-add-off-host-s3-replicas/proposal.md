## Why

Verified snapshots currently exist only on the Vault host, so loss of that host still destroys every backup copy. Plan item 7 closes this durability gap by making required S3-compatible replicas independently verifiable and usable for restore drills without blocking local snapshot operations during a remote outage.

## What Changes

- Configure one or more S3-compatible replica targets with strict finite transfer and backlog limits; access credentials are accepted only through redacted environment-backed configuration.
- Stream immutable bundle and manifest bytes from the local BlobStore to each required replica, then stream the stored remote bytes back through SHA-256 and size verification before recording success.
- Persist replica targets, per-artifact placement inventory, attempt history, verification timestamps, and bounded pending work in the one current `schema.sql`.
- Include required-replica convergence in snapshot health without rewriting local verification evidence: an unreachable replica degrades off-host health while local snapshot and verification work continues.
- Extend restore-drill admission and execution so an eligible drill can select a verified local or replica source, download replica bytes into its confined scratch root, verify them, and restore without consulting the live mirror.
- Exercise the S3 path against an in-process S3-compatible harness, including upload/download digest equality, corruption and unreachability, backlog bounding, and replica-aware drill selection.
- Document the recommended remote bucket policy and explicitly leave provider lifecycle configuration to operators.

## Capabilities

### New Capabilities

- `off-host-replication`: S3-compatible replica configuration, bounded asynchronous replication, re-download verification, durable inventory, retry/backlog behavior, and required-replica health semantics.

### Modified Capabilities

- `configuration`: Add strict environment-backed replica targets, credential references, transfer deadlines, concurrency, and backlog ceilings.
- `git-vault-schema`: Persist replica destinations, artifact placements, attempts, terminal verification evidence, and bounded pending replication without migrations.
- `snapshot-bundles`: Make required verified replica placement part of snapshot health while preserving local immutable snapshot evidence during remote failures.
- `restore-verification`: Select a verified local or off-host artifact source and prove replica-origin restore through the existing isolated drill contract.

## Impact

The change affects the core configuration and snapshot-health models, `schema.sql` and PostgreSQL persistence, the BlobStore boundary, Vault replication/restore orchestration, telemetry, deterministic integration harnesses, and operator documentation. It adds one maintained S3-compatible Rust client after design-time license, security, compatibility, and dependency-graph review. No cross-repository message contract, new API version, migration, provider-specific lifecycle rule, LFS collector, or destructive retention behavior is introduced.
