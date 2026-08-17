# Vault domain model

## Terms

- **Desired target:** versioned requested preservation policy for one source.
- **Mirror:** mutable local bare repository used to acquire Git objects/refs.
- **Snapshot:** immutable preservation artifact from a known mirror state.
- **Manifest:** signed/hash-addressed metadata describing source, refs, tools, artifacts, and verification.
- **Auxiliary collector:** LFS, wiki, releases, issues, or other non-core-Git data.
- **Verification:** checksum, Git object/ref, bundle, and remote-object checks.
- **Restore drill:** isolated reconstruction and comparison against manifest.
- **Tombstone:** logical retirement before physical deletion.

## Invariants

1. Desired and actual state are distinct and versioned.
2. Mutable mirrors are not immutable backups.
3. Snapshot artifacts and manifests never change in place.
4. `healthy` means the required verification/restore policy passed.
5. Unstar/deletion upstream does not imply immediate local deletion.
6. Pinned targets cannot be retired by automatic policy.
7. No untrusted repository command/config executes outside confinement.
