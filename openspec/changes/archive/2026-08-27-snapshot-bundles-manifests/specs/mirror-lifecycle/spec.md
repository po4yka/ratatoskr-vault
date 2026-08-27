## ADDED Requirements

### Requirement: Healthy observations are immutable snapshot inputs

After a successful mirror lifecycle run has recorded integrity and object evidence, Vault SHALL
make that exact observation eligible for snapshot creation. Snapshot construction SHALL read the
published bare mirror without changing the completed lifecycle evidence; failure to create or
publish a snapshot SHALL leave the last successful mirror observation intact and SHALL NOT advance
the target to a verified or healthy snapshot state.

#### Scenario: Snapshot publication failure preserves the mirror observation

- **WHEN** a healthy mirror observation is selected for a snapshot and local artifact publication
  fails
- **THEN** the recorded successful mirror observation remains available, no immutable snapshot is
  claimed, and the target does not report snapshot verification or health
