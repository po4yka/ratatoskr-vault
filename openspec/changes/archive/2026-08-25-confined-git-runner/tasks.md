# Tasks: confined-git-runner

Conventions: every behaviour is a RED/GREEN pair — the RED task adds a test that must fail for the reason stated (not for a compile error), the GREEN task makes exactly that test pass. Tests run with `cargo nextest run --locked -p ratatoskr-vault-gitrunner <name>` (nextest is available locally; plain `cargo test` behaves identically). No database is needed anywhere in this change.

## 1. Crate scaffold (crates/gitrunner)

- [x] 1.1 Create `crates/gitrunner` (`ratatoskr-vault-gitrunner`) with an empty documented `lib.rs`, wire it into workspace members and `[workspace.dependencies]`, add deps tokio/thiserror/tracing/libc. Cannot start from a failing test: scaffolding and manifest wiring are configuration, not behaviour. Verify: `cargo build -p ratatoskr-vault-gitrunner --locked` exits 0.

## 2. Subcommand allowlist (src/command.rs)

- [x] 2.1 RED: create `Subcommand` as a closed enum containing only `Version`, scaffold `GitRunner::run` to spawn unconditionally, and add `tests/allowlist.rs::disallowed_subcommand_is_refused_before_spawn`: config enables only `Version`, request `Fsck` against a sentinel "git binary" (a test-built helper executable that creates a marker file when executed); assert the call returns `DisallowedSubcommand` and the marker file does not exist. Confirm failure: the stub spawns anyway, so the marker exists.
- [x] 2.2 GREEN: gate every spawn behind the configured allowlist check before touching `Command`. Verify 2.1 passes.

## 3. Source URL validation (src/url.rs)

- [x] 3.1 RED: create `SourceUrl` accepting everything, and unit tests `dash_leading_url_is_refused`, `exotic_transport_scheme_is_refused`, `allowlisted_schemes_are_accepted` asserting `-x evil`, `ext::sh -c touch /tmp/pwned`, `gitserver::x`, `http://insecure` each yield validation errors while `https://…` and `file:///…` parse. Confirm failure: the stub accepts all inputs.
- [x] 3.2 GREEN: implement scheme allowlist {https, file} and dash-prefix rejection. Verify 3.1 passes.

## 4. Filesystem confinement (src/confine.rs)

- [x] 4.1 RED: create `ConfinedPath::new(root, relative)` returning everything unchanged, and tests in-module: `destination_outside_root_is_refused` (relative `../escape`) and `symlinked_intermediate_component_is_refused` (root/sub → symlink outside) each assert confinement errors; confirm failure on the pass-through stub.
- [x] 4.2 GREEN: canonicalize at use time and require the resolved path to stay under the resolved root; refuse symlinked intermediates leaving it. Verify 4.1 passes.
- [x] 4.3 RED: add `mirror_path_for_target` tests `provider_names_never_become_paths`: hostile display names (`../../pwned`, `/etc/passwd`, name with null byte) alongside a normal id must produce distinct identifier-derived paths inside `<root>/mirrors/<shard>/<id>.git` and never the hostile strings. Confirm failure: function does not exist yet compiles via stub returning the raw name.
- [x] 4.4 GREEN: derive layout from the internal id only. Verify 4.3 passes.

## 5. Hardened environment (src/env.rs)

- [x] 5.1 RED: scaffold `child_environment(run_dir) -> BTreeMap<OsString, OsString>` returning an empty map, and add `tests/bounds.rs::inherited_variable_never_reaches_child` using `/usr/bin/env` as probe binary through the runner: export `VAULT_HOSTILE=1` in the test process, assert the captured stdout contains no `VAULT_HOSTILE` but does contain `GIT_CONFIG_NOSYSTEM=1` and `GIT_TERMINAL_PROMPT=0`. Confirm failure: empty env means the guards are absent. *(Implemented as an exhaustiveness check against `CHILD_ENV_KEYS` instead of planting a variable: the workspace forbids unsafe, and edition 2024 makes `set_var` unsafe; the closed-set assertion proves the same property more strongly — ambient CI secrets were observed leaking under the stub.)*
- [x] 5.2 GREEN: build the exact documented environment map (PATH from config, HOME inside run dir, Git hardening guards, LANG). Verify 5.1 passes.

## 6. Hooks never execute (hostile suite)

- [x] 6.1 RED: add `tests/support/fixtures.rs` with the deterministic generator skeleton plus `hooked_mirror()` fixture, and `tests/hostile_repositories.rs::hooks_configured_in_mirror_never_execute`: mirror carries `core.hooksPath` pointing at a `reference-transaction` hook writing a marker file; run the runner's fetch updating a ref; assert success and marker absence anywhere under the root. Confirm failure: current spawn lacks the hooks override, so git executes the hook and the marker appears.
- [x] 6.2 GREEN: add `-c core.hooksPath=/dev/null` (and askpass/prompt guards) to every invocation. Verify 6.1 passes.

## 7. Bounded execution (src/spawn.rs)

- [x] 7.1 RED: add `tests/bounds.rs::hung_child_is_killed_at_deadline`: probe binary ignores arguments and reads stdin forever, deadline 500 ms; assert timeout error and that the child pid no longer exists after the call returns. Confirm failure: no deadline exists, call hangs — bounded by the test's own watchdog assertion of elapsed time exceeding the deadline without returning. *(Observed failure was stronger than predicted: with stdin inherited the hang-probe hit EOF and completed, so the run returned `Ok` outright — same missing-deadline root cause, observable instead of hanging.)*
- [x] 7.2 GREEN: wrap the wait loop in a timeout, kill the child process group (`libc::kill(-pid, SIGKILL)`), reap, return typed timeout. Verify 7.1 passes. *(Group kill goes through the safe `nix` wrappers instead of raw `libc`: this workspace forbids `unsafe`, and `libc::kill` is an unsafe extern call.)*
- [x] 7.3 RED: add `oversized_stdout_terminates_child_and_reports_stream` and `oversized_stderr_terminates_child_and_reports_stream`: probe emits cap+1 bytes; assert stream-specific overflow errors and retained length ≤ cap. Confirm failure: unbounded read returns Ok with full bytes.
- [x] 7.4 GREEN: bounded reader tasks per stream with kill-on-overflow. Verify 7.3 passes.
- [x] 7.5 RED: add `clean_completion_reports_structured_evidence`: probe exits 0 printing known bytes; assert exit code, both streams, truncation flags false, duration measured. Confirm failure: outcome type lacks the fields.
- [x] 7.6 GREEN: define `RunOutcome` fully and populate it. Verify 7.5 and earlier bounds tests pass together.

## 8. Credentials out of band (src/credentials.rs)

- [x] 8.1 RED: add `src/bin/git-credential-helper.rs` reading a secret file path argument and `tests/credentials.rs::helper_answers_credential_protocol_from_secret_file`: prepare a `0600` file with username/password lines, feed `url=https://example.com/repo.git\n\n` on stdin, assert stdout equals exactly `username=u\npassword=s\n`. Confirm failure: binary does not exist.
- [x] 8.2 GREEN: implement the helper endpoint and per-run secret-file materialization (`0600` in `0700` run dir, drop-guard deletion). Verify 8.1 passes.
- [x] 8.3 RED: add `secret_material_absent_from_argv_and_environment`: run with credentials attached against the env-printing probe; assert neither captured argv echo nor environment contains the secret bytes, and structurally assert the built command vector contains only the helper path + secret file path words around credentials. Confirm failure: credentials currently have no injection path, so the structural assertions cannot hold.
- [x] 8.4 GREEN: wire credential helper into argv (`-c credential.helper=<path> <file>`) and keep secrets out of env. Verify 8.3 passes.
- [x] 8.5 RED: add `leaked_secret_is_redacted_from_output`: probe echoes the secret onto stdout/stderr; assert delivered streams contain `[redacted]` and no raw secret. Confirm failure: no redaction exists.
- [x] 8.6 GREEN: scan captured streams against active-secret set before returning. Verify 8.5 passes.

## 9. Hostile repository suite completion

- [x] 9.1 RED: add fixture `deterministic_history()` and `tests/hostile_repositories.rs::generator_reproduces_identical_commit_ids`: build twice into different temp roots, assert identical `git rev-list --all` oids read via the runner. Confirm failure: generator not yet deterministic (uses wall-clock dates).
- [x] 9.2 GREEN: pin author/committer identities and timestamps in the generator. Verify 9.1 passes.
- [x] 9.3 RED: add fixtures `hostile_refnames()` and test `hostile_ref_names_survive_mirror_as_data`: names mixing shell metacharacters, leading dashes, Unicode, long segments; mirror then list refs via runner; assert byte-equal ref sets and no marker artifacts under root. Confirm failure: mirror op missing/unfiltered. *(Two fixture-validity failures came first — modern Git refuses `$(`-style refnames outright, and the initial branch cannot be recreated; both fixed in the fixture. Once valid, the property held immediately: containment is structural because no shell exists anywhere between intent and exec, which the allowlist and environment cycles pinned. Spec scenario wording updated to match Git's actual ref syntax.)*
- [x] 9.4 GREEN: complete clone-mirror and ref-listing builders over validated inputs. Verify 9.3 passes.
- [x] 9.5 RED: add fixture `large_blob_repo()` (~8 MiB pattern blob) and test `large_object_repository_verifies_within_bounds`: mirror, fsck via runner with defaults; assert success within deadline and caps. Confirm failure: fsck builder missing. *(The fsck builder already existed from section 2 scaffolding, so a behavioral RED was not observable; two assertion iterations failed instead — the first because zlib compressed the repeating pattern to kilobytes inside a packfile. Fixture now uses deterministic incompressible bytes and asserts logical object-set equality plus bulk physical size. Retained as regression coverage of the bounded-capture path under bulk data.)*
- [x] 9.6 GREEN: add `FsckFull` builder. Verify 9.5 passes.
- [x] 9.7 RED: add fixture `promisor_mirror()` (partial clone with missing blobs) and test `promisor_integrity_failure_is_classified_not_hung`: fsck via runner; assert integrity-classified failure with stderr excerpt, completing well inside deadline. Confirm failure: classification absent, error is generic. *(Confirmed exactly: `Ok(RunOutcome { exit_code: 2 })`. Building the fixture needed empirical grounding — modern Git ignores `--filter` on `--mirror` clones, so the fixture withdraws objects from a full mirror after marking it promisor; fsck then fails reliably while a *present* partial clone stays green by design.)*
- [x] 9.8 GREEN: classify fsck/bundle-verify nonzero exits as integrity failures carrying bounded excerpts. Verify 9.7 passes. *(fsck classified now; bundle verify joins at snapshot item 5 through the same match arm.)*

## 10. Documentation

- [x] 10.1 Update README.md status paragraph (runner implemented, plan items remaining shift), DEVELOPMENT.md "Git … commands" section with the runner's exact command surface and helper contract. Cannot start from a failing test: documentation. Verify: rendered text matches implementation (`rg` spot checks).

## 11. Full gate verification

- [x] 11.1 Run the DEVELOPMENT.md gate in order, requiring exit 0 from each: `cargo fetch --locked`; `cargo deny check`; `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo build --workspace --locked`; `cargo test --workspace --locked`; `cargo build --workspace --locked --release`. Standalone verification across all preceding tasks; outputs recorded as delivery evidence. *(All seven exit 0 locally; `cargo deny` reports advisories/bans/licenses/sources ok with `nix 0.30`; tests ran against a disposable Postgres exposed at 127.0.0.1:15440 via `VAULT_TEST_DATABASE_URL` because another project occupies host port 5432; the CI file-length step (`wc -l | awk > 850`) also passes per-file.)*
