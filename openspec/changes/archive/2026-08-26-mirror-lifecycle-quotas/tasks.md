## 1. Lifecycle configuration and domain vocabulary

- [x] 1.1 RED: add `crates/core/tests/config_strictness.rs::mirror_lifecycle_budget_requires_finite_positive_limits`, supplying zero per-mirror bytes, zero global bytes, and a fifth operation slot; assert each configuration is rejected with its field name. Confirm the assertions fail because mirror lifecycle configuration does not exist. Verify with `cargo test --locked -p ratatoskr-vault-core --test config_strictness mirror_lifecycle_budget`.
- [x] 1.2 GREEN: add strict mirror-root, work-root, per-mirror/global byte budgets, and a four-operation maximum to the typed configuration, validating finite nonzero limits and the deployment-target cap. Verify 1.1 passes.
- [x] 1.3 RED: add `crates/core/tests/mirror.rs` test `quota_refusal_and_interruption_have_closed_lifecycle_results`, asserting the lifecycle result vocabulary distinguishes succeeded, quota-refused, interrupted, and integrity-failed results. Confirm failure because the types do not exist.
- [x] 1.4 GREEN: implement documented core mirror request, checkpoint, count, and closed result/failure types without planner I/O. Verify 1.3 passes.

## 2. Durable mirror evidence and reservations

- [x] 2.1 RED: add `crates/persistence/tests/mirror_lifecycle.rs::quota_reservation_refuses_global_overcommit_without_mutating_existing_reservation`, reserving one target then attempting an over-budget second target; assert the second returns `QuotaExceeded` and the first row remains. Confirm failure because no reservation store exists.
- [x] 2.2 GREEN: edit `schema.sql` in place and add persistence operations for target-owned mirror observations, lifecycle runs/checkpoints, and transactional quota reservations. Verify 2.1 passes against a disposable database; no migration is created.
- [x] 2.3 RED: add `crates/persistence/tests/mirror_lifecycle.rs::interrupted_run_releases_reservation_and_keeps_checkpoint`, asserting completion of an interrupted run releases its reservation but retains its resumable checkpoint and no successful observation. Confirm failure because completion evidence is not persisted.
- [x] 2.4 GREEN: persist success, quota-refused, interrupted, and integrity-failed lifecycle outcomes atomically with appropriate reservation release and last-successful-observation preservation. Verify 2.3 passes.

## 3. Initial clone lifecycle

- [x] 3.1 RED: add `services/vault/tests/mirror_lifecycle.rs::initial_clone_of_local_fixture_publishes_verified_mirror_and_transitions_ready`, building a local source fixture with the real Git binary and asserting the request transitions `requested -> cloning -> ready`, publishes one identifier-derived bare mirror, and persists counts. Confirm failure because no lifecycle executor runs Git.
- [x] 3.2 GREEN: implement the lifecycle executor's clone path using only `GitRunner`, target-owned staging, atomic publish after fsck/count checks, and legal target transitions. Verify 3.1 passes.

## 4. Explicit quota refusal

- [x] 4.1 RED: add `services/vault/tests/mirror_lifecycle.rs::per_mirror_quota_refusal_does_not_spawn_or_publish_a_clone`, configuring a reservation larger than the per-mirror budget and asserting no fixture mirror path exists, the run is quota-refused, and the target is degraded. Confirm failure because the clone starts or no run is recorded.
- [x] 4.2 GREEN: admit clone/fetch only after both per-mirror and global reservation checks; map refusal to durable quota evidence and the legal degraded transition with no automatic pruning. Verify 4.1 and 2.1 pass.

## 5. Cooperative clone cancellation

- [x] 5.1 RED: add `services/vault/tests/mirror_lifecycle.rs::cancelled_clone_removes_staging_and_releases_capacity`, using a lifecycle cancellation seam to cancel after admission and before publish; assert no staging/published mirror survives, the run is interrupted, and the reservation is released. Confirm failure because cancellation leaves work behind or records success.
- [x] 5.2 GREEN: thread cooperative cancellation through clone execution and cleanup, await runner termination, delete only run-owned work, and persist the interrupted result. Verify 5.1 passes without timing sleeps.

## 6. Fetch checkpoint and retry semantics

- [x] 6.1 RED: add `services/vault/tests/mirror_lifecycle.rs::cancelled_fetch_keeps_prior_mirror_and_next_run_fetches_pending_commit`, cloning a fixture, adding a commit, cancelling a gated fetch, then retrying; assert the original mirror remains valid, an interrupted checkpoint exists, and retry updates that same mirror. Confirm failure because refresh/checkpoint behavior is absent.
- [x] 6.2 GREEN: implement fetch against the existing mirror, interrupted checkpoint persistence, prior-mirror integrity recheck, and idempotent retry through the normal fetch path. Verify 6.1 passes.

## 7. Integrity checks and degradation

- [x] 7.1 RED: add `services/vault/tests/mirror_lifecycle.rs::integrity_check_failure_marks_target_degraded_and_preserves_last_success`, corrupting a local fixture mirror after Git work and asserting fsck failure records bounded diagnostics, transitions to degraded, and leaves the previous successful observation untouched. Confirm failure because post-operation evidence is not enforced.
- [x] 7.2 GREEN: run fsck, show-ref, and rev-list after every clone/fetch; strictly parse bounded counts and classify a failed or impossible check as `MirrorCorruption` without recording success. Verify 7.1 passes.

## 8. Deployment-aligned concurrency

- [x] 8.1 RED: add `services/vault/tests/mirror_lifecycle.rs::fifth_mirror_operation_waits_for_one_of_four_admissions`, using gated local-fixture operations to hold four slots and asserting a fifth has not spawned before one releases. Confirm failure because there is no shared cap.
- [x] 8.2 GREEN: apply one shared four-permit lifecycle semaphore around clone and fetch work, releasing permits on success, failure, and cancellation. Verify 8.1 passes.

## 9. Documentation and full verification

- [x] 9.1 Update README and architecture/development documentation to describe the implemented mirror lifecycle, finite quota behavior, four-operation cap, checkpoint semantics, and explicit exclusion of snapshots/off-host copies. This cannot start from a failing test because it documents already-tested behavior. Verify the affected links and `git diff`.
- [x] 9.2 Run `openspec validate mirror-lifecycle-quotas --strict`, then the full fenced DEVELOPMENT.md gate through `build-gate --` for compiler-backed commands; require every command to exit 0 and record the exact results. This is integration verification across all preceding tasks.
