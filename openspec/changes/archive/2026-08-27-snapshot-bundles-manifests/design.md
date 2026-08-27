## Context

See [proposal.md](proposal.md) for motivation and the delta specs for behavior. Item 4 leaves
healthy bare mirrors and immutable terminal `mirror_lifecycle_runs`; there is no BlobStore adapter,
snapshot worker, or snapshot persistence API today. The workspace `blob-references` spec requires
Vault to own its bytes under a content-addressed path and expose references rather than a shared
blob service.

## Goals / Non-Goals

**Goals:**

- Turn one successful, healthy mirror observation into a full all-ref bundle and immutable
  evidence records without mutating mirror evidence.
- Stream staged bundle and manifest bytes through a local, Vault-owned, content-addressed store
  with bounded sizes, atomic publication, and deterministic retry semantics.
- Keep the manifest independently parseable and complete enough for the later verification and
  restore slices.

**Non-Goals:**

- `git bundle verify`, production restore results, or transitions to `verified`/`restorable`.
- S3/off-host replicas, LFS/wiki/auxiliary collectors, retention, deletion, or a BlobStore HTTP
  service.
- Reusing arbitrary pre-existing filesystem files or accepting caller-controlled storage keys.

## Decisions

### A typed Vault-owned local BlobStore is a narrow filesystem adapter

Introduce a small `blobstore` workspace crate with an immutable `BlobRef` value and a
`LocalBlobStore` adapter. A BlobRef contains owner `ratatoskr-vault`, `sha256:<hex>`, media type,
and byte length; its filesystem location is derived solely by the adapter as
`<configured-root>/ratatoskr-vault/sha256/<hex>`. The adapter never acts as a network service and
never accepts a path from a caller.

`publish_file(expected_ref, source)` streams from a Vault-owned regular staging file into a
unique file in the configured BlobStore root while hashing and enforcing the configured finite
size cap. It compares the streamed bytes with the expected SHA-256 and length, fsyncs the file,
then atomically hard-links it into the content-addressed destination. `AlreadyExists` verifies the
existing immutable bytes against the claimed reference before returning the original BlobRef; a
different stream supplied for an existing claimed reference fails without modifying it. Temporary
files are cleaned only when owned by that publication attempt.

This is intentionally publish-after-build rather than piping Git stdout: Git creates the bundle
under a confined run root first, then the adapter streams it. It preserves Git's typed file
operation, permits a digest before publication, and follows the repository's required
build-in-temporary/publish-after-success pattern. A direct shared BlobStore client was rejected:
the fleet contract expressly has no blob service.

### Bundle generation remains inside the confined Git runner

Extend the closed Git operation enum with `bundle create <confined-output> --all`; retain the
existing typed `show-ref` operation for the manifest's ref evidence. The snapshot engine invokes
only those operations using a runner rooted in its owned staging directory. It parses `show-ref`
output strictly into object-ID/ref-name records, sorts them by raw ref name, and hashes an
unambiguous canonical newline-delimited representation. Ref strings are data only and never
become paths, commands, or keys.

The alternative of spawning `git` from the snapshot engine is rejected because it would create a
second execution-hardening path.

### Manifests use one canonical, self-contained JSON representation

Add pure snapshot model types: `SnapshotManifest`, `BundleEvidence`, `RefEvidence`, and
`BlobRef`. The manifest is serialized from ordered structures and contains a fixed manifest schema
identifier, generator package version, UTC timestamp, optional parent manifest BlobRef, sorted
ref/OID list plus ref-set digest, and bundle BlobRefs. The manifest's own BlobRef is computed from
its serialized bytes and stored separately. Fields for LFS, offsite placement, bundle verification,
and restore status are absent in this item, preventing false success claims.

JSON is chosen because its structure is inspectable for operators and already supported by the
workspace; strongly typed ordered values and no maps prevent unstable field or ref ordering. An
opaque binary manifest was rejected because the next restore slice needs independently inspectable
evidence.

### Snapshot persistence links an immutable input observation to immutable outputs

Edit the current `schema.sql` in place. Add a source `mirror_lifecycle_run_id` and optional
`parent_snapshot_id` to `snapshots`; add BlobRef owner, digest algorithm, media type, and byte
length data to artifacts and the manifest row. Use foreign keys, uniqueness, closed checks, and
append-only insert APIs so one snapshot has exactly one manifest and never points at an arbitrary
mirror state. The persistence API selects only a successful ready mirror observation and writes
the snapshot/artifact/manifest rows in one transaction after both BlobRefs are published.

Database rows remain metadata and references, not a substitute for the blob bytes. Orphaned
published bytes after a database failure are retained for a later retention/recovery slice rather
than deleted speculatively.

### The service composes, but does not yet schedule, snapshot work

Add a `SnapshotLifecycle` service module taking `Database`, snapshot settings, `GitRunner`, clock,
and `LocalBlobStore`. Its public request contains an internal target/mirror identifier only. It
selects the latest successful observation, creates the staged full bundle, captures refs, chooses
the latest manifest reference as the optional parent, publishes bundle then manifest, and appends
persistence records. Failures return typed errors, leave the mirror and prior snapshot evidence
unchanged, and do not claim verification. Tests call this bounded lifecycle directly; queue/event
scheduling arrives with later plan items.

## Risks / Trade-offs

- [A local full bundle can exhaust disk during staging] → Require separate absolute staging and
  BlobStore roots plus a positive per-artifact byte cap; stream with cap enforcement and remove
  only run-owned temporary files.
- [Content-address paths can be raced or tampered with locally] → Use confined roots, private
  permissions, create-only atomic publication, fsync, and digest/length re-check on duplicate
  resolution.
- [A bundle can be valid-looking yet fail restore] → The acceptance test clones from an emitted
  bundle, but no production verification status is introduced until item 6.
- [Schema persistence fails after artifact publication] → Leave content-addressed bytes intact;
  never delete unknown-orphan evidence in this change.
- [SHA-256 implementation correctness] → use the owner-approved, maintained `sha2` crate for
  bounded streaming SHA-256 rather than a custom digest implementation.

## Migration Plan

1. Add the adapter, model, runner operation, schema fields, persistence API, and lifecycle behind
   the new optional snapshot configuration; existing mirror-only deployments keep their behavior.
2. Run the fixture and disposable-database tests, then the exact `DEVELOPMENT.md` gate through
   `build-gate --` for every compiler-backed command.
3. Roll back by disabling snapshot configuration and redeploying the prior binary. Existing
   mirrors remain untouched; published content-addressed blobs are intentionally retained until a
   later authorized retention workflow manages them.
