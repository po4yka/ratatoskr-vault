## Why

Core Git bundles do not contain Git LFS object bytes or a repository's separate wiki repository, so the current verified snapshot can still be incomplete for targets that explicitly require those assets. Plan item 8 closes those two preservation gaps while keeping every later provider auxiliary collector behind an explicit owner-approval boundary.

## What Changes

- Collect all referenced Git LFS objects during an enabled target's mirror lifecycle, account for their stored bytes in the existing finite quota model, and fail with durable degraded evidence when required objects are missing or cannot fit.
- Publish immutable content-addressed LFS artifacts and extend the existing schema-version-1 snapshot manifest with deterministic LFS object and aggregate digests.
- Extend restore drills to reconstruct and verify the LFS object store from stored artifacts without source-network access before reporting an LFS-enabled snapshot restorable.
- Discover an enabled repository wiki through a bounded, typed remote probe and mirror a present wiki as an explicitly related sibling target with its own lifecycle, snapshots, verification, quota, and health evidence.
- Add a closed auxiliary-collector registration and documentation pattern that requires a separately owner-approved OpenSpec change before any provider collector becomes executable. No releases, release-assets, issues, or other provider API collector is implemented by this change.

## Capabilities

### New Capabilities

- `lfs-wiki-collection`: Explicitly enabled Git LFS preservation, wiki sibling discovery/mirroring, and the closed approval gate for future auxiliary collectors.

### Modified Capabilities

- `configuration`: Require an explicit Git LFS executable and finite LFS staging limit when LFS collection is enabled for the process.
- `git-execution`: Add typed, confined Git LFS operations and a bounded wiki-remote discovery probe without shell execution or inherited unsafe configuration.
- `mirror-lifecycle`: Admit LFS bytes through the existing quota boundary and reconcile a discovered wiki through an independently evidenced sibling lifecycle.
- `snapshot-bundles`: Record immutable LFS artifacts and deterministic content digests in the existing manifest version without weakening Git bundle evidence.
- `restore-verification`: Require artifact-only LFS reconstruction and content verification before an LFS-enabled snapshot can be reported restorable.
- `git-vault-schema`: Persist LFS artifact/object evidence, wiki sibling relationships, and only the collector kinds executable in this approved slice in the single current schema definition.

## Impact

The change affects the core evidence model, `schema.sql` and PostgreSQL operations, typed process execution, mirror/snapshot/restore orchestration, local BlobStore publication, quota accounting, telemetry, synthetic Git/LFS/wiki fixtures, and operator documentation. It adds a deployment prerequisite on the system `git-lfs` binary for targets that enable LFS, but introduces no provider API credential, cross-repository contract, migration, later API version, or provider metadata ownership.
