## 1. Relay health reaches readiness (R2-03)

- [x] 1.1 Add `crates/persistence/tests/outbox_relay.rs::the_publish_backlog_reports_the_oldest_row_and_whether_any_row_failed` with a stub `publish_backlog` that returns no backlog, run it against disposable PostgreSQL, and observe the oldest-row assertion fail
- [x] 1.2 Implement `publish_backlog`; rerun the test and observe it pass
- [x] 1.3 Add `crates/telemetry/src/metrics.rs` unit test `an_outbox_publication_is_counted_by_outcome` with a stub `record_outbox_publication` that records nothing, run it, and observe the rendered-counter assertion fail
- [x] 1.4 Implement the counter; rerun the test and observe it pass
- [x] 1.5 Move the authorized-broker scaffolding of `authorized_bus.rs` into `ratatoskr_vault::test_support::authorized` and add `Broker::reload` (a refactor of test scaffolding: it adds no behaviour and `authorized_bus.rs` must stay green)
- [x] 1.6 Add `services/vault/tests/policy_bus.rs::a_refused_acknowledgement_flips_readiness_until_it_publishes` with a `bus_publish` check variant that is never reported, run it against an authorization-enabled broker whose Vault stanza omits the acknowledgement subject, and observe the 503 assertion fail because readiness stays 200
- [x] 1.7 Report the check from the relay per decision 2 and rerun the test, observing it pass
- [x] 1.8 Cover the readiness route and the stuck-row decision directly: `crates/http/tests/admin.rs::a_failing_publisher_is_a_named_check_beside_a_connected_bus` and `relay.rs::the_relay_is_unhealthy_only_when_the_oldest_row_is_stuck` (they pin behaviour 1.7 already implemented, so they pass on first run; the RED evidence for the behaviour is 1.6)

## 2. Operator example (R2-10; configuration)

- [ ] 2.1 Add `services/vault/tests/boot.rs::the_shipped_example_loads`, run it, and observe it fail because `deploy/systemd/vault.conf.example` is absent
- [ ] 2.2 Add `deploy/systemd/vault.conf.example` and a `DEVELOPMENT.md` pointer; rerun the test and observe it pass
