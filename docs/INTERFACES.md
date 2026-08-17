# Vault interfaces

## Inbound

Versioned target desired-state, sync, snapshot, verify, restore-drill, pause/resume, retain/retire/delete commands; credential references from an approved secret boundary.

## Outbound

Target state, snapshot created/verified, restore succeeded/failed, degradation, retention, and deletion events plus safe operation progress.

## Internal boundaries

- `GitRunner`: structured executable/args/env/cwd/time/quota/cancellation result.
- `MirrorStore`: validated repository work area.
- `SnapshotBuilder`: immutable bundle/archive and manifest.
- `BlobStore`: local/off-host put/get/head/verify/delete.
- `RestoreVerifier`: isolated restore and semantic comparison.
- auxiliary collectors with explicit policy and credentials.

## Rules

Commands are idempotent by target/policy/version. Credentials are references, never event payloads. Failure classes distinguish auth, unavailable source, policy, quota, Git integrity, storage, verification, restore, and permanent deletion blocks. Physical deletion requires an approved tombstone/retention transition.
