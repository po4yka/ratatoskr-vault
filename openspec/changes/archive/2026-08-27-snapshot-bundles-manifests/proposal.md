## Why

A healthy bare mirror is mutable local state: corruption after a successful fetch can silently
destroy the only preserved copy. Vault needs immutable, portable point-in-time evidence before
later verification, restore, and off-host work can make honest preservation claims.

## What Changes

- Create a snapshot pipeline that builds a full `git bundle --all` from a healthy bare mirror at a
  snapshot point and stores the bytes in Vault's local content-addressed BlobStore.
- Define a canonical cryptographic snapshot manifest containing bundle BlobRefs and digests, the
  complete ref/OID set and digest, generator version, timestamp, and optional parent snapshot
  linkage; publish the manifest through the same write-once BlobStore semantics.
- Persist append-only snapshot, artifact, and manifest records that link the source mirror
  observation to its immutable BlobRefs.
- Extend the confined Git runner only with typed, allowlisted operations required to create a full
  bundle and enumerate refs. Restore-drill verification and off-host replicas remain out of scope.

## Capabilities

### New Capabilities

- `snapshot-bundles`: Creation and immutable local storage of full Git-bundle snapshot evidence
  and its complete manifest.

### Modified Capabilities

- `git-execution`: Add typed, non-shell Git bundle construction and ref enumeration operations.
- `git-vault-schema`: Record snapshot artifacts and manifest BlobRefs with immutable linkage to
  the mirror observation.
- `mirror-lifecycle`: Schedule a snapshot only from a healthy mirror observation without changing
  its completed mirror evidence.

## Impact

- Affects the `core`, `gitrunner`, `persistence`, and service composition crates, `schema.sql`,
  the local BlobStore adapter, synthetic Git fixtures, and the development/CI gate.
- Conforms to the workspace `blob-references` contract; it introduces no BlobStore service API,
  off-host adapter, LFS collector, or restore-drill result.
