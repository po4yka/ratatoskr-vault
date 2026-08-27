## Purpose

Preserves explicitly requested Git LFS content and repository wikis with independently verifiable evidence while keeping every later provider API collector disabled until the owner approves a separate change.

## ADDED Requirements

### Requirement: Git LFS collection is explicit and truthful

Vault SHALL collect Git LFS only when the governing target preservation level requires LFS. For an enabled target, Vault MUST distinguish no LFS usage, complete collection, missing or corrupt objects, quota refusal, tool unavailability, and other collection failure; it MUST NOT report the target healthy when required LFS content is incomplete. A target without LFS enablement SHALL perform no Git LFS process or LFS artifact publication.

#### Scenario: Enabled fixture collects every LFS object

- **WHEN** a target governed by `git_mirror_with_lfs` mirrors a fixture whose reachable refs contain Git LFS pointers
- **THEN** the collection evidence is complete and names every referenced LFS object exactly once

#### Scenario: Disabled target does not invoke LFS

- **WHEN** the same fixture is mirrored under `git_mirror`
- **THEN** no Git LFS process runs, no LFS artifact is published, and the snapshot does not claim LFS inclusion

#### Scenario: Missing required object degrades honestly

- **WHEN** an enabled fixture references an LFS object that the confined source cannot supply
- **THEN** the run records an incomplete LFS failure, the target is degraded, and no LFS-complete snapshot claim is published

### Requirement: LFS object evidence is immutable and content addressed

Every collected LFS object SHALL be verified against its lowercase SHA-256 object identifier before publication to Vault-owned content-addressed storage. Collection evidence SHALL record the canonical sorted object set, each object's digest and byte length, total bytes, tool version, and one aggregate digest. Retries MAY reuse identical stored objects but MUST append new run evidence rather than rewrite a terminal result.

#### Scenario: Repeated collection has stable content evidence

- **WHEN** the same unchanged LFS-enabled fixture is collected twice
- **THEN** both terminal runs report the same sorted object set and aggregate digest while stored object identities remain content addressed

#### Scenario: Object bytes disagree with pointer identifier

- **WHEN** staged LFS bytes do not hash to the object identifier referenced by Git history
- **THEN** publication is refused and the terminal collection evidence identifies corruption without replacing prior verified objects

### Requirement: Wiki discovery creates an independently evidenced sibling target

When the governing policy enables wiki preservation, Vault SHALL derive the repository's wiki Git source without using provider catalog metadata and perform a bounded discovery probe. A present wiki SHALL reconcile as a distinct sibling target related to the parent repository, using its own identifier-derived mirror path, lifecycle runs, quota, snapshots, verification, and health. A confirmed absent wiki SHALL be recorded as absent without inventing a target; authentication, transport, or ambiguous probe failures MUST remain failures rather than being treated as absence.

#### Scenario: Present wiki becomes a sibling target

- **WHEN** wiki discovery probes a fixture source with a non-empty sibling wiki repository
- **THEN** exactly one related wiki target is enrolled and repeated discovery converges on that same target

#### Scenario: Confirmed missing wiki stays absent

- **WHEN** wiki discovery receives the typed confirmed-absent result for an enabled parent
- **THEN** durable discovery evidence records absence and no wiki target, mirror, or snapshot is created

#### Scenario: Discovery failure is not absence

- **WHEN** the wiki probe fails because of authentication, timeout, or an unclassified remote error
- **THEN** Vault records the typed failure, does not create a sibling target, and does not report the wiki requirement satisfied

### Requirement: Future provider collectors require separate owner approval

The executable auxiliary-collector registry SHALL be closed and contain only collector kinds approved by a named OpenSpec change. Adding issues, releases, release assets, pull requests, discussions, settings, or another provider API collector MUST occur in a separately owner-approved change that defines its credential boundary, completeness contract, immutable artifacts, quota behavior, verification, restore/export evidence, and failure semantics. Policy flags for an unimplemented collector MUST remain non-executable and MUST NOT trigger credential access or provider network calls.

#### Scenario: Unapproved provider collectors are inert

- **WHEN** the current executable collector inventory and schema vocabulary are checked after this change
- **THEN** Git LFS and wiki are the only approved collector kinds, and releases and issues requests are refused as unsupported before any credential or network access
