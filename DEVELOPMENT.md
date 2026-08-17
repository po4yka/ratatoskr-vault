# Developing Ratatoskr Vault

> Status: Proposed  
> Last reviewed: 2026-08-17

Architecture bootstrap: Vault workers, Git runner, storage adapters, schema, manifests, and restore tooling are not implemented.

## Intended toolchain

Rust/Tokio, system Git and Git LFS through structured process execution, SQLx/PostgreSQL, local/S3-compatible BlobStore, checksums/manifests, testcontainers, tracing, and isolated filesystem/process test harnesses.

## Workflow

1. Start from versioned desired state; never infer policy from filesystem presence.
2. Treat every repository, ref, path, hook, filter, submodule, and object as hostile.
3. Run allowlisted Git commands without shell interpolation under bounded user/environment/time/disk/network controls.
4. Produce immutable artifacts and manifests, verify off-host copies, and execute restore drills.
5. Test interruption, retry, cancellation, quota, corruption, and deletion transitions.

The first scaffold PR must document exact build/test/migration/Git/LFS/storage/restore commands. Default tests use generated local repositories, never production credentials.
