## 1. Inbox prefix (R3-01)

- [x] 1.1 Add `services/vault/tests/authorized_bus.rs::the_policy_lane_completes_jetstream_requests_under_its_inbox_prefix` against a broker whose VAULT stanza subscribes to `_INBOX_vault.>`, with the lane's options in one function (`policy_bus::connect_options`, still without a prefix) that the test scaffolding also uses; run it and observe `CONSUMER.INFO` time out
- [x] 1.2 Add `const INBOX_PREFIX: &str = "_INBOX_vault";` applied with `custom_inbox_prefix`; rerun the test and observe it pass, with the two other authorized-lane tests

## 2. The example names the shared database (R3-11)

- [ ] 2.1 Add `services/vault/tests/boot.rs::the_example_names_the_shared_database_and_the_role_file_exists`, run it, and observe it fail because the example still names the role `vault` and the database `vault`
- [ ] 2.2 Edit the example, add `deploy/postgres/role.sql` and the README deployment paragraph; rerun the test and observe it pass; apply the schema as the new role on a database where `git_vault` does not exist

## 3. Documentation (R3-19; no failing test can precede it)

- [ ] 3.1 Rewrite the README "Workspace integration" paragraph from the workspace as it is (documentation, so no test comes first)
