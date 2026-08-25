# Ratatoskr Vault

`ratatoskr-vault` is the durable backup and restore bounded context for Ratatoskr. It converges desired repository-backup policies into verified Git mirrors, immutable snapshots, content manifests, off-host copies, and repeatable restore drills.

> **Status:** foundation and confined Git execution implemented. A Rust workspace runs the `ratatoskr-vault` deployable with typed configuration, structured telemetry, an operator health plane (`/health/live`, `/health/ready`, `/metrics`, `/version`), the first version of the `git_vault` PostgreSQL schema (`schema.sql`, applied in place — no migrations), desired-state reconciliation with a guarded target state machine, and `crates/gitrunner`, the confined system-Git runner with its generated hostile-repository suite. No mirror worker, snapshot format, storage backend, or restore verifier exists yet; those are implementation plan items 4–10 in `docs/IMPLEMENTATION_PLAN.md`.

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

Implemented in `crates/gitrunner` (`ratatoskr-vault-gitrunner`). The primary implementation uses the system Git CLI through `tokio::process::Command` rather than reimplementing backup semantics in an embedded Git library.

Execution requirements, and where each one lives today:

- no shell interpolation — structural: operations are typed argument vectors; there is no shell on the path from intent to `exec`;
- allowlisted commands and arguments — a closed `Subcommand` enum gates every spawn before any process starts;
- sanitized environment — children receive a constructed environment (no inheritance), with system/global Git configuration excluded;
- dedicated Unix user — deployment concern (container/user isolation), outside the library's scope;
- disabled hooks and user-level Git configuration — `-c core.hooksPath=/dev/null` plus `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_SYSTEM=/dev/null`;
- bounded wall-clock time — per-invocation deadline; overruns SIGKILL the child's whole process group through safe `nix` wrappers;
- process-group cancellation — every child leads its own process group (`process_group(0)`), armed with a kill-on-drop guard;
- disk, CPU, memory, and network limits — planned with mirror lifecycle quotas (plan item 4);
- path canonicalization and root confinement — canonicalize-at-use validation against Vault-owned roots; intermediate symlinks leaving a root are refused; mirror paths derive from internal identifiers only;
- structured stdout/stderr capture with secret redaction — per-stream caps with overflow kills, and captured output scanned against active credential material before it leaves the runner.

Source URLs are validated before use: only `https` and `file` transports are allowed, and option-shaped strings (leading `-`) are refused. Credentials reach Git exclusively through the Git credential-helper contract: the shipped `git-credential-helper` binary reads an owner-only secret file inside an owner-only run directory; secrets never appear in argv or environment, and are deleted when the operation ends.

A typical mirror lifecycle uses:

```text
git clone --mirror
git remote update --prune
git fsck --full
git bundle create --all
git bundle verify
```

The runner implements clone-mirror, fetch-all, fsck-full, rev-list, and show-ref today; remote update --prune and the bundle verbs arrive with mirror lifecycle and snapshots. A failing `fsck` surfaces as a typed integrity failure carrying a bounded diagnostic excerpt, never as a bare exit code.

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

The implemented foundation exports:

```text
vault_build_info
vault_readiness
```

Target, sync, mirror, snapshot, verification, restore, storage, LFS, and retention metrics remain
planned with the capabilities that would produce them.

Future target, operation, attempt, command, storage, and result spans will include their identifiers
and classifications, but never embedded credentials. The implemented foundation currently records
process lifecycle and startup or shutdown context.

## Non-goals

- GitHub OAuth, stars, lists, or catalog UI.
- Repository LLM analysis or semantic search.
- General-purpose arbitrary shell execution.
- Treating a local bare mirror as sufficient disaster recovery.
- Automatic deletion immediately after upstream removal.
- Claiming complete GitHub backup when only Git objects were preserved.

## Implementation plan

The authoritative sequence is [`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md). Items 1–3 (service foundation, desired-state reconciliation, confined Git runner with hostile-repository suite) are implemented. Items 4 through 10 remain planned.

## Workspace integration

The planned `ratatoskr-workspace` topology will pin Vault with compatible desired-state contracts and
GitHub Catalog changes. No workspace pin or GitHub-to-Vault integration profile exists yet. Future
system integration tests must include real temporary repositories, corruption fixtures, off-host
emulation, and verified restore scenarios. Vault remains independently operable if the public Edge
service is unavailable.

## Project status

Implemented so far: the service foundation (plan item 1: `crates/{core,telemetry,persistence,http}`, `services/vault`, typed configuration, tracing with optional OTLP export, the operator health plane, `schema.sql` applied to a fresh database, graceful SIGTERM shutdown); desired-state reconciliation (plan item 2: delivery validation, deduplicated ingestion, the guarded target state machine, a pure planner, transactional convergence); and the confined Git runner (plan item 3: `crates/gitrunner` with structural command construction, subcommand allowlisting, filesystem confinement, hardened child environments, wall-clock deadlines, output caps, process-group kills, out-of-band credentials via `git-credential-helper`, redacted results, and the deterministic hostile-repository test suite). The repository gate is `.github/workflows/ci.yml`; `DEVELOPMENT.md` documents the identical command list.

Not yet implemented: mirror lifecycle (4), snapshots and manifests (5), verification and restore drills (6), off-host replicas (7), LFS and auxiliary collectors (8), retention and deletion (9), legacy adoption (10). The schema carries their tables as first-version placeholders; no code claims capabilities they do not have.
