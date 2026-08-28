## MODIFIED Requirements

### Requirement: Closed status vocabulary

A target status MUST be one of `requested`, `cloning`, `ready`, `fetching`, `snapshotting`, `verifying`, `healthy`, `degraded`, `paused`, `excluded`, `deleting`, `deleted`. Any other value MUST be refused by storage.

#### Scenario: Unknown status refused

- **WHEN** a write attempts to set a target status outside the vocabulary
- **THEN** the write fails and the previously stored status is retained

### Requirement: Explicit legal-transition map

Status changes MUST move only between statuses named as adjacent by an explicit transition map that both the application and the database consult. The map MUST permit `excluded` to `deleting` only for an executable grace-complete deletion plan, `deleting` to `deleted` only after every required stage completes, and `excluded` to `requested` while deletion has not begun. A disordered pair MUST be refused with a transition error, leaving the original status intact.

#### Scenario: Illegal jump refused

- **WHEN** a direct transition from `requested` to `healthy` is attempted
- **THEN** it is refused with a transition error and the target remains `requested`

#### Scenario: Legal step accepted

- **WHEN** a transition from `requested` to `cloning` is performed
- **THEN** the target status becomes `cloning`

#### Scenario: Deleting requires executable plan

- **WHEN** a transition from `excluded` to `deleting` is requested before its deletion deadline
- **THEN** it is refused and the target remains `excluded`

### Requirement: Terminal status is final

Once a target reaches `deleted`, no further status transition MUST be accepted. A target in `deleting` MAY advance only to `deleted` after every required deletion stage has terminal completion evidence.

#### Scenario: Transition out of terminal state

- **WHEN** any transition away from `deleted` is attempted
- **THEN** it is refused and the target remains `deleted`
