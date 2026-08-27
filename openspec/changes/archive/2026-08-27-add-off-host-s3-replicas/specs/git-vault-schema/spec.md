## ADDED Requirements

### Requirement: Replica inventory and terminal transfer evidence are persisted

The current `schema.sql` SHALL contain replica-target identity, per-artifact placement inventory, and append-only replication-attempt evidence. A placement SHALL link one existing immutable snapshot artifact to one replica target and record a collision-safe object key, state, upload observation, and last successful verification observation. Terminal attempts SHALL carry stable failure classes and ordered timestamps, and database constraints SHALL prevent a failed or unverified attempt from manufacturing a verified placement. No credential value SHALL have a database column.

#### Scenario: Fresh schema records a verified remote placement

- **WHEN** a disposable database receives the current schema and a bundle passes remote re-download verification
- **THEN** one placement identifies its snapshot artifact, replica target, object key, upload time, and last-verified time while one append-only passed attempt records the terminal transfer evidence

#### Scenario: Failed attempt cannot be rewritten to passed

- **WHEN** an update attempts to convert a committed checksum-mismatch replication attempt into a pass
- **THEN** PostgreSQL rejects the mutation and a retry must append a new attempt identifier
