# Developing Ratatoskr Vault

> Status: Proposed  
> Last reviewed: 2026-08-20

Architecture bootstrap: Vault workers, Git runner, storage adapters, schema, manifests, and restore tooling are not implemented.

## Intended toolchain

Rust/Tokio, system Git and Git LFS through structured process execution, SQLx/PostgreSQL, local/S3-compatible BlobStore, checksums/manifests, testcontainers, tracing, and isolated filesystem/process test harnesses.

## Code size limits

There is no code here yet, so no limit is enforced yet. The commit that brings the first manifest brings the configuration that carries the limits with it: `clippy.toml` beside a `Cargo.toml`, `eslint.config.js` beside a `package.json`. `fleet.yml` fails the gate when a manifest arrives without one, so the rule has a check behind it and not only this paragraph.

`ratatoskr-workspace/docs/QUALITY_GATES.md` holds the numbers the repositories with code use today, the command that measured each one, and the limits that were rejected with the reason. Read it before you choose numbers, then measure this tree. Each limit is set at the worst case the tree already has, so that the check fails on a regression and not on work that has not been done yet.

## Workflow

1. Start from versioned desired state; never infer policy from filesystem presence.
2. Treat every repository, ref, path, hook, filter, submodule, and object as hostile.
3. Run allowlisted Git commands without shell interpolation under bounded user/environment/time/disk/network controls.
4. Produce immutable artifacts and manifests, verify off-host copies, and execute restore drills.
5. Test interruption, retry, cancellation, quota, corruption, and deletion transitions.

The first scaffold PR must document exact build/test/migration/Git/LFS/storage/restore commands. Default tests use generated local repositories, never production credentials.

## What a clone needs before you plan a change

A change is planned with OpenSpec, which is a CLI a clone installs for itself. Use the version
`.github/workflows/openspec.yml` pins, so your terminal and the gate answer the same:

```bash
npm install --global @fission-ai/openspec@1.10.0
```

Cross-repository behaviour lives in a store, and registering one is per-machine state that no
repository can turn on for you — the same kind of step as `git config core.hooksPath .githooks`:

```bash
git clone git@github.com:po4yka/ratatoskr-workspace.git <path>
openspec store register <path> --id ratatoskr-workspace
```

`openspec doctor` reports whether both are in place.
