## ADDED Requirements

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
