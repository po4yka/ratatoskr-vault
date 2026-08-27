## MODIFIED Requirements

### Requirement: Restore drills use only verified stored artifacts in an isolated environment

A restore drill SHALL select an explicit source consisting of either verified local BlobRefs or a verified off-host replica placement. For a replica source, Vault SHALL stream the selected manifest and bundle into the drill-owned scratch environment and verify each SHA-256 digest and byte length before Git work begins. The drill SHALL reconstruct the repository in a newly empty scratch environment. Git reconstruction MUST have no network access, MUST NOT read or write the live mirror root, MUST verify Git object connectivity, and MUST compare the complete restored ref-name/object-ID set with the signed manifest. Scratch cleanup MUST affect only the drill-owned directory, and the terminal report SHALL identify whether local or replica bytes were selected.

#### Scenario: Stored bundle restores exact refs without the source

- **WHEN** a due snapshot has a valid stored bundle and its source and live mirror are unavailable
- **THEN** the drill succeeds from the artifact alone, and the restored ref names and object IDs exactly equal the manifest evidence

#### Scenario: Verified replica is selected for a replica-aware drill

- **WHEN** replica-origin restore is requested and one placement contains a currently verified manifest and every required bundle
- **THEN** the selector chooses that replica, the drill verifies downloaded bytes before network-disabled Git work, and the report identifies the replica source

#### Scenario: Replica-required selection does not fall back silently

- **WHEN** replica-origin restore is required but no replica has a complete verified artifact set
- **THEN** the drill is deferred or fails with a typed source-unavailable result and MUST NOT claim replica restore by using local bytes

#### Scenario: Live mirror access is refused

- **WHEN** a drill operation is given a path that resolves into or through the configured live mirror root
- **THEN** the runner refuses it before spawning, records an isolation failure, and no live mirror file is read, changed, or deleted
