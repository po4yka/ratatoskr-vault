## ADDED Requirements

### Requirement: Current schema records policy commands and relay state

The single current `schema.sql` SHALL persist a ledger of policy commands (command id, policy version, outcome, rejection code, acknowledgement outbox reference) with at most one accepted row per policy version, SHALL record on every desired-state revision the inbox source that delivered it, SHALL store revision correlation identifiers as text, and SHALL give the outbox a text aggregate reference, a closed list of allowed event types and relay bookkeeping (attempt count, last error class, next attempt time).

#### Scenario: An unknown outbox event type cannot be stored

- **WHEN** a write inserts an outbox row with an event type outside the closed list
- **THEN** PostgreSQL rejects the write

#### Scenario: Two accepted rows for one version are impossible

- **WHEN** a second accepted command row is inserted for a policy version that already has one
- **THEN** PostgreSQL rejects the write
