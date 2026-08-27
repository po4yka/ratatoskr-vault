## Context

See `proposal.md` for motivation. The current service already has typed confined Git execution, quota-admitted mirror lifecycle runs, immutable local BlobStore artifacts, signed schema-version-1 manifests, off-host replication, and artifact-only restore drills. Git bundles intentionally omit LFS bytes, and a GitHub wiki is a separate Git repository. Existing desired state already carries `git_mirror_with_lfs` and `include_wiki`, while `collector_runs` and several auxiliary names are only forward-looking schema placeholders with no executable collector.

The design must preserve the development rules: edit the single schema in place, keep the first manifest/API/database version, update every caller directly, and introduce no compatibility path. Repository content, LFS pointer text, remote diagnostics, and wiki names remain hostile input. The host currently provides `git-lfs`; deployment must configure its absolute path explicitly.

## Goals / Non-Goals

**Goals:**

- Make LFS an independently evidenced but mandatory component of an explicitly LFS-enabled mirror/snapshot/restore lifecycle.
- Reuse content-addressed BlobStore objects directly instead of inventing an LFS archive format.
- Model a present wiki as a normal independently operated target related to its parent.
- Make quota, cancellation, corruption, partial collection, and absent/failing wiki results durable and truthful.
- Leave a closed, testable registration point for a later owner-approved provider collector.

**Non-Goals:**

- Provider API collection, provider metadata synchronization, releases, release assets, issues, pull requests, discussions, settings, or their credentials.
- Working-tree checkout, LFS smudge/clean filters, arbitrary Git/LFS arguments, or repository-provided code execution.
- A new manifest major version, migrations, archive compatibility, incremental LFS snapshots, retention/deletion, or remote bucket lifecycle changes.
- Replacing the existing Git bundle, BlobStore, replica, or restore architecture.

## Decisions

### D1: One closed collector vocabulary, with provider collectors absent

Add a core `CollectorKind` vocabulary containing only `GitLfs` and `Wiki`, plus typed terminal completeness/failure evidence. Persistence constraints and an inventory test use that same exact set. Desired `include_releases` or `include_issues` flags remain accepted as upstream policy input, but planning reports an explicit unsupported-collector divergence and executes no provider credential or network path.

Adding a provider collector requires a later owner-approved OpenSpec change to extend the enum, schema constraint, planner, credential boundary, artifact model, verification/restore or export evidence, quota tests, and documentation together. A documentation-only registry or a schema placeholder with no positive capability is insufficient.

Alternative considered: retain every planned collector name in the schema. Rejected because it makes an unapproved kind look executable and defeats the requested approval gate.

### D2: Reuse the process sandbox through a dedicated typed LFS runner

`crates/gitrunner` gains a dedicated `LfsRunner`/`LfsOperation` surface rather than accepting a raw executable or adding free-form arguments to `GitOperation`. Its configured binary is one absolute `git-lfs` path; operations are limited to version, fetch-all, list-all-long, and fsck-objects. The existing spawn/process-group/output/redaction machinery is shared, while the environment builder gains only fixed runner-owned Git configuration entries needed to select the source bare repository, run-owned `lfs.storage`, disabled hooks/filters/prompts, file/HTTPS protocol policy, and the existing credential helper. No repository config is mutated.

The runner operates against a bare repository and never checks out a tree. `list-all-long` output is parsed as untrusted bounded data. Object paths are derived only from validated lowercase 64-hex OIDs under the run-owned LFS staging root.

Alternative considered: run `git lfs ...` through the existing Git subcommand enum. Rejected because helper lookup would make the actual LFS executable implicit and would blur the binary allowlist and configuration boundary.

### D3: Stage, verify, and publish individual LFS objects

An enabled mirror run performs core Git acquisition and `fsck` first. It then creates `WORK_ROOT/runs/<run-id>/lfs`, points the typed LFS runner at that storage, fetches all refs, enumerates referenced OIDs, and verifies the staged store. A final recursive no-symlink scan enforces the pre-admitted stage ceiling before publication; the bounded enumeration enforces the object-count ceiling. Process cancellation is implemented at the typed runner boundary and owned run roots are removed after every terminal result.

For every referenced OID, Vault opens the expected standard LFS object path without following symlinks, streams SHA-256 and size verification, and publishes the file with create-only BlobStore semantics. The OID must equal the BlobRef digest. Entries are sorted by OID; the aggregate digest hashes canonical `oid size blob-digest` records. Identical objects naturally deduplicate across snapshots because their BlobRefs are content addressed.

Alternative considered: create a tar or zip LFS archive. Rejected because it adds an archive parser/dependency, duplicates already content-addressed bytes, and creates another format whose deterministic build and path traversal behavior would need independent proof.

### D4: LFS consumes the existing quota envelope before it can publish

Extend the mirror request with explicit LFS enablement and an LFS reservation inside its total conservative reservation. Admission still occurs before any Git process. The database reservation covers the target's active Git mirror plus net-new local LFS BlobStore bytes; content already present under the same BlobRef contributes zero net-new bytes. The configured stage ceiling is also bounded by the per-target/global limits and controls transient download space.

After collection, terminal evidence records Git bytes, staged LFS bytes, net-new published LFS bytes, object count, and released reservation. A refusal or observed overrun maps to `QuotaExceeded`, degrades the target, leaves prior mirrors/snapshots/LFS blobs untouched, and never invokes retention.

Alternative considered: fetch first and account afterward. Rejected because an attacker-controlled LFS set could consume the host before Vault decides it does not fit.

### D5: LFS evidence extends manifest version 1 without a parallel format

Add `LfsEvidence` and sorted `LfsObjectEvidence` to the existing manifest and its unsigned signature payload. `SnapshotManifest::new` receives either no LFS requirement or one complete collection tied to the selected successful mirror lifecycle run. `includes_lfs` is derived, never supplied separately. Object blobs publish before the signed manifest; the manifest publishes before the short database transaction, preserving the current orphan-safe ordering.

The manifest keeps `schema_version = 1` as required by development status. All constructors, readers, verifiers, replica planners, and tests change together; there is no old-manifest compatibility branch. Replica placement automatically includes every manifest-referenced LFS BlobRef alongside the bundle and manifest when offsite is required.

Alternative considered: retain the current manifest and store LFS only in database rows. Rejected because database state would not independently describe or restore the immutable snapshot.

### D6: Restore materializes the standard LFS object layout from BlobRefs

Verification first validates the signed manifest and each LFS BlobRef. The restore drill then creates the ordinary bare repository from the bundle, writes each verified object to `repository/lfs/objects/<2>/<2>/<oid>` using create-new files under the UUID-owned scratch root, and recomputes the aggregate digest directly from the immutable object set. Terminal reports and persistence gain expected/observed LFS counts, bytes, aggregate digests, and stage outcomes. The typed `fsck-objects` runner operation remains available for repository shapes where Git LFS supports it; the bare artifact drill does not rely on working-tree/HEAD semantics.

The live mirror root remains a denied operand and all source transports are disabled during the drill. Missing/corrupt LFS bytes fail the drill even when Git refs and connectivity pass.

Alternative considered: checkout files and let smudge restore content. Rejected because checkout executes repository-controlled attribute/filter behavior and proves less than direct object-set verification.

### D7: Wiki discovery is a controlled sibling derivation followed by the normal pipeline

For an enabled parent, Vault derives exactly one sibling source from the already validated source: `owner/repository.git` becomes `owner/repository.wiki.git`; deterministic `file://` fixtures use the same suffix rule. A typed `ls-remote` probe runs with the same credential boundary after parent access has succeeded. `Present` requires at least one valid ref. `Absent` is accepted only for a local missing fixture or a provider-specific not-found response paired with successful authenticated parent access in the same cycle; every ambiguous, authentication, timeout, or transport result is failure.

`schema.sql` adds `target_kind` (`repository` or `wiki`) and nullable `parent_target_id` with shape constraints and a unique wiki-per-parent rule. A wiki reuses the parent's provider external id but has its own internal target id, status, mirror path, lifecycle runs, quota, snapshots, replication, verification, and restore evidence. Its effective enrollment is derived from the parent's current `include_wiki` revision; it does not create an independent upstream policy source of truth. Discovery results are append-only collector runs. Repeated discovery uses upsert-by-relationship semantics and cannot create a duplicate sibling.

Alternative considered: store a wiki bundle as an artifact of the parent snapshot. Rejected because it couples two independent Git histories and prevents normal mirror health, quota, retry, and restore scheduling.

### D8: Persistence uses existing placeholders plus two focused evidence tables

Edit `schema.sql` in place. Reshape `collector_runs` into append-only Git-LFS/wiki discovery evidence tied to the owning target and optional related lifecycle/snapshot/child target, with collector-specific consistency constraints. Add `lfs_snapshot_objects` for the one-to-many immutable object evidence a snapshot manifest names and extend restore reports with LFS proof fields. Add target-kind/parent constraints described in D7. Terminal guard triggers follow the existing verification/replication pattern.

The collector constraint is narrowed to `git_lfs` and `wiki`. No releases/issues table, credential column, migration, or cross-schema foreign key is added.

### D9: End-to-end fixtures use real local Git and Git LFS

Extend test support to build deterministic local bare sources with fixed author/committer identity and timestamps. The LFS fixture commits at least two objects reachable from different refs, publishes its local LFS server content, and can selectively remove or corrupt one object. A sibling `.wiki.git` fixture contains an independent commit. Test setup resolves the host's `git-lfs` executable to an absolute path and passes it through production configuration; tests invoke it only through production runners and do not skip when it is missing, because the gate must prove the deployment prerequisite.

The primary acceptance test performs mirror plus LFS collection, manifest publication, source/mirror removal, and restore drill verification from BlobStore artifacts. Separate tests cover present/absent/failing wiki discovery, quota refusal before LFS publication, staging overrun cleanup, missing/corrupt objects, disabled policy, canonical manifest order, immutable persistence, and the closed collector inventory.

## Risks / Trade-offs

- [A repository can contain enormous numbers of tiny LFS pointers] → Enforce both byte and object-count ceilings before manifest construction; existing output and manifest-size bounds remain hard failures.
- [Directory scanning cannot provide a kernel-enforced aggregate quota on every development host] → Reserve before spawn, monitor run-owned staging during execution, kill on overrun, verify the final size before publication, and retain deployment filesystem quotas as defense in depth; never claim publication fit from reservation alone.
- [GitHub not-found output can conceal an authorization problem] → Classify absence only with successful parent access under the same credential context; otherwise fail closed and keep the requirement unsatisfied.
- [One BlobRef per LFS object increases manifest and database row counts] → Canonical bounded object lists avoid archive-format risk; the configured object ceiling bounds memory, JSON, SQL, and replica backlog work.
- [A wiki child can lag while the parent Git snapshot remains valid] → Preserve independent component health and last-good evidence; parent policy health is degraded until the required wiki child is verified, without invalidating the parent's already verified Git data.
- [Publishing some content-addressed objects before a later failure leaves unreferenced blobs] → Treat them as harmless immutable reusable bytes; retention will eventually own reclamation, and no snapshot claim exists until manifest and database commit succeed.

## Migration Plan

1. Install and verify the configured Git LFS executable on the deployment host, then deploy configuration with finite LFS limits.
2. Recreate the development database from the edited `schema.sql`; there is no migration or data-preservation path while development status applies.
3. Deploy the binary with LFS/wiki execution disabled by default, run the deterministic fixture suite and one operator-owned smoke target, then enable target policies explicitly.
4. Observe quota, missing-object, wiki-discovery, snapshot, replica, and restore evidence before treating an enabled target as healthy.

Rollback restores the prior binary/configuration and recreates the disposable database from the prior schema. Existing content-addressed LFS blobs may remain orphaned and immutable; rollback MUST NOT delete them or reinterpret an LFS-enabled snapshot as Git-complete.
