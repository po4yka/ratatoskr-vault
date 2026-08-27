## ADDED Requirements

### Requirement: Required replica verification participates in snapshot health

Snapshot health SHALL distinguish verified local preservation from required off-host convergence. When the governing desired state requires off-host storage, the snapshot SHALL be healthy only after every required bundle and manifest placement has current remote digest-and-size verification. Missing, failed, stale, or deferred required placements SHALL produce degraded off-host health while preserving all prior local verification evidence. A snapshot whose policy does not require off-host storage MUST NOT be degraded merely because an optional replica is unavailable.

#### Scenario: Required unverified replica degrades snapshot health

- **WHEN** a locally verified snapshot requires off-host storage and its bundle upload is deferred or fails verification
- **THEN** snapshot health reports local verification as passed and off-host convergence as degraded rather than reporting the snapshot healthy

#### Scenario: Verified required placements make health converge

- **WHEN** every required bundle and manifest placement for a locally verified snapshot passes remote re-download verification
- **THEN** snapshot health reports the required off-host component verified and the snapshot can become healthy without rewriting its local verification report
