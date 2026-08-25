# Target State Machine Specification (delta)

## Purpose

Guards the eleven-value target status vocabulary so that every status change is legal, intentional, durably evidenced, and impossible to bypass through the database alone.

## ADDED Requirements

### Requirement: Closed status vocabulary

A target status MUST be one of `requested`, `cloning`, `ready`, `fetching`, `snapshotting`, `verifying`, `healthy`, `degraded`, `paused`, `excluded`, `deleting`. Any other value MUST be refused by storage.

#### Scenario: Unknown status refused

- **WHEN** a write attempts to set a target status outside the vocabulary
- **THEN** the write fails and the previously stored status is retained

### Requirement: Explicit legal-transition map

Status changes MUST move only between statuses named as adjacent by an explicit transition map that both the application and the database consult. A disordered pair MUST be refused with a transition error, leaving the original status intact.

#### Scenario: Illegal jump refused

- **WHEN** a direct transition from `requested` to `healthy` is attempted
- **THEN** it is refused with a transition error and the target remains `requested`

#### Scenario: Legal step accepted

- **WHEN** a transition from `requested` to `cloning` is performed
- **THEN** the target status becomes `cloning`

### Requirement: Terminal status is final

Once a target reaches `deleting`, no further status transition MUST be accepted.

#### Scenario: Transition out of terminal state

- **WHEN** any transition away from `deleting` is attempted
- **THEN** it is refused and the target remains `deleting`

### Requirement: Annotation rewrites

Writing a target's current status back MUST succeed as an annotation and MUST NOT emit a state-change event.

#### Scenario: Same-status write

- **WHEN** a target in status `healthy` is written with status `healthy`
- **THEN** the write succeeds and no state-change event is produced

### Requirement: Evidenced transitions

Each successful transition MUST atomically persist the new status together with exactly one `vault.target.state_changed.v1` event recording the target, the previous status, the new status, the governing policy revision, and the correlation identifier. When the surrounding write fails, neither the status change nor the event MAY remain.

#### Scenario: Event accompanies transition

- **WHEN** a legal transition commits
- **THEN** exactly one `vault.target.state_changed.v1` event is readable from the outbox and reports the previous and new status

#### Scenario: Failed write leaves no trace

- **WHEN** the transaction containing a transition fails before commit
- **THEN** the target keeps its previous status and no event exists for the attempt

### Requirement: Machine agreement across boundaries

The transition map enforced inside the database and the map consulted by application code MUST agree exactly on every ordered status pair, and an automated comparison MUST verify this against a disposable database.

#### Scenario: Agreement verification

- **WHEN** the agreement check runs against a disposable database
- **THEN** every pair the application map calls legal is accepted by the database guard and every pair the database guard refuses is absent from the application map
