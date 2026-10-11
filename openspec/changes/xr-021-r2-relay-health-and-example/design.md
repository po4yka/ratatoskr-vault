## Context

The relay in `services/vault/src/policy_bus/relay.rs` publishes due acknowledgement rows. A publish that the broker does not confirm is logged with the class `publish_unacknowledged`, recorded through `record_publish_failure` with a capped exponential backoff, and the loop continues. A lost connection already ends the lane and the process. This change only adds the signal for the case between those two: connected and failing.

## Decisions

### 1. Readiness reads one flag the relay writes

`RuntimeState` already holds the bus connection as a tri-state atomic (absent, up, down). It gains a second one for publisher health with the same three states, set by `policy_bus::start` to up when the lane starts and by the relay afterwards. The check is named `bus_publish` and sorts beside `bus`. It is absent when no bus is configured, like the `bus` check. The reason token is a new `publish_failing`, never a formatted error.

### 2. The relay decides after each pass, from the database

The rule of R2-03 is stated over "the oldest unpublished publishable row", which only the database knows (a row can be backed off and absent from the due batch). After a pass the relay reads one summary row: the attempt count and age of the oldest unpublished acknowledgement and whether any unpublished acknowledgement has failed before. A pass that failed at least one publish sets the flag to `not (attempt_count >= 3 or age > 300 s)`. A pass that failed nothing sets it healthy only when no unpublished row has a recorded failure, so a row that is merely backed off between attempts does not make the check flap to healthy on every empty pass; a backlog with no failing row, or no backlog at all, is healthy.

Alternative considered: set healthy on any pass without a failure, as the contract sentence reads literally. Rejected because a due-batch with only the healthy neighbours of a stuck row would flip readiness back to 200 until the stuck row's next attempt.

### 3. The counter has two closed outcomes

`vault_outbox_publications_total{outcome="published"|"failed"}` is incremented by the relay once per attempted row. No event id, subject or error text becomes a label.

### 4. The operator example is a plain environment file

`deploy/systemd/vault.conf.example` is a systemd `EnvironmentFile`: `KEY=value` lines. The test `boot.rs::the_shipped_example_loads` parses those lines, replaces the absolute seed path with a temporary file only where validation needs the file to exist (the loader checks that the path is absolute, not that it exists, so the shipped path is asserted verbatim), and loads them through `VaultConfig` so a key that stops being read fails the test.
