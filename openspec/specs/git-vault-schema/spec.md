# git-vault-schema Specification

## Purpose
The first version of the `git_vault` PostgreSQL schema: the one file Vault applies to a fresh database, owned entirely by this bounded context.

## Requirements

### Requirement: One schema definition, applied in place

The schema SHALL be one `schema.sql` at the repository root, embedded in the binary at compile time. A schema change edits that file in place; there is no migration ledger and no version history.

#### Scenario: A fresh database receives the whole schema from the embedded file

- **WHEN** a disposable database is created and the embedded schema is applied
- **THEN** every table listed below exists under the `git_vault` schema with its columns and constraints

### Requirement: Applying the schema is idempotent

Applying the schema to a database that already has it SHALL change nothing and succeed; two processes applying concurrently SHALL serialize so neither fails.

#### Scenario: Second apply on a populated database succeeds and changes nothing

- **WHEN** the schema has been applied once and a second apply runs
- **THEN** it succeeds without recreating or clearing any object

### Requirement: The first version carries the tables the architecture names

The `git_vault` schema SHALL contain, at minimum: `targets`, `desired_state_revisions`, `mirrors`, `sync_runs`, `snapshots`, `snapshot_artifacts`, `manifests`, `integrity_checks`, `restore_drills`, `retention_policies`, `tombstones`, `storage_locations`, `collector_runs`, `outbox`, and `inbox`.

#### Scenario: Every named table exists after apply

- **WHEN** the schema is applied to a fresh database and the catalogue is queried for `git_vault` tables
- **THEN** all fifteen named tables are present

### Requirement: The schema follows the fleet database conventions

Identifiers SHALL be UUID primary keys minted by the application (no database default), closed vocabularies SHALL be checked text rather than PostgreSQL enums, timestamps SHALL be `timestamptz`, state machines SHALL be enforced by CHECK constraints where expressible, and no foreign key SHALL cross a schema boundary.

#### Scenario: A state value outside the vocabulary is refused by the database

- **WHEN** a row is inserted into `git_vault.targets` with a status outside the closed vocabulary
- **THEN** PostgreSQL rejects the insert with a constraint violation

### Requirement: Snapshot evidence records name immutable blob references

The current `schema.sql` definition SHALL persist each snapshot's source mirror and successful
mirror observation, bundle artifact BlobRef fields (owner, SHA-256 digest, media type, byte length),
and one manifest BlobRef with the same fields. Snapshot, artifact, and manifest rows SHALL be
append-only evidence and SHALL use database constraints to prevent a snapshot from naming more than
one manifest or a manifest from being associated with more than one snapshot.

#### Scenario: Fresh schema records a snapshot and its manifest reference

- **WHEN** the current schema is applied to a disposable database and a snapshot is persisted
- **THEN** the database accepts exactly one immutable manifest reference linked to that snapshot,
  its source mirror observation, and its content-addressed bundle artifact

### Requirement: Verification, drill, and event evidence is append-only and linked

The current schema SHALL persist each verification attempt and restore drill with its snapshot,
manifest, immutable report fields, outcome, typed failure, timings, and correlation metadata. It
SHALL constrain terminal outcomes and prevent a terminal report from being rewritten. A terminal
report and its canonical fleet event SHALL commit atomically through the Vault outbox, with a stable
uniqueness key making replay idempotent.

#### Scenario: Drill failure and alert event commit together

- **WHEN** a restore drill records a terminal ref-mismatch failure
- **THEN** the same transaction persists one failed immutable report and one uniquely keyed outbox event linked to it, or persists neither

#### Scenario: Terminal report cannot change outcome

- **WHEN** an update attempts to convert a committed failed verification or drill report to passed
- **THEN** PostgreSQL rejects the mutation and a later retry must use a new run identifier

### Requirement: Replica inventory and terminal transfer evidence are persisted

The current `schema.sql` SHALL contain replica-target identity, per-artifact placement inventory, and append-only replication-attempt evidence. A placement SHALL link one existing immutable snapshot artifact to one replica target and record a collision-safe object key, state, upload observation, and last successful verification observation. Terminal attempts SHALL carry stable failure classes and ordered timestamps, and database constraints SHALL prevent a failed or unverified attempt from manufacturing a verified placement. No credential value SHALL have a database column.

#### Scenario: Fresh schema records a verified remote placement

- **WHEN** a disposable database receives the current schema and a bundle passes remote re-download verification
- **THEN** one placement identifies its snapshot artifact, replica target, object key, upload time, and last-verified time while one append-only passed attempt records the terminal transfer evidence

#### Scenario: Failed attempt cannot be rewritten to passed

- **WHEN** an update attempts to convert a committed checksum-mismatch replication attempt into a pass
- **THEN** PostgreSQL rejects the mutation and a retry must append a new attempt identifier

### Requirement: Current schema persists LFS collection and restore evidence

The single current `schema.sql` SHALL persist append-only LFS collection runs, each verified LFS object's SHA-256 identifier, byte length and BlobRef, deterministic aggregate evidence, quota outcome, tool version, and LFS restore proof fields. Constraints SHALL prevent duplicate object identities within one snapshot, negative counts or sizes, complete outcomes with missing object evidence, and mutation of terminal results.

#### Scenario: Fresh schema records complete LFS evidence

- **WHEN** the current schema is applied to a disposable database and a complete fixture collection and restore report are persisted
- **THEN** the snapshot resolves to exactly its verified object set and immutable terminal collection and restore evidence

#### Scenario: Inconsistent complete result is refused

- **WHEN** a write claims complete LFS collection with an object count or aggregate digest that does not match its linked object evidence
- **THEN** the transaction fails and no complete result remains

### Requirement: Current schema relates wiki siblings without provider path ownership

The current schema SHALL distinguish repository and wiki targets and relate each wiki target to exactly one parent repository target. Provider external identity and relationship kind SHALL be constrained so repeated discovery cannot create a second wiki sibling, and neither provider names nor source URLs SHALL become storage paths.

#### Scenario: Repeated wiki enrollment converges

- **WHEN** the same parent and wiki identity are enrolled twice in a disposable database
- **THEN** one wiki target and one parent relationship remain, and its storage path is derived only from the internal wiki target identifier

### Requirement: Executable collector vocabulary is approval closed

The current schema SHALL accept terminal collector evidence only for Git LFS and wiki. Provider auxiliary kinds that have no separately owner-approved implementation change, including releases and issues, MUST be rejected by the collector-kind constraint.

#### Scenario: Unapproved collector evidence is rejected

- **WHEN** a write attempts to persist a `releases` or `issues` collector run
- **THEN** the database rejects it while accepting independently valid `git_lfs` and `wiki` evidence

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
