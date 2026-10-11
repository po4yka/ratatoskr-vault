## ADDED Requirements

### Requirement: A relay that cannot publish makes the process not ready

Vault SHALL report a `bus_publish` readiness check whenever a bus is configured. The check SHALL fail when the last relay pass failed to publish at least one row and the oldest unpublished acknowledgement has at least three failed attempts or is older than 300 seconds, and SHALL pass again once no unpublished acknowledgement has a recorded failure. A failing check SHALL NOT stop the relay or end the process, and the relay SHALL count each attempted row in `vault_outbox_publications_total{outcome}` with the outcomes `published` and `failed`.

#### Scenario: A refused acknowledgement flips readiness until it publishes

- **WHEN** the broker's Vault identity lacks the acknowledgement subject, a valid policy command is decided, and the acknowledgement row fails three publish attempts
- **THEN** `/health/ready` answers 503 with a failed `bus_publish` check while the relay keeps running, and after the subject is permitted again the next pass publishes the row and `/health/ready` answers 200

### Requirement: A loadable operator example ships with the repository

The repository SHALL ship `deploy/systemd/vault.conf.example`, an environment file that sets the operator listener `127.0.0.1:9570`, the bus URL, the nkey seed path `/etc/ratatoskr/vault.nkey` through `RATATOSKR__BUS__NKEY_SEED_PATH`, and a database URL placeholder, and a test SHALL load it through the service's configuration loader.

#### Scenario: The shipped example loads

- **WHEN** the example is loaded through the configuration loader
- **THEN** the operator address, the bus URL, the seed path and the database URL are the values the example names
