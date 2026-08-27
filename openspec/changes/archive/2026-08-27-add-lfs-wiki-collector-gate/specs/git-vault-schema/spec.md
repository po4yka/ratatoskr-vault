## ADDED Requirements

### Requirement: Current schema persists LFS collection and restore evidence

The single current `schema.sql` SHALL persist append-only LFS collection runs, each verified LFS object's SHA-256 identifier, byte length and BlobRef, deterministic aggregate evidence, quota outcome, tool version, and LFS restore proof fields. Constraints SHALL prevent duplicate object identities within one snapshot, negative counts or sizes, complete outcomes with missing object evidence, and mutation of terminal results.

#### Scenario: Fresh schema records complete LFS evidence

- **WHEN** the current schema is applied to a disposable database and a complete fixture collection and restore report are persisted
- **THEN** the snapshot resolves to exactly its verified object set and immutable terminal collection and restore evidence

#### Scenario: Inconsistent complete result is refused

- **WHEN** a write claims complete LFS collection with an object count or aggregate digest that does not match its linked object evidence
- **THEN** the transaction fails and no complete result remains

### Requirement: Current schema relates wiki siblings without provider path ownership

The current schema SHALL distinguish repository and wiki targets and relate each wiki target to exactly one parent repository target. Provider external identity and relationship kind SHALL be constrained so repeated discovery cannot create a second wiki sibling, and neither provider names nor source URLs SHALL become storage paths.

#### Scenario: Repeated wiki enrollment converges

- **WHEN** the same parent and wiki identity are enrolled twice in a disposable database
- **THEN** one wiki target and one parent relationship remain, and its storage path is derived only from the internal wiki target identifier

### Requirement: Executable collector vocabulary is approval closed

The current schema SHALL accept terminal collector evidence only for Git LFS and wiki. Provider auxiliary kinds that have no separately owner-approved implementation change, including releases and issues, MUST be rejected by the collector-kind constraint.

#### Scenario: Unapproved collector evidence is rejected

- **WHEN** a write attempts to persist a `releases` or `issues` collector run
- **THEN** the database rejects it while accepting independently valid `git_lfs` and `wiki` evidence
