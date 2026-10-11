## ADDED Requirements

### Requirement: The policy lane receives replies on its own inbox prefix

The policy lane SHALL connect with the inbox prefix `_INBOX_vault`, and the VAULT stanza in `deploy/nats/identity.conf` SHALL allow subscribing to `_INBOX_vault.>` and nothing else.

#### Scenario: The lane completes JetStream requests under the narrowed ACL

- **WHEN** a broker enforces the shipped VAULT stanza and the lane's connection options are used by the Vault identity
- **THEN** the identity verifies and pulls `ratatoskr_vault_backup_policy` and publishes its acknowledgement with a PubAck

### Requirement: The operator example names the shared database and a role a checked-in file creates

`deploy/systemd/vault.conf.example` SHALL name the database URL `postgres://ratatoskr_vault:CHANGE-ME@127.0.0.1:5432/ratatoskr`, and `deploy/postgres/role.sql` SHALL create the role `ratatoskr_vault` with `connect` and `create` on the database `ratatoskr`. The schema SHALL apply as that role on a database where `git_vault` does not yet exist.

#### Scenario: The example and the role file agree

- **WHEN** the example is loaded through the configuration loader and `deploy/postgres/role.sql` is read
- **THEN** the URL names the role and database above, and the file creates that role with that grant
