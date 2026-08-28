## ADDED Requirements

### Requirement: Current schema persists retention policy and pin evidence

The single current `schema.sql` SHALL persist keep-N, age-floor and grace inputs plus durable operator/user snapshot pins and their revocations. Constraints SHALL reject invalid policy bounds, unknown pin sources, and a deletion decision that treats any actively pinned snapshot as eligible.

#### Scenario: Active pin cannot be selected

- **WHEN** a disposable database contains an active pin and a write attempts to persist an executable automatic deletion plan for that snapshot
- **THEN** PostgreSQL rejects the write and retains both the pin and snapshot

### Requirement: Current schema persists tombstones and staged deletion

The current schema SHALL persist target tombstones, per-snapshot deletion plans with fixed not-before deadlines, and ordered local and per-replica stage attempts. Constraints and guards SHALL prevent stage execution before grace, replica execution before local completion, plan completion with missing stages, and mutation or removal of terminal evidence.

#### Scenario: Early stage is rejected by storage

- **WHEN** a write attempts to start a deletion stage before its plan deadline in a disposable database
- **THEN** PostgreSQL rejects it and no running or completed stage remains

#### Scenario: Replica stage cannot precede local stage

- **WHEN** a write attempts to start replica deletion before the local stage has terminal completion evidence
- **THEN** PostgreSQL rejects it without changing the plan or placement

### Requirement: Current schema preserves a complete retention audit

The current schema SHALL retain append-only evaluation, candidate, protection, tombstone, plan, and stage evidence with policy inputs, reasons, correlation identity, and timestamps. Indexed queries by target or snapshot SHALL return stable chronological evidence after physical deletion.

#### Scenario: Audit survives completed deletion

- **WHEN** a disposable database completes every deletion stage and the snapshot audit is queried
- **THEN** the query returns the selecting evaluation and every stage outcome while the immutable snapshot and manifest identity rows remain
