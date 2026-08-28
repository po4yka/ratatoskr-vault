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
BlobRef with its cryptographic digest and byte length, a signing-key identifier, and a signature over
the canonical unsigned manifest bytes. The parent BlobRef SHALL form a verifiable digest chain to an
explicit trust anchor. The manifest bytes themselves SHALL be published as an immutable
content-addressed BlobRef. Verification and restore outcomes SHALL remain separate append-only
reports and MUST NOT be rewritten into the signed snapshot manifest.

#### Scenario: Manifest names every required evidence field

- **WHEN** Vault publishes a snapshot from a healthy fixture mirror
- **THEN** resolving the manifest BlobRef yields the generator version, timestamp, parent linkage,
  signing-key identity and verifiable signature, exact ref evidence, and the digest, media type, and
  byte length of its bundle BlobRef

#### Scenario: Parent linkage is a cryptographic chain

- **WHEN** a signed child manifest names a parent manifest
- **THEN** verification resolves the exact parent BlobRef, verifies its digest and signature, and continues until the configured trust anchor is reached without rewriting either manifest

### Requirement: Snapshot verification is evidence, not a metadata claim

A stored-byte verification SHALL be called passed only after manifest and bundle bytes pass digest
and size checks and the signature and parent digest chain reach a trusted anchor. A restore drill
SHALL be called passed only after Git verifies the bundle, reconstructs it, checks connectivity, and
matches refs. A later failed attempt SHALL append failure evidence without deleting the snapshot or
changing a prior successful report.

#### Scenario: Metadata cannot convert corruption to success

- **WHEN** stored bundle bytes fail their manifest digest check
- **THEN** no database-only status update can produce verified evidence and the mismatch remains a failed verification report

### Requirement: Snapshot persistence is append-only and mirror-linked

Vault SHALL persist a new snapshot record for every published snapshot, linked to the exact mirror
and successful mirror observation it captured, plus artifact and manifest records that name the
published BlobRefs. Repeating a request after publication SHALL reuse immutable content references
where bytes match but SHALL NOT rewrite prior snapshot, artifact, manifest, or mirror evidence.

#### Scenario: Repeated snapshot preserves prior evidence

- **WHEN** a snapshot request is retried after its bundle and manifest were already published
- **THEN** the persisted records still identify the original mirror observation and immutable
  BlobRefs, and no prior evidence row is updated in place

### Requirement: Required replica verification participates in snapshot health

Snapshot health SHALL distinguish verified local preservation from required off-host convergence. When the governing desired state requires off-host storage, the snapshot SHALL be healthy only after every required bundle and manifest placement has current remote digest-and-size verification. Missing, failed, stale, or deferred required placements SHALL produce degraded off-host health while preserving all prior local verification evidence. A snapshot whose policy does not require off-host storage MUST NOT be degraded merely because an optional replica is unavailable.

#### Scenario: Required unverified replica degrades snapshot health

- **WHEN** a locally verified snapshot requires off-host storage and its bundle upload is deferred or fails verification
- **THEN** snapshot health reports local verification as passed and off-host convergence as degraded rather than reporting the snapshot healthy

#### Scenario: Verified required placements make health converge

- **WHEN** every required bundle and manifest placement for a locally verified snapshot passes remote re-download verification
- **THEN** snapshot health reports the required off-host component verified and the snapshot can become healthy without rewriting its local verification report

### Requirement: LFS-enabled manifests cover every immutable LFS object

The existing schema-version-1 signed snapshot manifest SHALL carry an optional LFS evidence section. When present, it SHALL include the Git LFS tool version, canonical object count and total bytes, one sorted entry per referenced object containing its lowercase SHA-256 object identifier, size, and immutable BlobRef, plus a deterministic aggregate digest over the complete object set. `includes_lfs` MUST be true exactly when complete LFS evidence is present; a Git bundle alone MUST NOT satisfy that claim.

#### Scenario: LFS manifest is canonical

- **WHEN** an LFS-enabled fixture is collected and its object observations arrive in different orders
- **THEN** manifest serialization, object ordering, aggregate digest, and signature input are identical and cover every object exactly once

#### Scenario: Partial LFS set cannot produce complete manifest

- **WHEN** any referenced object lacks verified published bytes
- **THEN** Vault publishes no manifest claiming `includes_lfs=true` and retains the prior verified snapshot unchanged

### Requirement: Snapshot publication orders LFS evidence before the manifest

Vault SHALL create or reuse verified content-addressed LFS object blobs before signing and create-only publishing the manifest that references them. A retry after partial publication SHALL converge on the same object BlobRefs and create a new terminal attempt without overwriting an existing manifest or object.

#### Scenario: Manifest publication failure preserves reusable objects without a snapshot claim

- **WHEN** all LFS objects publish but manifest create-only publication fails
- **THEN** no new complete snapshot claim exists, the content-addressed objects remain verifiable and reusable, and a retry cannot overwrite prior immutable evidence

### Requirement: Retention removes bytes without rewriting snapshot evidence

An authorized retention workflow MAY mark an eligible snapshot expired and later deleted and remove its unshared physical artifact bytes only after the required tombstone and staged-deletion evidence exists. The snapshot, manifest, artifact identity, hash, size, source observation, verification, restore, pin, retention, and deletion records MUST remain immutable and queryable; metadata mutation alone MUST NOT claim that bytes were deleted.

#### Scenario: Deleted bytes retain immutable identity

- **WHEN** every authorized local and replica deletion stage for a snapshot completes
- **THEN** its physical availability reports deleted while its original manifest, BlobRefs, verification reports, restore reports, and deletion audit remain queryable and unchanged

### Requirement: Retention respects shared immutable artifacts

Physical deletion of a content-addressed artifact MUST be suppressed while any retained snapshot references the same local digest or replica object key. Suppression SHALL be terminal evidence for the retiring snapshot and MUST NOT misreport the shared bytes as absent.

#### Scenario: Shared bundle remains available

- **WHEN** two snapshots resolve to the same bundle digest and only one snapshot is retention-eligible
- **THEN** the bundle remains readable for the retained snapshot and the retiring snapshot records shared-reference retention instead of a physical delete
