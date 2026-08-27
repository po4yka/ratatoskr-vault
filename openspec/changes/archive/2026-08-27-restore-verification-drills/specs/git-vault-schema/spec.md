## ADDED Requirements

### Requirement: Verification, drill, and event evidence is append-only and linked

The current schema SHALL persist each verification attempt and restore drill with its snapshot,
manifest, immutable report fields, outcome, typed failure, timings, and correlation metadata. It
SHALL constrain terminal outcomes and prevent a terminal report from being rewritten. A terminal
report and its canonical fleet event SHALL commit atomically through the Vault outbox, with a stable
uniqueness key making replay idempotent.

#### Scenario: Drill failure and alert event commit together

- **WHEN** a restore drill records a terminal ref-mismatch failure
- **THEN** the same transaction persists one failed immutable report and one uniquely keyed outbox event linked to it, or persists neither

#### Scenario: Terminal report cannot change outcome

- **WHEN** an update attempts to convert a committed failed verification or drill report to passed
- **THEN** PostgreSQL rejects the mutation and a later retry must use a new run identifier
