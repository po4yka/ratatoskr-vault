# Vault requirements

## Goals

1. Reconcile versioned desired preservation state into verifiable actual state.
2. Maintain bare mirrors, optional LFS/auxiliary archives, immutable snapshots, and manifests.
3. Verify local and off-host artifacts and prove restore in isolation.
4. Apply retention and deletion safely, with pinning and grace periods.
5. Isolate untrusted Git execution and resource usage.

## Non-goals

Owning GitHub stars/lists/metadata, LLM analysis, assuming a mirror alone is a backup, or deleting immediately after upstream unstar/removal.

## Requirements

- Desired state version and target identity make reconciliation idempotent.
- Git execution is structured, allowlisted, bounded, cancellable, and hook/filter-safe.
- Snapshots are immutable and content-addressed with complete manifests.
- `fsck`, bundle verification, hashes, upload verification, and restore result are recorded separately.
- A backup is healthy only under its policy's verification/restore SLA.
- Pinning overrides automatic retirement; deletion is staged and auditable.

First slice: desired `git_mirror` target -> mirror -> bundle+manifest -> verify -> isolated restore -> healthy result event.
