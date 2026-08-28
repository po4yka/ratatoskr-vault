## 1. Deterministic retention domain

- [x] 1.1 Add `crates/core/tests/retention.rs::selection_is_deterministic_per_mirror` with the minimum compiling domain seam, run it through `build-gate -- cargo test -p ratatoskr-vault-core --test retention --locked selection_is_deterministic_per_mirror`, and observe its eligible/protected ordering assertion fail rather than a compile error
- [x] 1.2 Implement per-mirror keep-N and age-floor classification with UUID tie-breakers in `crates/core/src/retention.rs`, rerun `selection_is_deterministic_per_mirror`, and observe it pass
- [x] 1.3 Add `crates/core/tests/retention.rs::pins_and_last_restorable_snapshot_are_protected`, run the isolated test, and observe the assertion fail because an operator/user-pinned or last-restorable snapshot is selected
- [x] 1.4 Implement pin precedence and last-restorable protection, rerun `pins_and_last_restorable_snapshot_are_protected`, and observe it pass
- [x] 1.5 Add `crates/core/tests/retention.rs::quota_pressure_uses_fixed_tiers_then_refuses`, run the isolated test, and observe the tier/reason assertion fail
- [x] 1.6 Implement grace-complete ordinary then inactive-target quota tiers and terminal allocation refusal without weakening protections, rerun `quota_pressure_uses_fixed_tiers_then_refuses`, and observe it pass

## 2. Current schema, pins, grace, and audit persistence

- [x] 2.1 Extend `crates/persistence/tests/schema.rs::current_schema_has_retention_deletion_evidence` to require policy floors, source-scoped pins, immutable target tombstones, deletion plans/stages, physical-object claims, and audit tables/guards; run it against disposable PostgreSQL and observe the missing-schema assertion fail
- [x] 2.2 Edit only root `schema.sql` to add the first-version retention/deletion structures, constraints, indexes, `deleted` target state, and append-only guards; rerun `current_schema_has_retention_deletion_evidence` and observe it pass (schema definition cannot itself begin from a separate test beyond task 2.1)
- [x] 2.3 Add `crates/persistence/tests/retention.rs::active_pin_blocks_automatic_deletion_plan`, run it against disposable PostgreSQL, and observe the assertion fail because an active pin is not yet enforced
- [x] 2.4 Implement source-scoped pin/unpin and atomic plan creation in `crates/persistence/src/retention.rs`, including database rechecks and audit rows; rerun `active_pin_blocks_automatic_deletion_plan` and observe it pass
- [x] 2.5 Add `crates/persistence/tests/retention.rs::deletion_claim_before_not_before_is_refused_and_journaled`, run it, and observe the assertion fail because early work can start or lacks `grace_active` evidence
- [x] 2.6 Implement leased local/replica stage claims using database time, immutable deadlines, ordered-stage guards, and early-refusal journaling; rerun `deletion_claim_before_not_before_is_refused_and_journaled` and observe it pass
- [x] 2.7 Add `crates/persistence/tests/retention.rs::snapshot_audit_survives_completed_deletion_in_stable_order`, run it, and observe the missing/incomplete audit projection assertion fail
- [x] 2.8 Implement finite paginated target/snapshot audit queries ordered by timestamp and UUID with linked decision/stage evidence; rerun `snapshot_audit_survives_completed_deletion_in_stable_order` and observe it pass
- [x] 2.9 Add `crates/persistence/tests/retention.rs::evaluation_persists_every_ordered_candidate_and_audit_event`, run it against disposable PostgreSQL, and observe the missing evaluation/candidate/audit assertion fail
- [x] 2.10 Implement atomic policy-snapshot, ordered-candidate, pin-source, and per-decision audit persistence; rerun `evaluation_persists_every_ordered_candidate_and_audit_event` and observe it pass
- [x] 2.11 Add `crates/persistence/tests/retention.rs::concurrent_pin_serializes_before_stage_claim`, hold an uncommitted pin behind the snapshot lock, run it, and observe the stage claim finish early instead of waiting for pin evidence
- [x] 2.12 Serialize automatic stage claims with snapshot pin writes under one row lock, rerun `concurrent_pin_serializes_before_stage_claim`, and observe the claim wait then return `ProtectedPinned`

## 3. Desired-state tombstones and target lifecycle

- [x] 3.1 Add `crates/persistence/tests/ingest.rs::none_policy_tombstones_once_and_reactivation_cancels_before_deletion`, run it, and observe the tombstone/deadline/reactivation assertion fail while snapshot rows remain readable
- [x] 3.2 Extend desired-state convergence so governing `none` atomically excludes and tombstones unpinned targets, replay reuses the tombstone, pinned targets remain held back, and pre-effect reactivation cancels pending automatic plans; rerun the isolated test and observe it pass
- [x] 3.3 Extend `crates/persistence/tests/agreement.rs::application_and_database_transitions_agree` and `transitions.rs::deletion_transitions_require_stage_evidence`, run both, and observe failure because `deleted` and evidence-gated deletion transitions are absent
- [x] 3.4 Update the application transition map and database guard for excluded-to-deleting, deleting-to-deleted, deleted terminality, and pre-effect reactivation; rerun both transition tests and observe them pass

## 4. Narrow verified storage deletion

- [x] 4.1 Add `crates/blobstore/tests/local_store.rs::delete_verified_blob_is_confined_idempotent_and_absent`, using only digest-derived fixture paths, run it, and observe the result assertion fail through a compiling fail-closed delete seam
- [x] 4.2 Implement local digest deletion with root confinement, symlink/non-regular refusal, idempotent exact-identity handling, and absence verification; rerun `delete_verified_blob_is_confined_idempotent_and_absent` and observe it pass
- [x] 4.3 Add `crates/blobstore/tests/s3_replica.rs::delete_requires_verified_remote_absence`, including a fixture that acknowledges DELETE while retaining GET bytes, run it, and observe the assertion fail because verified deletion is unavailable
- [x] 4.4 Add exact persisted-key replica deletion plus bounded absence verification and typed failure outcomes, update only the local S3 fixture protocol surface, rerun `delete_requires_verified_remote_absence`, and observe it pass

## 5. Staged executor, sharing, and recovery

- [x] 5.1 Add `services/vault/tests/retention.rs::eligible_snapshot_deletes_local_then_each_replica_with_journal_entries`, run it against disposable PostgreSQL/local/S3 fixtures, and observe the order/completion assertion fail before any real credential or provider is used
- [x] 5.2 Implement bounded retention orchestration in `services/vault/src/retention.rs` that claims and verifies local work before per-replica work and completes snapshot/target projections only from terminal stage evidence; rerun the staged test and observe it pass
- [x] 5.3 Add `services/vault/tests/retention.rs::shared_blob_is_retained_while_another_snapshot_references_it`, run it, and observe the assertion fail because the shared local digest or replica key is removed/called
- [x] 5.4 Implement durable physical-identity claims shared with snapshot publication and replication claiming, plus `shared_reference_retained` terminal evidence; rerun the shared-blob test and observe no physical delete call and a passing assertion
- [x] 5.5 Add `services/vault/tests/retention.rs::expired_stage_lease_reconciles_effect_without_repeating_it`, run it with a fixture crash boundary after storage deletion, and observe the duplicate-effect or incomplete-journal assertion fail
- [x] 5.6 Implement expired-lease abandonment, exact-identity absence reconciliation, and resume from the first incomplete stage; rerun the crash-boundary test and observe it pass
- [x] 5.7 Add `crates/persistence/tests/replication.rs::deletion_staged_artifacts_are_not_due_or_claimable`, run it, and observe the assertion fail because replication still admits the artifact
- [x] 5.8 Exclude deletion-staged physical identities from due-replication discovery and claims while preserving prior attempt/placement evidence; rerun `deletion_staged_artifacts_are_not_due_or_claimable` and observe it pass
- [x] 5.9 Add `services/vault/tests/retention.rs::failed_replica_is_journaled_and_resumable`, run it, and observe the stage remain `running` instead of recording the remote verification failure
- [x] 5.10 Implement atomic failed-stage and released-claim evidence plus replica-only retry resumption; rerun `failed_replica_is_journaled_and_resumable` and observe it pass
- [x] 5.11 Extend `eligible_snapshot_deletes_local_then_each_replica_with_journal_entries` to require a terminal `mirror_local` stage before replica work for a tombstoned target, run it, and observe the missing stage assertion fail
- [x] 5.12 Implement confined idempotent mirror deletion, include its exact persisted path in execution plans, and enforce mirror-local completion before tombstoned replica/plan completion; rerun the staged test and observe it pass
- [x] 5.13 Add `crates/persistence/tests/retention.rs::concurrent_shared_digest_serializes_before_delete_claim`, publish a retained reference while holding the physical identity lock, run it, and observe deletion claim miss the shared reference
- [x] 5.14 Serialize snapshot publication, replication claims, and deletion claims on the same physical identity advisory key, then recheck sharing under that lock; rerun `concurrent_shared_digest_serializes_before_delete_claim` and observe `SharedReferenceRetained`

## 6. Telemetry and operator contract

- [x] 6.1 Add `crates/telemetry` tests that require bounded retention decision, refusal, stage outcome, and duration labels without target/digest/key labels; run the targeted telemetry tests and observe the missing metric assertion fail
- [x] 6.2 Implement the bounded retention telemetry and service instrumentation, rerun the targeted telemetry tests, and observe them pass
- [x] 6.3 Update `README.md`, `DEVELOPMENT.md`, `docs/ARCHITECTURE.md`, `docs/THREAT_MODEL.md`, and plan-item status with keep-N/age/grace bounds, pin sources, fixed quota escalation, local-first replica-second semantics, credential/storage impact, halt/rollback procedure, and residual real-provider verification boundary; documentation cannot begin from a failing executable test, so verify terminology and plan status with `rg`

## 7. Full validation and delivery readiness

- [x] 7.1 Run every new isolated retention/storage/reconciliation test through `build-gate`, then the affected-package test suites, and confirm every previously observed RED is GREEN with no skipped/focused/fake-success test
- [x] 7.2 Run `cargo fmt --all`, inspect the complete task-branch diff for scope/security/generated-file mistakes, validate `retention-tombstones-staged-deletion` strictly, and mark tasks complete only for commands and assertions actually observed
- [x] 7.3 Run the exact full gate from `DEVELOPMENT.md` with compiler-backed commands through `build-gate` and disposable PostgreSQL, including archived OpenSpec validation, dependency policy, format, Clippy `-D warnings`, debug build, workspace tests, tracked Rust file limits, and release build; record every command and outcome for delivery
