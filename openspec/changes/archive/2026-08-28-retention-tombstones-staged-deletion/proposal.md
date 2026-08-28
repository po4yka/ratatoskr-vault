## Why

Verified snapshots accumulate indefinitely today, while a withdrawn desired policy only excludes a target and cannot safely reclaim either local or off-host bytes. Because backup artifacts are evidence, Ratatoskr needs deterministic retention and a durable grace-and-deletion workflow before any storage pressure can authorize physical removal.

## What Changes

- Add a deterministic retention policy engine that selects eligible snapshots per mirror using keep-N protection, age floors, pin/hold precedence, and a documented quota-pressure escalation order.
- Make operator and upstream/user snapshot pins durable and exempt pinned snapshots from every automatic retention or deletion decision until explicitly unpinned.
- Turn a governing inactive desired policy into a durable target tombstone and grace deadline without deleting bytes; reactivation cancels pending automatic deletion while preserving the audit history.
- Add an idempotent two-stage deletion workflow that cannot start before the tombstone deadline, removes local artifact bytes before verified replica objects, and journals every planned, refused, attempted, completed, or failed stage.
- Keep tombstones, manifests, artifact identities, and decision evidence queryable after physical bytes are removed, including the exact policy inputs and reasons behind each outcome.
- Document that quota pressure first removes ordinary eligible snapshots, then inactive/tombstoned target snapshots, and otherwise refuses allocation; pins, age floors, and the tombstone window are never overridden.
- Keep the existing first-version API, contract, and editable `schema.sql`; no migration, compatibility route, provider pin UI, or encryption-at-rest change is introduced.

## Capabilities

### New Capabilities

- `retention-deletion`: Deterministic snapshot retention, durable pins and tombstones, non-bypassable grace, staged local/replica deletion, and complete decision/stage audit queries.

### Modified Capabilities

- `desired-state-ingestion`: Governing inactive and reactivated policy revisions now create or cancel automatic tombstone eligibility atomically with target convergence.
- `target-state-machine`: Target lifecycle gains an evidenced transition from excluded through staged deletion to a terminal deleted state while preserving reactivation before deletion begins.
- `snapshot-bundles`: Immutable snapshot evidence distinguishes retained metadata from physically deleted artifact bytes, and retention may expire only explicitly eligible snapshots.
- `off-host-replication`: Replication scheduling excludes deletion-staged artifacts and verified remote deletion becomes a separately journaled stage after local deletion.
- `git-vault-schema`: The current schema persists pins, richer retention policy inputs, tombstone deadlines, deletion plans/stages, and append-only retention audit evidence.

## Impact

The change affects core retention models, desired-state convergence, the target state machine, the single root `schema.sql`, PostgreSQL repositories and disposable-database tests, local BlobStore deletion, S3-compatible replica deletion and verification, Vault orchestration, telemetry, and operator documentation. It consumes the existing desired-policy contract and replica inventory without changing any cross-repository message shape or adding a dependency.
