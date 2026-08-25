# Desired-State Ingestion Specification (delta)

## Purpose

Turns each delivered desired-backup-policy revision into durable, deduplicated target state so that later pipeline stages always act on the newest authorized intent and never lose evidence of what was delivered.

## ADDED Requirements

### Requirement: Delivery deduplication

A redelivered message MUST be recognized by its source-and-message identity and applied at most once. A replay MUST leave target and revision state unchanged and MUST report the delivery as already applied.

#### Scenario: Same message delivered twice

- **WHEN** an identical delivery (same source and message identifier) is applied a second time
- **THEN** no new revision row and no target change result, and the second application reports the delivery as already applied

### Requirement: Validated input boundary

A delivery that does not carry a known preservation level, a positive policy revision, and the required repository identity MUST be refused in full, leaving no partially written rows.

#### Scenario: Unknown preservation level

- **WHEN** a delivery names a preservation level outside the five defined levels
- **THEN** the delivery is refused with a validation error and neither a target nor a revision row is created

### Requirement: Append-only revision evidence

Every valid delivery MUST be recorded as a new immutable revision row carrying its policy revision number and correlation identifier. Existing revision rows MUST never be rewritten or removed by ingestion.

#### Scenario: Stale delivery still leaves evidence

- **WHEN** a delivery numbered older than the newest accepted revision is received
- **THEN** its revision row is stored together with its revision number and correlation identifier

### Requirement: Newest revision governs

Only the highest recorded policy revision of a target MAY drive state changes. Recording a lower-numbered revision MUST NOT alter target status and MUST NOT produce events.

#### Scenario: Out-of-order delivery

- **WHEN** revision 7 has been accepted and revision 5 arrives afterwards
- **THEN** revision 5 is stored, the target keeps the state established by revision 7, and no state-change event is emitted for revision 5

### Requirement: Enrollment on first active policy

The first delivery with a preservation level other than `none` for an unknown repository MUST create a target for that repository with status `requested`.

#### Scenario: Unknown repository receives active policy

- **WHEN** the first `git_mirror` delivery for a repository arrives
- **THEN** a target for that repository exists with status `requested`

### Requirement: None policy excludes unpinned targets

When the governing revision carries preservation level `none` and the target is not pinned, the target MUST be marked `excluded`.

#### Scenario: Backup withdrawal

- **WHEN** the governing revision sets preservation level `none` for an unpinned target in status `requested`
- **THEN** the target status becomes `excluded`

### Requirement: Pinned targets survive withdrawal

A pinned target MUST NOT be automatically excluded when its governing policy becomes `none`. Its status stays unchanged while the withdrawal is recorded, keeping the divergence observable to operators.

#### Scenario: Withdrawal for pinned target

- **WHEN** the governing revision sets preservation level `none` for a pinned target in status `requested`
- **THEN** the target remains `requested`, the revision is stored, and the convergence result identifies the held-back exclusion

### Requirement: Re-activation after exclusion

When the governing revision becomes active again for a target in status `excluded`, the target MUST return to `requested`.

#### Scenario: Repository re-enrolled

- **WHEN** a new active-policy revision governs a target in status `excluded`
- **THEN** the target status becomes `requested`

### Requirement: Convergence idempotence

Re-applying the governing revision after convergence MUST perform no transitions, create no new revision rows, and emit no new events. The state after the second pass MUST equal the state after the first.

#### Scenario: Second convergence pass

- **WHEN** ingestion and convergence run twice for the same governing revision
- **THEN** the second pass reports zero changes and the resulting state matches the first pass exactly
