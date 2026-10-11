## Context

The policy lane connects in `services/vault/src/policy_bus/connection.rs`. The authorized-broker tests start a real `nats-server` from `deploy/nats/identity.conf` and build their Vault client in `services/vault/src/test_support/authorized.rs`. R3-01 narrows the stanza's `subscribe.allow` from `_INBOX.>` to `_INBOX_vault.>`, which only works when the client asks for replies on that prefix.

## Decisions

### 1. One options function, used by production and by the tests

`policy_bus::connect_options(seed)` builds the `ConnectOptions` (nkey or none, plus the inbox prefix). `connection::connect` calls it, and `test_support::authorized::connect_vault` calls it, so a test that passes proves the shipped options and not a hand-written copy. The prefix is `const INBOX_PREFIX: &str = "_INBOX_vault";` beside the connection code so the workspace text scan finds it inside a `custom_inbox_prefix(` call.

### 2. The administrator in the test broker keeps the plain options

The administrator stands in for Edge, which keeps `_INBOX.>`; it uses the unprefixed `connect`.

### 3. The role file is the extractor's file

`deploy/postgres/role.sql` is modelled on the extractor's: create the role if absent without a password, no elevated attributes, `grant connect, create on database ratatoskr`, `\connect ratatoskr`, `revoke all on schema public`. `create` on the database is all the schema application needs, because `schema.sql` creates the `git_vault` schema and applies only when `git_vault` is absent. It was confirmed once by hand rather than by a test (a test needs a PostgreSQL administrator, which the suite's disposable databases do not model): `role.sql` run twice on a database `ratatoskr` created with the ICU locale, then the real binary started with the role's URL created `git_vault` (29 tables, owned by `ratatoskr_vault`) and answered `/health/ready` 200.

### 4. The example test reads the checked-in files

`boot.rs` already loads the example through the configuration loader. The new case asserts the URL and reads `deploy/postgres/role.sql` for the role name and the grant, so the example and the file cannot drift apart.
