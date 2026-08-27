## Purpose

Defines how Vault creates, verifies, inventories, and retries bounded off-host replicas of immutable snapshot artifacts on S3-compatible storage.

## ADDED Requirements

### Requirement: Replica uploads are independently verified

Vault SHALL stream each required bundle and manifest from its verified local BlobRef to a deterministic collision-safe object key, then re-read the completed remote object and recompute its SHA-256 digest and byte length before recording a verified placement. An upload response, provider ETag, or metadata-only check MUST NOT by itself produce verified replica evidence.

#### Scenario: Uploaded bytes round-trip with the same digest

- **WHEN** a bundle and manifest are replicated to an available S3-compatible target
- **THEN** Vault re-downloads each object, observes the exact local digest and byte length, and records both placements as verified

#### Scenario: Remote corruption is not accepted

- **WHEN** the object returned after upload differs from the local BlobRef digest or byte length
- **THEN** the attempt records a checksum-mismatch failure and no verified placement or healthy off-host result is produced

### Requirement: Replica inventory and attempts are durable evidence

Vault SHALL durably identify each configured replica target and each artifact placement by replica, immutable artifact identity, and object key. It SHALL record when bytes were uploaded, when they were last successfully verified, and every terminal attempt outcome without rewriting prior terminal evidence. Credentials MUST NOT be persisted in inventory, attempt evidence, logs, or metrics.

#### Scenario: Re-verification preserves placement history

- **WHEN** a previously verified placement is verified again
- **THEN** its current last-verified observation advances and a new terminal attempt is appended without changing the earlier attempt

### Requirement: Replication backlog and concurrency are bounded

Each scheduler pass SHALL admit no more than the configured pending-item, byte, concurrency, and operation-deadline ceilings. Due work beyond those ceilings SHALL remain durably discoverable from local snapshot evidence and SHALL be reported as deferred rather than queued in unbounded process memory, discarded, or mislabeled as complete.

#### Scenario: More due replicas than the backlog ceiling

- **WHEN** an unreachable replica leaves more due artifact placements than one configured backlog window can admit
- **THEN** only the bounded window is admitted, excess work remains due for later passes, and local snapshot creation and verification continue

### Requirement: Remote unreachability degrades only off-host convergence

A timeout, authentication failure, provider error, or unreachable endpoint SHALL produce a typed failed replication attempt and leave verified local snapshot evidence unchanged. Vault MUST NOT delete, rewrite, or stop creating local snapshots solely because a replica target is unavailable.

#### Scenario: Local work continues during a replica outage

- **WHEN** the S3-compatible endpoint cannot be reached within the configured deadline
- **THEN** the replication attempt fails with bounded evidence, the work remains due, and subsequent local snapshot and verification operations remain admissible
