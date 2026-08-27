# restore-verification Specification

## Purpose

Defines how Vault repeatedly proves that immutable snapshot artifacts remain intact and can restore
the exact recorded Git refs in a confined, network-disabled environment with durable evidence.

## Requirements

### Requirement: Stored artifacts and manifest trust chains are verified before success

Vault SHALL re-read the stored manifest and every bundle it names, compute their SHA-256 digests and
byte lengths, verify the manifest signature against an explicitly configured trusted key, and
validate every parent-manifest BlobRef back to a configured trust anchor. A missing artifact,
digest/size mismatch, invalid signature, broken parent link, or failed bundle check MUST produce
failed terminal evidence and MUST NOT produce a successful restore report.

#### Scenario: Corrupted bundle is detected

- **WHEN** bytes behind a stored bundle reference differ from the digest or length recorded by its signed manifest
- **THEN** verification records a hash-mismatch failure, emits no verified fact, and leaves prior immutable snapshot evidence unchanged

#### Scenario: Broken manifest chain is detected

- **WHEN** a manifest signature is invalid or its parent reference cannot be resolved and verified back to the configured trust anchor
- **THEN** verification records the exact trust-chain stage as failed and does not report the snapshot verified

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

### Requirement: Verification and drill reports are complete immutable evidence

Every verification and restore drill SHALL append one terminal report containing stable run and
snapshot identifiers, the manifest BlobRef, start and completion timestamps, total and per-stage
durations, checked artifact digests and sizes, expected and observed ref counts and ref-set digests,
bundle/connectivity/signature/chain/isolation outcomes, the final outcome, and a bounded typed failure
when unsuccessful. A terminal report MUST NOT be rewritten into another outcome.

#### Scenario: Successful report contains every proof field

- **WHEN** a restore drill completes successfully for a fixture snapshot
- **THEN** its persisted report identifies the artifact and manifest, records every check as passed, carries timings and exact ref evidence summaries, and contains no invented failure

#### Scenario: Failed drill remains failed

- **WHEN** restored refs differ from the signed manifest and the terminal failure report has committed
- **THEN** a retry creates a new report and cannot update the failed report to passed

### Requirement: Scheduling is configurable and bounded by finite budgets

Vault SHALL select due snapshots according to configured verification and drill frequencies and a
configured sample-size ceiling. Selection SHALL prefer snapshots whose successful evidence is oldest
or absent, SHALL admit no more than the configured concurrency, scratch-byte, and execution-time
budgets, and SHALL record a deferred budget outcome rather than beginning unbounded work. A deferred
snapshot remains due for a later schedule pass.

#### Scenario: Sample and scratch budget bound one schedule pass

- **WHEN** more snapshots are due than the configured sample size and admitting the next snapshot would exceed the scratch-byte budget
- **THEN** the pass starts only the oldest due snapshots that fit both limits and records the remaining due work as deferred without opening their artifacts

### Requirement: Alert-worthy outcomes use the fleet event grammar

Vault SHALL atomically append a versioned Vault outbox fact with each committed terminal
verification or drill failure. Corruption, signature or chain failure, isolation failure, and
restored-ref mismatch SHALL have distinct stable classifications. The outbox record MUST follow the
fleet event-subject grammar and MUST NOT claim bus publication before an outbox publisher exists.

#### Scenario: Restore mismatch commits one alert-worthy fact

- **WHEN** a restore report commits with a restored-ref mismatch
- **THEN** the same database transaction appends exactly one replay-safe `vault.restore.failed.v1` outbox fact carrying the report identity and alert-worthy failure classification

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
