# Ratatoskr Vault

`ratatoskr-vault` is the durable backup and restore bounded context for Ratatoskr. It converges desired repository-backup policies into verified Git mirrors, immutable snapshots, content manifests, off-host copies, and repeatable restore drills.

> **Status:** service scaffold implemented. A Rust workspace runs the `ratatoskr-vault` deployable with typed configuration, structured telemetry, an operator health plane (`/health/live`, `/health/ready`, `/metrics`, `/version`), and the first version of the `git_vault` PostgreSQL schema (`schema.sql`, applied in place — no migrations). No mirror worker, Git command runner, snapshot format, storage backend, or restore verifier exists yet; those are implementation plan items 2–10 in `docs/IMPLEMENTATION_PLAN.md`.

> [!IMPORTANT]
> **Ratatoskr is in development.** No database holds data that has to survive a schema change.
> While this status holds, these two rules replace what the documents below plan:
>
> - the API and the database keep their first version. There is no `v2` and no later major
>   version.
> - the database has no migrations. One schema definition exists, and a schema change edits it in
>   place.
>
> Only the repository owner changes this status.

## Role in Ratatoskr

GitHub Catalog decides **what should be preserved**. Git Vault proves **what has actually been preserved and can be restored**.

Vault owns:

- Git mirror enrollment and lifecycle;
- initial clone and periodic fetch;
- all-ref preservation;
- Git LFS collection where requested;
- immutable bundle or archive snapshots;
- cryptographic manifests;
- filesystem and off-host BlobStore placement;
- integrity verification;
- retention, tombstones, and pruning;
- restore drills and recovery reporting;
- operational health and storage metrics.

Vault does not own GitHub stars, star lists, repository analysis, or user-facing catalog semantics. It consumes desired-state contracts from `ratatoskr-github` and reports actual state through versioned events.

## Desired state and actual state

A backup target arrives as a policy such as:

```text
none
metadata_only
git_mirror
git_mirror_with_lfs
complete_archive
```

Additional attributes may include:

```text
pinned
retention_policy
include_wiki
include_releases
include_issues
offsite_required
```

Vault independently reconciles that policy with actual storage. A successful policy write does not imply that a mirror or off-host snapshot already exists; clients inspect convergence and verification state separately.

## Target state machine

Planned target states:

```text
requested
cloning
ready
fetching
snapshotting
verifying
healthy
degraded
paused
excluded
deleting
```

Every transition is explicit and auditable. Retry attempts, backoff, last successful fetch, last verified snapshot, and last restore drill are stored independently rather than inferred from nullable fields.

## Git execution model

The primary implementation uses the system Git CLI through `tokio::process::Command` rather than reimplementing backup semantics in an embedded Git library.

Execution requirements:

- no shell interpolation;
- allowlisted commands and arguments;
- sanitized environment;
- dedicated Unix user;
- disabled hooks and user-level Git configuration;
- bounded wall-clock time;
- process-group cancellation;
- disk, CPU, memory, and network limits;
- path canonicalization and root confinement;
- structured stdout/stderr capture with secret redaction.

A typical mirror lifecycle uses:

```text
git clone --mirror
git remote update --prune
git fsck --full
git bundle create --all
git bundle verify
```

Git LFS is collected explicitly when policy requires it. An ordinary Git mirror or bundle must not be mislabeled as a complete LFS backup.

## Snapshot architecture

The recommended initial strategy favors full, immutable snapshots over complex incremental chains:

1. update the bare mirror;
2. verify the object database;
3. capture all refs;
4. create a portable snapshot;
5. calculate cryptographic hashes;
6. write a manifest;
7. upload to the configured BlobStore;
8. verify the uploaded artifact;
9. periodically restore into an isolated temporary repository.

Incremental bundles may be introduced only after measured storage pressure justifies the additional restore complexity.

### Snapshot manifest

```json
{
  "repository_id": "github:123456",
  "source_url": "https://github.com/owner/repository",
  "created_at": "2026-08-17T10:00:00Z",
  "git_version": "...",
  "refs_hash": "...",
  "artifact_sha256": "...",
  "artifact_size": 123456789,
  "object_count": 12345,
  "includes_lfs": true,
  "includes_wiki": false,
  "fsck_result": "ok",
  "bundle_verify_result": "ok",
  "restore_result": "ok"
}
```

A backup is not considered healthy merely because `git fetch` returned zero. The latest required artifact must exist, match its manifest, pass format verification, and satisfy the configured restore policy.

## Complete archive collectors

`complete_archive` is a policy composition rather than one Git command. Depending on configuration it may preserve:

- Git refs and object history;
- Git LFS objects;
- repository wiki as a separate Git repository;
- releases and release assets;
- issues and comments;
- pull-request metadata;
- discussions;
- selected repository settings;
- portable metadata exports.

Each collector records independent completeness and warning status. Partial success is preserved honestly; a missing release asset does not erase a successfully verified Git snapshot.

## Data ownership

Vault owns a `git_vault.*` PostgreSQL schema. Expected tables include:

```text
backup_targets
backup_policies_observed
mirrors
mirror_attempts
snapshots
snapshot_artifacts
snapshot_manifests
verification_runs
restore_drills
retention_actions
storage_locations
outbox_events
inbox_events
```

Large mirrors live on a dedicated filesystem volume. Immutable snapshots and manifests use the shared content-addressed BlobStore under Vault-owned references. Database records never act as a substitute for checking that an artifact exists and verifies.

## Storage backends

Initial backends:

- local filesystem for active bare mirrors;
- local content-addressed BlobStore for snapshots;
- S3-compatible storage for off-host copies.

Future adapters may include immutable or WORM storage. Every backend reports integrity, capacity, latency, and reachability separately.

## Retention and deletion

An upstream unstar does not automatically destroy a backup.

```text
unstar observed
  -> desired policy reevaluated
  -> target becomes inactive when appropriate
  -> grace period
  -> tombstone/excluded state
  -> verified snapshots retained
  -> physical deletion only by explicit retention policy
```

`pinned=true` always overrides automatic release caused by star-state changes. This protects repositories that were explicitly tracked and later happened to be starred and unstarred.

Physical deletion is multi-stage:

1. mark deletion intent;
2. verify policy and grace period;
3. remove references from active rotation;
4. safely remove confined local paths;
5. expire off-host objects according to policy;
6. retain an audit tombstone;
7. verify that no required snapshot was unintentionally removed.

## Restore drills

Restore is a first-class scheduled operation, not a manual emergency-only script.

A drill may:

- download the selected snapshot from the target storage location;
- verify SHA-256 and manifest metadata;
- run `git bundle verify` or equivalent format checks;
- clone or unbundle into an isolated temporary directory;
- verify expected refs;
- run `git fsck` on the restored repository;
- confirm LFS availability where required;
- optionally build or inspect repository-specific smoke targets;
- destroy the temporary workspace after recording results.

The system reports restore age and failure state separately from fetch age.

## Commands and events

Expected contracts include:

```text
vault.target.desired.v1
vault.target.reconcile_requested.v1
vault.mirror.updated.v1
vault.snapshot.created.v1
vault.snapshot.verified.v1
vault.snapshot.failed.v1
vault.restore_drill.requested.v1
vault.restore_drill.completed.v1
vault.target.degraded.v1
vault.target.excluded.v1
```

Commands and events are idempotent. A duplicate desired-state event reconciles the same target rather than enrolling a second mirror.

## Failure handling

Failures are classified as:

- authentication or authorization;
- repository unavailable or deleted;
- rate-limited;
- transient network;
- storage capacity;
- local filesystem corruption;
- Git object corruption;
- LFS incomplete;
- snapshot upload or verification failure;
- restore failure;
- unsafe path or policy rejection.

Per-target retries use bounded exponential backoff and circuit breaking. One damaged repository never aborts the entire reconciliation run.

## Security invariants

1. Git commands execute without a shell under a dedicated identity.
2. Hooks and untrusted repository configuration are not executed.
3. All paths must resolve inside configured storage roots.
4. Provider credentials are supplied only for the scoped operation and never persisted in command output.
5. Snapshot manifests and hashes are immutable evidence.
6. `pinned` targets are never automatically pruned.
7. Off-host storage credentials remain inside Vault.
8. Restore drills use isolated disposable directories.
9. A successful fetch cannot overwrite evidence of an unverified or failed snapshot.
10. Deletion requires explicit policy convergence and a retained audit trail.

## Observability

Core metrics include:

```text
vault_targets_by_state
vault_sync_duration
vault_sync_failures
vault_mirror_age
vault_snapshot_age
vault_snapshot_bytes
vault_verification_failures
vault_restore_age
vault_restore_failures
vault_storage_bytes
vault_storage_free_bytes
vault_lfs_incomplete
vault_pruned_artifacts
```

Structured logs and traces include target ID, operation ID, attempt ID, command class, storage backend, and result classification, but never embedded credentials.

## Non-goals

- GitHub OAuth, stars, lists, or catalog UI.
- Repository LLM analysis or semantic search.
- General-purpose arbitrary shell execution.
- Treating a local bare mirror as sufficient disaster recovery.
- Automatic deletion immediately after upstream removal.
- Claiming complete GitHub backup when only Git objects were preserved.

## Initial milestones

1. Define target, mirror, snapshot, and verification schemas.
2. Implement the confined Git command runner.
3. Support initial `git clone --mirror` and periodic updates.
4. Add `git fsck` and full bundle snapshots.
5. Add content-addressed local storage and manifests.
6. Add S3-compatible off-host upload and verification.
7. Add LFS and wiki collectors.
8. Add scheduled restore drills.
9. Import and adopt existing legacy mirrors without unnecessary recloning.
10. Add retention, tombstones, and safe pruning.

## Workspace integration

`ratatoskr-workspace` pins Vault with compatible desired-state contracts and GitHub Catalog changes. System integration tests must include real temporary repositories, corruption fixtures, off-host emulation, and verified restore scenarios. Vault remains independently operable if the public Edge service is unavailable.

## Project status

The service scaffold (implementation plan item 1) is implemented: `crates/{core,telemetry,persistence,http}` and `services/vault` per the layout in `docs/ARCHITECTURE.md` section 3. The binary loads strict typed configuration from the environment, installs tracing with optional OTLP export, serves the operator health plane on the admin listener, applies `schema.sql` to a fresh database, and stops gracefully on SIGTERM. The repository gate is `.github/workflows/ci.yml`; `DEVELOPMENT.md` documents the identical command list.

Not yet implemented: desired-state reconciliation and state machines (plan item 2), the confined Git runner (3), mirror lifecycle (4), snapshots and manifests (5), verification and restore drills (6), off-host replicas (7), LFS and auxiliary collectors (8), retention and deletion (9), legacy adoption (10). The schema carries their tables as first-version placeholders; no code claims capabilities they do not have.
