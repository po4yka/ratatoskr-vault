## Why

Changeset XR-021 round 2 found two defects in the Vault side of the policy lane. First, the acknowledgement relay logs a refused publish and records the backoff, but nothing tells readiness, so an ACL drift or a refused subject leaves `/health/ready` at 200 while no acknowledgement ever leaves Vault (XR-021 CONTRACTS.md section R2-03, extending S02 rule 5). Second, Vault ships no operator example for its bus keys, so an operator has to assemble `RATATOSKR__BUS__URL`, `RATATOSKR__BUS__NKEY_SEED_PATH` and the database URL from prose (section R2-10).

## What Changes

- Add a `bus_publish` readiness check fed by the relay. It is reported once the lane starts and fails when the last pass failed to publish a row and the oldest unpublished acknowledgement has at least three failed attempts or is older than 300 seconds. It recovers when the backlog holds no failing row. A failing check never stops the relay and never ends the process; it only flips readiness.
- Add the counter `vault_outbox_publications_total{outcome}` with the closed outcomes `published` and `failed`.
- Add `deploy/systemd/vault.conf.example` with the operator address, the bus URL, the nkey seed path `/etc/ratatoskr/vault.nkey` and a database URL placeholder, and point `DEVELOPMENT.md` at it.
- Move the authorization-enabled broker scaffolding of `authorized_bus.rs` into the `test-support` library module so two test binaries share it, and let a test broker reload its configuration.

No wire shape changes and no contracts commit is needed: the repository stays on the contracts commit it already pins.

Deliberate breaks, named per CONTRACTS.md S00: `/health/ready` gains a check that can fail while the broker connection is up; a deployment whose relay cannot publish now reports not ready instead of ready.

## Capabilities

### Modified Capabilities

- `backup-policy-lane`: the readiness requirement gains the publisher-health check, and an operator example is shipped and kept loadable.

## Impact

`crates/http` (the check), `crates/persistence` (one backlog query), `crates/telemetry` (one counter), `services/vault` (the relay, the lane wiring, test scaffolding and tests), `deploy/systemd/vault.conf.example`, `DEVELOPMENT.md`. The cross-repository rule is XR-021 CONTRACTS.md sections R2-03 and R2-10 and is not restated here.
