## Context

See `proposal.md` for motivation. Ratatoskr already persists immutable snapshot/manifest evidence, local content-addressed artifacts, replica placements, desired-policy revisions, and target pins. The current retention and tombstone tables are placeholders: they cannot express age floors, snapshot pins, fixed grace deadlines, a deletion plan, or stage evidence, and neither storage adapter exposes an authorized delete operation.

The design must preserve the existing first version and edit the one current `schema.sql` in place. Snapshot bytes may be deduplicated across evidence rows, replica operations can fail independently, desired-policy deliveries are at least once, and a deletion worker can crash after an external effect but before recording its outcome. Physical deletion is irreversible, so policy authorization and the fixed grace deadline must be rechecked from durable state at every claim rather than trusted from an in-memory plan.

## Goals / Non-Goals

**Goals:**

- Keep policy selection pure, deterministic, and independently testable at an explicit evaluation time.
- Make pins, tombstones, selection reasons, storage effects, failures, and retries durable enough to explain every decision after bytes are gone.
- Serialize conflicting publication, replication, and deletion work by physical object identity so deduplication cannot delete bytes still needed by retained evidence.
- Keep destructive effects behind narrow local and replica storage operations with path/key derivation from validated immutable identities.
- Make the full local-to-replica workflow restartable after any crash boundary.

**Non-Goals:**

- Client or GitHub pin controls, changes to the desired-policy wire contract, provider lifecycle rules, legal-hold UX, encryption changes, incremental bundle retention, or deletion of audit/manifest metadata.
- Automatic deletion to make a currently refused allocation succeed immediately; grace is never shortened for capacity.
- A generic cross-service garbage collector or arbitrary storage-key/path deletion API.

## Decisions

### 1. Use a pure policy engine with explicit, totally ordered inputs

Core receives an explicit UTC evaluation instant, one validated policy, and normalized snapshot rows grouped by mirror. It classifies every row rather than returning only deletions. Protection precedence is active snapshot pin, age floor, newest keep-N restorable snapshots (with keep-N at least one), active grace/shared-reference constraints, then eligibility. Within each mirror, newest protection uses `(created_at DESC, snapshot_id DESC)` and deletion uses `(created_at ASC, snapshot_id ASC)`; cross-mirror output adds `mirror_id` as a stable tie-breaker.

Quota pressure is an evaluation mode, not permission to weaken policy. It consumes only grace-complete plans in two tiers: ordinary policy-due snapshots and then grace-complete inactive-target snapshots. If their finite estimated bytes are insufficient, the result is `allocation_refused`. This keeps the item-4 rule, “refuse and mark degraded, never silently prune,” true while making already authorized cleanup useful.

Alternatives considered: SQL-only selection would couple policy semantics to query plans and make identical fixture testing harder; sorting only by timestamps would be nondeterministic at equal precision; allowing pressure to create and execute a plan in one pass would bypass grace.

### 2. Represent pins as durable source-scoped records plus audit events

`snapshot_pins` stores one source-scoped pin identity (`operator` or `user`), its creation, optional single revocation, and correlation identity. A partial unique index prevents duplicate active identities. A guard permits only the one-way null-to-revoked transition and rejects identity mutation or deletion. Pin/unpin writes append the same transaction's audit row. Target-level `targets.pinned` continues to block automatic target tombstones; snapshot pins independently protect individual evidence even after a target is untracked.

Alternatives considered: a Boolean on `snapshots` loses source and history; append-only toggle events require every safety query to reconstruct last-event state and make database constraints on active pins unnecessarily fragile.

### 3. Separate target tombstones, per-snapshot plans, and append-only decisions

`tombstones` becomes a durable target-retirement record with governing revision, fixed `not_before`, and one-way cancellation/completion fields. Convergence to a governing `none` revision atomically excludes the target and inserts or reuses the active tombstone. Reactivation may cancel it only before any linked physical stage starts.

Every retention-selected snapshot receives a `deletion_plan`, which is also its snapshot-level tombstoned intent. It records the selecting evaluation, optional target tombstone, fixed `tombstoned_at`/`not_before`, reason, estimated bytes, and current projection status. The deadline is immutable; policy changes can affect a future plan but cannot shorten an existing plan. `retention_evaluations` and `retention_candidates` are append-only and store the normalized policy snapshot and reason for every considered snapshot, including protected ones.

This split allows keep-N retirement of old snapshots on an active target without pretending the whole target is inactive, while still giving every physical deletion its own grace window. A target can reach `deleting` only from an executable target tombstone, and reaches terminal `deleted` only when all linked snapshot and mirror-local deletion work has completed.

Alternatives considered: using target tombstones for every old snapshot would incorrectly exclude active targets; mutating snapshot status alone cannot prove authorization or grace; a single opaque JSON audit blob cannot enforce stage invariants or support precise queries.

### 4. Claim external effects durably and serialize by physical identity

`deletion_stage_attempts` holds leased `local`, `mirror_local`, or `replica` work with the exact immutable digest or replica target/key identity, start/finish times, outcome, failure class, and observed absence. Terminal attempts are immutable. A unique live-stage constraint and a physical-object claim prevent concurrent delete effects for the same identity.

Claiming occurs in a short transaction that locks the plan, checks database time against `not_before`, rechecks active pins/current target policy, validates stage ordering, proves all references to the physical identity belong to deletion-eligible plans, and records the lease. Snapshot persistence and replication claiming check the same physical-object claim, so they cannot attach a new retained reference between the shared-reference check and the external delete. The external storage operation runs after commit and without holding a database transaction; completion/failure is recorded in a second transaction. Expired leases become explicit abandoned attempts and may be reconciled by checking actual storage state before retry.

A physical identity shared by any retained record produces `shared_reference_retained`; the retiring evidence row remains, but no delete call occurs. A later newly published artifact may reuse content-addressed bytes only after publication has verified/recreated them and advanced the separate physical-availability projection; this never rewrites old deletion evidence.

Alternatives considered: holding a transaction across S3/filesystem calls risks pool exhaustion and unknown rollback/effect ordering; an in-process mutex is not crash-safe or multi-instance safe; deleting once per artifact row ignores content-addressed sharing.

### 5. Execute and verify local before replicas

The executor completes local content objects and the confined target mirror before replica work. `LocalBlobStore` gains a digest-derived delete method that opens only beneath its configured root, refuses symlink/non-regular targets, performs idempotent removal, and verifies absence. The S3 adapter gains a delete-by-derived-key method followed by a bounded read/metadata absence check; provider acknowledgement alone is not success. Replica work is one stage per placement, so partial provider failure is visible and retryable.

The plan projection advances only from journal evidence: `planned -> local_deleting -> replica_deleting -> completed`, with `cancelled` allowed only before the first effect. Snapshot physical availability becomes `retained -> deletion_staged -> local_deleted -> deleted`; original snapshot/artifact/manifest rows and prior verification/restore evidence are never updated or deleted. Due-replication queries and claims exclude `deletion_staged` or later identities.

Alternatives considered: remote-first weakens the requested local-then-replica contract; deleting all replicas in one stage hides partial progress; treating not-found as unconditional success could conceal an unsafe key or wrong target, so idempotent not-found is accepted only for the exact previously journaled identity and is still recorded as observed absence.

### 6. Make audit a first-class repository query

Every evaluation, candidate classification, pin mutation, tombstone mutation, early refusal, stage claim, terminal effect, shared-reference suppression, and plan completion appends a typed audit entry with target/snapshot/plan identity, correlation ID, bounded reason/failure class, and database timestamp. The repository exposes finite paginated queries by target or snapshot ordered by `(occurred_at, audit_id)`. Stage tables remain the authoritative execution evidence; the audit stream is a query projection linked to those immutable rows, not a substitute for them.

Telemetry counts bounded outcome/reason/stage labels and durations; target, repository names, storage keys, credentials, and digests do not become metric labels.

Alternatives considered: logs are not durable verified state; querying only current plan columns cannot explain refusals or retries; an unbounded audit endpoint would make operational inspection itself a resource risk.

## Risks / Trade-offs

- [A crash occurs after a storage delete but before database completion] -> Expire the lease, verify the exact journaled identity's absence, and append a reconciled terminal attempt; never infer success from elapsed time.
- [A shared digest is linked while deletion is in flight] -> Use a durable unique physical-identity claim checked by snapshot publication and replication claims, with bounded retry rather than racing the effect.
- [Local deletion succeeds and every replica is temporarily unavailable] -> Keep the plan visibly incomplete and retry replica stages; prior immutable metadata and remaining replicas still identify exactly what exists.
- [The storage adapter addresses an unsafe path or key] -> Accept only digest-derived local paths and previously persisted content-derived replica keys; refuse symlinks, traversal, unchecked repository names, and arbitrary caller strings.
- [Policy or clock changes appear to shorten grace] -> Persist an immutable deadline using database time at plan creation and compare claims with database time; later evaluation creates new evidence but cannot edit the deadline.
- [Audit volume grows with every candidate] -> Store compact bounded reason codes, index target/snapshot/time, paginate queries, and retain audit indefinitely because it is deletion evidence.
- [Adding `deleted` breaks callers that assumed `deleting` was terminal] -> This repository is in development and explicitly rejects compatibility paths; update every local caller, database guard, and test together and name the contract break in delivery.
- [Physical deletion is irreversible] -> Require grace, pins, shared-reference proof, ordered verification, and durable audit. Rollback can stop future stages but cannot recreate bytes after every verified copy is removed.

## Migration Plan

1. Edit the single current `schema.sql` in place and update fresh-schema/disposable-PostgreSQL tests; add no migration files or version negotiation.
2. Land pure selection and schema/repository behavior behind no externally callable arbitrary delete surface; prove red/green tests for deterministic selection, pins, grace, state transitions, append-only evidence, and audit queries.
3. Add narrow local and S3 delete/absence operations and integration fixtures, then wire the leased executor, replication exclusion, crash reconciliation, shared-reference protection, and target completion.
4. Document policy bounds, quota escalation, storage/credential impact, operator halt procedure, and irreversible rollback boundary. Run the repository's complete archived-OpenSpec/Cargo/PostgreSQL gate before integration.
5. Rollback before an effect by disabling/stopping the retention executor and reverting code/schema together. After a local stage, halt before replica stages if preservation is in doubt and restore local bytes from a still-verified replica. After all copies are verified absent, only metadata/audit can be rolled back; bytes require an independent external backup and are not claimed recoverable by this feature.
