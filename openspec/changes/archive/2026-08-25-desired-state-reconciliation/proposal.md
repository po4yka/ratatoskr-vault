# Proposal: Desired-State Target Reconciliation

Implementation plan item 2 (`docs/IMPLEMENTATION_PLAN.md`). Planning artifacts only — see design.md for decisions and tasks.md for the test-first breakdown.

## Why

Vault cannot answer "what should be preserved, and is anything converging toward it?" today: desired backup policies have nowhere to land, and target lifecycle states exist only as a CHECK constraint nobody drives. Every later capability (mirror workers, snapshots, retention) depends on an explicit, idempotent convergence loop and a single-authority state machine, so this is the next foundational layer.

## What Changes

- Introduce the Vault-owned desired-state input contract (preservation levels `none|metadata_only|git_mirror|git_mirror_with_lfs|complete_archive`, optional `pinned` and inclusion/offsite flags, correlation ID, monotonic `policy_revision`), delivered through a narrow source trait with a fixture stub — no live publisher exists in `ratatoskr-github`, and coupling Vault to catalog types is forbidden.
- Add desired-state ingestion: delivery validation, inbox deduplication on `(source, message_id)`, append-only revision evidence, newest-revision-wins discipline.
- Add the target state machine: closed 11-status vocabulary and one explicit transition map, enforced twice (Rust `Transition` table and a PostgreSQL guard trigger) with an automated agreement proof over all ordered pairs.
- Add the pure reconciliation planner computing `f(desired, observed) -> plan`: today it executes only Enroll / Exclude / Reactivate / RecordRevisionOnly and reports runner-class work (clone/fetch, snapshot) as planned-not-executable with structured counters — never as success.
- Emit `vault.target.state_changed.v1` transactionally through the existing outbox; serialize conflicting work per target with a row lock.
- Edit `schema.sql` in place: add `target_status_rank(text)`, `target_guard_status_transition()`, and the `targets_guard_status_transition` trigger. All required tables already exist from item 1; no migrations.
- No scheduler or daemon ships in this change; the reconciliation entry point is a library function callable by future workers.

## Capabilities

### New Capabilities

- `desired-state-ingestion`: consuming desired backup-policy deliveries safely — validated input boundary, effective exactly-once processing via inbox dedup, append-only revision evidence where stale revisions are kept but never govern, enrollment/exclusion/reactivation semantics including pinned-target protection, and idempotent convergence.
- `target-state-machine`: the authoritative target lifecycle — closed status vocabulary, explicit legal transition map (including `excluded -> requested` reactivation and terminal `deleting`), identical enforcement at database and application boundaries, same-status annotation writes without events, and transitions that always carry transactionally committed evidence.

### Modified Capabilities

None — `openspec/specs/` is intentionally empty until this change archives its deltas.

## Impact

- `crates/core`: domain types, `Transition` table, delivery validation, planner, error variants, unit tests.
- `crates/persistence`: guarded transition and ingestion store operations, integration tests (agreement walk, atomicity, dedup, staleness, serialization).
- `services/vault`: thin reconcile module binding source → ingest → converge.
- `schema.sql`: three new guard objects; table set unchanged.
- No external API, dependency, or CI-gate changes; cross-repo publication of the input contract to the workspace store is deferred until a second consumer exists.
