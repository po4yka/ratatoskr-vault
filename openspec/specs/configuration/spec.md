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
