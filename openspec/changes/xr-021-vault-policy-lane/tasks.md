## 1. Contracts dependency

- [x] 1.1 Pin `ratatoskr-backup-contracts`, `ratatoskr-event-envelope` and `ratatoskr-identifiers` to the XR-021 contracts commit `ad16855c4e7f3d52cd118274faa3b8f3ab4da576` in the workspace manifest and `crates/core/Cargo.toml`, allow the contracts git source in `deny.toml`, and refresh `Cargo.lock` (dependency pin: it cannot start from a failing test; the existing suite must stay green)

## 2. Policy feed adapter (crates/core)

- [x] 2.1 Add `crates/core/tests/policy_feed.rs::policy_document_becomes_one_delivery_per_repository_and_none_for_dropped_targets` with a signature-only `deliveries_for_policy` stub returning an empty list, run it, and observe the delivery-count assertion fail
- [x] 2.2 Implement `crates/core/src/policy_feed.rs` (pure, no I/O) and move `IncomingDelivery` to `ratatoskr_vault_core::delivery`; rerun the test and observe it pass
- [x] 2.3 Add `crates/core/tests/policy_feed.rs::acknowledgements_are_complete_envelopes_for_both_outcomes` with a stub `acknowledgement_envelope` returning a placeholder-free but empty-payload envelope, run it, and observe the payload assertion fail
- [x] 2.4 Implement `acknowledgement_envelope` (producer, ids, correlation, causation, outcome, reasons, last applied version); rerun the test and observe it pass

## 3. Source-aware ingest and the command ledger (schema.sql, crates/persistence)

- [x] 3.1 Add `crates/persistence/tests/policy_state.rs::ingest_keeps_source_and_text_correlation_and_lists_governed_targets` with a stub `targets_governed_by_source`, run it against disposable PostgreSQL, and observe the ingest of a `backup_policy:7` correlation fail with `InvalidDelivery` instead of succeeding
- [x] 3.2 Edit `schema.sql` in place (`desired_state_revisions.source`, text `correlation_id`), thread the source through `ingest_delivery`, derive the UUID correlation key for retention evidence, and implement `targets_governed_by_source`; rerun the test and observe it pass
- [x] 3.3 Add `crates/persistence/tests/policy_state.rs::a_policy_decision_is_recorded_once_with_its_acknowledgement` with stub `record_policy_decision` and `last_applied_policy_version`, run it, and observe the one-command-row and one-outbox-row assertions fail
- [x] 3.4 Edit `schema.sql` in place (`backup_policy_commands`, text outbox `aggregate_id`, closed `event_type` list, relay bookkeeping columns), update the existing outbox inserts and the tests that read them, and implement the decision transaction; rerun the test and observe it pass
- [x] 3.5 Add `crates/persistence/tests/outbox_relay.rs::only_acknowledgements_are_claimed_and_failures_back_off` with stub relay queries, run it, and observe the claimed-row assertion fail
- [x] 3.6 Implement the claim, publish-mark and failure-backoff queries; rerun the test and observe it pass

## 4. Bus configuration (crates/core)

- [x] 4.1 Add `crates/core/tests/config_strictness.rs::bus_section_is_validated_and_needs_a_database` with a `BusConfig` section that is parsed but not validated, run it, and observe the missing-violation assertions fail
- [x] 4.2 Validate the bus URL scheme, the absolute nkey seed path and the database requirement; rerun the test and observe it pass

## 5. Readiness and task supervision (crates/http)

- [x] 5.1 Add `crates/http/tests/admin.rs::a_down_bus_is_a_failed_named_check_and_not_ready` and `a_down_database_makes_the_process_not_ready` with a stub `set_bus_connected`, run them, and observe the not-ready assertions fail
- [x] 5.2 Add the `bus` check and make `is_ready` require every reported check; rerun both tests and observe them pass
- [x] 5.3 Add `crates/http/tests/supervision.rs::the_first_task_to_return_ends_the_wait_before_a_signal` with a `wait_for_stop` stub that only waits for the signal, run it, and observe the stop-reason assertion fail
- [x] 5.4 Implement `wait_for_stop`, supervise `Serving.tasks` in `run`, and exit non-zero when a task returned; rerun the test and observe it pass

## 6. Policy consumer and relay (services/vault)

- [x] 6.1 Add `services/vault/tests/policy_bus.rs` (enrolment, redelivery, stale version, malformed envelope, dropped target, unpublished neighbours) against a real JetStream broker and a disposable database with stub `policy_bus` loops that never act, run it, and observe the enrolment and acknowledgement assertions fail
- [x] 6.2 Implement the consumer, the acknowledgement decision and the ack-gated relay in `services/vault/src/policy_bus`; rerun `policy_bus.rs` and observe it pass
- [x] 6.3 Add `services/vault/tests/policy_bus_process.rs::losing_the_broker_flips_readiness_and_exits_non_zero` that runs the shipped binary against a private broker, run it, and observe it fail because readiness stays 200 and the process keeps running
- [x] 6.4 Wire the bus into `services/vault/src/main.rs` as a serving task with readiness hooks; rerun the test and observe it pass

## 7. Vault bus identity

- [x] 7.1 Add `services/vault/tests/authorized_bus.rs` driving an authorization-enabled broker built from `deploy/nats/identity.conf`, run it, and observe the missing-fragment assertion fail
- [x] 7.2 Add `deploy/nats/identity.conf` (the VAULT stanza of S03), rerun the test and observe it pass (fetch and ack a command, publish the acknowledgement, two refusals)

## 8. Documentation, CI and gate

- [x] 8.1 Update `AGENTS.md` current phase, the `delivery.rs` module comment, `README.md`, `DEVELOPMENT.md` (bus section, NATS test prerequisites) and `docs/` references, and document `/etc/ratatoskr/vault.nkey` (documentation: it cannot start from a failing test)
- [x] 8.2 Provide a NATS server to the CI gate (`.github/workflows/ci.yml`) for `VAULT_TEST_NATS_URL` and the spawned brokers (configuration: it cannot start from a failing test)
- [x] 8.3 Run the full gate (`cargo fmt`, `clippy`, the 850-line check, `cargo test --workspace --locked`, `cargo build --workspace --locked --release`, `cargo deny check`, `openspec validate --all --strict`) and record the results
