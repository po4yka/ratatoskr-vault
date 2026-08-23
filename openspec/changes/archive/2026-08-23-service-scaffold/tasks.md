# Tasks: service-scaffold

## 1. Workspace scaffolding

- [x] 1.1 Create workspace manifests: root `Cargo.toml` (resolver 3, members, workspace lints, shared dependencies), `rust-toolchain.toml` (1.97.0 + rustfmt + clippy), `rustfmt.toml`, `clippy.toml` (fleet size limits: functions 100, args 7, nesting 5; test allowances), `deny.toml`. Verify: `cargo fetch --locked` succeeds and `rustup show` reports the pinned toolchain. (No failing test possible: configuration and generated files.)
- [x] 1.2 Create empty crate skeletons for `crates/core`, `crates/telemetry`, `crates/persistence`, `crates/http`, `services/vault` with package manifests wiring workspace lints and `lib.rs`/`main.rs` stubs. Verify: `cargo build --workspace --locked` compiles.

## 2. Configuration (tests first)

- [x] 2.1 Add `crates/core/tests/config_strictness.rs` with tests that must fail before implementation: nested env populates `admin.bind`; unknown key (`RATATOSKR__NO_SUCH_SECTION__X`) fails extraction naming the key; wrong type on `RATATOSKR__ADMIN__BIND` fails naming `admin.bind`; invalid log filter is a validation violation naming `telemetry.log_filter`; drain+grace above ceiling reports violations; two simultaneous violations both appear in one report; database URL secret never appears in report or debug rendering. Verify: tests fail for the stated behavioral reason.
- [x] 2.2 Implement `vault-core::config`: model tree with `deny_unknown_fields`, figment loader (`RATATOSKR__` prefix, `__` split), defaults per design D3, validation collecting all violations with static strings only, value-free report rendering, exit code 78. Verify: `cargo test -p ratatoskr-vault-core --test config_strictness` green.

## 3. Error taxonomy

- [x] 3.1 Add `crates/core/tests/error_taxonomy.rs`: every failure class carries a stable code plus its failure group (transient / action-required / integrity), from which retryability is derived; subsystem labels are a closed lowercase set. (Revised during apply: Vault serves no client-facing surface at this milestone, so there is no HTTP status to carry; the group taxonomy is what AGENTS.md's failure model names.) Verify: fails before implementation.
- [x] 3.2 Implement `vault-core::error` per design D5. Verify: taxonomy test green.

## 4. Telemetry

- [x] 4.1 Add `crates/telemetry` unit tests: OTLP header secret reaches metadata but never renders in config debug or exporter errors; identity constants agree with manifest (`SERVICE_NAME = "ratatoskr-vault"`); an https endpoint builds an exporter without a collector. Verify: redaction test fails before implementation.
- [x] 4.2 Implement `vault-telemetry::init` (layer order per D4), `TelemetryGuard`, wire identity, Prometheus recorder install. Verify: telemetry tests green.

## 5. Persistence and schema

- [x] 5.1 Write `schema.sql` at repository root: `git_vault` schema, fifteen tables from ARCHITECTURE.md section 4, fleet conventions (app-minted UUID PKs without DEFAULT, checked-text vocabularies encoding target/snapshot state machines, timestamptz, no cross-schema foreign keys). This is data definition; verified by the tests in 5.2 rather than its own failing test.
- [x] 5.2 Add `crates/persistence` integration test (feature-gated dev-deps): disposable database gets schema applied; catalogue shows all fifteen tables; second apply succeeds changing nothing; inserting a target row with out-of-vocabulary status is rejected by constraint; ping answers. Verify: fails before implementation exists.
- [x] 5.3 Implement `Database` (connect with acquire timeout, advisory-locked apply-if-absent, ping, close) and `test_support::TestDatabase` behind `test-support`. Verify: persistence tests green.

## 6. HTTP harness and admin plane

- [x] 6.1 Add `crates/http/tests/admin.rs`: liveness answers 200 during drain; readiness 503 before startup completes then 200 after; configured-down database yields failed `database` check with `dependency_unavailable`; two consecutive readiness bodies byte-identical; metrics returns exposition content type with build-info series; version carries service/version/git_sha/rust_version; unknown path sets `Cache-Control: no-store`. Verify: tests fail before implementation.
- [x] 6.2 Implement `RuntimeState` (startup/drain/database tri-state), `admin_router` with no-store middleware, graceful shutdown helpers (`serve`, `drain_and_close`, signal handling), and the shared `run` lifecycle returning documented exit codes. Verify: admin tests green.

## 7. Service binary

- [x] 7.1 Add `services/vault/tests/boot.rs`: spawn the built binary against a disposable database (env-driven), probe `/health/live` 200, `/health/ready` 200 with database check pass after startup, `/version` fields correct; SIGTERM exits 0; with an invalid environment it exits 78 having bound nothing. Verify: fails before wiring.
- [x] 7.2 Implement `services/vault/src/main.rs`: lifecycle build step connecting the database when configured, applying the schema, refusing startup when the database is unreachable; `check-config` mode. Verify: boot test green.

## 8. Gate and documentation

- [x] 8.1 Add `.github/workflows/ci.yml` per design D8 and the identical command list as the fenced block under `### Rust — also the CI gate` in DEVELOPMENT.md, plus build/test/run instructions and local Postgres setup. Verify: the drift-guard awk over ci.yml equals DEVELOPMENT.md's block.
- [x] 8.2 Add `.env.example` documenting every variable and default; update README status block to describe what now exists and what still does not. Verify: every variable in `.env.example` appears in the config model and vice versa.
- [x] 8.3 Run the full gate locally in order: `cargo fetch --locked`, `cargo deny check`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, file-length check, `cargo build --workspace --locked`, `cargo test --workspace --locked`, `cargo build --workspace --locked --release`. Verify: every command exits 0 against the local Postgres.

## 9. Archive

- [x] 9.1 `openspec validate --change service-scaffold --strict` passes, tasks all ticked, change archived folding deltas into `openspec/specs/`. Verify: `openspec validate --archived` passes.
