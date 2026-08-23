# service-runtime Specification

## Purpose
The process lifecycle of the `ratatoskr-vault` binary: how it starts, what refuses to start it, how it reports readiness, and how it stops.

## Requirements

### Requirement: The binary runs the shared lifecycle and nothing else

`ratatoskr-vault` SHALL start by loading and validating configuration, then installing telemetry, then building its routes, then binding its listeners, and SHALL exit with a documented exit code for each failure class. A `check-config` first argument SHALL load, validate, and report the effective configuration or the failure report without binding anything.

#### Scenario: Invalid configuration refuses startup with exit 78

- **WHEN** the process starts with an environment that fails configuration validation
- **THEN** it writes a value-free failure report to stderr, binds no listener, and exits with code 78

#### Scenario: check-config validates without serving

- **WHEN** the process runs with the `check-config` argument and a valid environment
- **THEN** it writes the effective configuration (secrets redacted) to stderr and exits 0 without opening a listener

#### Scenario: Unreachable database refuses route build

- **WHEN** a database is configured but the server refuses connections
- **THEN** the process exits non-zero and never reports readiness

### Requirement: Readiness reflects startup and drain

The process SHALL answer `/health/ready` with 503 until every configured listener is bound, and SHALL fail readiness immediately when a shutdown signal arrives while the listeners remain open.

#### Scenario: Readiness flips only after startup completes

- **WHEN** the listeners are bound and startup is marked complete
- **THEN** `/health/ready` answers 200 with a `ready` state and named checks

#### Scenario: Drain fails readiness before the listener closes

- **WHEN** a shutdown signal arrives
- **THEN** `/health/ready` answers 503 with a drain-failed check while `/health/live` still answers 200

### Requirement: Graceful stop drains then closes

On SIGTERM or SIGINT the process SHALL stop accepting, keep serving in-flight requests for the configured drain window, allow the configured grace window for completion, close the database pool after the listeners close, flush telemetry, and exit 0.

#### Scenario: Clean shutdown exits 0

- **WHEN** the process receives SIGTERM after startup completed
- **THEN** it closes both phases in order and exits with code 0
