## Purpose

Defines deterministic, pin-aware retention and non-bypassable tombstone and deletion evidence for local and off-host backup artifacts.

## ADDED Requirements

### Requirement: Retention selection is deterministic per mirror

For the same snapshots, policy, pins, target state, tombstone state, evaluation time, and pressure mode, Vault SHALL produce the same ordered decision. It SHALL protect every actively pinned snapshot, every snapshot younger than the policy age floor, and the newest configured keep-N restorable snapshots of each mirror; it SHALL order remaining eligible snapshots oldest first by creation time and then snapshot identifier.

#### Scenario: Input order does not change selection

- **WHEN** the same mirror snapshots are evaluated in different input orders with equal policy inputs and evaluation time
- **THEN** both evaluations return the same protected set, eligible set, reason for every snapshot, and oldest-first eligible ordering

#### Scenario: Last restorable evidence survives degraded refreshes

- **WHEN** a mirror has one prior restorable snapshot and newer failed or degraded snapshot attempts
- **THEN** retention protects the restorable snapshot regardless of the failed attempts

### Requirement: Active snapshot pins override automatic retention

Vault SHALL support durable `operator` and `user` pin sources for snapshots. Any active pin SHALL exempt its snapshot from scheduled retention, quota-pressure escalation, target tombstone deletion, and physical deletion until an explicit unpin is durably recorded; pin and unpin history SHALL remain queryable.

#### Scenario: Either pin source protects a snapshot

- **WHEN** a snapshot otherwise qualifies for deletion and has an active operator or user pin
- **THEN** the decision records `protected_pinned`, creates no executable deletion stage for that snapshot, and identifies the active pin source

#### Scenario: Explicit unpin enables a later evaluation

- **WHEN** the final active pin is revoked and retention is evaluated again
- **THEN** the prior pin history remains and the new decision may select the snapshot only under the ordinary policy and grace rules

### Requirement: Every physical deletion has a non-bypassable tombstone window

Vault MUST persist a tombstoned deletion intent and a fixed `not_before` deadline before any local or replica removal. Neither scheduled retention, quota pressure, operator invocation, retry, nor clock/input manipulation MAY execute a deletion stage before that deadline; a rejected early attempt SHALL append refusal evidence without changing artifact state.

#### Scenario: Early deletion is impossible

- **WHEN** a worker attempts local or replica deletion one instant before the persisted `not_before` deadline
- **THEN** no storage delete operation is invoked, artifact and placement state remain retained, and the audit trail records a `grace_active` refusal

#### Scenario: Quota pressure cannot shorten grace

- **WHEN** quota pressure is evaluated while all otherwise eligible deletion intents are still inside their grace windows
- **THEN** Vault refuses the allocation instead of advancing a deadline or deleting bytes

### Requirement: Inactive desired state creates a durable target tombstone

When the newest governing desired policy becomes inactive for an unpinned target, Vault SHALL atomically exclude the target and record a target tombstone carrying the policy revision, reason, recorded time, and purge deadline. Repeated convergence SHALL reuse that active tombstone. Reactivation before deletion starts SHALL cancel pending automatic plans without erasing their evidence.

#### Scenario: Untracking starts grace without deleting

- **WHEN** an unpinned active target converges to a governing `none` policy
- **THEN** exactly one active target tombstone exists, the target is excluded, every snapshot remains readable, and no storage deletion has run

#### Scenario: Reactivation during grace preserves bytes

- **WHEN** a newer active policy arrives before any stage of an automatic target-deletion plan starts
- **THEN** the target returns to requested, the tombstone and plan are marked cancelled for audit, and all local and replica bytes remain

### Requirement: Deletion is local-first and replica-second

After grace, Vault SHALL execute an idempotent plan in ordered stages: first remove every unshared local artifact byte and verify absence, then remove every unshared replica object and verify absence, and only then complete the plan. A replica stage MUST NOT start until the local stage has terminal success or explicit shared-reference retention evidence. Failures SHALL remain retryable from the incomplete stage without repeating a completed physical effect.

#### Scenario: Local and replica stages are journaled in order

- **WHEN** an eligible snapshot has one local artifact and verified placements on two replica targets
- **THEN** the journal shows local attempt and verified completion before separately identified replica attempts and completions, followed by one completed plan record

#### Scenario: Partial replica failure is visible and resumable

- **WHEN** local deletion and one replica deletion succeed but another replica deletion fails
- **THEN** the plan remains incomplete, completed stages are retained, the failure is queryable, and a retry resumes at the failed replica without recreating local deletion

### Requirement: Shared content is never deleted while retained evidence references it

Before each storage delete, Vault MUST prove that no retained, pinned, grace-active, or otherwise undeleted snapshot references the same local digest or replica object key. A shared object SHALL remain physically present and the plan SHALL record a terminal `shared_reference_retained` stage outcome for that snapshot.

#### Scenario: Deduplicated blob remains for retained snapshot

- **WHEN** a deletion-eligible snapshot and a retained snapshot reference the same content-addressed local blob and replica key
- **THEN** neither physical object is deleted and the eligible snapshot's journal explains the shared-reference retention

### Requirement: Quota pressure follows a fixed escalation order

Quota-pressure evaluation SHALL consider only deletion intents whose grace has elapsed and SHALL process: ordinary policy-due snapshot intents first, then eligible snapshots of grace-complete inactive targets, both in deterministic oldest-first order. If the requested capacity still cannot be made available, Vault SHALL refuse allocation; it MUST NOT override pins, age floors, keep-N protection, shared references, or grace.

#### Scenario: Pressure exhausts safe candidates

- **WHEN** eligible ordinary and inactive-target intents free fewer bytes than the requested capacity and remaining snapshots are protected
- **THEN** the decision records both tiers in order and ends in `allocation_refused` without selecting any protected snapshot

### Requirement: Retention and deletion audit is complete and queryable

Vault SHALL preserve append-only evidence for every evaluation and execution attempt, including policy inputs, evaluation time, pressure mode, ordered candidates, protection or eligibility reason, pins, tombstone and deadline, stage, storage location, outcome, failure class, correlation identity, and timestamps. Queries by target or snapshot SHALL return this evidence in stable chronological and identifier order after deletion completes.

#### Scenario: Completed deletion remains explainable

- **WHEN** an operator queries a snapshot after all local and replica bytes were deleted
- **THEN** the result identifies the decision that selected it, the policy snapshot, every pin/tombstone observation, every ordered stage attempt, and the final outcome without requiring deleted bytes
