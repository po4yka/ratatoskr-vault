# Design: service-scaffold

## Context

The repository holds intent documents and no code. The fleet conventions are established in `ratatoskr-platform` (workspace layout, gate, size limits, lifecycle shape) and this change copies the proven shape rather than inventing one. Development status is binding: one version only, no migrations, product name Ratatoskr.

## Goals / Non-Goals

Goals:

- a workspace that compiles locked and passes its own gate locally and in CI;
- configuration strictness that fails closed with value-free reports;
- an honest health surface (liveness independent of dependencies; readiness that names failures);
- a first-version `git_vault` schema that can be applied to a fresh database idempotently;
- tests written first for every behavior in the specs.

Non-goals: mirror workers, Git process execution, snapshots/manifests, retention, off-host storage, NATS eventing, public API routes. The design leaves seams for all of them but implements none.

## Decisions

### D1. Workspace shape mirrors ratatoskr-platform

`crates/{core,telemetry,persistence,http}` + `services/vault`, workspace-level lints (`unsafe_code = forbid`, `missing_docs`, deny `unwrap_used`/`expect_used`/`panic`/`indexing_slicing`/`string_slice`), edition 2024, resolver 3, pinned toolchain 1.97.0 matching platform's pin so one rustup install serves both.

Alternative considered: fewer crates (one crate). Rejected: platform's split keeps axum out of persistence and telemetry, which matters as soon as workers import them without an HTTP server.

### D2. One deployable role, no role enum yet

Platform has three roles because it has three binaries. Vault has one binary; ARCHITECTURE.md section 25 says worker splits come later. The config tree therefore has no `public` table and no role switch: `admin.bind`, optional `database`, `shutdown`, `telemetry`. Adding roles later is an additive config change while no deployment exists.

### D3. Configuration: figment over defaults + env, `deny_unknown_fields`

Identical mechanism to platform: `Serialized::defaults` then `Env::prefixed("RATATOSKR__").split("__")`; serde `deny_unknown_fields` on every node makes unknown keys fail extraction. Validation collects every violation into `Vec<Violation>` whose members are all `&'static str`, so a supplied value structurally cannot reach the report. The database URL is `SecretString` with `skip_serializing`.

Vault-specific rules at this milestone: admin bind must be loopback by default (warning when not), drain/grace within the supervisor ceiling (same numbers as platform: 60s drain, 120s total), log filter must parse, database URL must parse as a URL when present.

### D4. Telemetry: same layer order as platform

Registry → EnvFilter → OpenTelemetry layer → fmt layer (JSON default, pretty for dev). OTLP optional; absent exporter still mints real W3C trace ids. Prometheus recorder installed once, rendered by the admin route via handle closure (no second HTTP server). Wire identity constants live here: `SERVICE_NAME = "ratatoskr-vault"`, version/git-SHA/toolchain from env at compile time.

### D5. Errors: typed taxonomy without wire contracts yet

`thiserror` hierarchy: `ConfigError` (source/invalid), `TelemetryError`, `PersistenceError`, plus a top-level internal classification reserved for later HTTP mapping. Platform's client-facing `ErrorEnvelope` machinery depends on `ratatoskr-contracts` git dependency; Vault has no public API surface at this milestone, so importing contracts now would couple the scaffold to a dependency nothing consumes. The taxonomy is shaped so the contracts-based envelope can slot in at the milestone that adds public routes.

### D6. Persistence: embedded schema, advisory lock, runtime-checked queries

Same as platform ADR-0004: `include_str!("../../../schema.sql")`, transaction-scoped advisory lock, apply-if-`to_regnamespace('git_vault')`-is-null, plain `sqlx::query` (no compile-time macros) so integration tests against real PostgreSQL are the checker. Test support behind a `test-support` feature creates a disposable per-test database (ICU locale pinned like platform) and drops it explicitly.

Schema content: the fifteen tables ARCHITECTURE.md section 4 names. Placeholder-safe: columns carry identity, state vocabulary (checked text), timestamps, and correlation fields; detailed columns arrive with the features that need them. CHECK constraints encode the target and snapshot state machines from AGENTS.md so invalid states are refusable by the database today. UUIDv7 minted by the application, no DEFAULT on id columns; no cross-schema foreign keys (references to catalog identity are unenforced uuid columns).

### D7. HTTP harness: admin-only listener, shared lifecycle trait

`vault_http::run(build)` owns the whole sequence (config → telemetry → build → bind → mark ready → signal → drain → close). The service contributes a `build` function returning routes + optional database + tasks. Admin router is the four routes with `no-store` middleware; readiness state machine copied from platform's proven `RuntimeState` (startup/drain booleans, tri-state database) minus the bus.

### D8. Gate: ci.yml and DEVELOPMENT.md are one list

Copy platform's ci.yml minus NATS: postgres service container (pinned digest), env `VAULT_TEST_DATABASE_URL`, then `cargo fetch --locked`, `cargo deny check`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, file-length check, `cargo build --workspace --locked`, `cargo test --workspace --locked`, `cargo build --workspace --locked --release`, and the drift guard comparing the job's cargo lines to DEVELOPMENT.md's fenced block under `### Rust — also the CI gate`.

Size limits: adopt the measured fleet numbers unchanged (function 100 / args 7 / nesting 5 / file 850). This tree starts far below all of them; the numbers are fleet-consistent ratchets, not local ambitions.

### D9. Tests first

Each spec scenario maps to a named test. Order of work per behavior: write the test, watch it fail for the stated reason (minimal stubs make it compile), implement until green. Config strictness, error taxonomy, telemetry redaction, admin routes, schema apply/state checks, and a binary boot test (spawn the built binary against a disposable database, probe `/health/live`, `/health/ready`, `/version`) cover every scenario.

## Risks / Trade-offs

[Toolchain 1.97.0 must download on first build] → pinned in `rust-toolchain.toml`; CI installs it via rustup automatically.
[Postgres-dependent tests fail without a server] → deliberate (platform convention): they fail loudly, not skip; compose.yaml-style local setup documented in DEVELOPMENT.md.
[Placeholder schema columns may need widening later] → acceptable under development status: schema edits in place while no data survives.
[Admin plane without auth on non-loopback binds] → default is loopback; startup warns on any wider bind, matching platform until Vault gets a deployment profile.

## Migration Plan

Nothing to migrate: first code lands on an empty repository; the schema applies only to fresh databases. Rollback is reverting the branch.

## Open Questions

None.
