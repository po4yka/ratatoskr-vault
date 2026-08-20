# Ratatoskr Vault Agent Instructions

## Scope

These instructions apply to the `ratatoskr-vault` repository.

This repository owns physical Git preservation: mirrors, Git LFS objects, immutable snapshots, manifests, integrity checks, retention, and verified restore workflows.

## Repository mission

Vault answers two separate questions:

1. Is the requested repository data durably stored according to policy?
2. Can the stored data be independently verified and restored?

A successful `git fetch` is not a successful backup. Backup success requires verified artifacts and a demonstrated restore path.

## Current phase

The repository is in architecture bootstrap. Do not assume Rust crates, workers, storage adapters, migrations, sandboxing, or CI commands exist unless they are present in the checkout.

When creating initial implementation:

- design the state machine and manifest before adding orchestration shortcuts;
- keep Git execution isolated behind a narrow runner;
- make verification and restore first-class states;
- avoid coupling Vault to GitHub star/list models.

### Development status

Ratatoskr is in development. No database holds data that has to survive a schema change. While this
status holds, these rules are binding, and they override anything else in this repository that
plans otherwise, including the rest of this file:

- **One version only.** The API, the database, and the contracts keep their first version. Do not
  add a `v2` or a later major version, and do not add version negotiation, deprecation windows, or
  parallel-major routing.
- **No database migrations.** Do not add a migration file, and do not add migration tooling. A
  schema change edits the current schema definition in place, and a test database is created from
  that definition.
- **The product is `Ratatoskr`.** It is not "Ratatoskr Next". Do not write that name in code,
  documentation, identifiers, comments, or commit messages.

Only the repository owner changes this status. Ask before you write anything these rules forbid.

## Sources of truth

Use this order:

1. active task/changeset, backup policy, and accepted ADRs;
2. `README.md`;
3. Vault contracts from `ratatoskr-contracts`;
4. immutable snapshot manifests and verification evidence;
5. repository tests and restore fixtures;
6. implementation details.

Catalog metadata is input to reconciliation, not proof of stored state. The verified manifest is the evidence for a specific snapshot.

## Hard bounded-context rules

### Vault owns

- backup target identity and source clone/fetch configuration;
- actual mirror/snapshot state;
- Git process execution records;
- local mirror and staging storage references;
- Git LFS acquisition state;
- immutable snapshot artifacts and manifests;
- integrity and bundle verification;
- restore drill execution and results;
- retention, tombstones, quarantine, and deletion workflow;
- off-host upload and verification state;
- Vault-specific outbox/inbox records.

### Vault does not own

- GitHub account tokens used for catalog synchronization;
- star, unstar, star-list, or repository mode semantics;
- the user's desired policy source of truth;
- LLM repository analysis;
- web/mobile/Telegram interaction state;
- generic BlobStore business ownership for unrelated services.

Consume complete desired backup state from `ratatoskr-github` or another explicit policy owner. Never infer user intent from missing stars or provider listing gaps.

## Desired-state reconciliation

Vault receives desired state such as:

```text
none
metadata_only
git_mirror
git_mirror_with_lfs
complete_archive
```

Optional policy may include:

- `pinned`;
- retention policy;
- wiki/releases/issues inclusion;
- offsite requirement;
- schedule or freshness target.

Rules:

- reconcile actual state toward the latest authorized desired state;
- persist desired-state version and correlation/idempotency metadata;
- make reconciliation repeatable;
- do not execute stale policy after a newer version is accepted;
- do not report healthy until all required policy components are verified;
- preserve explicit `pinned` targets against automatic unenrollment/deletion;
- treat policy `none` as an input to retention/deletion evaluation, not immediate destructive deletion.

## Target and snapshot state machines

State must be explicit and durable. Representative target states include:

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

Snapshot-specific states should distinguish:

```text
building
built
verifying
verified
offsite_uploading
offsite_verified
restore_testing
restorable
failed
quarantined
expired
deleted
```

Rules:

- transitions are idempotent;
- retries do not create ambiguous duplicate artifacts;
- terminal evidence is immutable;
- cancellation leaves diagnosable state and cleans only owned temporary resources;
- failed verification cannot be converted to success by metadata update alone;
- a target may remain available from a prior verified snapshot while a new sync is degraded;
- progress events cannot regress from stale delivery.

## Git CLI execution

Use the system Git CLI through `tokio::process::Command` or an equivalent direct argument API.

Mandatory rules:

- never invoke Git through a shell command string;
- allowlist binaries and subcommands;
- pass arguments as distinct values;
- sanitize environment variables;
- set explicit working directories;
- disable hooks and unsafe inherited configuration;
- use a dedicated unprivileged OS user/container;
- enforce process, wall-clock, CPU, memory, disk, and output limits;
- manage process groups so cancellation terminates descendants;
- capture bounded stdout/stderr and structured exit diagnostics;
- validate all paths before use;
- never run repository-provided scripts, hooks, build tools, or checkout code as part of backup.

Do not replace the primary backup path with `libgit2` merely for aesthetic purity. Any alternate implementation must prove equivalent refs, bundle, LFS, verification, and restore semantics.

## Untrusted repository model

Every remote repository is hostile input.

- Repository names and refs cannot define arbitrary filesystem paths.
- Symlinks, alternates, submodules, worktrees, hooks, config includes, and external filters are untrusted.
- Never checkout a working tree unless a reviewed collector explicitly requires it.
- Do not execute clean/smudge filters.
- Disable credential helpers that can expose unrelated secrets.
- Restrict network egress to required source/offsite endpoints.
- Keep staging and restore tests in isolated temporary roots.
- Prevent repository content from escaping its allocated storage quota.
- Treat Git, archive, release, issue, and LFS metadata as untrusted strings.

## Mirror workflow

A Git mirror flow should use explicit steps with evidence:

1. create or adopt a mirror;
2. configure the expected remote safely;
3. fetch/update all intended refs;
4. prune only according to documented mirror policy;
5. record refs/object statistics and source request metadata;
6. run integrity checks;
7. build immutable snapshots;
8. verify snapshot artifacts;
9. upload/verify offsite copies when required;
10. execute restore drills according to policy.

Typical commands may include `git clone --mirror`, `git remote update --prune`, and `git fsck --full`, but exact command plans must be represented as safe typed operations rather than arbitrary user strings.

Adopting an existing mirror requires identity, remote, ownership, layout, and integrity validation before it becomes trusted state.

## Snapshot rules

- Snapshots are immutable after creation.
- Artifact names/keys are content-addressed or otherwise collision-safe.
- Build in a temporary location and publish atomically after successful construction.
- Compute cryptographic hashes while streaming where practical.
- Record tool versions and command outcomes.
- Never overwrite the only verified snapshot with an unverified replacement.
- Keep full snapshots as the default until incremental-chain complexity is justified by evidence.
- If incremental bundles are introduced, record prerequisite chains and test complete chain restore.

A local bare mirror alone is not an off-host backup.

## Git bundle verification

For bundle-based snapshots:

- create from the intended refs, commonly `--all` for a full snapshot;
- run `git bundle verify`;
- record prerequisites, refs, object counts, size, and hash;
- verify the artifact again after offsite transfer;
- test cloning/fetching from the artifact in an isolated destination;
- ensure the restored repository contains the expected refs.

A zero exit code from artifact upload does not prove the uploaded bytes match the local artifact.

## Git LFS

Git object storage and Git LFS are separate backup layers.

When policy includes LFS:

- detect LFS usage through safe repository metadata;
- run `git lfs fetch --all` in a confined environment when supported;
- record LFS client version, object count, total bytes, missing/corrupt objects, and artifact references;
- store/verify LFS objects separately or in a documented complete archive;
- test restore with LFS availability;
- do not mark `git_mirror_with_lfs` healthy when only pointer files are preserved.

Missing LFS must produce a degraded/partial result, not silent success.

## Complete archive collectors

A complete GitHub archive may include separate collectors for:

- Git refs/history;
- Git LFS;
- wiki repository;
- releases and release assets;
- issues/comments;
- pull requests;
- discussions;
- repository settings.

Each collector has its own contract, authentication, completeness, artifact, verification, and retention status.

Do not imply that `git clone --mirror` preserves all GitHub product data.

Provider API collection credentials must be passed through a narrowly scoped secure mechanism and not reused as Vault-owned catalog credentials.

## Snapshot manifest

Every snapshot manifest should contain enough immutable evidence to identify and verify it, including where applicable:

- internal target and provider repository IDs;
- observed source URL/remote identity;
- desired policy version;
- created/completed timestamps;
- Git and LFS tool versions;
- ref-set digest and expected refs;
- artifact hash, size, object count, and storage location reference;
- LFS/wiki/collector inclusion flags and results;
- `fsck` result;
- bundle verification result;
- offsite verification result;
- restore drill result;
- source snapshot/sync correlation IDs;
- manifest schema/version.

Manifests are append-only/immutable evidence. Corrections create a new revision that references the prior manifest; they do not rewrite history.

## Restore drills

Restore verification is a product requirement.

A restore drill must:

- use the stored artifact, not the live source mirror;
- run in an isolated empty destination;
- verify artifact hash before use;
- reconstruct the repository according to the documented procedure;
- verify expected refs and object connectivity;
- verify LFS and other collectors when required;
- produce bounded logs and a durable result;
- clean temporary data without deleting the source snapshot;
- fail explicitly when prerequisites are missing.

Test drills periodically, after material tool/format changes, and before claiming a new backup implementation is production-ready.

## Retention, tombstones, and deletion

Unstar or target deactivation is not immediate deletion.

Expected flow:

```text
policy becomes inactive
  -> grace period
  -> tombstone/retention evaluation
  -> preserve required verified snapshots
  -> explicit deletion eligibility
  -> staged deletion
  -> deletion verification/audit
```

Rules:

- `pinned=true` blocks automatic deletion;
- retention uses policy and verified snapshot state, not only last fetch time;
- failed/degraded targets do not lose the last healthy snapshot during routine cleanup;
- offsite and local deletion states are tracked separately;
- destructive deletion is idempotent, authorized, auditable, and recoverable within any configured grace window;
- never follow untrusted symlinks or paths during cleanup;
- quarantine suspicious/corrupt artifacts instead of deleting evidence automatically.

## Storage adapters

Storage operations use a narrow interface and stable blob/artifact references.

- Local filesystem is a supported self-hosted backend.
- S3-compatible/offsite storage is a separate adapter.
- Writes are atomic or publish-after-verify.
- Reads verify expected identity/hash where applicable.
- Multipart/resumable uploads preserve final integrity evidence.
- Storage keys never include unchecked repository names or secrets.
- Access policies are least privilege.
- Lifecycle policies must not delete artifacts earlier than Vault retention state.

Do not assume eventual consistency cannot affect immediately following verification.

## Concurrency and scheduling

- Bound global and per-host/source concurrency.
- Serialize conflicting operations on the same target.
- Use leases with expiry/renewal for long operations.
- Ensure a crashed worker can be safely recovered or reaped.
- Apply disk reservation/quota checks before cloning or snapshotting.
- Provide backpressure rather than starting unbounded Git processes.
- Scheduler requests work; Vault owns execution timing, retries, and provider/source constraints.

## Persistence and migrations

Vault writes only its owned schema.

Conceptual data includes:

```text
vault_targets
vault_desired_states
vault_mirrors
vault_runs
vault_snapshots
vault_artifacts
vault_manifests
vault_verifications
vault_restore_drills
vault_retention_actions
vault_tombstones
vault_outbox
vault_inbox
```

No cross-schema writes or foreign keys. Preserve verification and audit history through migrations.

Schema changes that affect artifact interpretation require manifest/version compatibility and restore testing.

## Commands and events

Representative messages include:

```text
vault.target.desired.v1
vault.sync.requested.v1
vault.target.state_changed.v1
vault.snapshot.created.v1
vault.snapshot.verified.v1
vault.restore.completed.v1
vault.restore.failed.v1
```

Use transactional outbox, inbox deduplication, correlation/causation IDs, and at-least-once-safe handlers.

Do not publish `verified` or `restorable` until the corresponding evidence exists.

## Credentials and security

- Use narrowly scoped credentials for source clone/API collectors and offsite storage.
- Encrypt credentials and avoid persisting short-lived tokens longer than needed.
- Never place credentials in remote URLs stored in logs/manifests.
- Redact command output before user display.
- Protect local mirrors and artifacts with restrictive filesystem permissions.
- Separate service runtime, worker, and restore-test identities when practical.
- Do not allow arbitrary command, path, URL, or environment input from public clients.
- Audit credential changes and destructive retention actions.

## Observability

Required telemetry should cover:

- target/run/snapshot state transitions;
- clone/fetch/snapshot/verification/restore durations;
- bytes, objects, refs, and LFS counts;
- Git exit/failure classes;
- disk usage/reservation/quota pressure;
- worker leases and queue lag;
- offsite upload and verification;
- restore success rate and age of last successful drill;
- retention/deletion actions;
- correlation and target IDs without leaking credentials or private names in metric labels.

Logs are diagnostic evidence, not the source of verified state.

## Testing expectations

When implementation exists, include applicable tests for:

- typed Git command construction and shell-injection resistance;
- path traversal, symlink, config, hook, filter, and hostile repository cases;
- target/snapshot state-machine idempotency;
- lease/crash recovery;
- mirror adoption and remote validation;
- ref completeness and prune policy;
- `fsck`/bundle verification failures;
- LFS complete/partial/missing scenarios;
- atomic artifact publication and hash verification;
- offsite corruption or truncated upload;
- restore from artifact without source access;
- retention, pin precedence, grace period, and deletion safety;
- disk quota and cancellation behavior;
- migrations and manifest compatibility;
- outbox/inbox replay.

Use synthetic repositories and local fixture servers. Never rely on personal production repositories or credentials in normal tests.

## Cross-repository change rules

Use a workspace changeset when changing:

- desired backup policy contracts from GitHub Catalog;
- Vault status/result contracts consumed by Platform, Telegram, web, or mobile;
- artifact/blob interfaces;
- manifest schema;
- deployment isolation, filesystem mounts, or offsite requirements;
- migration/adoption of legacy mirrors;
- retention semantics.

The changeset must include rollout, rollback, compatibility, storage migration, and restore evidence requirements.

## Git and PR workflow

- Separate destructive retention changes from unrelated refactors.
- Include restore evidence for backup-format or Git-command changes.
- Document resource, storage, and credential impact.
- Do not add star/list/catalog business logic.
- Do not accept arbitrary shell commands or repository-provided execution.
- Do not commit private repository data, mirrors, bundles, credentials, or production manifests.
- Record security assumptions and failure modes.
- Update README/ADRs when verification, retention, or ownership changes.

## Completion criteria

A task is complete only when:

- responsibility belongs to Vault;
- desired-state reconciliation is idempotent and version-aware;
- Git execution is direct, allowlisted, isolated, bounded, and non-shell;
- artifacts are immutable and cryptographically identified;
- Git/LFS/collector completeness is reported truthfully;
- verification and restore evidence support every success claim;
- last healthy snapshots survive degraded refreshes;
- retention/deletion respects pinning, grace, audit, and path safety;
- storage/offsite integrity is validated;
- relevant hostile-input, state-machine, and restore tests pass;
- contracts, migrations, telemetry, and cross-repository rollout are documented.
