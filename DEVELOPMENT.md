# Developing Ratatoskr Vault

> Status: Implemented (foundation, reconciliation, confined Git runner, local mirror lifecycle, local bundle snapshots)
> Last reviewed: 2026-08-25

The service foundation exists: a Rust workspace with typed configuration, telemetry, the admin plane, and the first version of the `git_vault` schema. Desired-state reconciliation converges delivered policies into guarded target state, `crates/gitrunner` executes the system Git binary under structural confinement, and the local lifecycle performs initial clone and periodic fetch with four permits, finite byte reservations, cancellation checkpoints, and post-operation integrity evidence. Local immutable bundle snapshots and manifests are implemented; retention, off-host storage, and eventing remain later plan items.

## Toolchain

Rust/Tokio on the toolchain `rust-toolchain.toml` pins (1.97.0, with rustfmt and clippy). rustup installs it on first use; nothing else is required to build.

## Local dependencies

A PostgreSQL server is needed by every persistence test and by the boot test:

```bash
docker compose up -d
```

`compose.yaml` serves PostgreSQL 17 on 5432 with user/password/database `vault`, byte-identical to `.env.example`, the default in `crates/persistence/src/test_support.rs`, and CI. If another Ratatoskr repository's postgres occupies 5432, either stop it or point the suite elsewhere with `VAULT_TEST_DATABASE_URL`. No NATS is required. Snapshot tests use a temporary local filesystem BlobStore and never contact an off-host object store.

### Rust — also the CI gate

The gate is `.github/workflows/ci.yml`; this list and that job's `run:` steps are one list, and a step in ci.yml fails when they differ. Run them in this order before declaring any change done:

```bash
cargo fetch --locked
cargo deny check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
cargo build --workspace --locked --release
```

Notes on individual steps:

- `cargo deny check` reads RustSec advisories, licences, duplicate versions and the source policy (`deny.toml`). Install once with `cargo install cargo-deny`.
- `cargo clippy` carries the size limits from `clippy.toml`: functions at most 100 code lines, signatures at most 7 arguments, blocks at most 5 deep. An exception is `#[expect(clippy::too_many_lines, reason = "...")]` at the site, never a raised number.
- The file-length limit (850 tracked `.rs` lines) runs as the awk step between clippy and build in ci.yml; it has no local equivalent because it reads `git ls-files`.
- The tests need the database above; they fail loudly when it is unreachable rather than skipping.

## Running the service

```bash
export RATATOSKR__DATABASE__URL="postgres://vault:vault@127.0.0.1:5432/vault"
cargo run -p ratatoskr-vault
```

The operator listener binds `127.0.0.1:9570` by default:

- `GET /health/live` — liveness; consults nothing external;
- `GET /health/ready` — readiness with named checks (startup, drain, database when configured);
- `GET /metrics` — Prometheus text exposition;
- `GET /version` — build identity.

Validate an environment without starting anything:

```bash
ratatoskr-vault check-config   # exit 0 valid, exit 78 invalid
```

Exit codes: `0` clean run, `1` runtime startup failure (telemetry, route build, bind), `78` `EX_CONFIG`.

## Schema

One file: `schema.sql` at the repository root, applied by the binary at startup to a fresh database, embedded at compile time into `ratatoskr-vault-persistence`. There are no migrations and no migration tooling: a schema change edits `schema.sql` in place, and a test database is created from that definition (`TestDatabase::create`). While the development status holds this is binding; see README.

To reset a dirty local database: drop it (`docker compose down -v` recreates the cluster) — there is no upgrade path to preserve.

## Git commands (runner, plan item 3)

`crates/gitrunner` owns every Git process. Operations are typed argument vectors against a closed `Subcommand` allowlist; there is no shell anywhere between intent and `exec`. The exact command surface today:

```text
git -c core.hooksPath=/dev/null clone --mirror <source-url> <confined-destination>
git -c core.hooksPath=/dev/null fetch <source-url> +refs/*:refs/*
git -c core.hooksPath=/dev/null fsck --full
git -c core.hooksPath=/dev/null rev-list [--all | --objects --all]
git -c core.hooksPath=/dev/null show-ref
git version
```

Every child also receives a constructed environment: minimal `PATH`, `HOME` inside the run directory, `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_SYSTEM=/dev/null`, `GIT_TERMINAL_PROMPT=0`, `GIT_ASKPASS=/dev/null`, `SSH_ASKPASS=/dev/null`, pager disabled, `LANG=C.UTF-8`. Nothing is inherited from Vault's own process.

Source URLs accept only `https` and `file` schemes and must not begin with `-`. Destinations resolve inside Vault-owned roots at use time; intermediate symlinks leaving a root are refused; mirror paths derive from internal target ids (`<root>/mirrors/<shard>/<id>.git`). Each invocation carries a wall-clock deadline and per-stream output caps; overruns SIGKILL the child's process group (safe `nix` wrappers — this workspace forbids `unsafe`). A nonzero `fsck` becomes `GitRunnerError::IntegrityCheckFailed` with a bounded redacted excerpt.

Credentials travel out of band through the Git credential-helper contract:

```text
git -c credential.helper=<path-to-git-credential-helper> <secret-file-path> fetch …
```

The helper binary (`ratatoskr-vault-gitrunner` ships it as `git-credential-helper`) reads an owner-only secret file inside an owner-only run directory and answers the credential protocol on stdout. Secrets never appear in argv or environment blocks; captured output is scanned against active secret material before leaving the runner; the secret file is deleted when the operation ends. The trade-off — a brief `0600` file instead of fd passing — is recorded in the change design because fd inheritance beyond stdio is not expressible under the `unsafe` ban.

LFS collection, production bundle verification, and restore drills are later plan items. Local bundle creation uses the typed `git bundle create <confined-output> --all` operation and streams completed staging artifacts into Vault-owned content-addressed BlobStore storage.

## Local mirror lifecycle (plan item 4)

Set the `RATATOSKR__MIRROR__*` section only on a host that may execute mirror work. It requires two
distinct absolute roots, positive finite per-mirror and global byte budgets, and exactly four
concurrent-operation permits — the Pi deployment target has four CPU cores. Each operation takes
a durable reservation before Git starts. Refusal marks the target degraded and creates evidence;
it never prunes existing data. Initial clones use `WORK_ROOT/runs/<run-id>/` and rename only a
verified bare repository into `ROOT/mirrors/<shard>/<target-id>.git`. Cancellation cleans that
owned staging directory. Fetch cancellation retains the previous mirror and records a resumable
`fetch_pending` checkpoint. Clone and fetch both run fsck, show-ref, and object enumeration; an
integrity failure degrades the target without overwriting its prior successful observation.

## Tests

```bash
cargo test --workspace --locked          # unit + integration + the binary boot test
cargo test -p ratatoskr-vault-core       # configuration strictness and the error taxonomy only
cargo test -p ratatoskr-vault-gitrunner  # confinement, bounds, credentials, hostile repositories
```

The gitrunner suite needs no database and no network: fixtures are generated local repositories under temporary roots, and the bounds tests substitute a probe executable for the trusted Git binary path.

Integration tests create their own disposable databases (`vault_test_<uuid>`) against the server `VAULT_TEST_DATABASE_URL` names, apply the schema themselves, and drop them on success. A panicking test leaves its database behind on purpose, for post-mortem.

## Workflow

1. Start from versioned desired state; never infer policy from filesystem presence.
2. Treat every repository, ref, path, hook, filter, submodule, and object as hostile.
3. Produce immutable artifacts and manifests, verify off-host copies, and execute restore drills (later milestones).
4. Test interruption, retry, cancellation, quota, corruption, and deletion transitions as those features arrive.

Default tests use generated local repositories, never production credentials.

## What a clone needs before you plan a change

A change is planned with OpenSpec, which is a CLI a clone installs for itself. Use the version `.github/workflows/openspec.yml` pins, so your terminal and the gate answer the same:

```bash
npm install --global @fission-ai/openspec@1.10.0
```

Cross-repository behaviour lives in a store, and registering one is per-machine state that no repository can turn on for you — the same kind of step as `git config core.hooksPath .githooks`:

```bash
git clone git@github.com:po4yka/ratatoskr-workspace.git <path>
openspec store register <path> --id ratatoskr-workspace
```

`openspec doctor` reports whether both are in place.

## The Rust skills in this repository

`.agents/skills/` holds eighteen Rust skills vendored from `po4yka/rust-skills`, and `.claude/skills/` symlinks to them. Unlike the steps above this needs nothing from your machine: the files are in the tree, so a fresh clone already has them.

Update them with the catalogue and never by hand:

```bash
npx skills update
```

That rewrites `.agents/skills/` and `skills-lock.json` from the catalogue. Run it in one repository, read the diff, then apply the same change to every Ratatoskr repository whose stack is Rust. `ratatoskr-workspace/.github/workflows/drift.yml` fails when one copy differs from the others.
