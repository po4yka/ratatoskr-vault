# Proposal: service-scaffold

## Why

Vault is in architecture bootstrap: the repository holds intent documents and no code. Implementation plan item 1 is the scaffold every later milestone stands on. Without it there is no process to run, no typed configuration to refuse bad deployments, no telemetry, no health surface for an operator or a supervisor to probe, no first-version `git_vault` schema, and no gate that keeps the tree honest as code arrives.

## What Changes

- Add a Rust workspace following the fleet conventions (`crates/` + `services/` layout) with the pinned toolchain, workspace lints, `clippy.toml` size limits, `deny.toml`, `rustfmt.toml`, and a locked build.
- Add crates: `vault-core` (typed configuration with strict validation, runtime role, error taxonomy), `vault-telemetry` (tracing subscriber, optional OTLP span export, Prometheus recorder, wire identity), `vault-persistence` (SQLx pool, embedded `schema.sql`, readiness ping), `vault-http` (process lifecycle: config load, telemetry init, admin listener, drain-and-close), and a test-support surface behind a feature flag.
- Add the `services/vault` deployable: one binary whose `main` is the shared lifecycle; refuses to start on invalid configuration (exit 78), applies the schema when a database is configured.
- Add `schema.sql` at the repository root: the first version of the `git_vault` schema covering the tables ARCHITECTURE.md section 4 names, in place (no migration ledger), placeholder-safe but valid.
- Add an admin plane on the operator listener: `/health/live`, `/health/ready`, `/metrics`, `/version`, each answering with stable JSON/Prometheus bodies and `Cache-Control: no-store`.
- Add `.github/workflows/ci.yml` (the repository gate) and the matching command list in DEVELOPMENT.md; add `.env.example`; update README.md status to describe what now exists.

Out of scope (later plan items): mirror workers, Git execution, snapshot/manifest engines, retention, off-host storage, event bus consumption, public API routes.

## Capabilities

### New Capabilities

- `service-runtime`: The process lifecycle of the `ratatoskr-vault` binary - configuration loading and refusal, telemetry installation, listener startup, graceful shutdown, exit codes.
- `configuration`: The typed configuration tree, its environment naming scheme, strictness (unknown keys refused), and startup validation rules reported without echoing values.
- `operator-health`: The admin-plane endpoints `/health/live`, `/health/ready`, `/metrics`, `/version` and what each must answer.
- `git-vault-schema`: The first version of the `git_vault` PostgreSQL schema applied from one `schema.sql`, idempotently, with no migration history.

### Modified Capabilities

None. `openspec/specs/` starts empty by design; this change creates the first capabilities.

## Impact

- New code: workspace manifests, five new crate/binary locations, `schema.sql`, CI workflow, DEVELOPMENT.md command list, README status block, `.env.example`.
- Dependencies introduced at first version: tokio, axum (+tower/tower-http), figment, secrecy, url, sqlx (postgres), tracing/tracing-subscriber/tracing-opentelemetry/opentelemetry(+otlp), metrics-exporter-prometheus, uuid, thiserror, serde, time/jiff, http.
- Fleet gates affected: `fleet.yml` begins enforcing "manifest implies ci.yml", "ci.yml runs tests", and "Cargo.toml implies clippy.toml" from this commit; all three are satisfied by it.
- No database holds data that must survive a schema change; the schema is edited in place going forward.
