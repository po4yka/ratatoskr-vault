## ADDED Requirements

### Requirement: Required LFS work shares mirror lifecycle admission and quota

For an LFS-enabled target, the admitted mirror operation SHALL include Git acquisition plus a conservative finite LFS staging/publication ceiling. Vault MUST reserve that combined budget before Git starts, enforce the per-run staging ceiling before object publication, and refuse work that cannot fit the per-target or global quota. Quota refusal or observed overrun SHALL append terminal evidence, degrade the target, clean only run-owned staging, preserve every prior mirror and verified artifact, and never silently prune content.

#### Scenario: LFS bytes exceed remaining quota

- **WHEN** the Git mirror fits but the enabled fixture's required LFS objects exceed the remaining per-target or global budget
- **THEN** LFS publication is refused, the target becomes degraded with quota evidence, prior verified data remains byte-for-byte intact, and no other target is pruned

#### Scenario: LFS bytes fit the reserved envelope

- **WHEN** the Git mirror and required LFS objects fit the admitted finite envelope
- **THEN** terminal lifecycle and collector evidence records the resulting Git and LFS bytes and releases the conservative live reservation only after the terminal mirror result is durable

### Requirement: Wiki sibling work uses normal bounded lifecycle semantics

A discovered wiki target SHALL use the same global concurrency cap, per-target serialization, quota admission, confined identifier-derived paths, integrity checks, immutable terminal runs, and last-good preservation rules as a repository target. Wiki work MUST NOT mutate the parent repository mirror or use the parent display name as a path.

#### Scenario: Wiki mirror competes for the shared finite permit set

- **WHEN** four repository or wiki operations hold all permits and another discovered wiki is ready
- **THEN** the fifth operation waits without spawning Git and later runs under its own target identity when a permit becomes available
