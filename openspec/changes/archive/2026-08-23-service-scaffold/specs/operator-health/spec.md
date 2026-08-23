# operator-health delta

## Purpose

The admin-plane endpoints an operator, supervisor, or metrics stack reads: liveness, readiness, Prometheus metrics, and build version.

## ADDED Requirements

### Requirement: Liveness never consults a dependency

`GET /health/live` SHALL answer 200 from the moment the admin listener binds until the process exits, including throughout the drain, without consulting the database or any other dependency.

#### Scenario: Liveness answers during drain

- **WHEN** `/health/live` is requested after a shutdown signal
- **THEN** it answers 200 with `state: live`

### Requirement: Readiness names its failing checks

`GET /health/ready` SHALL answer 200 only when startup is complete and no drain has begun, and 503 otherwise; the body SHALL carry name-sorted checks whose failure reasons are from a closed set, and a configured-but-down database SHALL appear as a failed `database` check.

#### Scenario: Down database appears in readiness

- **WHEN** a database is configured, startup is complete, and the last probe did not answer
- **THEN** `/health/ready` answers 503 with a failed `database` check and reason `dependency_unavailable`

#### Scenario: Two consecutive bodies are byte-identical

- **WHEN** `/health/ready` is requested twice with no state change between
- **THEN** the two bodies are identical byte for byte

### Requirement: Metrics renders Prometheus text

`GET /metrics` SHALL render the process-internal metrics registry as Prometheus text exposition with the matching content type; there is no second metrics HTTP server.

#### Scenario: Metrics scrape returns exposition format

- **WHEN** `/metrics` is requested
- **THEN** it answers 200 with content type `text/plain; version=0.0.4` and includes a build-info series

### Requirement: Version reports the build identity

`GET /version` SHALL report the service name, crate version, git SHA (or `unknown` when not supplied at build time), and toolchain version.

#### Scenario: Version fields are present

- **WHEN** `/version` is requested
- **THEN** the body carries `service`, `version`, `git_sha`, and `rust_version`

### Requirement: The admin plane carries no error envelope and no caching

Every admin response, including unknown paths and failures, SHALL set `Cache-Control: no-store`; admin bodies are read by people and scrapes, not by Vault clients, so they carry plain JSON or text rather than a client error envelope.

#### Scenario: Unknown admin path sets no-store

- **WHEN** an unknown admin path is requested
- **THEN** the bare 404 response carries `Cache-Control: no-store`
