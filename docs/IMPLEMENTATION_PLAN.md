# Vault implementation plan

1. Scaffold service, typed config, telemetry, errors, health, and `git_vault` schema. *(implemented)*
2. Implement desired-state target reconciliation and state machine. *(implemented)*
3. Implement confined system-Git runner and generated hostile-repository tests. *(implemented)*
4. Implement mirror lifecycle with quotas, cancellation, and integrity checks. *(implemented)*
5. Create full Git bundles and immutable manifests in local BlobStore. *(implemented)*
6. Verify bundles/hashes and execute isolated restore drills. *(implemented)*
7. Add off-host S3-compatible replicas and verification. *(implemented)*
8. Add Git LFS, wiki, then separately approved auxiliary collectors. *(implemented: LFS and wiki;
   the approval gate exists and no provider-API auxiliary collector is approved or implemented)*
9. Implement retention, pinning, tombstones, staged deletion, and audit. *(implemented)*
10. Adopt legacy mirrors, verify/re-snapshot, and cut over from legacy jobs.

Definition of Done: one desired target is restored from an independently verified artifact; hostile
execution, corruption, interruption, retention, current-schema, and workspace integration checks
pass. Deferred: incremental bundle chains and broad GitHub issue/release archival until needed.
