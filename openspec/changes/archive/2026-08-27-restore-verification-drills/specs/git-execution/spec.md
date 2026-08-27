## ADDED Requirements

### Requirement: Typed bundle verification and artifact-only restore operations

The Git runner SHALL expose bundle verification, bare restore from a bundle, complete ref
enumeration, and object-connectivity checking only as typed allowlisted direct-argument operations.
Restore inputs and destinations MUST be confined to the immutable artifact and drill scratch roots,
the live mirror root MUST be an explicit denied root, and the child environment MUST disable every
network transport, credential helper, hook, filter, and inherited Git configuration.

#### Scenario: Restore command cannot contact a remote

- **WHEN** a stored bundle contains repository configuration or refs shaped to request a network transport during restore verification
- **THEN** the typed drill operations use only the local artifact, no network connection is attempted, and repository-controlled hooks or filters do not execute

#### Scenario: Live mirror path is denied before spawn

- **WHEN** a typed restore operation names a source or destination resolving within the live mirror root
- **THEN** the runner returns a confinement error before a child starts and the mirror remains byte-for-byte unchanged
