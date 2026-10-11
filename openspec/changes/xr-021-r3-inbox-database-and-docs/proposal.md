## Why

Changeset XR-021 round 3 found three defects on the Vault side. Every non-Edge identity may subscribe to the shared `_INBOX.>` namespace, so a compromised service can read the JetStream deliveries addressed to Vault and to every other service (XR-021 CONTRACTS.md section R3-01). The operator example names the database `vault` and the role `vault`, which no checked-in file creates, while the deployment target runs one database, `ratatoskr`, with one role per service (section R3-11). The README still says that no workspace pin exists (section R3-19).

## What Changes

- The policy lane connects with the inbox prefix `_INBOX_vault` (`custom_inbox_prefix`), and the VAULT stanza in `deploy/nats/identity.conf` subscribes to `_INBOX_vault.>` only. The literal is one `const` in `services/vault/src/policy_bus/connection.rs`, and the test scaffolding builds its Vault identity through the same options function as production.
- `deploy/systemd/vault.conf.example` names `postgres://ratatoskr_vault:CHANGE-ME@127.0.0.1:5432/ratatoskr`; `deploy/postgres/role.sql` creates the role `ratatoskr_vault` with `connect, create` on the database `ratatoskr`; the README gets a deployment paragraph.
- The README "Workspace integration" paragraph is rewritten from the workspace as it is.

Deliberate breaks, named per CONTRACTS.md S00: a Vault build from before this change cannot complete JetStream requests under the new ACL (the reply is never delivered and the call times out), so NATS is reloaded and the services of one snapshot are deployed together; an operator who copied the old example must change the role and the database in the URL.

## Capabilities

### Modified Capabilities

- `backup-policy-lane`: the bus connection uses its own inbox prefix, and the shipped operator example names the shared database and a role that a checked-in file creates.

## Impact

`services/vault/src/policy_bus/connection.rs`, `services/vault/src/test_support/authorized.rs`, `services/vault/tests/authorized_bus.rs`, `services/vault/tests/boot.rs`, `deploy/nats/identity.conf`, `deploy/systemd/vault.conf.example`, `deploy/postgres/role.sql`, `README.md`. The cross-repository rules are XR-021 CONTRACTS.md sections R3-01, R3-11 and R3-19 and are not restated here.
