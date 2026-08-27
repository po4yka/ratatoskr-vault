## ADDED Requirements

### Requirement: LFS-enabled manifests cover every immutable LFS object

The existing schema-version-1 signed snapshot manifest SHALL carry an optional LFS evidence section. When present, it SHALL include the Git LFS tool version, canonical object count and total bytes, one sorted entry per referenced object containing its lowercase SHA-256 object identifier, size, and immutable BlobRef, plus a deterministic aggregate digest over the complete object set. `includes_lfs` MUST be true exactly when complete LFS evidence is present; a Git bundle alone MUST NOT satisfy that claim.

#### Scenario: LFS manifest is canonical

- **WHEN** an LFS-enabled fixture is collected and its object observations arrive in different orders
- **THEN** manifest serialization, object ordering, aggregate digest, and signature input are identical and cover every object exactly once

#### Scenario: Partial LFS set cannot produce complete manifest

- **WHEN** any referenced object lacks verified published bytes
- **THEN** Vault publishes no manifest claiming `includes_lfs=true` and retains the prior verified snapshot unchanged

### Requirement: Snapshot publication orders LFS evidence before the manifest

Vault SHALL create or reuse verified content-addressed LFS object blobs before signing and create-only publishing the manifest that references them. A retry after partial publication SHALL converge on the same object BlobRefs and create a new terminal attempt without overwriting an existing manifest or object.

#### Scenario: Manifest publication failure preserves reusable objects without a snapshot claim

- **WHEN** all LFS objects publish but manifest create-only publication fails
- **THEN** no new complete snapshot claim exists, the content-addressed objects remain verifiable and reusable, and a retry cannot overwrite prior immutable evidence
