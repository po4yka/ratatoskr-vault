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

### Requirement: Restore drills use only stored artifacts in an isolated environment

A restore drill SHALL reconstruct the repository from the verified stored bundle in a newly empty
scratch environment. The drill MUST have no network access, MUST NOT read or write the live mirror
root, MUST verify Git object connectivity, and MUST compare the complete restored ref-name/object-ID
set with the signed manifest. Scratch cleanup MUST affect only the drill-owned directory.

#### Scenario: Stored bundle restores exact refs without the source

- **WHEN** a due snapshot has a valid stored bundle and its source and live mirror are unavailable
- **THEN** the drill succeeds from the artifact alone, and the restored ref names and object IDs exactly equal the manifest evidence

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
