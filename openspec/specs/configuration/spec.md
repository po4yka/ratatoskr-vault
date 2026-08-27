# configuration Specification

## Purpose
The typed configuration tree of a Vault process, the environment variables that populate it, and the strictness rules that make a bad deployment fail loudly at startup.

## Requirements

### Requirement: Environment is the only configuration source

Configuration SHALL be read from `RATATOSKR__` environment variables with `__` separating nesting levels over built-in defaults. There SHALL be no configuration file and no other source.

#### Scenario: Nested variable populates nested field

- **WHEN** `RATATOSKR__ADMIN__BIND` is set to a socket address
- **THEN** the loaded configuration carries it at `admin.bind`

### Requirement: Unknown keys are refused

An environment variable under the prefix that names no field in the tree SHALL fail extraction rather than being ignored.

#### Scenario: Unknown key fails startup

- **WHEN** `RATATOSKR__NO_SUCH_SECTION__X` is set
- **THEN** configuration loading fails with an error naming the unknown key

### Requirement: Wrong types are refused

A value that does not parse into its field type SHALL fail extraction with an error naming the key.

#### Scenario: Non-numeric port fails startup

- **WHEN** `RATATOSKR__ADMIN__BIND` is set to a value that is not a socket address
- **THEN** configuration loading fails naming `admin.bind`

### Requirement: Validation reports every violation without echoing values

Semantic validation SHALL collect every violated rule into one report, and the report SHALL contain rule text, key, and environment variable name but never a supplied value.

#### Scenario: All violations reported in one round trip

- **WHEN** two independent rules are violated at once (for example an unparseable log filter and a drain-plus-grace total above the ceiling)
- **THEN** the failure report names both violations

#### Scenario: A secret configured by environment never appears in the report

- **WHEN** `RATATOSKR__DATABASE__URL` carries a password and another field violates a rule
- **THEN** neither the failure report nor any debug rendering of the configuration contains that password

### Requirement: Database configuration is optional and secret

The database URL SHALL be absent by default, SHALL be required before any route that needs it starts, and SHALL be a secret type that has no `Display` and redacts in `Debug`.

#### Scenario: No database means probes serve and readiness reports no database check

- **WHEN** the process starts with no database configured
- **THEN** it binds the admin listener and `/health/ready` reports checks without a database entry

### Requirement: Telemetry defaults are explicit

Log format SHALL default to JSON, the log filter SHALL default to a valid directive string that is validated at configuration time, and OTLP export SHALL be optional; when absent the process warns at startup and still mints real trace ids.

#### Scenario: Invalid log filter is a configuration error

- **WHEN** `RATATOSKR__TELEMETRY__LOG_FILTER` is not a valid directive string
- **THEN** configuration loading fails naming `telemetry.log_filter`

### Requirement: Verification and restore policy is finite and strict

When scheduled verification is enabled, configuration SHALL require positive verification and drill
frequencies, a positive sample-size ceiling, a positive scratch-byte budget, a finite per-drill time
budget, and a bounded concurrency ceiling. It SHALL require an absolute scratch root distinct from
the configured live-mirror root and trusted manifest verification keys. Missing, zero, overlapping,
relative, or unknown settings MUST fail startup without echoing secret signing material.

#### Scenario: Unsafe restore roots fail configuration

- **WHEN** the restore scratch root equals or is lexically inside the live mirror root
- **THEN** configuration loading fails naming the scratch-root setting and prints no configured key material

#### Scenario: Zero schedule budget fails configuration

- **WHEN** scheduled drills are enabled with a zero sample size, byte budget, time budget, or concurrency limit
- **THEN** configuration loading reports every violated finite-budget rule before any job starts

### Requirement: Off-host replica configuration is strict, finite, and environment-only

Each replica target SHALL be configured through the existing `RATATOSKR__` environment-only tree with a stable target name, HTTPS S3-compatible endpoint, bucket, region, optional object-key prefix, access-key secret, secret-access-key secret, and optional session-token secret. The configuration SHALL also require positive transfer deadlines, a positive byte ceiling, and bounded backlog and concurrency limits. Plain HTTP MUST be refused except for a loopback test endpoint. Vault MUST construct the client from these explicit values and MUST NOT consult credential files, instance metadata, container metadata, or an ambient provider credential chain.

#### Scenario: Missing credentials and zero limits fail startup without leakage

- **WHEN** a replica target omits a credential or sets any transfer, byte, backlog, or concurrency limit to zero
- **THEN** startup reports every invalid setting by key and environment-variable name without rendering endpoint credentials or secret values

#### Scenario: Non-loopback plaintext endpoint is refused

- **WHEN** a replica target uses an HTTP endpoint whose host is not loopback
- **THEN** startup fails before creating an S3 client and names the endpoint rule without echoing credentials
