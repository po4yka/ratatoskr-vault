## ADDED Requirements

### Requirement: LFS-enabled restore drills reconstruct from stored object artifacts

When a verified manifest includes LFS evidence, a restore drill SHALL use only the manifest and its stored bundle and LFS object BlobRefs. Before reporting success it MUST verify every object's BlobRef size and digest, reconstruct the standard LFS object layout inside the isolated restored repository, and recompute the aggregate object-set digest from the materialized immutable bytes. Live mirrors and source remotes MUST remain denied. The drill MUST NOT require working-tree checkout, smudge filters, a source remote, or bare-repository `HEAD` behavior from Git LFS.

#### Scenario: Stored bundle and LFS objects restore end to end

- **WHEN** the LFS-enabled fixture snapshot is restored after its live source and mirror are made unavailable
- **THEN** the isolated repository has the expected refs and complete verified LFS object set, and the terminal drill report records both Git and LFS stages as passed

#### Scenario: Corrupt stored LFS object fails before success

- **WHEN** one stored LFS object is truncated or its bytes disagree with its manifest digest
- **THEN** the drill records a typed LFS integrity failure, reports no restorable result, and does not consult the live source or mirror

### Requirement: Restore reports carry LFS proof fields when required

Every terminal drill report for an LFS-enabled snapshot SHALL record expected and observed LFS object counts, total bytes, aggregate digests, and the outcome of object-layout verification. A retry MUST append a new report and MUST NOT rewrite a failed LFS result.

#### Scenario: LFS mismatch report is immutable

- **WHEN** a drill commits an LFS aggregate-digest mismatch and a later retry succeeds
- **THEN** the failed and successful attempts remain separately readable with their original LFS proof fields
