# Tasks: desired-state-reconciliation

Conventions: every behaviour is a RED/GREEN pair — the RED task adds a test that must fail for the reason stated (not for a compile error), the GREEN task makes exactly that test pass. Verification commands assume the local Postgres from DEVELOPMENT.md is up (`docker compose up -d`) so disposable test databases rebuild from `schema.sql`.

## 1. Core status vocabulary (crates/core)

- [x] 1.1 RED: create `crates/core/src/target_state.rs` scaffolding `TargetStatus` with a single placeholder variant plus `pub const ALL`, and add test `status_vocabulary_is_the_closed_set_of_eleven_states` asserting `ALL` equals the eleven canonical snake_case names from design.md. Run `cargo nextest run -p vault-core status_vocabulary` and confirm it fails on the element-count assertion (1 vs 11), not on compilation.
- [x] 1.2 GREEN: replace the placeholder with the eleven variants (`requested, cloning, ready, fetching, snapshotting, verifying, healthy, degraded, paused, excluded, deleting`), keep `ALL` derived from them. Verify the same test passes and `cargo clippy -p vault-core -- -D warnings` stays clean.

## 2. Core transition table (crates/core)

- [x] 2.1 RED: add `Transition::is_legal(from, to)` stubbed to `false` and test `transition_table_matches_design_matrix` in `crates/core/src/target_state.rs` iterating every ordered pair over `TargetStatus::ALL`, asserting `true` exactly for the design.md matrix (including `excluded -> requested`) and `false` otherwise. Confirm failure enumerates the wrongly-rejected legal pairs (empty table).
- [x] 2.2 GREEN: encode the `TRANSITIONS` const table exactly per design.md (same-status is never a table entry — annotations are handled separately in 5.4). Verify `transition_table_matches_design_matrix` passes with no other behaviour added.

## 3. Delivery validation boundary (crates/core)

- [x] 3.1 RED: add `DesiredStateDelivery`, `ValidatedDelivery`, `validate_delivery()` currently accepting everything, and `VaultError::InvalidDelivery`; test `malformed_deliveries_are_rejected` in `crates/core/src/delivery.rs` asserts unknown preservation level, missing `policy_revision`, and blank correlation id each return `InvalidDelivery`. Confirm it fails because the stub returns `Ok` for all inputs.
- [x] 3.2 GREEN: implement validation — level ∈ {none, metadata_only, git_mirror, git_mirror_with_lfs, complete_archive}, optional flags (`pinned`, wiki/releases/issues/offsite), required monotonic u64 revision, non-empty correlation id. Verify the test passes.

## 4. Pure reconciliation planner (crates/core)

Planner is `f(desired, observed) -> ReconciliationPlan` with `Vec<WorkItem>` and structured counters; no I/O.

- [x] 4.1 RED: create `crates/core/src/planner.rs` with `plan()` stubbed to an empty vec; test `first_active_policy_enrolls_a_new_target` feeds a fixture active policy with no observed target and asserts the plan contains exactly `WorkItem::Enroll`. Confirm failure: plan is empty.
- [x] 4.2 GREEN: implement the Enroll branch. Verify 4.1 passes.
- [x] 4.3 RED: test `none_policy_excludes_unpinned_active_target` asserts a single `WorkItem::Exclude` when governing policy becomes `none` and the target is not pinned. Confirm failure on the empty plan.
- [x] 4.4 GREEN: implement the Exclude branch for unpinned targets. Verify 4.3 passes.
- [x] 4.5 RED: test `pinned_target_survives_withdrawal` asserts zero executable items and a recorded divergence entry when policy is `none` and `pinned=true`. Confirm failure: current Exclude ignores the pin flag.
- [x] 4.6 GREEN: honor `pinned` in the Exclude branch. Verify 4.3 and 4.5 both pass.
- [x] 4.7 RED: test `excluded_target_reactivates_on_new_active_policy` asserts `WorkItem::Reactivate` when observed status is `excluded` and the newest desired policy is active. Confirm failure.
- [x] 4.8 GREEN: implement the Reactivate branch. Verify 4.7 passes.
- [x] 4.9 RED: test `unchanged_newest_revision_records_evidence_only` asserts `[WorkItem::RecordRevisionOnly]` when the newest desired policy is semantically equal to observed state. Confirm failure.
- [x] 4.10 GREEN: implement RecordRevisionOnly. Verify 4.9 passes.
- [x] 4.11 RED: test `second_convergence_pass_is_a_no_op` runs `plan()` twice on evolved state and asserts the second pass proposes zero items. Confirm failure while any branch misclassifies converged state.
- [x] 4.12 GREEN: close the remaining branches until the double pass is clean. Verify all planner tests pass.
- [x] 4.13 RED: test `runner_work_is_reported_planned_not_executable` applies a `git_mirror_with_lfs` policy to a ready target and asserts the plan reports CloneAndFetch/Snapshot-class items flagged planned-not-executable plus counters `{ proposed, executable }` matching those counts. Confirm failure: forward variants do not exist.
- [x] 4.14 GREEN: add forward `WorkItem` variants and the counter struct, executed nowhere. Verify the full planner suite passes and no variant reaches execution.

## 5. Database guard (schema.sql, crates/persistence)

The item-1 foundation already provides every table this change needs (`targets` with the 11-value status CHECK but no guard, append-only `desired_state_revisions`, `outbox`, `inbox` deduped on `(source, message_id)`); only the guard SQL is added here.

- [x] 5.1 RED: add `crates/persistence/tests/agreement.rs` with `machine_agreement_walk_covers_every_pair`: for every ordered pair over the vocabulary, call a new naive `set_target_status` store op (plain UPDATE) and require the DB outcome to match `Transition::is_legal` exactly. Confirm failure: the unguarded UPDATE accepts the illegal set, so the walk reports disagreements. Verify the failure output enumerates the disagreeing pairs.
- [x] 5.2 GREEN: add `target_status_rank(text)`, `target_guard_status_transition()`, and trigger `targets_guard_status_transition ... ON UPDATE OF status` to `schema.sql`; map the raised SQLSTATE to `VaultError::IllegalTransition { from, to }`. Verify the agreement walk passes for all 121 pairs.
- [x] 5.3 RED: add `apply_transition_contract` tests in `crates/persistence/tests/transitions.rs` with three assertions against a stubbed `apply_transition` op: (a) a legal move persists the new status and inserts exactly one `outbox` row whose payload carries `target_id, from_status, to_status, policy_revision, correlation_id`; (b) a same-status call persists the annotation and inserts zero outbox rows; (c) an induced guard failure after an in-transaction revision insert rolls back leaving neither status nor outbox changed. Confirm failure: the op is unimplemented.
- [x] 5.4 GREEN: implement `apply_transition` as one transaction holding `SELECT ... FOR UPDATE` on the `targets` row, writing the status, inserting the `vault.target.state_changed.v1` outbox event only when the status actually changes, and surfacing guard violations as `IllegalTransition`. Verify all three assertions pass.

## 6. Desired-state ingestion transaction (crates/persistence)

- [x] 6.1 RED: test `duplicate_delivery_is_rejected_without_side_effects` in `crates/persistence/tests/ingest.rs` calls `ingest_delivery` twice with the same `(source, message_id)` and asserts the second call returns `VaultError::DuplicateDelivery` with the revision table still holding one row. Confirm failure: the op is a stub.
- [x] 6.2 GREEN: implement `ingest_delivery` — one transaction: inbox insert guarded by the unique constraint, validated revision append, target row ensured on first sight. Verify 6.1 passes.
- [x] 6.3 RED: test `stale_revisions_are_kept_but_never_govern` delivers revision 5 then revision 3 and asserts both rows persist while the governed input for planning remains revision 5. Confirm failure under last-write-wins.
- [x] 6.4 GREEN: select the governing policy by `max(policy_revision)`; stale rows stay as evidence only. Verify 6.3 passes.
- [x] 6.5 RED: test `conflicting_ingests_serialize_on_target_row` runs two pooled ingests of different revisions concurrently with an ordered barrier and asserts both commit with distinct revision rows and the governed state equals the higher revision. Confirm failure from a lost update while the transaction lacks the row lock.
- [x] 6.6 GREEN: extend the ingest transaction with `SELECT ... FOR UPDATE` on the target row. Verify 6.5 passes repeatedly (`cargo nextest run -p vault-persistence ingest --test-threads=4`) without flakes.

## 7. Service wiring end-to-end (services/vault)

- [x] 7.1 RED: add `services/vault/src/reconcile.rs` calling nothing yet; test `ingest_then_converge_applies_first_policy_end_to_end` in `services/vault/tests/reconcile.rs` pushes one valid delivery through a fake source into `run_cycle` and asserts the target row exists with the expected status and the enrollment outbox event was emitted. Confirm failure: `run_cycle` is unimplemented.
- [x] 7.2 GREEN: implement the thin wiring — validate → ingest → plan → execute only executable items → emit — logging the plan counters. Verify 7.1 passes.
- [x] 7.3 RED: test `run_cycle_twice_is_fully_idempotent` runs a second cycle over the same state and asserts zero new outbox rows, unchanged status, and a plan report with zero executable items. Confirm failure until converged state flows through RecordRevisionOnly silently.
- [x] 7.4 GREEN: finalize the loop. Verify 7.1 and 7.3 pass together.

## 8. Full gate verification (spans all tasks)

- [x] 8.1 Run the DEVELOPMENT.md gate in order and require exit 0 from each: `cargo fetch --locked`; `cargo deny check`; `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo build --workspace --locked`; `cargo test --workspace --locked`; `cargo build --workspace --locked --release`. This is a standalone verification task because it checks integration across every preceding task; paste the outputs as PR evidence.
