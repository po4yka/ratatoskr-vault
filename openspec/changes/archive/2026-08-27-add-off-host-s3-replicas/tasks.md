## 1. Strict replica configuration

- [x] 1.1 RED: add `crates/core/tests/config_strictness.rs::replica_policy_rejects_missing_secrets_zero_budgets_and_plaintext_remote` and run it through `build-gate -- cargo nextest run --locked -p ratatoskr-vault-core replica_policy_rejects_missing_secrets_zero_budgets_and_plaintext_remote`; the assertion must fail because no replica violations are reported, not because the test does not compile.
- [x] 1.2 GREEN: add the keyed environment-only replica configuration, redacted secret fields, HTTPS/loopback and finite-budget validation, and `.env.example` entries until 1.1 passes; then run the core crate tests and confirm supplied credentials never render in errors or `Debug`.

## 2. S3-compatible streaming adapter

- [x] 2.1 RED: after owner approval of the production dependency justified in `design.md`, add the minimal `object_store` 0.14.x dependency/features, an Axum `127.0.0.1:0` in-memory S3 multipart harness, a compiling `ReplicaStore` seam, and `crates/blobstore/tests/s3_replica.rs::upload_redownload_round_trip_digest_equality`; run it through `build-gate -- cargo nextest run --locked -p ratatoskr-vault-blobstore upload_redownload_round_trip_digest_equality` and observe the equality assertion fail on the adapter's explicit not-yet-implemented result.
- [x] 2.2 GREEN: implement explicit `AmazonS3Builder::new()` construction, safe content-derived keys, bounded multipart streaming upload, full streamed GET SHA-256/length verification, and idempotent existing-object verification until 2.1 passes; then run the blobstore crate tests.
- [x] 2.3 RED: add `crates/blobstore/tests/s3_replica.rs::remote_corruption_never_returns_verified_placement` and run it through the gate; the assertion must fail because a harness-corrupted GET is still accepted or lacks the expected checksum-mismatch class.
- [x] 2.4 GREEN: add truncated/oversized/digest-mismatch classification, create-new scratch download, deadline propagation, and multipart abort cleanup until 2.3 passes; then rerun the entire S3 adapter test binary with no public network or fixed port.

## 3. Replica inventory and attempt persistence

- [x] 3.1 RED: add `crates/persistence/tests/replication.rs::replica_inventory_records_location_and_rejects_terminal_rewrite` against the disposable PostgreSQL harness and run it through the gate; the schema assertion must fail because replica tables/constraints are absent.
- [x] 3.2 GREEN: edit the one current `schema.sql` in place and add persistence APIs for credential-free replica targets, artifact placements, leased attempts, atomic successful placement/finalization, and terminal immutability until 3.1 passes; no migration file or tooling is permitted.
- [x] 3.3 RED: add `crates/persistence/tests/replication.rs::reverification_appends_attempt_and_advances_inventory` and run it through the gate; preserve both successful attempts and the causal `last_verified_at`, then observe the bounded freshness read fail because the due-unit seam returns no stale placement.
- [x] 3.4 GREEN: implement idempotent target observation, placement upsert, append-only re-verification attempts, stable failure classes, and due-unit reads until 3.3 passes; then run persistence schema, agreement, snapshot, restore, and replication tests against the verified disposable database endpoint.

## 4. Bounded replication admission and recovery

- [x] 4.1 RED: add `services/vault/tests/replication.rs::backlog_admission_bounds_items_bytes_and_preserves_deferred_work` and run it through the gate; the assertion must fail because no planner returns a bounded admitted window plus still-due deferrals.
- [x] 4.2 GREEN: implement the pure absent/oldest-first snapshot/replica planner with `max_backlog_items + 1` retrieval, item/byte ceilings, stable ordering, and bounded concurrency admission until 4.1 passes.
- [x] 4.3 RED: add `crates/persistence/tests/replication.rs::expired_claim_is_recoverable_without_duplicate_live_attempt` and run it through the gate; the assertion must fail because attempts have no exclusive finite lease and crash-recovery transition.
- [x] 4.4 GREEN: implement atomic claim, partial live-attempt uniqueness, lease expiry to terminal abandoned evidence, and re-claim with a new attempt identity until 4.3 passes; rerun persistence and service replication tests.

## 5. Replica-aware snapshot health

- [x] 5.1 RED: add `services/vault/tests/replication.rs::required_replica_controls_snapshot_health_without_rewriting_local_pass` and run it through the gate; the assertion must fail because a locally verified snapshot with missing required placements is still indistinguishable from fully healthy.
- [x] 5.2 GREEN: implement separate local/off-host health components and the required-target complete-placement projection until 5.1 passes, preserving the original local verification report and last healthy snapshot evidence.
- [x] 5.3 RED: add `services/vault/tests/replication.rs::optional_replica_failure_does_not_degrade_snapshot_health` and run it through the gate; the assertion must fail until the projection distinguishes optional placement failure from required convergence.
- [x] 5.4 GREEN: implement optional-versus-required target handling and explicit no-required-target misconfiguration behavior until 5.3 passes; rerun service replication, snapshot, reconciliation, and restore tests.

## 6. Replica-aware restore drills

- [x] 6.1 RED: add `services/vault/tests/restore_verification.rs::replica_aware_drill_selection_prefers_complete_verified_replica` and run it through the gate; the assertion must fail because the current selector can only return local BlobRefs.
- [x] 6.2 GREEN: implement deterministic `local`, `replica_preferred`, and eligible-complete-replica source selection plus source identity in the in-memory report model until 6.1 passes.
- [x] 6.3 RED: add `services/vault/tests/restore_verification.rs::replica_required_selection_never_falls_back_to_local` and run it through the gate; the assertion must fail if an incomplete/stale replica is selected or local bytes are silently substituted.
- [x] 6.4 GREEN: implement typed replica-source unavailable/deferred results and freshness/completeness checks until 6.3 passes.
- [x] 6.5 RED: add `services/vault/tests/restore_verification.rs::verified_replica_bundle_restores_exact_refs_from_downloaded_bytes` using the real in-memory S3-compatible harness and run it through the gate; the assertion must fail because the drill cannot yet acquire, hash, and reconstruct replica bytes.
- [x] 6.6 GREEN: stream replica manifest/bundles into create-new drill scratch files, verify them before the existing network-disabled Git stages, persist the actual source and placement evidence in the drill report/current schema, and clean only the drill-owned subtree until 6.5 passes; rerun all restore-verification tests.

## 7. Worker isolation, outage behavior, and telemetry

- [x] 7.1 RED: add `services/vault/tests/replication.rs::unreachable_replica_leaves_local_snapshot_and_verification_admissible` and run it through the gate; the assertion must fail because no worker records a bounded remote failure while an independent local operation succeeds.
- [x] 7.2 GREEN: compose the bounded replication worker with separate permits/deadlines from local snapshot/verification work, durable failure recording, later-pass retry, and graceful admission shutdown until 7.1 passes.
- [x] 7.3 RED: add `services/vault/tests/replication.rs::shutdown_deadline_leaves_an_owned_attempt_recoverable` and run it through the gate; the assertion must fail because an in-flight transfer cannot yet be cancelled/joined while preserving an expiring durable claim.
- [x] 7.4 GREEN: implement fixed-order worker shutdown, owned multipart abort where possible, task joining, and recoverable lease state until 7.3 passes; run the service replication and boot/shutdown suites.
- [x] 7.5 RED: add `crates/telemetry/src/metrics.rs::replication_diagnostics_use_closed_fields_and_never_render_secrets` and run it through the gate; the assertion must fail because the replication metric/log vocabulary is absent.
- [x] 7.6 GREEN: add bounded replication backlog, attempt, duration, bytes, failure-class, and last-verification-age telemetry without URLs, keys, provider text, repository names, or credentials until 7.5 passes; run telemetry and service tests.

## 8. Documentation, integration, and delivery validation

- [x] 8.1 Update `README.md`, `DEVELOPMENT.md`, architecture/testing/threat documentation, and the implementation-plan status for the actual S3 request subset, local-versus-off-host health, replica drills, deployment smoke-test gap, and recommended least-privilege/versioning/encryption/Object-Lock bucket policy; no failing test applies because this task changes operator narrative only, then verify all named documents describe remote deletion/lifecycle as still absent.
- [x] 8.2 Run `cargo tree --locked --duplicates`, `cargo deny --locked check`, and the repository advisory gate; review and justify every new direct/transitive dependency, duplicate, license, native crypto component, and advisory without weakening policy.
- [x] 8.3 Run the targeted round-trip, backlog, replica-aware drill, outage, schema, and hostile/corruption suites through `build-gate`; confirm the S3 harness uses loopback port 0, no personal repository/credential, no ignored test, and no fixture presented as real-provider proof.
- [x] 8.4 Run `openspec validate add-off-host-s3-replicas --strict`, then execute the complete fenced `DEVELOPMENT.md` gate exactly through one top-level `build-gate`; fix root causes and keep the DEVELOPMENT/CI command lists synchronized if either must change.
- [x] 8.5 Review the final diff for scope, credential leakage, unbounded work, migrations/version drift, remote deletion, stale generated files, and incomplete call sites; mark only observed task pairs complete, sync all five delta specs into current specs, and validate the completed change before archival.
