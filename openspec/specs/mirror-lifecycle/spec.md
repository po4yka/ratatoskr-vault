# Mirror Lifecycle Specification

## Purpose

Defines how Vault creates and refreshes local Git mirrors without exceeding its one-host storage
budget, losing cancellation evidence, or treating unchecked Git data as preserved state.

## Requirements

### Requirement: State-machine-driven mirror lifecycle

For an active desired policy that requires Git preservation, Vault SHALL create a target-owned bare
mirror from `requested` and refresh a ready, degraded, or healthy mirror through the legal
`fetching` transition. A successful clone SHALL leave the target `ready`; a successful fetch SHALL
leave it ready for the later snapshot stage and SHALL NOT report the target healthy.

#### Scenario: Initial clone becomes ready

- **WHEN** an enrolled `requested` target with a Git-preservation policy is reconciled against a
  valid local fixture repository and sufficient capacity
- **THEN** Vault creates the identifier-derived bare mirror, records the clone run and integrity
  evidence, and transitions the target to `ready`

#### Scenario: Refresh uses the existing mirror

- **WHEN** a `ready` target's source fixture receives a new commit and the target is reconciled
- **THEN** Vault fetches into its existing mirror, records a fetch run with object-count evidence,
  and the mirror contains the new commit without creating a second target mirror

### Requirement: Explicit storage-budget admission

Vault SHALL reserve both the configured per-mirror byte budget and the configured global mirror
budget before it starts clone or fetch work. If either reservation cannot be made, Vault SHALL
refuse that run, record a quota-exceeded result, transition the target to `degraded`, and preserve
all existing mirrors unchanged. Vault SHALL NOT silently prune, delete, or evict another mirror to
admit the work.

#### Scenario: Per-mirror limit refuses enrollment

- **WHEN** a new mirror's configured reservation exceeds its per-mirror byte budget
- **THEN** no Git clone starts, no mirror directory is published, the run records the per-mirror
  quota refusal, and the target becomes `degraded`

#### Scenario: Global limit refuses a competing mirror

- **WHEN** one admitted mirror holds the available global reservation and a second target requests
  an operation whose reservation would exceed the remaining global budget
- **THEN** the second run is refused and degraded while the first mirror and its reservation remain
  intact

### Requirement: Cooperative cancellation and resumable observations

Vault SHALL accept cooperative cancellation for each lifecycle run. Cancelling an initial clone
SHALL remove its Vault-owned staging directory and SHALL NOT publish a partial mirror. Cancelling a
fetch SHALL retain the prior valid bare mirror, record an interrupted run and a resumable
checkpoint, and allow the next reconciliation to attempt the fetch again. A cancelled run SHALL
release its quota reservation and SHALL NOT be recorded as successful.

#### Scenario: Cancelled clone leaves no partial garbage

- **WHEN** cancellation arrives while an initial clone of a local fixture is in progress
- **THEN** the target has no published mirror or staging directory, its run is recorded as
  interrupted, and its reservation is available to a later run

#### Scenario: Cancelled fetch resumes from the prior mirror

- **WHEN** cancellation arrives while a refresh is in progress after a target already has a valid
  mirror
- **THEN** the prior mirror remains usable, the run records an interrupted checkpoint, and a later
  reconciliation fetches the pending source commit into that same mirror

### Requirement: Post-operation integrity evidence

After every successful clone or fetch, Vault SHALL run full Git object-database verification and
sanity-check the mirror's observable object and ref counts before recording the run as successful.
An integrity failure or impossible count result SHALL record diagnostics, transition the target to
`degraded`, and SHALL NOT overwrite the last successful mirror observation.

#### Scenario: Fsck failure degrades target

- **WHEN** a completed lifecycle operation is followed by a failing Git integrity check against a
  corrupted local fixture mirror
- **THEN** the run records an integrity failure, the target becomes `degraded`, and no successful
  observation is recorded for that run

#### Scenario: Successful check records bounded evidence

- **WHEN** clone or fetch completes and its mirror passes verification
- **THEN** the persisted run includes its operation kind, object count, byte observation, and
  integrity result

### Requirement: Deployment-aligned admission concurrency

Vault SHALL cap concurrent mirror operations at the deployment target's documented four CPU cores.
The cap SHALL be applied before spawning Git and shall include clones and fetches together, so a
burst of scheduled targets cannot create unbounded processes or reservations.

#### Scenario: Fifth simultaneous operation waits for admission

- **WHEN** four mirror operations hold admission and a fifth is requested
- **THEN** Vault does not start a fifth Git process until one of the four operations completes or
  is cancelled

### Requirement: Healthy observations are immutable snapshot inputs

After a successful mirror lifecycle run has recorded integrity and object evidence, Vault SHALL
make that exact observation eligible for snapshot creation. Snapshot construction SHALL read the
published bare mirror without changing the completed lifecycle evidence; failure to create or
publish a snapshot SHALL leave the last successful mirror observation intact and SHALL NOT advance
the target to a verified or healthy snapshot state.

#### Scenario: Snapshot publication failure preserves the mirror observation

- **WHEN** a healthy mirror observation is selected for a snapshot and local artifact publication
  fails
- **THEN** the recorded successful mirror observation remains available, no immutable snapshot is
  claimed, and the target does not report snapshot verification or health
