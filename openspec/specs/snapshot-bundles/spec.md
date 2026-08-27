# snapshot-bundles Specification

## Purpose

Defines immutable, portable Git-bundle snapshots and their cryptographic manifests, so a healthy
mirror observation is not the only evidence that Vault preserved repository history.

## Requirements

### Requirement: Full snapshot captures every mirror ref

Vault SHALL create a full bundle from a healthy bare-mirror observation that includes every ref
visible at the snapshot point. The manifest SHALL record every ref name with its object ID and a
SHA-256 digest of a canonical ordering of that complete ref set. The bundle and manifest SHALL be
associated with the same mirror observation; a changed or unhealthy mirror MUST NOT be represented
as that observation's snapshot.

#### Scenario: Fixture refs are complete in the manifest

- **WHEN** a healthy fixture mirror has branches, tags, and non-head refs at a snapshot point
- **THEN** its persisted manifest contains exactly those ref-name/object-ID pairs and the recorded
  ref-set digest matches the canonical complete list

### Requirement: Snapshot artifacts have immutable BlobRefs

Vault SHALL write each full-bundle and manifest byte stream to its own local content-addressed
BlobStore and SHALL persist the resulting BlobRef with SHA-256 digest, media type, and byte length.
The store SHALL publish bytes atomically and SHALL reject a second write that would alter content
at an existing digest. A retry of identical bytes SHALL produce the same deterministic outcome and
MUST NOT create another artifact identity.

#### Scenario: Existing digest cannot be overwritten

- **WHEN** a BlobStore write is attempted for a digest that already names published bytes
- **THEN** a differing byte stream is rejected without changing the stored artifact, while an
  identical retry returns the pre-existing immutable BlobRef deterministically

### Requirement: Manifest is self-contained immutable snapshot evidence

Every published snapshot manifest SHALL include its own schema/generator version, creation
timestamp, optional parent-snapshot manifest BlobRef, complete ref list and digest, and each bundle
BlobRef with its cryptographic digest and byte length. The manifest bytes themselves SHALL be
published as an immutable content-addressed BlobRef. Manifest metadata MUST NOT claim bundle
verification, restore-drill success, LFS collection, or off-host placement that this slice has not
performed.

#### Scenario: Manifest names every required evidence field

- **WHEN** Vault publishes a snapshot from a healthy fixture mirror
- **THEN** resolving the manifest BlobRef yields the generator version, timestamp, parent linkage
  state, exact ref evidence, and the digest, media type, and byte length of its bundle BlobRef

### Requirement: Snapshot persistence is append-only and mirror-linked

Vault SHALL persist a new snapshot record for every published snapshot, linked to the exact mirror
and successful mirror observation it captured, plus artifact and manifest records that name the
published BlobRefs. Repeating a request after publication SHALL reuse immutable content references
where bytes match but SHALL NOT rewrite prior snapshot, artifact, manifest, or mirror evidence.

#### Scenario: Repeated snapshot preserves prior evidence

- **WHEN** a snapshot request is retried after its bundle and manifest were already published
- **THEN** the persisted records still identify the original mirror observation and immutable
  BlobRefs, and no prior evidence row is updated in place
