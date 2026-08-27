## ADDED Requirements

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
