## MODIFIED Requirements

### Requirement: None policy excludes unpinned targets

When the governing revision carries preservation level `none` and the target is not pinned, the target MUST be marked `excluded` and exactly one active target tombstone MUST be recorded atomically with the governing revision and its policy-defined grace deadline. This convergence MUST NOT physically delete snapshot bytes.

#### Scenario: Backup withdrawal

- **WHEN** the governing revision sets preservation level `none` for an unpinned target in status `requested`
- **THEN** the target status becomes `excluded`, one active target tombstone records that revision and deadline, and all snapshot artifacts remain available

### Requirement: Pinned targets survive withdrawal

A pinned target MUST NOT be automatically excluded or tombstoned when its governing policy becomes `none`. Its status stays unchanged while the withdrawal is recorded, keeping the divergence observable to operators.

#### Scenario: Withdrawal for pinned target

- **WHEN** the governing revision sets preservation level `none` for a pinned target in status `requested`
- **THEN** the target remains `requested`, no target tombstone is created, the revision is stored, and the convergence result identifies the held-back exclusion

### Requirement: Re-activation after exclusion

When the governing revision becomes active again for a target in status `excluded` and physical deletion has not started, the target MUST return to `requested`, the active tombstone MUST be cancelled, and pending automatic deletion plans MUST become non-executable without removing their audit evidence.

#### Scenario: Repository re-enrolled

- **WHEN** a new active-policy revision governs an excluded target before physical deletion starts
- **THEN** the target becomes `requested`, its tombstone and pending automatic plans are cancelled, and all snapshot bytes remain available
