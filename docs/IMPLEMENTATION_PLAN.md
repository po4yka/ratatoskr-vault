# Vault implementation plan

1. Scaffold service, typed config, telemetry, errors, health, and `git_vault` schema. *(implemented)*
2. Implement desired-state target reconciliation and state machine.
3. Implement confined system-Git runner and generated hostile-repository tests.
4. Implement mirror lifecycle with quotas, cancellation, and integrity checks.
5. Create full Git bundles and immutable manifests in local BlobStore.
6. Verify bundles/hashes and execute isolated restore drills.
7. Add off-host S3-compatible replicas and verification.
8. Add Git LFS, wiki, then separately approved auxiliary collectors.
9. Implement retention, pinning, tombstones, staged deletion, and audit.
10. Adopt legacy mirrors, verify/re-snapshot, and cut over from legacy jobs.

Definition of Done: one desired target is restored from an independently verified artifact; hostile
execution, corruption, interruption, retention, current-schema, and workspace integration checks
pass. Deferred: incremental bundle chains and broad GitHub issue/release archival until needed.
