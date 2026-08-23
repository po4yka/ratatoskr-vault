# git-vault-schema Specification

## Purpose
The first version of the `git_vault` PostgreSQL schema: the one file Vault applies to a fresh database, owned entirely by this bounded context.

## Requirements

### Requirement: One schema definition, applied in place

The schema SHALL be one `schema.sql` at the repository root, embedded in the binary at compile time. A schema change edits that file in place; there is no migration ledger and no version history.

#### Scenario: A fresh database receives the whole schema from the embedded file

- **WHEN** a disposable database is created and the embedded schema is applied
- **THEN** every table listed below exists under the `git_vault` schema with its columns and constraints

### Requirement: Applying the schema is idempotent

Applying the schema to a database that already has it SHALL change nothing and succeed; two processes applying concurrently SHALL serialize so neither fails.

#### Scenario: Second apply on a populated database succeeds and changes nothing

- **WHEN** the schema has been applied once and a second apply runs
- **THEN** it succeeds without recreating or clearing any object

### Requirement: The first version carries the tables the architecture names

The `git_vault` schema SHALL contain, at minimum: `targets`, `desired_state_revisions`, `mirrors`, `sync_runs`, `snapshots`, `snapshot_artifacts`, `manifests`, `integrity_checks`, `restore_drills`, `retention_policies`, `tombstones`, `storage_locations`, `collector_runs`, `outbox`, and `inbox`.

#### Scenario: Every named table exists after apply

- **WHEN** the schema is applied to a fresh database and the catalogue is queried for `git_vault` tables
- **THEN** all fifteen named tables are present

### Requirement: The schema follows the fleet database conventions

Identifiers SHALL be UUID primary keys minted by the application (no database default), closed vocabularies SHALL be checked text rather than PostgreSQL enums, timestamps SHALL be `timestamptz`, state machines SHALL be enforced by CHECK constraints where expressible, and no foreign key SHALL cross a schema boundary.

#### Scenario: A state value outside the vocabulary is refused by the database

- **WHEN** a row is inserted into `git_vault.targets` with a status outside the closed vocabulary
- **THEN** PostgreSQL rejects the insert with a constraint violation
